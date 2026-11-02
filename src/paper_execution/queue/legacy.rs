//! 比較専用: 6581cfab の queue.rs。公開診断型のみ親モジュールと共有。
#![allow(dead_code)]
use super::*;
use crate::{depth::Depth, market_event::MarketDepthSnapshot};
use rust_decimal::prelude::FromPrimitive;

#[derive(Debug, Clone)]
struct Book {
    evidence: PaperBookEvidence,
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
}

#[derive(Debug)]
struct Level {
    side: OrderSide,
    price: Decimal,
    displayed: Decimal,
    segments: Vec<PaperQueueSegment>,
}

#[derive(Debug, Default)]
pub(super) struct QueueModel {
    book: Option<Book>,
    observed_mode: Option<String>,
    levels: Vec<Level>,
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
            .map(|l| PaperQueueLevel {
                side: l.side,
                price: l.price,
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
        for level in &mut self.levels {
            let displayed = match level.side {
                OrderSide::Buy => &book.bids,
                OrderSide::Sell => &book.asks,
            }
            .get(&level.price)
            .copied()
            .unwrap_or_default();
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
        let index = self
            .levels
            .iter()
            .position(|l| l.side == order.side && l.price == order.price);
        let level = if let Some(i) = index {
            &mut self.levels[i]
        } else {
            self.levels.push(Level {
                side: order.side,
                price: order.price,
                displayed,
                segments: vec![PaperQueueSegment {
                    effective_after_ms: at,
                    external_amount: displayed,
                    order_id: None,
                }],
            });
            self.levels.last_mut().unwrap()
        };
        level.segments.push(PaperQueueSegment {
            effective_after_ms: at,
            external_amount: Decimal::ZERO,
            order_id: Some(id),
        });
    }
    pub fn remove(&mut self, id: OrderId) {
        for level in &mut self.levels {
            level.segments.retain(|s| s.order_id != Some(id));
        }
        // 自注文がなくても消化済み外部数量の履歴は残す。
        // 古い観測板から再初期化すると、watermarkを先に進めた経路だけ外部数量が復活する。
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
        let mut indices = self
            .levels
            .iter()
            .enumerate()
            .filter(|(_, l)| match (trade.side, l.side) {
                (OrderSide::Buy, OrderSide::Sell) => trade.price >= l.price,
                (OrderSide::Sell, OrderSide::Buy) => trade.price <= l.price,
                _ => false,
            })
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        indices.sort_by(|&a, &b| match trade.side {
            OrderSide::Buy => self.levels[a].price.cmp(&self.levels[b].price),
            OrderSide::Sell => self.levels[b].price.cmp(&self.levels[a].price),
        });
        let mut remaining = trade.amount;
        let mut fills = Vec::new();
        for index in indices {
            let level = &mut self.levels[index];
            let through = trade.price != level.price;
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
        }
        fills
    }
}
