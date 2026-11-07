//! 受信済みL2板による受付近似と、価格レベル共有のFIFO数量モデル。
use super::*;
use crate::book_units::{Price, Quantity};
use crate::depth::Depth;
use crate::market_event::MarketDepthSnapshot;
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
    pub best_bid: Price,
    pub best_ask: Price,
    pub lowest_bid: Price,
    pub highest_ask: Price,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PaperQueueLevel {
    pub side: OrderSide,
    pub price: Price,
    pub segments: Vec<PaperQueueSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PaperQueueSegment {
    /// この数量/注文が列へ入る取引所時計。過去成立の取引で消化しない。
    pub effective_after_ms: i64,
    pub external_amount: Quantity,
    pub order_id: Option<OrderId>,
}

#[derive(Debug, Clone)]
struct Book {
    evidence: PaperBookEvidence,
    snapshot: MarketDepthSnapshot,
}

type LevelKey = (OrderSide, Price);
#[derive(Debug, Clone)]
struct Segment {
    effective_after_ms: i64,
    external_amount: i64,
    order_id: Option<OrderId>,
}

#[derive(Debug, Clone)]
struct Level {
    displayed: i64,
    segments: Vec<Segment>,
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
    empty_levels: BTreeMap<LevelKey, i64>,
    // 直前表示が正の価格。次の板から消えた価格も一度だけ更新する。
    displayed_levels: BTreeSet<LevelKey>,
    order_levels: BTreeMap<OrderId, LevelKey>,
    removed_segments: u64,
    pub issues: Vec<String>,
}

#[cfg(test)]
thread_local! { pub(super) static STATE_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }

pub(super) struct AllocationPlan {
    pub fills: Vec<(OrderId, Quantity)>,
    updates: Vec<(LevelKey, Level)>,
    removed: u64,
}
impl QueueModel {
    pub fn observed_mode(&self) -> Option<String> {
        self.observed_mode.clone()
    }
    pub fn evidence(&self) -> Option<PaperBookEvidence> {
        self.book.as_ref().map(|b| b.evidence.clone())
    }
    pub fn state(&self) -> Vec<PaperQueueLevel> {
        #[cfg(test)]
        STATE_CALLS.with(|n| n.set(n.get() + 1));
        self.levels
            .iter()
            .map(|(&(side, price), l)| PaperQueueLevel {
                side,
                price,
                segments: l
                    .segments
                    .iter()
                    .map(|s| PaperQueueSegment {
                        effective_after_ms: s.effective_after_ms,
                        external_amount: Quantity::from_units(s.external_amount),
                        order_id: s.order_id,
                    })
                    .collect(),
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
        let parsed = (|| {
            let evidence = PaperBookEvidence {
                exchange_at_ms: depth.last_timestamp(),
                received_at_ms: local_ms,
                best_bid: *depth.bids().last_key_value()?.0,
                best_ask: *depth.asks().first_key_value()?.0,
                lowest_bid: *depth.bids().first_key_value()?.0,
                highest_ask: *depth.asks().last_key_value()?.0,
            };
            if evidence.best_bid >= evidence.best_ask {
                return None;
            }
            let delta = self
                .book
                .as_ref()
                .and_then(|b| depth.changes_since(&b.snapshot));
            let mut updates = BTreeMap::new();
            let mut add = |side, price: Price, amount: Quantity, allow_zero: bool| {
                let amount = amount.units();
                if price <= Price::ZERO || amount < 0 || (!allow_zero && amount == 0) {
                    return None;
                }
                updates.insert((side, price), amount);
                Some(())
            };
            if let Some(delta) = delta {
                for &(side, price, amount) in delta {
                    add(side, price, amount, true)?;
                }
            } else {
                for (&p, &q) in depth.bids() {
                    add(OrderSide::Buy, p, q, false)?;
                }
                for (&p, &q) in depth.asks() {
                    add(OrderSide::Sell, p, q, false)?;
                }
            }
            Some((evidence, delta.is_some(), updates))
        })();
        let Some((evidence, incremental, mut updates)) = parsed else {
            self.book = None;
            self.issue("incomplete or invalid observed book (requires exact 8-decimal units)");
            return;
        };
        let mut book = if incremental {
            self.book.take().expect("incremental parent")
        } else {
            // 再同期では直前の表示から消えた保持価格も0にする。
            for &key in &self.displayed_levels {
                updates.entry(key).or_insert(0);
            }
            Book {
                evidence: evidence.clone(),
                snapshot: depth.clone(),
            }
        };
        for (key, displayed) in updates {
            if let Some(old) = self.empty_levels.get_mut(&key) {
                if displayed > *old {
                    let old = *old;
                    self.empty_levels.remove(&key);
                    self.levels.insert(
                        key,
                        Level {
                            displayed: old,
                            segments: Vec::new(),
                        },
                    );
                } else {
                    *old = displayed;
                }
            }
            if let Some(level) = self.levels.get_mut(&key) {
                // 表示減少は先頭を消費しない。増加分だけを末尾へ追加。
                if displayed > level.displayed {
                    level.segments.push(Segment {
                        effective_after_ms: exchange_now.max(depth.last_timestamp()),
                        external_amount: displayed - level.displayed,
                        order_id: None,
                    });
                }
                level.displayed = displayed;
            }
            if self.levels.contains_key(&key) || self.empty_levels.contains_key(&key) {
                if displayed > 0 {
                    self.displayed_levels.insert(key);
                } else {
                    self.displayed_levels.remove(&key);
                }
            }
        }
        book.evidence = evidence;
        book.snapshot = depth.clone();
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
            OrderSide::Buy => book.snapshot.bids(),
            OrderSide::Sell => book.snapshot.asks(),
        }
        .get(&order.price)
        .map(|q| q.units())
        .unwrap_or_default();
        let key = (order.side, order.price);
        let previous = self.empty_levels.remove(&key);
        let level = self.levels.entry(key).or_insert_with(|| Level {
            displayed: previous.unwrap_or(displayed),
            segments: if previous.is_some() || displayed == 0 {
                Vec::new()
            } else {
                vec![Segment {
                    effective_after_ms: at,
                    external_amount: displayed,
                    order_id: None,
                }]
            },
        });
        self.order_levels.insert(id, key);
        if displayed > 0 {
            self.displayed_levels.insert(key);
        }
        level.segments.push(Segment {
            effective_after_ms: at,
            external_amount: 0,
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
    pub fn plan_allocations(
        &self,
        trade: &MarketTrade,
        orders: &BTreeMap<OrderId, OpenOrder>,
        cancel_at: &BTreeMap<OrderId, i64>,
    ) -> AllocationPlan {
        let active = |segment: &Segment| {
            segment
                .order_id
                .and_then(|id| orders.get(&id).map(|o| (id, o)))
                .filter(|(id, o)| {
                    segment.effective_after_ms < trade.executed_at
                        && o.remaining_amount > Quantity::ZERO
                        && cancel_at.get(id).is_none_or(|t| trade.executed_at < *t)
                })
        };
        let price = trade.price;
        let mut remaining = trade.amount.units();
        let mut fills = Vec::new();
        let mut updates = Vec::new();
        let mut removed = 0;
        // side/price順の範囲検索。Buy側は高価格、Sell側は低価格が優先。
        let (ascending, descending) = match trade.side {
            OrderSide::Buy => (
                Some(
                    self.levels
                        .range((OrderSide::Sell, Price::MIN)..=(OrderSide::Sell, price)),
                ),
                None,
            ),
            OrderSide::Sell => (
                None,
                Some(
                    self.levels
                        .range((OrderSide::Buy, price)..=(OrderSide::Buy, Price::MAX)),
                ),
            ),
        };
        for (&key, level) in ascending
            .into_iter()
            .flatten()
            .chain(descending.into_iter().flatten().rev())
        {
            let mut level = level.clone();
            let through = price != key.1;
            // 旧自注文の後ろの外部数量も、その成立時刻に存在したなら消化する。
            // 将来の自注文だけを飛ばし、確定を遅らせても列を同じ状態へ進める。
            for segment in &mut level.segments {
                if remaining <= 0 && !through {
                    break;
                }
                if segment.effective_after_ms >= trade.executed_at {
                    continue;
                }
                if let Some((id, order)) = active(segment) {
                    if remaining <= 0 {
                        continue;
                    }
                    let amount = order.remaining_amount.units().min(remaining);
                    remaining -= amount;
                    fills.push((id, Quantity::from_units(amount)));
                } else if segment.order_id.is_none() {
                    if through {
                        // 価格通過は先頭数量が通過済みとの近似。自己約定は公開数量を上限とする。
                        segment.external_amount = 0;
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
                .retain(|s| s.order_id.is_some() || s.external_amount > 0);
            removed += (before - level.segments.len()) as u64;
            updates.push((key, level));
        }
        AllocationPlan {
            fills,
            updates,
            removed,
        }
    }
    pub fn commit_allocations(&mut self, plan: AllocationPlan) {
        self.removed_segments += plan.removed;
        for (key, level) in plan.updates {
            self.levels.insert(key, level);
            self.retire_empty(key);
        }
    }
    #[allow(dead_code)] // 比較benchmarkと直接kernelテスト用。
    pub fn allocations(
        &mut self,
        trade: &MarketTrade,
        orders: &BTreeMap<OrderId, OpenOrder>,
        cancel_at: &BTreeMap<OrderId, i64>,
    ) -> Vec<(OrderId, Quantity)> {
        let plan = self.plan_allocations(trade, orders, cancel_at);
        let fills = plan.fills.clone();
        self.commit_allocations(plan);
        fills
    }
}

#[cfg(test)]
#[path = "queue/legacy.rs"]
mod legacy;
#[cfg(test)]
mod tests {
    use super::*;

    fn book(t: i64, quantity: Quantity) -> MarketDepthSnapshot {
        MarketDepthSnapshot::new(
            [(110.into(), quantity), (120.into(), literal("1.0"))].into(),
            [(90.into(), literal("1.0")), (100.into(), quantity)].into(),
            t,
        )
    }
    fn normalized(state: Vec<PaperQueueLevel>) -> serde_json::Value {
        let mut state = state
            .into_iter()
            .filter_map(|mut l| {
                l.segments
                    .retain(|s| s.order_id.is_some() || s.external_amount > literal("0"));
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
                        let b = book(t, Quantity::from((random % 7 + 1) as i32));
                        new.observe_book(&b, t + 5, t + 2);
                        old.observe_book(&b, t + 5, t + 2);
                    }
                    2 if new.book.is_some() => {
                        let side = if random & 8 == 0 {
                            OrderSide::Buy
                        } else {
                            OrderSide::Sell
                        };
                        let offset = ((random >> 8) % 6) as i32;
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
                                order_type: crate::order_domain::OrderType::Limit,
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
                            amount: Quantity::from((random % 11 + 1) as i32),
                        };
                        let fills = new.allocations(&trade, &orders, &cancel);
                        assert_eq!(fills, old.allocations(&trade, &orders, &cancel));
                        for (id, amount) in fills {
                            let order = orders.get_mut(&id).unwrap();
                            order.remaining_amount =
                                order.remaining_amount.checked_sub(amount).unwrap();
                            if order.remaining_amount == literal("0") {
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

    fn captured_book(
        converter: &mut crate::bitbank_bot::BitbankMarketEventConverter,
        value: serde_json::Value,
        whole: bool,
    ) -> MarketDepthSnapshot {
        use crate::bitbank_bot::BitbankInboundMessage;
        let message = if whole {
            BitbankInboundMessage::DepthWhole(serde_json::from_value(value).unwrap())
        } else {
            BitbankInboundMessage::DepthDiff(serde_json::from_value(value).unwrap())
        };
        let MarketEvent::DepthUpdated { depth, .. } = converter.convert(message).unwrap().unwrap()
        else {
            panic!("expected depth")
        };
        depth
    }
    fn whole_value(sequence: u64, time: i64) -> serde_json::Value {
        serde_json::json!({"asks":[["110","1.00000001"],["120","2"]], "bids":[["100","3.00000001"],["90","4"]],
            "asks_over":"0","bids_under":"0","asks_under":"0","bids_over":"0","ask_market":"0","bid_market":"0", "timestamp":time,"sequenceId":sequence.to_string()})
    }
    #[test]
    fn incremental_and_resynchronized_books_preserve_fifo_and_fills() {
        let mut converter = crate::bitbank_bot::BitbankMarketEventConverter::new("xrp_jpy".into());
        let mut incremental = QueueModel::default();
        let mut full = QueueModel::default();
        let mut previous = captured_book(&mut converter, whole_value(1, 1), true);
        incremental.observe_book(&previous, 1, 1);
        full.observe_book(&previous, 1, 1);
        let mut orders = BTreeMap::new();
        for sequence in 2..200u64 {
            let t = sequence as i64 * 10;
            let order = DesiredLimitOrder::limit(
                "xrp_jpy".into(),
                OrderSide::Buy,
                "0.00000003".parse().unwrap(),
                100.into(),
            );
            let id = OrderId(sequence);
            incremental.accept(&order, id, t - 1);
            full.accept(&order, id, t - 1);
            orders.insert(
                id,
                OpenOrder {
                    order_id: id,
                    pair: order.pair.clone(),
                    side: order.side,
                    order_type: crate::order_domain::OrderType::Limit,
                    remaining_amount: order.amount,
                    price: Some(order.price),
                    post_only: Some(true),
                },
            );
            let mut next = captured_book(
                &mut converter,
                serde_json::json!({"a":[["111","5"],["111","0"]], "b":[["100", if sequence % 3 == 0 {"0"} else {"3.00000002"}]], "t":t,"s":sequence.to_string()}),
                false,
            );
            assert!(next.changes_since(&previous).is_some());
            if sequence % 7 == 0 {
                // 未配送更新を挟んだ場合、差分の親が一致せず全体へ戻る。
                next = captured_book(&mut converter, whole_value(sequence, t), true);
                assert!(next.changes_since(&previous).is_none());
            }
            let snapshot = MarketDepthSnapshot::new(
                next.asks().clone(),
                next.bids().clone(),
                next.last_timestamp(),
            );
            incremental.observe_book(&next, t, t);
            full.observe_book(&snapshot, t, t);
            assert_eq!(
                incremental.admission(&order, true),
                full.admission(&order, true)
            );
            let trade = MarketTrade {
                transaction_id: t,
                executed_at: t + 1,
                side: OrderSide::Sell,
                price: 100.into(),
                amount: 4.into(),
            };
            assert_eq!(
                incremental.allocations(&trade, &orders, &BTreeMap::new()),
                full.allocations(&trade, &orders, &BTreeMap::new())
            );
            incremental.remove(id);
            full.remove(id);
            orders.remove(&id);
            assert_eq!(incremental.state(), full.state(), "sequence {sequence}");
            assert_eq!(incremental.diagnostics(), full.diagnostics());
            // 既に公開したsnapshotは後続更新でも不変。
            assert_eq!(previous.changes_since(&previous), Some(&[][..]));
            previous = next;
        }
    }
    #[test]
    fn missed_updates_and_connection_switch_require_full_resynchronization() {
        let mut converter = crate::bitbank_bot::BitbankMarketEventConverter::new("xrp_jpy".into());
        let first = captured_book(&mut converter, whole_value(1, 1), true);
        let middle = captured_book(
            &mut converter,
            serde_json::json!({"a":[],"b":[["100","0"]],"t":2,"s":"2"}),
            false,
        );
        let last = captured_book(
            &mut converter,
            serde_json::json!({"a":[],"b":[["99","2"]],"t":3,"s":"3"}),
            false,
        );
        assert!(last.changes_since(&first).is_none());
        assert!(last.changes_since(&middle).is_some());
        assert_eq!(*first.best_bid().unwrap().0, literal("100"));
        assert_eq!(*last.best_bid().unwrap().0, literal("99"));
        let order =
            DesiredLimitOrder::limit("xrp_jpy".into(), OrderSide::Buy, 1.into(), 100.into());
        let mut q = QueueModel::default();
        q.observe_book(&first, 1, 1);
        q.accept(&order, OrderId(1), 1);
        q.observe_book(&last, 3, 3);
        assert_eq!(q.levels[&(OrderSide::Buy, Price::from(100))].displayed, 0);
        assert_eq!(
            q.book
                .as_ref()
                .unwrap()
                .snapshot
                .bids()
                .get(&Price::from(100)),
            None
        );
        let mut other = crate::bitbank_bot::BitbankMarketEventConverter::new("xrp_jpy".into());
        let next = captured_book(&mut other, whole_value(1, 4), true);
        assert!(next.changes_since(&last).is_none());
        q.observe_book(&next, 4, 4);
        assert_eq!(
            q.levels[&(OrderSide::Buy, Price::from(100))].displayed,
            literal::<Quantity>("3.00000001").units()
        );
    }

    #[test]
    fn captured_quantities_keep_units_beyond_float_precision() {
        let mut converter = crate::bitbank_bot::BitbankMarketEventConverter::new("xrp_jpy".into());
        let mut value = whole_value(1, 1);
        value["bids"][0][1] = serde_json::json!("90000000000.00000001");
        let depth = captured_book(&mut converter, value, true);
        assert_eq!(
            depth.bids()[&Price::from(100)],
            Quantity::from_units(9_000_000_000_000_000_001)
        );
        let mut q = QueueModel::default();
        q.observe_book(&depth, 1, 1);
        let order = DesiredLimitOrder::limit(
            "xrp_jpy".into(),
            OrderSide::Buy,
            "0.00000001".parse().unwrap(),
            100.into(),
        );
        assert!(q.admission(&order, true).is_ok());
        q.accept(&order, OrderId(1), 1);
        assert_eq!(
            q.state()[0].segments[0].external_amount,
            "90000000000.00000001".parse().unwrap()
        );
    }

    #[test]
    fn fixed_units_are_exact_and_never_round_unsupported_precision() {
        assert!("0.000000001".parse::<Price>().is_err());
        assert!("0.000000001".parse::<Quantity>().is_err());
        let mut q = QueueModel::default();
        q.observe_book(&book(1, literal("4161.5933")), 1, 1);
        let order = DesiredLimitOrder::limit(
            "xrp_jpy".into(),
            OrderSide::Buy,
            literal("0.00000001"),
            100.into(),
        );
        q.accept(&order, OrderId(1), 1);
        assert_eq!(
            q.state()[0].segments[0].external_amount,
            literal("4161.5933")
        );
    }

    #[test]
    fn exhausted_history_keeps_only_price_evidence() {
        let mut q = QueueModel::default();
        for t in 1..=2000 {
            let b = book(
                t * 3,
                if t % 2 == 0 {
                    literal("1.0")
                } else {
                    literal("2.0")
                },
            );
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
