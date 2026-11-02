//! 受信済みL2板による受付近似と、価格レベル共有のFIFO数量モデル。
use super::*;
use crate::{depth::Depth, market_event::MarketDepthSnapshot};
use rust_decimal::prelude::FromPrimitive;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PaperModelUnavailable {
    MissingBook,
    PriceOutsideVisibleBook,
    UnsupportedMarketMode,
    UnsupportedTaker,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PaperBookEvidence {
    pub exchange_at_ms: i64,
    pub received_at_ms: i64,
    pub best_bid: Decimal,
    pub best_ask: Decimal,
    pub lowest_bid: Decimal,
    pub highest_ask: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PaperQueueLevel {
    pub side: OrderSide,
    pub price: Decimal,
    pub segments: Vec<PaperQueueSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PaperQueueSegment {
    /// この数量/注文が列へ入る取引所時計。過去成立の取引で消化しない。
    pub effective_after_ms: i64,
    pub external_amount: Decimal,
    pub order_id: Option<OrderId>,
}

#[derive(Debug, Clone)]
struct Book {
    evidence: PaperBookEvidence,
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
}

type LevelKey = (OrderSide, Decimal);

#[derive(Debug)]
struct Level {
    displayed: Decimal,
    segments: Vec<PaperQueueSegment>,
}

/// キュー本体をコピーせず取得できる保持量。空の価格根拠は retained_levels に含む。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct PaperQueueDiagnostics {
    /// 生存自注文を持つ価格数。
    pub active_levels: usize,
    /// 空価格の最終表示数量を含む、再受付の根拠を保持する価格数。
    pub retained_levels: usize,
    /// 正の外部数量または自注文segmentを持つ価格数。
    pub queue_levels: usize,
    /// 現在保持するsegment数。
    pub segments: usize,
    /// 生存自注文数。
    pub live_orders: usize,
    /// 消化済み外部数量と終了自注文のsegment除去累計。
    pub removed_segments: u64,
}

#[derive(Debug, Default)]
pub(super) struct QueueModel {
    book: Option<Book>,
    observed_mode: Option<String>,
    // 数量/自注文を持つ価格だけを約定検索する。
    levels: BTreeMap<LevelKey, Level>,
    // 空の価格にも最終表示数量を残す。再受付で消化済み数量を復活させない。
    empty_levels: BTreeMap<LevelKey, Decimal>,
    // 直前表示が正の価格。次の板から消えた価格も一度だけ更新する。
    displayed_levels: BTreeSet<LevelKey>,
    order_levels: BTreeMap<OrderId, LevelKey>,
    removed_segments: u64,
    pub issues: Vec<String>,
}

impl QueueModel {
    pub fn observed_mode(&self) -> Option<String> {
        self.observed_mode.clone()
    }
    pub fn evidence(&self) -> Option<PaperBookEvidence> {
        self.book.as_ref().map(|b| b.evidence.clone())
    }
    pub fn state(&self) -> Vec<PaperQueueLevel> {
        self.levels
            .iter()
            .map(|(&(side, price), l)| PaperQueueLevel {
                side,
                price,
                segments: l.segments.clone(),
            })
            .collect()
    }
    pub fn observe_mode(&mut self, mode: &str) {
        self.observed_mode = Some(mode.to_owned());
        if mode != "NONE" {
            self.issue("unsupported trading mode observed");
        }
    }
    pub fn issue(&mut self, reason: &str) {
        if !self.issues.iter().any(|s| s == reason) {
            self.issues.push(reason.to_owned());
        }
    }
    pub fn observe_book(&mut self, depth: &MarketDepthSnapshot, local_ms: i64, exchange_now: i64) {
        // empty/incompleteは接続世代の無効化も表す。timestamp=0でも古い完全板を温存しない。
        if !depth.is_complete() {
            self.book = None;
            self.issue("incomplete observed book");
            return;
        }
        if self
            .book
            .as_ref()
            .is_some_and(|b| depth.last_timestamp() < b.evidence.exchange_at_ms)
        {
            // 受信が逆順でも新しい観測板を古いsnapshotへ巻き戻さない。
            return;
        }
        let convert = |map: &BTreeMap<Decimal, f64>| -> Option<BTreeMap<Decimal, Decimal>> {
            map.iter()
                .map(|(&p, &q)| {
                    if p <= Decimal::ZERO || !q.is_finite() || q <= 0.0 {
                        return None;
                    }
                    let q = Decimal::from_f64(q)?;
                    (q > Decimal::ZERO).then_some((p, q))
                })
                .collect()
        };
        let parsed = (|| {
            if !depth.is_complete() {
                return None;
            }
            let bids = convert(depth.bids())?;
            let asks = convert(depth.asks())?;
            let best_bid = *bids.last_key_value()?.0;
            let best_ask = *asks.first_key_value()?.0;
            if best_bid >= best_ask {
                return None;
            }
            Some(Book {
                evidence: PaperBookEvidence {
                    exchange_at_ms: depth.last_timestamp(),
                    received_at_ms: local_ms,
                    best_bid,
                    best_ask,
                    lowest_bid: *bids.first_key_value()?.0,
                    highest_ask: *asks.last_key_value()?.0,
                },
                bids,
                asks,
            })
        })();
        let Some(book) = parsed else {
            self.book = None;
            self.issue("incomplete or invalid observed book");
            return;
        };
        // 過去の全価格ではなく、新板の表示価格と前回表示が正だった価格だけを更新。
        let keys: BTreeSet<_> = self
            .displayed_levels
            .iter()
            .copied()
            .chain(book.bids.keys().map(|&p| (OrderSide::Buy, p)))
            .chain(book.asks.keys().map(|&p| (OrderSide::Sell, p)))
            .collect();
        for key @ (side, price) in keys {
            let displayed = match side {
                OrderSide::Buy => &book.bids,
                OrderSide::Sell => &book.asks,
            }
            .get(&price)
            .copied()
            .unwrap_or_default();
            if let Some(old) = self.empty_levels.get(&key).copied() {
                if displayed > old {
                    self.empty_levels.remove(&key);
                    self.levels.insert(
                        key,
                        Level {
                            displayed: old,
                            segments: Vec::new(),
                        },
                    );
                } else {
                    *self.empty_levels.get_mut(&key).unwrap() = displayed;
                }
            }
            if let Some(level) = self.levels.get_mut(&key) {
                // 取消/減少から先頭数量は減らさない。増加分は既存注文の後ろへ置く。
                if displayed > level.displayed {
                    level.segments.push(PaperQueueSegment {
                        effective_after_ms: exchange_now.max(depth.last_timestamp()),
                        external_amount: displayed - level.displayed,
                        order_id: None,
                    });
                }
                level.displayed = displayed;
            }
            if self.levels.contains_key(&key) || self.empty_levels.contains_key(&key) {
                if displayed > Decimal::ZERO {
                    self.displayed_levels.insert(key);
                } else {
                    self.displayed_levels.remove(&key);
                }
            }
        }
        self.book = Some(book);
    }
    pub fn admission(
        &self,
        order: &DesiredLimitOrder,
        assume_normal: bool,
    ) -> Result<PaperBookEvidence, PaperRejectReason> {
        let mode_ok = self
            .observed_mode
            .as_deref()
            .map_or(assume_normal, |mode| mode == "NONE");
        if !mode_ok {
            return Err(PaperRejectReason::ExecutionModelUnavailable(
                PaperModelUnavailable::UnsupportedMarketMode,
            ));
        }
        let book = self
            .book
            .as_ref()
            .ok_or(PaperRejectReason::ExecutionModelUnavailable(
                PaperModelUnavailable::MissingBook,
            ))?;
        let crossing = match order.side {
            OrderSide::Buy => order.price >= book.evidence.best_ask,
            OrderSide::Sell => order.price <= book.evidence.best_bid,
        };
        if crossing {
            return Err(if order.post_only == Some(true) {
                PaperRejectReason::PostOnlyWouldTakeLiquidity
            } else {
                PaperRejectReason::ExecutionModelUnavailable(
                    PaperModelUnavailable::UnsupportedTaker,
                )
            });
        }
        if match order.side {
            OrderSide::Buy => order.price < book.evidence.lowest_bid,
            OrderSide::Sell => order.price > book.evidence.highest_ask,
        } {
            return Err(PaperRejectReason::ExecutionModelUnavailable(
                PaperModelUnavailable::PriceOutsideVisibleBook,
            ));
        }
        Ok(book.evidence.clone())
    }
    pub fn accept(&mut self, order: &DesiredLimitOrder, id: OrderId, at: i64) {
        let book = self.book.as_ref().expect("validated admission");
        let displayed = match order.side {
            OrderSide::Buy => &book.bids,
            OrderSide::Sell => &book.asks,
        }
        .get(&order.price)
        .copied()
        .unwrap_or_default();
        let key = (order.side, order.price);
        let previous = self.empty_levels.remove(&key);
        let level = self.levels.entry(key).or_insert_with(|| Level {
            displayed: previous.unwrap_or(displayed),
            segments: if previous.is_some() || displayed == Decimal::ZERO {
                Vec::new()
            } else {
                vec![PaperQueueSegment {
                    effective_after_ms: at,
                    external_amount: displayed,
                    order_id: None,
                }]
            },
        });
        self.order_levels.insert(id, key);
        if displayed > Decimal::ZERO {
            self.displayed_levels.insert(key);
        }
        level.segments.push(PaperQueueSegment {
            effective_after_ms: at,
            external_amount: Decimal::ZERO,
            order_id: Some(id),
        });
    }
    pub fn remove(&mut self, id: OrderId) {
        if let Some(key) = self.order_levels.remove(&id) {
            let level = self.levels.get_mut(&key).expect("indexed order level");
            level.segments.retain(|s| s.order_id != Some(id));
            self.removed_segments += 1;
            self.retire_empty(key);
        }
    }
    fn retire_empty(&mut self, key: LevelKey) {
        if self.levels.get(&key).is_some_and(|l| l.segments.is_empty()) {
            let level = self.levels.remove(&key).unwrap();
            self.empty_levels.insert(key, level.displayed);
        }
    }
    pub fn diagnostics(&self) -> PaperQueueDiagnostics {
        PaperQueueDiagnostics {
            active_levels: self.order_levels.values().collect::<BTreeSet<_>>().len(),
            retained_levels: self.levels.len() + self.empty_levels.len(),
            queue_levels: self.levels.len(),
            segments: self.levels.values().map(|l| l.segments.len()).sum(),
            live_orders: self.order_levels.len(),
            removed_segments: self.removed_segments,
        }
    }
    pub fn allocations(
        &mut self,
        trade: &MarketTrade,
        orders: &BTreeMap<OrderId, OpenOrder>,
        cancel_at: &BTreeMap<OrderId, i64>,
    ) -> Vec<(OrderId, Decimal)> {
        let active = |segment: &PaperQueueSegment| {
            segment
                .order_id
                .and_then(|id| orders.get(&id).map(|o| (id, o)))
                .filter(|(id, o)| {
                    segment.effective_after_ms < trade.executed_at
                        && o.remaining_amount > Decimal::ZERO
                        && cancel_at.get(id).is_none_or(|t| trade.executed_at < *t)
                })
        };
        let mut remaining = trade.amount;
        let mut fills = Vec::new();
        let mut emptied = Vec::new();
        // side/price順の範囲検索。Buy側は高価格、Sell側は低価格が優先。
        let (ascending, descending) = match trade.side {
            OrderSide::Buy => {
                (
                    Some(self.levels.range_mut(
                        (OrderSide::Sell, Decimal::MIN)..=(OrderSide::Sell, trade.price),
                    )),
                    None,
                )
            }
            OrderSide::Sell => (
                None,
                Some(
                    self.levels
                        .range_mut((OrderSide::Buy, trade.price)..=(OrderSide::Buy, Decimal::MAX)),
                ),
            ),
        };
        for (&key, level) in ascending
            .into_iter()
            .flatten()
            .chain(descending.into_iter().flatten().rev())
        {
            let through = trade.price != key.1;
            // 旧自注文の後ろの外部数量も、その成立時刻に存在したなら消化する。
            // 将来の自注文だけを飛ばし、確定を遅らせても列を同じ状態へ進める。
            for segment in &mut level.segments {
                if remaining <= Decimal::ZERO && !through {
                    break;
                }
                if segment.effective_after_ms >= trade.executed_at {
                    continue;
                }
                if let Some((id, order)) = active(segment) {
                    if remaining <= Decimal::ZERO {
                        continue;
                    }
                    let amount = order.remaining_amount.min(remaining);
                    remaining -= amount;
                    fills.push((id, amount));
                } else if segment.order_id.is_none() {
                    if through {
                        // 価格通過は先頭数量が通過済みとの近似。自己約定は公開数量を上限とする。
                        segment.external_amount = Decimal::ZERO;
                    } else {
                        let consumed = segment.external_amount.min(remaining);
                        segment.external_amount -= consumed;
                        remaining -= consumed;
                    }
                }
            }
            let before = level.segments.len();
            level
                .segments
                .retain(|s| s.order_id.is_some() || s.external_amount > Decimal::ZERO);
            self.removed_segments += (before - level.segments.len()) as u64;
            if level.segments.is_empty() {
                emptied.push(key);
            }
        }
        for key in emptied {
            self.retire_empty(key);
        }
        fills
    }
}

#[cfg(test)]
#[path = "queue/legacy.rs"]
mod legacy;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::order_domain::OrderType;

    fn book(t: i64, quantity: f64) -> MarketDepthSnapshot {
        MarketDepthSnapshot::new(
            [(110.into(), quantity), (120.into(), 1.0)].into(),
            [(90.into(), 1.0), (100.into(), quantity)].into(),
            t,
        )
    }
    fn normalized(state: Vec<PaperQueueLevel>) -> serde_json::Value {
        let mut state = state
            .into_iter()
            .filter_map(|mut l| {
                l.segments
                    .retain(|s| s.order_id.is_some() || s.external_amount > Decimal::ZERO);
                (!l.segments.is_empty()).then_some(l)
            })
            .collect::<Vec<_>>();
        state.sort_by_key(|l| (l.side, l.price));
        serde_json::to_value(state).unwrap()
    }
    #[test]
    fn differential_against_023_queue() {
        // 受信順と成立順を混ぜ、同ms/過去時刻の資格も直接比較する。
        for seed in 1..=12u64 {
            let mut random = seed;
            let mut new = QueueModel::default();
            let mut old = legacy::QueueModel::default();
            let mut orders = BTreeMap::<OrderId, OpenOrder>::new();
            let mut cancel = BTreeMap::new();
            let mut ids = Vec::new();
            for t in 1..=1200 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                match random % 5 {
                    0 | 1 => {
                        let b = book(t, (random % 7 + 1) as f64);
                        new.observe_book(&b, t + 5, t + 2);
                        old.observe_book(&b, t + 5, t + 2);
                    }
                    2 if new.book.is_some() => {
                        let side = if random & 8 == 0 {
                            OrderSide::Buy
                        } else {
                            OrderSide::Sell
                        };
                        let offset = ((random >> 8) % 6) as i64;
                        let price = if side == OrderSide::Buy {
                            100 - offset
                        } else {
                            110 + offset
                        };
                        let order = DesiredLimitOrder::limit(
                            "btc_jpy".into(),
                            side,
                            1.into(),
                            price.into(),
                        );
                        assert_eq!(new.admission(&order, true), old.admission(&order, true));
                        let id = OrderId(t as u64);
                        new.accept(&order, id, t);
                        old.accept(&order, id, t);
                        orders.insert(
                            id,
                            OpenOrder {
                                order_id: id,
                                pair: order.pair.clone(),
                                side,
                                order_type: OrderType::Limit,
                                remaining_amount: 1.into(),
                                price: Some(order.price),
                                post_only: Some(true),
                            },
                        );
                        if random & 32 != 0 {
                            cancel.insert(id, t + 10);
                        }
                        ids.push(id);
                    }
                    3 => {
                        if let Some(id) = ids.pop() {
                            new.remove(id);
                            old.remove(id);
                            orders.remove(&id);
                            cancel.remove(&id);
                        }
                    }
                    _ => {
                        let trade = MarketTrade {
                            transaction_id: t,
                            executed_at: t - (random % 9) as i64,
                            side: if random & 8 == 0 {
                                OrderSide::Buy
                            } else {
                                OrderSide::Sell
                            },
                            price: (if random & 16 == 0 { 100 } else { 110 }).into(),
                            amount: (random % 11 + 1).into(),
                        };
                        let fills = new.allocations(&trade, &orders, &cancel);
                        assert_eq!(fills, old.allocations(&trade, &orders, &cancel));
                        for (id, amount) in fills {
                            let order = orders.get_mut(&id).unwrap();
                            order.remaining_amount -= amount;
                            if order.remaining_amount == Decimal::ZERO {
                                orders.remove(&id);
                                new.remove(id);
                                old.remove(id);
                            }
                        }
                    }
                }
                assert_eq!(
                    normalized(new.state()),
                    normalized(old.state()),
                    "seed={seed}, step={t}"
                );
            }
        }
    }

    #[test]
    fn exhausted_history_keeps_only_price_evidence() {
        let mut q = QueueModel::default();
        for t in 1..=2000 {
            let b = book(t * 3, if t % 2 == 0 { 1.0 } else { 2.0 });
            q.observe_book(&b, t * 3, t * 3);
            let o =
                DesiredLimitOrder::limit("btc_jpy".into(), OrderSide::Buy, 1.into(), 100.into());
            q.accept(&o, OrderId(t as u64), t * 3);
            q.remove(OrderId(t as u64));
            q.allocations(
                &MarketTrade {
                    transaction_id: t,
                    executed_at: t * 3 + 1,
                    side: OrderSide::Sell,
                    price: 100.into(),
                    amount: 100.into(),
                },
                &BTreeMap::new(),
                &BTreeMap::new(),
            );
            assert!(q.state().is_empty());
            assert_eq!(q.diagnostics().segments, 0);
            assert_eq!(q.diagnostics().retained_levels, 1);
        }
        assert!(q.diagnostics().removed_segments >= 3000);
    }
}
