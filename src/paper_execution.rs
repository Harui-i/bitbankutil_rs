use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

use rust_decimal::Decimal;

use crate::{
    market_event::{MarketEvent, MarketTrade},
    order_domain::{BalanceSnapshot, DesiredLimitOrder, OpenOrder, OrderId, OrderSide, OrderType},
    trading_venue::{
        AccountSnapshot, PlacedOrder, PlacementRequest, TradingVenue, TradingVenueError,
        TradingVenueFuture,
    },
};

/// ローカルUTC Unix msを取引所UTC Unix msへ変換する時計モデル。
/// E = L + exchange_minus_local_ms。0も同期確認ではなく仮定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "model", rename_all = "snake_case")]
pub enum PaperTimeModel {
    FixedOffset { exchange_minus_local_ms: i64 },
}

impl Default for PaperTimeModel {
    fn default() -> Self {
        Self::FixedOffset {
            exchange_minus_local_ms: 0,
        }
    }
}

impl PaperTimeModel {
    /// ローカル時計にだけ適用する。取引所時刻・watermarkは変換しない。
    /// i64の範囲を超える場合は丸めずInvalidClockを返す。
    pub fn local_to_exchange_ms(self, local_ms: i64) -> Result<i64, PaperExecutionError> {
        let Self::FixedOffset {
            exchange_minus_local_ms,
        } = self;
        local_ms
            .checked_add(exchange_minus_local_ms)
            .ok_or(PaperExecutionError::InvalidClock)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PaperExecutionConfig {
    pub pair: String,
    pub fee_schedule: PaperFeeSchedule,
    pub next_order_id: OrderId,
    /// ローカル時計から取引所時計への写像。実行中は固定する。
    pub time_model: PaperTimeModel,
}

impl PaperExecutionConfig {
    pub fn bitbank_spot_default(pair: impl Into<String>) -> Result<Self, PaperExecutionError> {
        let pair = pair.into();
        Ok(Self {
            fee_schedule: PaperFeeSchedule::bitbank_spot_default(&pair)?,
            pair,
            next_order_id: OrderId(1),
            time_model: PaperTimeModel::default(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PaperFeeSchedule {
    pub maker_fee_rate_quote: Decimal,
    pub taker_fee_rate_quote: Decimal,
}

impl PaperFeeSchedule {
    pub fn new(maker_fee_rate_quote: Decimal, taker_fee_rate_quote: Decimal) -> Self {
        Self {
            maker_fee_rate_quote,
            taker_fee_rate_quote,
        }
    }

    pub fn bitbank_spot_default(pair: &str) -> Result<Self, PaperExecutionError> {
        if !pair.ends_with("_jpy") {
            return Err(PaperExecutionError::UnsupportedPair(pair.to_owned()));
        }

        if pair == "btc_jpy" {
            Ok(Self::new(Decimal::ZERO, Decimal::new(1, 3)))
        } else {
            Ok(Self::new(Decimal::new(-2, 4), Decimal::new(12, 4)))
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PaperEvent {
    OrderAccepted {
        order_id: OrderId,
        order: DesiredLimitOrder,
        effective_after_ms: i64,
    },
    OrderRejected {
        order: DesiredLimitOrder,
        reason: PaperRejectReason,
    },
    /// 取消を受け付けたが、残数量と資金解放はwatermarkまで未確定。
    OrderCancelPending {
        order_id: OrderId,
        effective_at_ms: i64,
    },
    OrderCancelled {
        order_id: OrderId,
        order: OpenOrder,
    },
    OrderFilled {
        order_id: OrderId,
        order: DesiredLimitOrder,
        price: Decimal,
        amount: Decimal,
        fee_amount_quote: Decimal,
        trade: MarketTrade,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PaperRejectReason {
    PairMismatch {
        expected: String,
        actual: String,
    },
    NonPositiveOrder {
        amount: Decimal,
        price: Decimal,
    },
    InsufficientFunds {
        asset: String,
        required: Decimal,
        free: Decimal,
    },
    UnsupportedPair(String),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum PaperExecutionError {
    InvalidClock,
    InvalidWatermark,
    InvalidTradeStream,
    PairMismatch {
        expected: String,
        actual: String,
    },
    NonPositiveOrder {
        amount: Decimal,
        price: Decimal,
    },
    InsufficientFunds {
        asset: String,
        required: Decimal,
        free: Decimal,
    },
    MissingBalance(String),
    UnsupportedPair(String),
}

/// 確定境界と未確定状態。issuesがある評価は正常な損益として扱わない。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PaperSettlementStatus {
    /// 結果の比較・識別用に、記録の有無によらず設定を残す。
    pub time_model: PaperTimeModel,
    /// 取引所UTC Unix ms。
    pub watermark_ms: Option<i64>,
    pub max_trade_delay_ms: Option<i64>,
    pub buffered_trades: usize,
    pub pending_cancellations: usize,
    /// まだローカルへ応答・通知していない操作/状態更新の件数。
    pub pending_operations: usize,
    pub latency_model: PaperLatencyConfig,
    pub pending_operation_details: Vec<serde_json::Value>,
    pub unsettled: bool,
    pub issues: Vec<String>,
}

/// メモリ上に保持する最近のpaperイベントの最大件数。
pub const PAPER_EVENT_HISTORY_LIMIT: usize = 1024;

/// 注文受付時刻の時計。時刻はローカルUTCのUnixミリ秒。
/// `Replay` は呼び出し元が明示的に進め、実時計を参照しない。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PaperClock {
    Realtime,
    Replay { now_ms: i64 },
}

/// 公開約定の数量を使ってmaker指値注文を執行するエンジン。
/// 受信済み公開約定をwatermarkで確定し、注文の有効期間内の約定だけを適用する。
/// 時刻・時計差の制約は `docs/paper-execution.md` を参照。
#[derive(Debug)]
pub struct PaperExecutionEngine {
    config: PaperExecutionConfig,
    clock: PaperClock,
    venue_state: Option<Arc<Mutex<latency::State>>>,
    effective_after: BTreeMap<OrderId, i64>,
    // 有効期間・板による下限・watermarkはすべて取引所UTC Unix ms。
    time_floor: i64,
    cancel_at: BTreeMap<OrderId, i64>,
    buffered_trades: BTreeMap<(i64, i64), MarketTrade>,
    seen_trades: BTreeMap<i64, MarketTrade>,
    watermark_ms: Option<i64>,
    max_trade_delay_ms: Option<i64>,
    settlement_issues: Vec<String>,
    base_asset: String,
    quote_asset: String,
    balances: BTreeMap<String, BalanceSnapshot>,
    open_orders: BTreeMap<OrderId, OpenOrder>,
    event_history: VecDeque<PaperEvent>,
    discarded_events: u64,
    recorder: Option<crate::execution_recording::ExecutionRecorder>,
}

impl PaperExecutionEngine {
    /// 時計を明示して初期化する。paper tradingには `Realtime`、backtestには
    /// `Replay` を指定する。再生時計の初期値は最初の入力時刻以下にする。
    pub fn new(
        config: PaperExecutionConfig,
        clock: PaperClock,
        balances: Vec<BalanceSnapshot>,
        recorder: Option<crate::execution_recording::ExecutionRecorder>,
    ) -> Result<Self, PaperExecutionError> {
        let initial_local_ms = match clock {
            PaperClock::Replay { now_ms } => now_ms,
            PaperClock::Realtime => i64::try_from(crate::execution_recording::timestamp_ms())
                .map_err(|_| PaperExecutionError::InvalidClock)?,
        };
        config.time_model.local_to_exchange_ms(initial_local_ms)?;
        let (base_asset, quote_asset) = parse_jpy_pair(&config.pair)?;
        let balances = balances
            .into_iter()
            .map(|balance| (balance.asset.clone(), balance))
            .collect::<BTreeMap<_, _>>();

        if !balances.contains_key(&base_asset) {
            return Err(PaperExecutionError::MissingBalance(base_asset));
        }
        if !balances.contains_key(&quote_asset) {
            return Err(PaperExecutionError::MissingBalance(quote_asset));
        }

        let engine = Self {
            config,
            clock,
            venue_state: None,
            effective_after: BTreeMap::new(),
            time_floor: i64::MIN,
            cancel_at: BTreeMap::new(),
            buffered_trades: BTreeMap::new(),
            seen_trades: BTreeMap::new(),
            watermark_ms: None,
            max_trade_delay_ms: None,
            settlement_issues: Vec::new(),
            base_asset,
            quote_asset,
            balances,
            open_orders: BTreeMap::new(),
            event_history: VecDeque::new(),
            discarded_events: 0,
            recorder,
        };
        if let Some(recorder) = &engine.recorder {
            recorder.record(
                "paper_initial_state",
                serde_json::json!({
                    "config": engine.config(), "clock": engine.clock, "model": "limit_price_trade_volume_v4_fixed_offset",
                    "balances": engine.balances(), "open_orders": engine.open_orders()
                }),
            );
        }
        Ok(engine)
    }

    /// 再生時計を単調に進める。実時計のエンジンや時刻の逆行はエラー。
    pub fn advance_replay_time(&mut self, time_ms: i64) -> Result<(), PaperExecutionError> {
        self.config.time_model.local_to_exchange_ms(time_ms)?;
        match &mut self.clock {
            PaperClock::Replay { now_ms } if time_ms >= *now_ms => {
                *now_ms = time_ms;
                Ok(())
            }
            _ => Err(PaperExecutionError::InvalidClock),
        }
    }

    /// 指値注文を受理し、受付時計と観測済み板時刻による有効開始時刻を保存する。
    /// 同一ミリ秒の公開約定は対象外。部分約定でも有効開始時刻は変えない。
    pub fn place_order(
        &mut self,
        order: DesiredLimitOrder,
    ) -> Result<PlacedOrder, PaperExecutionError> {
        if order.pair != self.config.pair {
            let reason = PaperRejectReason::PairMismatch {
                expected: self.config.pair.clone(),
                actual: order.pair.clone(),
            };
            self.record_event(PaperEvent::OrderRejected {
                order,
                reason: reason.clone(),
            });
            return Err(PaperExecutionError::from(reason));
        }

        if order.amount <= Decimal::ZERO || order.price <= Decimal::ZERO {
            let reason = PaperRejectReason::NonPositiveOrder {
                amount: order.amount,
                price: order.price,
            };
            self.record_event(PaperEvent::OrderRejected {
                order,
                reason: reason.clone(),
            });
            return Err(PaperExecutionError::from(reason));
        }

        if !self.settlement_issues.is_empty() {
            return Err(PaperExecutionError::InvalidTradeStream);
        }
        // 変換失敗で資金を拘束せず、資金不足で操作時刻下限を進めない。
        let exchange_now = self.exchange_now_ms()?;
        let lock_result = self.lock_funds_for_order(&order);
        if let Err(err) = lock_result {
            self.record_event(PaperEvent::OrderRejected {
                order,
                reason: err.clone(),
            });
            return Err(PaperExecutionError::from(err));
        }

        self.observe_operation_time(exchange_now);
        let order_id = self.config.next_order_id;
        self.effective_after.insert(order_id, self.time_floor);
        self.config.next_order_id = OrderId(self.config.next_order_id.0 + 1);

        // 受理した指値注文はすべて板に残るmaker注文として扱う。
        // post-onlyでない注文も保持するが、板を横切るtaker約定とtaker手数料は再現しない。
        self.open_orders.insert(
            order_id,
            OpenOrder {
                order_id,
                pair: order.pair.clone(),
                side: order.side,
                order_type: OrderType::Limit,
                remaining_amount: order.amount,
                price: Some(order.price),
                post_only: order.post_only,
            },
        );
        self.record_event(PaperEvent::OrderAccepted {
            order_id,
            order: order.clone(),
            effective_after_ms: self.time_floor,
        });

        Ok(PlacedOrder {
            order_id: Some(order_id),
        })
    }

    /// 取消を即時有効化する。成功は受付を表し、資金解放はwatermark確定後。
    /// 取消と同じmsに成立した取引は対象外（有効期間は開始・終了とも開区間）。
    /// 封鎖済みwatermark以下への取消は挿入せず、最初の未確定ms以降へ有効化する。
    pub fn cancel_orders(&mut self, pair: &str, order_ids: Vec<OrderId>) {
        let mut time = match self.operation_time() {
            Ok(time) => time,
            Err(_) => {
                self.mark_incomplete("operation clock overflow");
                return;
            }
        };
        // 封鎖済みのmsに取消を挿入すると、先に確定した同msの部分約定と矛盾する。
        if let Some(watermark) = self.watermark_ms {
            let Some(first_unsealed_ms) = watermark.checked_add(1) else {
                self.mark_incomplete("cancellation after terminal watermark");
                return;
            };
            time = time.max(first_unsealed_ms);
            self.time_floor = self.time_floor.max(time);
        }
        for order_id in order_ids {
            let Some(order) = self.open_orders.get(&order_id) else {
                continue;
            };
            if order.pair != pair || self.cancel_at.contains_key(&order_id) {
                continue;
            }
            self.cancel_at.insert(order_id, time);
            self.record_event(PaperEvent::OrderCancelPending {
                order_id,
                effective_at_ms: time,
            });
        }
    }

    fn exchange_now_ms(&self) -> Result<i64, PaperExecutionError> {
        let local_ms = match self.clock {
            PaperClock::Realtime => i64::try_from(crate::execution_recording::timestamp_ms())
                .map_err(|_| PaperExecutionError::InvalidClock)?,
            PaperClock::Replay { now_ms } => now_ms,
        };
        self.config.time_model.local_to_exchange_ms(local_ms)
    }

    fn operation_time(&mut self) -> Result<i64, PaperExecutionError> {
        let now = self.exchange_now_ms()?;
        Ok(self.observe_operation_time(now))
    }

    fn observe_operation_time(&mut self, exchange_ms: i64) -> i64 {
        self.time_floor = self.time_floor.max(exchange_ms);
        // 確定済み時刻以前に新しい操作を挿入しない。
        self.time_floor = self.time_floor.max(self.watermark_ms.unwrap_or(i64::MIN));
        self.time_floor
    }

    /// 公開約定を受信順に登録する。約定・取消結果はwatermarkで確定する。
    /// 同一IDの重複は無視し、内容衝突や確定済み境界への遅着は評価異常として記録する。
    pub fn apply_market_event(&mut self, event: &MarketEvent) -> Vec<PaperEvent> {
        let received_exchange_ms = match self.exchange_now_ms() {
            Ok(time) => time,
            Err(_) => {
                self.mark_incomplete("market receipt clock overflow");
                return Vec::new();
            }
        };
        let delay_threshold = if let Some(delay) = self.max_trade_delay_ms {
            match received_exchange_ms.checked_sub(delay) {
                Some(time) => Some(time),
                None => {
                    self.mark_incomplete("trade delay threshold overflow");
                    return Vec::new();
                }
            }
        } else {
            None
        };
        if let MarketEvent::DepthUpdated { pair, depth } = event
            && pair == &self.config.pair
        {
            self.time_floor = self.time_floor.max(depth.last_timestamp());
        }
        if let MarketEvent::Transactions { pair, transactions } = event
            && pair == &self.config.pair
        {
            for trade in transactions {
                if let Some(previous) = self.seen_trades.get(&trade.transaction_id) {
                    if previous != trade {
                        self.mark_incomplete(format!(
                            "conflicting transaction ID {}",
                            trade.transaction_id
                        ));
                    }
                    continue;
                }
                self.seen_trades.insert(trade.transaction_id, trade.clone());
                if self.watermark_ms.is_some_and(|w| trade.executed_at <= w) {
                    self.mark_incomplete(format!(
                        "trade {} at {} arrived behind watermark",
                        trade.transaction_id, trade.executed_at
                    ));
                    continue;
                }
                if delay_threshold.is_some_and(|threshold| trade.executed_at < threshold) {
                    self.mark_incomplete(format!(
                        "trade {} exceeded configured delay",
                        trade.transaction_id
                    ));
                    continue;
                }
                if trade.amount <= Decimal::ZERO || trade.price <= Decimal::ZERO {
                    self.mark_incomplete(format!("invalid trade {}", trade.transaction_id));
                    continue;
                }
                self.buffered_trades
                    .insert((trade.executed_at, trade.transaction_id), trade.clone());
            }
        }
        if let Some(threshold) = delay_threshold {
            let Some(boundary) = threshold.checked_sub(1) else {
                self.mark_incomplete("trade watermark overflow");
                return Vec::new();
            };
            if self.watermark_ms.is_none_or(|w| boundary >= w) {
                return self.advance_trade_watermark(boundary).unwrap_or_default();
            }
        }
        Vec::new()
    }

    /// through_msは取引所UTC Unix ms（オフセットを再適用しない）。
    /// `executed_at <= through_ms` の全公開約定を受信済みと呼び出し元が保証する。
    /// 板時刻や単なるEOFはこの保証にならない。保証違反後は確定を停止する。
    /// 成立時刻・同msではtransaction ID順で処理し、約定後に取消残数量を解放する。
    pub fn advance_trade_watermark(
        &mut self,
        through_ms: i64,
    ) -> Result<Vec<PaperEvent>, PaperExecutionError> {
        if self.watermark_ms.is_some_and(|w| through_ms < w) {
            return Err(PaperExecutionError::InvalidWatermark);
        }
        if !self.settlement_issues.is_empty() {
            return Err(PaperExecutionError::InvalidTradeStream);
        }
        let mut events = Vec::new();
        while self
            .buffered_trades
            .first_key_value()
            .is_some_and(|(key, _)| key.0 <= through_ms)
        {
            let (_, trade) = self.buffered_trades.pop_first().unwrap();
            let mut remaining = trade.amount;
            for order_id in self.fill_order_ids_for_trade(&trade) {
                if remaining <= Decimal::ZERO {
                    break;
                }
                let order = self.open_orders.remove(&order_id).unwrap();
                let amount = order.remaining_amount.min(remaining);
                let event = self.fill_open_order(order, trade.clone(), amount);
                remaining -= amount;
                self.record_event(event.clone());
                events.push(event);
            }
        }
        self.watermark_ms = Some(through_ms);
        events.extend(self.finalize_cancellations());
        self.record_settlement();
        Ok(events)
    }

    fn finalize_cancellations(&mut self) -> Vec<PaperEvent> {
        if !self.settlement_issues.is_empty() {
            return Vec::new();
        }
        let ids: Vec<_> = self
            .cancel_at
            .iter()
            .filter(|(_, t)| self.watermark_ms.is_some_and(|w| **t <= w))
            .map(|(id, _)| *id)
            .collect();
        let mut events = Vec::new();
        for id in ids {
            self.cancel_at.remove(&id);
            self.effective_after.remove(&id);
            if let Some(order) = self.open_orders.remove(&id) {
                self.unlock_funds_for_open_order(&order);
                let event = PaperEvent::OrderCancelled {
                    order_id: id,
                    order,
                };
                self.record_event(event.clone());
                events.push(event);
            }
        }
        events
    }

    /// 有限遅延を仮定して自動確定する場合だけ明示設定する。既定では自動確定しない。
    /// 境界は取引所時計へ変換した受信時計 - delay - 1ms。利用者の仮定。
    pub fn with_max_trade_delay_ms(mut self, delay: i64) -> Result<Self, PaperExecutionError> {
        if delay < 0 {
            return Err(PaperExecutionError::InvalidWatermark);
        }
        self.max_trade_delay_ms = Some(delay);
        if let Some(r) = &self.recorder {
            r.record(
                "paper_delay_assumption",
                serde_json::json!({"max_trade_delay_ms": delay}),
            );
        }
        Ok(self)
    }

    /// 切断・欠損を記録し、以降の確定と新規注文を停止する。再接続だけでは解除しない。
    pub fn mark_incomplete(&mut self, reason: impl Into<String>) {
        self.settlement_issues.push(reason.into());
        self.record_settlement();
    }

    /// 終了時にも取得・保存する。未確定残高を確定済み損益へ混ぜない。
    pub fn settlement_status(&self) -> PaperSettlementStatus {
        let now = self.exchange_now_ms();
        PaperSettlementStatus {
            time_model: self.config.time_model,
            watermark_ms: self.watermark_ms,
            max_trade_delay_ms: self.max_trade_delay_ms,
            buffered_trades: self.buffered_trades.len(),
            pending_cancellations: self.cancel_at.len(),
            pending_operations: 0,
            latency_model: PaperLatencyConfig::default(),
            pending_operation_details: Vec::new(),
            unsettled: !self.settlement_issues.is_empty()
                || !self.buffered_trades.is_empty()
                || !self.cancel_at.is_empty()
                || self
                    .watermark_ms
                    .is_none_or(|w| now.map_or(true, |now| w < now.max(self.time_floor))),
            issues: self.settlement_issues.clone(),
        }
    }

    fn record_settlement(&self) {
        if let Some(r) = &self.recorder {
            r.record(
                "paper_settlement",
                serde_json::json!(self.settlement_status()),
            );
        }
    }

    /// 取消受付済み注文を除く。取消確定待ちの拘束資金はbalancesに残る。
    pub fn open_orders(&self) -> Vec<OpenOrder> {
        self.open_orders
            .values()
            .filter(|o| !self.cancel_at.contains_key(&o.order_id))
            .cloned()
            .collect()
    }

    pub fn balances(&self) -> Vec<BalanceSnapshot> {
        self.balances.values().cloned().collect()
    }

    /// 未取得のイベントを古い順に返す。上限を超えたイベントは古いものから破棄される。
    pub fn drain_events(&mut self) -> Vec<PaperEvent> {
        self.event_history.drain(..).collect()
    }

    /// 履歴の上限を超えて破棄されたイベントの累計件数。
    pub fn discarded_event_count(&self) -> u64 {
        self.discarded_events
    }

    pub fn config(&self) -> &PaperExecutionConfig {
        &self.config
    }

    fn lock_funds_for_order(&mut self, order: &DesiredLimitOrder) -> Result<(), PaperRejectReason> {
        let (asset, required) = match order.side {
            OrderSide::Buy => {
                let notional = order.amount * order.price;
                let fee =
                    positive_quote_fee(notional, self.config.fee_schedule.maker_fee_rate_quote);
                (self.quote_asset.clone(), notional + fee)
            }
            OrderSide::Sell => (self.base_asset.clone(), order.amount),
        };

        let balance = self
            .balances
            .get_mut(&asset)
            .expect("paper engine balances were validated at construction");
        if balance.free_amount < required {
            return Err(PaperRejectReason::InsufficientFunds {
                asset,
                required,
                free: balance.free_amount,
            });
        }

        balance.free_amount -= required;
        balance.locked_amount += required;
        balance.onhand_amount = balance.free_amount + balance.locked_amount;
        Ok(())
    }

    fn unlock_funds_for_open_order(&mut self, order: &OpenOrder) {
        let asset = match order.side {
            OrderSide::Buy => self.quote_asset.clone(),
            OrderSide::Sell => self.base_asset.clone(),
        };
        let amount =
            locked_amount_for_open_order(order, self.config.fee_schedule.maker_fee_rate_quote);
        move_locked_to_free(&mut self.balances, &asset, amount);
    }

    fn fill_order_ids_for_trade(&self, trade: &MarketTrade) -> Vec<OrderId> {
        let mut fill_candidates = self
            .open_orders
            .values()
            .filter(|order| {
                order.remaining_amount > Decimal::ZERO
                    && trade.executed_at > self.effective_after[&order.order_id]
                    && self
                        .cancel_at
                        .get(&order.order_id)
                        .is_none_or(|t| trade.executed_at < *t)
                    && order_matches_trade(order, trade)
            })
            .cloned()
            .collect::<Vec<_>>();

        fill_candidates.sort_by(|a, b| match a.side {
            OrderSide::Buy => b
                .price
                .cmp(&a.price)
                .then_with(|| a.order_id.cmp(&b.order_id)),
            OrderSide::Sell => a
                .price
                .cmp(&b.price)
                .then_with(|| a.order_id.cmp(&b.order_id)),
        });

        fill_candidates
            .into_iter()
            .map(|order| order.order_id)
            .collect()
    }

    fn fill_open_order(
        &mut self,
        mut open_order: OpenOrder,
        trade: MarketTrade,
        fill_amount: Decimal,
    ) -> PaperEvent {
        let mut order = open_order
            .to_desired_limit_order()
            .expect("paper engine only stores limit orders with prices");
        order.amount = fill_amount;
        let notional = fill_amount * order.price;
        let fee_amount_quote = notional * self.config.fee_schedule.maker_fee_rate_quote;

        match order.side {
            OrderSide::Buy => {
                let locked_amount = notional
                    + positive_quote_fee(notional, self.config.fee_schedule.maker_fee_rate_quote);
                decrease_locked(&mut self.balances, &self.quote_asset, locked_amount);
                add_free(&mut self.balances, &self.base_asset, fill_amount);
                if fee_amount_quote < Decimal::ZERO {
                    add_free(&mut self.balances, &self.quote_asset, -fee_amount_quote);
                }
            }
            OrderSide::Sell => {
                decrease_locked(&mut self.balances, &self.base_asset, fill_amount);
                add_free(
                    &mut self.balances,
                    &self.quote_asset,
                    notional - fee_amount_quote,
                );
            }
        }

        open_order.remaining_amount -= fill_amount;
        let order_id = open_order.order_id;
        let price = open_order
            .price
            .expect("paper engine limit order must have price");
        if open_order.remaining_amount > Decimal::ZERO || self.cancel_at.contains_key(&order_id) {
            self.open_orders.insert(order_id, open_order);
        } else {
            self.effective_after.remove(&order_id);
            self.cancel_at.remove(&order_id);
        }

        PaperEvent::OrderFilled {
            order_id,
            order,
            price,
            amount: fill_amount,
            fee_amount_quote,
            trade,
        }
    }

    fn record_event(&mut self, event: PaperEvent) {
        log::info!("paper event: {:?}", event);
        if let Some(r) = &self.recorder {
            r.record("paper_event",serde_json::json!({"event":event,"balances":self.balances(),"open_orders":self.open_orders()}));
        }
        if self.event_history.len() == PAPER_EVENT_HISTORY_LIMIT {
            self.event_history.pop_front();
            self.discarded_events = self.discarded_events.saturating_add(1);
        }
        self.event_history.push_back(event);
    }
}

mod latency;
pub use latency::{PaperLatencyConfig, PaperOperationLatency, PaperTradingVenue};

fn parse_jpy_pair(pair: &str) -> Result<(String, String), PaperExecutionError> {
    let Some(base) = pair.strip_suffix("_jpy") else {
        return Err(PaperExecutionError::UnsupportedPair(pair.to_owned()));
    };
    if base.is_empty() || base.contains('_') {
        return Err(PaperExecutionError::UnsupportedPair(pair.to_owned()));
    }

    Ok((base.to_owned(), "jpy".to_owned()))
}

fn order_matches_trade(order: &OpenOrder, trade: &MarketTrade) -> bool {
    let Some(price) = order.price else {
        return false;
    };

    match (trade.side, order.side) {
        (OrderSide::Buy, OrderSide::Sell) => trade.price >= price,
        (OrderSide::Sell, OrderSide::Buy) => trade.price <= price,
        _ => false,
    }
}

fn locked_amount_for_open_order(order: &OpenOrder, maker_fee_rate_quote: Decimal) -> Decimal {
    match order.side {
        OrderSide::Buy => {
            let notional = order.remaining_amount
                * order
                    .price
                    .expect("paper engine limit order must have price");
            notional + positive_quote_fee(notional, maker_fee_rate_quote)
        }
        OrderSide::Sell => order.remaining_amount,
    }
}

fn positive_quote_fee(notional: Decimal, fee_rate: Decimal) -> Decimal {
    let fee = notional * fee_rate;
    if fee > Decimal::ZERO {
        fee
    } else {
        Decimal::ZERO
    }
}

fn move_locked_to_free(
    balances: &mut BTreeMap<String, BalanceSnapshot>,
    asset: &str,
    amount: Decimal,
) {
    decrease_locked(balances, asset, amount);
    add_free(balances, asset, amount);
}

fn decrease_locked(balances: &mut BTreeMap<String, BalanceSnapshot>, asset: &str, amount: Decimal) {
    let balance = balances
        .get_mut(asset)
        .expect("paper engine balance must exist");
    balance.locked_amount -= amount;
    balance.onhand_amount = balance.free_amount + balance.locked_amount;
}

fn add_free(balances: &mut BTreeMap<String, BalanceSnapshot>, asset: &str, amount: Decimal) {
    let balance = balances
        .get_mut(asset)
        .expect("paper engine balance must exist");
    balance.free_amount += amount;
    balance.onhand_amount = balance.free_amount + balance.locked_amount;
}

impl From<PaperRejectReason> for PaperExecutionError {
    fn from(value: PaperRejectReason) -> Self {
        match value {
            PaperRejectReason::PairMismatch { expected, actual } => {
                Self::PairMismatch { expected, actual }
            }
            PaperRejectReason::NonPositiveOrder { amount, price } => {
                Self::NonPositiveOrder { amount, price }
            }
            PaperRejectReason::InsufficientFunds {
                asset,
                required,
                free,
            } => Self::InsufficientFunds {
                asset,
                required,
                free,
            },
            PaperRejectReason::UnsupportedPair(pair) => Self::UnsupportedPair(pair),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balance(asset: &str, free_amount: Decimal) -> BalanceSnapshot {
        BalanceSnapshot {
            asset: asset.to_owned(),
            free_amount,
            locked_amount: Decimal::ZERO,
            onhand_amount: free_amount,
        }
    }

    fn engine_with_balances(btc_free: Decimal, jpy_free: Decimal) -> PaperExecutionEngine {
        PaperExecutionEngine::new(
            PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap(),
            crate::paper_execution::PaperClock::Replay { now_ms: 0 },
            vec![balance("btc", btc_free), balance("jpy", jpy_free)],
            None,
        )
        .unwrap()
    }

    #[test]
    fn utc_rollback_does_not_reverse_operation_floor_or_sealed_boundary() {
        let mut engine = engine_with_balances(Decimal::ONE, Decimal::ZERO);
        // Realtimeの読み取り後と同じ境界へ、補正前後の取引所時計サンプルを渡す。
        assert_eq!(engine.observe_operation_time(1200), 1200);
        assert_eq!(engine.observe_operation_time(1100), 1200);
        engine.advance_trade_watermark(1300).unwrap();
        assert_eq!(engine.observe_operation_time(1150), 1300);
        // 逆行したローカル時計での取消も最初の未確定ms以降。
        engine
            .place_order(order(OrderSide::Sell, Decimal::ONE, Decimal::ONE))
            .unwrap();
        engine.cancel_orders("btc_jpy", vec![OrderId(1)]);
        assert_eq!(engine.cancel_at[&OrderId(1)], 1301);
    }

    fn order(side: OrderSide, amount: Decimal, price: Decimal) -> DesiredLimitOrder {
        DesiredLimitOrder::limit("btc_jpy".to_owned(), side, amount, price)
    }

    fn trade(side: OrderSide, amount: Decimal, price: Decimal, transaction_id: i64) -> MarketTrade {
        MarketTrade {
            amount,
            executed_at: 1_710_000_000_000,
            price,
            side,
            transaction_id,
        }
    }

    fn transactions(trades: Vec<MarketTrade>) -> MarketEvent {
        MarketEvent::Transactions {
            pair: "btc_jpy".to_owned(),
            transactions: trades,
        }
    }

    fn balance_of(engine: &PaperExecutionEngine, asset: &str) -> BalanceSnapshot {
        engine
            .balances()
            .into_iter()
            .find(|balance| balance.asset == asset)
            .unwrap()
    }

    #[test]
    fn bitbank_default_fee_depends_on_pair() {
        assert_eq!(
            PaperFeeSchedule::bitbank_spot_default("btc_jpy").unwrap(),
            PaperFeeSchedule::new(Decimal::ZERO, Decimal::new(1, 3))
        );
        assert_eq!(
            PaperFeeSchedule::bitbank_spot_default("eth_jpy").unwrap(),
            PaperFeeSchedule::new(Decimal::new(-2, 4), Decimal::new(12, 4))
        );
    }

    #[test]
    fn place_buy_order_locks_quote_balance() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(1_000_000, 0));

        let placed = engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();

        assert_eq!(placed.order_id, Some(OrderId(1)));
        assert_eq!(
            balance_of(&engine, "jpy").free_amount,
            Decimal::new(500_000, 0)
        );
        assert_eq!(
            balance_of(&engine, "jpy").locked_amount,
            Decimal::new(500_000, 0)
        );
        assert_eq!(engine.open_orders().len(), 1);
        assert!(matches!(
            engine.drain_events().as_slice(),
            [PaperEvent::OrderAccepted {
                order_id: OrderId(1),
                ..
            }]
        ));
    }

    #[test]
    fn place_sell_order_locks_base_balance() {
        let mut engine = engine_with_balances(Decimal::new(2, 1), Decimal::ZERO);

        engine
            .place_order(order(
                OrderSide::Sell,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();

        assert_eq!(balance_of(&engine, "btc").free_amount, Decimal::new(1, 1));
        assert_eq!(balance_of(&engine, "btc").locked_amount, Decimal::new(1, 1));
    }

    #[test]
    fn insufficient_funds_rejects_order_and_records_event() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(100_000, 0));
        let order = order(
            OrderSide::Buy,
            Decimal::new(1, 1),
            Decimal::new(5_000_000, 0),
        );

        let result = engine.place_order(order.clone());

        assert!(matches!(
            result,
            Err(PaperExecutionError::InsufficientFunds { .. })
        ));
        assert!(engine.open_orders().is_empty());
        assert!(matches!(
            engine.drain_events().as_slice(),
            [PaperEvent::OrderRejected {
                order: rejected_order,
                reason: PaperRejectReason::InsufficientFunds { .. }
            }] if rejected_order == &order
        ));
    }

    #[test]
    fn non_positive_amount_or_price_rejects_order_without_changing_balances() {
        for invalid_order in [
            order(OrderSide::Buy, Decimal::ZERO, Decimal::new(5_000_000, 0)),
            order(
                OrderSide::Buy,
                Decimal::new(-1, 1),
                Decimal::new(5_000_000, 0),
            ),
            order(OrderSide::Buy, Decimal::new(1, 1), Decimal::ZERO),
            order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(-5_000_000, 0),
            ),
            order(OrderSide::Sell, Decimal::ZERO, Decimal::new(5_000_000, 0)),
            order(
                OrderSide::Sell,
                Decimal::new(-1, 1),
                Decimal::new(5_000_000, 0),
            ),
            order(OrderSide::Sell, Decimal::new(1, 1), Decimal::ZERO),
            order(
                OrderSide::Sell,
                Decimal::new(1, 1),
                Decimal::new(-5_000_000, 0),
            ),
        ] {
            let mut engine = engine_with_balances(Decimal::new(1, 0), Decimal::new(1_000_000, 0));

            let result = engine.place_order(invalid_order);

            assert!(result.is_err());
            assert!(engine.open_orders().is_empty());
            assert_eq!(
                balance_of(&engine, "btc"),
                balance("btc", Decimal::new(1, 0))
            );
            assert_eq!(
                balance_of(&engine, "jpy"),
                balance("jpy", Decimal::new(1_000_000, 0))
            );
            assert!(matches!(
                engine.drain_events().as_slice(),
                [PaperEvent::OrderRejected {
                    reason: PaperRejectReason::NonPositiveOrder { .. },
                    ..
                }]
            ));
        }
    }

    #[test]
    fn cancel_order_unlocks_balance_and_missing_id_succeeds() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(1_000_000, 0));
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        engine.cancel_orders("btc_jpy", vec![OrderId(1), OrderId(999)]);
        engine.advance_trade_watermark(0).unwrap();

        assert!(engine.open_orders().is_empty());
        assert_eq!(
            balance_of(&engine, "jpy").free_amount,
            Decimal::new(1_000_000, 0)
        );
        assert_eq!(balance_of(&engine, "jpy").locked_amount, Decimal::ZERO);
        assert!(matches!(
            engine.drain_events().as_slice(),
            [
                PaperEvent::OrderCancelPending { .. },
                PaperEvent::OrderCancelled {
                    order_id: OrderId(1),
                    ..
                }
            ]
        ));
    }

    #[test]
    fn buy_trade_fills_sell_order_and_updates_balances() {
        let mut engine = engine_with_balances(Decimal::new(1, 1), Decimal::ZERO);
        engine
            .place_order(order(
                OrderSide::Sell,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Buy,
            Decimal::new(1, 1),
            Decimal::new(5_100_000, 0),
            10,
        )]));
        let events = engine.advance_trade_watermark(1_710_000_000_000).unwrap();

        assert!(engine.open_orders().is_empty());
        assert_eq!(balance_of(&engine, "btc").locked_amount, Decimal::ZERO);
        assert_eq!(
            balance_of(&engine, "jpy").free_amount,
            Decimal::new(500_000, 0)
        );
        assert!(matches!(
            events.as_slice(),
            [PaperEvent::OrderFilled {
                order_id: OrderId(1),
                price,
                amount,
                fee_amount_quote,
                ..
            }] if *price == Decimal::new(5_000_000, 0)
                && *amount == Decimal::new(1, 1)
                && *fee_amount_quote == Decimal::ZERO
        ));
    }

    #[test]
    fn sell_trade_fills_buy_order_and_updates_balances() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(1_000_000, 0));
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Sell,
            Decimal::new(1, 1),
            Decimal::new(4_900_000, 0),
            11,
        )]));
        let events = engine.advance_trade_watermark(1_710_000_000_000).unwrap();

        assert!(engine.open_orders().is_empty());
        assert_eq!(balance_of(&engine, "btc").free_amount, Decimal::new(1, 1));
        assert_eq!(balance_of(&engine, "jpy").locked_amount, Decimal::ZERO);
        assert_eq!(
            balance_of(&engine, "jpy").free_amount,
            Decimal::new(500_000, 0)
        );
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn positive_maker_fee_for_buy_order_is_not_charged_twice() {
        let mut engine = PaperExecutionEngine::new(
            PaperExecutionConfig {
                pair: "btc_jpy".to_owned(),
                fee_schedule: PaperFeeSchedule::new(Decimal::new(1, 3), Decimal::ZERO),
                next_order_id: OrderId(1),
                time_model: PaperTimeModel::default(),
            },
            crate::paper_execution::PaperClock::Replay { now_ms: 0 },
            vec![
                balance("btc", Decimal::ZERO),
                balance("jpy", Decimal::new(1_000_000, 0)),
            ],
            None,
        )
        .unwrap();
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Sell,
            Decimal::new(1, 1),
            Decimal::new(5_000_000, 0),
            12,
        )]));
        let events = engine.advance_trade_watermark(1_710_000_000_000).unwrap();

        assert_eq!(balance_of(&engine, "btc").free_amount, Decimal::new(1, 1));
        assert_eq!(balance_of(&engine, "jpy").locked_amount, Decimal::ZERO);
        assert_eq!(
            balance_of(&engine, "jpy").free_amount,
            Decimal::new(499_500, 0)
        );
        assert!(matches!(
            events.as_slice(),
            [PaperEvent::OrderFilled {
                fee_amount_quote, ..
            }] if *fee_amount_quote == Decimal::new(500, 0)
        ));
    }

    #[test]
    fn trade_does_not_fill_when_pair_side_or_price_do_not_match() {
        let mut engine = engine_with_balances(Decimal::new(1, 1), Decimal::new(1_000_000, 0));
        engine
            .place_order(order(
                OrderSide::Sell,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        assert!(
            engine
                .apply_market_event(&transactions(vec![trade(
                    OrderSide::Sell,
                    Decimal::new(1, 1),
                    Decimal::new(5_100_000, 0),
                    12,
                )]))
                .is_empty()
        );
        assert!(
            engine
                .apply_market_event(&transactions(vec![trade(
                    OrderSide::Buy,
                    Decimal::new(1, 1),
                    Decimal::new(4_900_000, 0),
                    13,
                )]))
                .is_empty()
        );
        assert!(
            engine
                .apply_market_event(&MarketEvent::Transactions {
                    pair: "eth_jpy".to_owned(),
                    transactions: vec![trade(
                        OrderSide::Buy,
                        Decimal::new(1, 1),
                        Decimal::new(5_100_000, 0),
                        14,
                    )],
                })
                .is_empty()
        );
        assert_eq!(engine.open_orders().len(), 1);
    }

    #[test]
    fn maker_rebate_is_applied_to_quote_balance() {
        let mut engine = PaperExecutionEngine::new(
            PaperExecutionConfig::bitbank_spot_default("eth_jpy").unwrap(),
            crate::paper_execution::PaperClock::Replay { now_ms: 0 },
            vec![
                balance("eth", Decimal::new(1, 0)),
                balance("jpy", Decimal::ZERO),
            ],
            None,
        )
        .unwrap();
        engine
            .place_order(DesiredLimitOrder::limit(
                "eth_jpy".to_owned(),
                OrderSide::Sell,
                Decimal::new(1, 0),
                Decimal::new(500_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        engine.apply_market_event(&MarketEvent::Transactions {
            pair: "eth_jpy".to_owned(),
            transactions: vec![trade(
                OrderSide::Buy,
                Decimal::new(1, 0),
                Decimal::new(500_000, 0),
                20,
            )],
        });

        let events = engine.advance_trade_watermark(1_710_000_000_000).unwrap();
        assert_eq!(
            balance_of(&engine, "jpy").free_amount,
            Decimal::new(500_100, 0)
        );
        assert!(matches!(
            events.as_slice(),
            [PaperEvent::OrderFilled {
                fee_amount_quote, ..
            }] if *fee_amount_quote == Decimal::new(-100, 0)
        ));
    }

    #[test]
    fn fills_multiple_orders_by_price_then_order_id() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(2_000_000, 0));
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(4_900_000, 0),
            ))
            .unwrap();
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Sell,
            Decimal::new(3, 1),
            Decimal::new(4_800_000, 0),
            30,
        )]));
        let events = engine.advance_trade_watermark(1_710_000_000_000).unwrap();

        let fill_ids = events
            .into_iter()
            .map(|event| match event {
                PaperEvent::OrderFilled { order_id, .. } => order_id,
                _ => panic!("unexpected paper event"),
            })
            .collect::<Vec<_>>();
        assert_eq!(fill_ids, vec![OrderId(2), OrderId(3), OrderId(1)]);
    }

    #[tokio::test]
    async fn paper_trading_venue_places_and_cancels() {
        let executor = PaperTradingVenue::new(engine_with_balances(
            Decimal::ZERO,
            Decimal::new(1_000_000, 0),
        ));

        let placed = executor
            .place_order(PlacementRequest::from(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            )))
            .await
            .unwrap();
        executor
            .cancel_orders("btc_jpy", vec![placed.order_id.unwrap()])
            .await
            .unwrap();

        let engine = executor.engine();
        let mut engine = engine.lock().unwrap();
        engine.advance_trade_watermark(0).unwrap();
        assert!(engine.open_orders().is_empty());
        assert!(matches!(
            engine.drain_events().as_slice(),
            [
                PaperEvent::OrderAccepted { .. },
                PaperEvent::OrderCancelPending { .. },
                PaperEvent::OrderCancelled { .. }
            ]
        ));
    }

    #[test]
    fn unsupported_non_jpy_pair_is_rejected_at_construction() {
        let result = PaperExecutionConfig::bitbank_spot_default("xrp_btc");

        assert!(matches!(
            result,
            Err(PaperExecutionError::UnsupportedPair(pair)) if pair == "xrp_btc"
        ));
    }

    #[test]
    fn all_paper_events_can_be_drained() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(1_000_000, 0));
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();

        assert_eq!(engine.drain_events().len(), 1);
        assert!(engine.drain_events().is_empty());
    }

    #[test]
    fn trade_amount_caps_fill_and_leaves_remaining_open_order_amount() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(1_500_000, 0));
        for _ in 0..3 {
            engine
                .place_order(order(
                    OrderSide::Buy,
                    Decimal::new(1, 1),
                    Decimal::new(5_000_000, 0),
                ))
                .unwrap();
        }
        engine.drain_events();

        engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Sell,
            Decimal::new(8, 2),
            Decimal::new(5_000_000, 0),
            40,
        )]));
        let events = engine.advance_trade_watermark(1_710_000_000_000).unwrap();

        assert!(matches!(
            events.as_slice(),
            [PaperEvent::OrderFilled {
                order_id,
                amount,
                fee_amount_quote,
                ..
            }] if *order_id == OrderId(1)
                && *amount == Decimal::new(8, 2)
                && *fee_amount_quote == Decimal::ZERO
        ));
        assert_eq!(
            engine.open_orders(),
            vec![
                OpenOrder {
                    order_id: OrderId(1),
                    pair: "btc_jpy".to_owned(),
                    side: OrderSide::Buy,
                    order_type: OrderType::Limit,
                    remaining_amount: Decimal::new(2, 2),
                    price: Some(Decimal::new(5_000_000, 0)),
                    post_only: Some(true),
                },
                OpenOrder {
                    order_id: OrderId(2),
                    pair: "btc_jpy".to_owned(),
                    side: OrderSide::Buy,
                    order_type: OrderType::Limit,
                    remaining_amount: Decimal::new(1, 1),
                    price: Some(Decimal::new(5_000_000, 0)),
                    post_only: Some(true),
                },
                OpenOrder {
                    order_id: OrderId(3),
                    pair: "btc_jpy".to_owned(),
                    side: OrderSide::Buy,
                    order_type: OrderType::Limit,
                    remaining_amount: Decimal::new(1, 1),
                    price: Some(Decimal::new(5_000_000, 0)),
                    post_only: Some(true),
                },
            ]
        );
        assert_eq!(balance_of(&engine, "btc").free_amount, Decimal::new(8, 2));
        assert_eq!(
            balance_of(&engine, "jpy").locked_amount,
            Decimal::new(1_100_000, 0)
        );
    }
}
