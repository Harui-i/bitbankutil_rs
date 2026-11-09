use crate::book_units::{Amount, Price, Quantity, Rate, literal};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

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
    pub fn exchange_to_local_ms(self, exchange_ms: i64) -> Result<i64, PaperExecutionError> {
        let Self::FixedOffset {
            exchange_minus_local_ms,
        } = self;
        exchange_ms
            .checked_sub(exchange_minus_local_ms)
            .ok_or(PaperExecutionError::InvalidClock)
    }
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
    /// mode未観測時だけNormal(NONE)を仮定する。観測済みmodeは常に優先。
    pub assume_normal_mode: bool,
}

impl PaperExecutionConfig {
    pub fn bitbank_spot_default(pair: impl Into<String>) -> Result<Self, PaperExecutionError> {
        let pair = pair.into();
        Ok(Self {
            fee_schedule: PaperFeeSchedule::bitbank_spot_default(&pair)?,
            pair,
            next_order_id: OrderId(1),
            time_model: PaperTimeModel::default(),
            assume_normal_mode: true,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PaperFeeSchedule {
    pub maker_fee_rate_quote: Rate,
    pub taker_fee_rate_quote: Rate,
}

impl PaperFeeSchedule {
    pub fn new(maker_fee_rate_quote: Rate, taker_fee_rate_quote: Rate) -> Self {
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
            Ok(Self::new(literal("0"), literal("0.001")))
        } else {
            Ok(Self::new(literal("-0.0002"), literal("0.0012")))
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
        price: Price,
        amount: Quantity,
        fee_amount_quote: Amount,
        trade: MarketTrade,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PaperRejectReason {
    Numeric(crate::book_units::NumericError),
    PostOnlyWouldTakeLiquidity,
    ExecutionModelUnavailable(PaperModelUnavailable),
    PairMismatch {
        expected: String,
        actual: String,
    },
    NonPositiveOrder {
        amount: Quantity,
        price: Price,
    },
    InsufficientFunds {
        asset: String,
        required: Amount,
        free: Amount,
    },
    UnsupportedPair(String),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum PaperExecutionError {
    Numeric(crate::book_units::NumericError),
    PostOnlyWouldTakeLiquidity,
    ExecutionModelUnavailable(PaperModelUnavailable),
    InvalidClock,
    InvalidWatermark,
    InvalidTradeStream,
    PairMismatch {
        expected: String,
        actual: String,
    },
    NonPositiveOrder {
        amount: Quantity,
        price: Price,
    },
    InsufficientFunds {
        asset: String,
        required: Amount,
        free: Amount,
    },
    MissingBalance(String),
    UnsupportedPair(String),
}

/// 確定境界と未確定状態。issuesがある評価は正常な損益として扱わない。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PaperSettlementStatus {
    pub execution_model: &'static str,
    pub simulation: Option<crate::simulation::SimulationConfig>,
    pub book: Option<PaperBookEvidence>,
    pub book_age_ms: Option<i64>,
    /// 数量または自注文を持つ価格のみ。side/price順。空価格の根拠は診断件数で確認する。
    pub queue: Vec<PaperQueueLevel>,
    pub queue_diagnostics: PaperQueueDiagnostics,
    /// 根拠不足・未対応の評価履歴。新しい板が届いても消さない。
    pub model_issues: Vec<String>,
    pub observed_market_mode: Option<String>,
    pub normal_mode_assumed: bool,
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
    /// 未確定の採取状態を応答した履歴。後のwatermarkで消さない。
    pub uncertain_query_observations: usize,
    pub unsettled: bool,
    pub issues: Vec<String>,
}

impl PaperSettlementStatus {
    /// 記録用の終了状態。queue本体は専用診断列に分離する。
    pub fn recording_summary(&self) -> serde_json::Value {
        serde_json::json!({"execution_model":self.execution_model,"simulation":self.simulation,"book":self.book,"book_age_ms":self.book_age_ms,"model_issues":self.model_issues,"observed_market_mode":self.observed_market_mode,"normal_mode_assumed":self.normal_mode_assumed,"time_model":self.time_model,"watermark_ms":self.watermark_ms,"max_trade_delay_ms":self.max_trade_delay_ms,"buffered_trades":self.buffered_trades,"pending_cancellations":self.pending_cancellations,"pending_operations":self.pending_operations,"latency_model":self.latency_model,"pending_operation_details":self.pending_operation_details,"uncertain_query_observations":self.uncertain_query_observations,"unsettled":self.unsettled,"issues":self.issues})
    }
}

/// 口座照会の採取に必要な確定状態。キューや保留操作の詳細を複製しない。
/// issuesはエンジンを借用し、採取値として保存する場合だけコピーする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PaperSettlementObservation<'a> {
    pub watermark_ms: Option<i64>,
    /// 現在の採取値の未確定性。過去の未確定照会の累積は含めない。
    pub unsettled: bool,
    pub issues: &'a [String],
    pub model_issues: &'a [String],
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
    simulation_active: bool,
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
    uncertain_query_observations: usize,
    queue_model: queue::QueueModel,
    base_asset: String,
    quote_asset: String,
    balances: BTreeMap<String, BalanceSnapshot>,
    open_orders: BTreeMap<OrderId, OpenOrder>,
    event_history: VecDeque<PaperEvent>,
    discarded_events: u64,
    queue_recording_id: u64,
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

        for balance in balances.values() {
            balance.free_amount.checked_add(balance.locked_amount)?;
        }
        let engine = Self {
            config,
            simulation_active: false,
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
            uncertain_query_observations: 0,
            queue_model: queue::QueueModel::default(),
            base_asset,
            quote_asset,
            balances,
            open_orders: BTreeMap::new(),
            event_history: VecDeque::new(),
            discarded_events: 0,
            queue_recording_id: recorder.as_ref().map_or(0, |r| r.next_id()),
            recorder,
        };
        if let Some(recorder) = &engine.recorder {
            recorder.record_lazy(
                "paper_initial_state",
                || serde_json::json!({
                    "config": engine.config(), "clock": engine.clock, "model": engine.execution_model(),
                    "queue_stream_id":engine.queue_recording_id,
                    "balances": engine.balances(), "open_orders": engine.open_orders()
                }),
            );
        }
        engine.record_queue(false);
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

        if order.amount <= Quantity::ZERO || order.price <= Price::ZERO {
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
        let evidence = match self
            .queue_model
            .admission(&order, self.config.assume_normal_mode)
        {
            Ok(evidence) => evidence,
            Err(reason) => {
                if let PaperRejectReason::ExecutionModelUnavailable(ref unavailable) = reason {
                    self.queue_model
                        .issue(&format!("admission unavailable: {unavailable:?}"));
                }
                if let Some(recorder) = &self.recorder {
                    recorder.record_lazy("paper_queue_rejection", || serde_json::json!({"order":order,"reason":reason,"exchange_ms":exchange_now,"book":self.queue_model.evidence(),"observed_market_mode":self.queue_model.observed_mode()}));
                }
                self.record_event(PaperEvent::OrderRejected {
                    order,
                    reason: reason.clone(),
                });
                return Err(reason.into());
            }
        };
        let effective = self
            .time_floor
            .max(exchange_now)
            .max(self.watermark_ms.unwrap_or(i64::MIN));
        if self.open_orders.values().any(|existing| {
            existing.side != order.side
                && existing.remaining_amount > Quantity::ZERO
                && self
                    .cancel_at
                    .get(&existing.order_id)
                    .is_none_or(|t| effective < *t)
                && existing.price.is_some_and(|p| match order.side {
                    OrderSide::Buy => order.price >= p,
                    OrderSide::Sell => order.price <= p,
                })
        }) {
            let reason = if order.post_only == Some(true) {
                PaperRejectReason::PostOnlyWouldTakeLiquidity
            } else {
                self.queue_model
                    .issue("admission unavailable: own crossing order requires taker");
                PaperRejectReason::ExecutionModelUnavailable(
                    PaperModelUnavailable::UnsupportedTaker,
                )
            };
            self.record_event(PaperEvent::OrderRejected {
                order,
                reason: reason.clone(),
            });
            return Err(reason.into());
        }
        let next_order_id = self
            .config
            .next_order_id
            .0
            .checked_add(1)
            .ok_or(crate::book_units::NumericError::Overflow)?;
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
        self.config.next_order_id = OrderId(next_order_id);
        self.queue_model.accept(&order, order_id, self.time_floor);
        if let Some(recorder) = &self.recorder {
            recorder.record_lazy("paper_queue_admission", || serde_json::json!({
                "order_id":order_id,"model":self.execution_model(),"book":evidence,
                "activation_exchange_ms":exchange_now,"activation_local_ms":self.config.time_model.local_to_exchange_ms(0).ok().and_then(|d|exchange_now.checked_sub(d)),"effective_after_exchange_ms":self.time_floor,
                "book_age_ms":exchange_now.checked_sub(evidence.exchange_at_ms),
                "normal_mode_assumed":self.config.assume_normal_mode && self.queue_model.observed_mode().is_none(),"observed_market_mode":self.queue_model.observed_mode(),
            }));
        }
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
        if !order_ids.iter().any(|id| {
            self.open_orders.get(id).is_some_and(|o| o.pair == pair)
                && !self.cancel_at.contains_key(id)
        }) {
            return;
        }
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
        self.apply_timed_market_event(event, None)
    }
    pub(crate) fn apply_timed_market_event(
        &mut self,
        event: &MarketEvent,
        received_local_ms: Option<i64>,
    ) -> Vec<PaperEvent> {
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
            let local_ms = match self.clock {
                PaperClock::Replay { now_ms } => now_ms,
                PaperClock::Realtime => crate::execution_recording::timestamp_ms() as i64,
            };
            self.queue_model.observe_book(
                depth,
                received_local_ms.unwrap_or(local_ms),
                received_exchange_ms,
            );
            self.record_queue(false);
            if let Some(recorder) = &self.recorder {
                recorder.record_lazy("paper_queue_book", || serde_json::json!({"model":self.execution_model(),"book":self.queue_model.evidence(),"issues":self.queue_model.issues}));
            }
        }
        if let MarketEvent::CircuitBreakInfo { pair, info } = event
            && pair == &self.config.pair
        {
            self.queue_model.observe_mode(&info.mode);
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
                if trade.amount <= Quantity::ZERO || trade.price <= Price::ZERO {
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
            let trade = self.buffered_trades.first_key_value().unwrap().1.clone();
            let plan =
                self.queue_model
                    .plan_allocations(&trade, &self.open_orders, &self.cancel_at);
            let allocations = plan.fills.clone();
            let mut balances = self.balances.clone();
            let mut orders = self.open_orders.clone();
            let mut staged = Vec::new();
            for &(id, quantity) in &allocations {
                let order = orders.remove(&id).expect("allocated order");
                let (order, event) = settle_fill(
                    &mut balances,
                    &self.base_asset,
                    &self.quote_asset,
                    order,
                    trade.clone(),
                    quantity,
                    self.config.fee_schedule.maker_fee_rate_quote,
                )?;
                if order.remaining_amount > Quantity::ZERO || self.cancel_at.contains_key(&id) {
                    orders.insert(id, order);
                }
                let account = self.stage_event_account(&balances, &orders);
                staged.push((event, account));
            }
            self.buffered_trades.pop_first();
            self.queue_model.commit_allocations(plan);
            self.balances = balances;
            self.open_orders = orders;
            for (id, _) in &allocations {
                if !self.open_orders.contains_key(id) {
                    self.queue_model.remove(*id);
                    self.effective_after.remove(id);
                    self.cancel_at.remove(id);
                }
            }
            self.record_queue(false);
            if let Some(recorder) = &self.recorder {
                recorder.record_lazy("paper_queue_trade",||serde_json::json!({"trade":trade,"allocations":allocations,"reason":"same_price_fifo_or_price_through_public_volume_cap"}));
            }
            for (event, account) in staged {
                self.record_event_with_account(event.clone(), account);
                events.push(event);
            }
        }
        events.extend(self.finalize_cancellations(through_ms)?);
        self.watermark_ms = Some(through_ms);
        self.record_settlement();
        Ok(events)
    }

    fn finalize_cancellations(
        &mut self,
        through_ms: i64,
    ) -> Result<Vec<PaperEvent>, PaperExecutionError> {
        if !self.settlement_issues.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<_> = self
            .cancel_at
            .iter()
            .filter(|(_, t)| **t <= through_ms)
            .map(|(id, _)| *id)
            .collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut balances = self.balances.clone();
        let mut orders = self.open_orders.clone();
        let mut staged = Vec::new();
        for id in &ids {
            if let Some(order) = orders.remove(id) {
                let asset = if order.side == OrderSide::Buy {
                    &self.quote_asset
                } else {
                    &self.base_asset
                };
                move_locked_to_free(
                    &mut balances,
                    asset,
                    locked_amount_for_open_order(
                        &order,
                        self.config.fee_schedule.maker_fee_rate_quote,
                    )?,
                )?;
                let event = PaperEvent::OrderCancelled {
                    order_id: *id,
                    order,
                };
                let account = self.stage_event_account(&balances, &orders);
                staged.push((event, account));
            }
        }
        // 全取消の数値検証が成功するまで、口座・注文・ログを変更しない。
        self.watermark_ms = Some(through_ms);
        self.balances = balances;
        self.open_orders = orders;
        for id in ids {
            self.cancel_at.remove(&id);
            self.effective_after.remove(&id);
            self.queue_model.remove(id);
        }
        let mut events = Vec::new();
        for (event, account) in staged {
            self.record_event_with_account(event.clone(), account);
            events.push(event);
        }
        Ok(events)
    }

    /// 有限遅延を仮定して自動確定する場合だけ明示設定する。既定では自動確定しない。
    /// 境界は取引所時計へ変換した受信時計 - delay - 1ms。利用者の仮定。
    pub fn with_max_trade_delay_ms(mut self, delay: i64) -> Result<Self, PaperExecutionError> {
        if delay < 0 {
            return Err(PaperExecutionError::InvalidWatermark);
        }
        self.max_trade_delay_ms = Some(delay);
        if let Some(r) = &self.recorder {
            r.record_lazy(
                "paper_delay_assumption",
                || serde_json::json!({"max_trade_delay_ms": delay}),
            );
        }
        Ok(self)
    }

    /// 切断・欠損を記録し、以降の確定と新規注文を停止する。再接続だけでは解除しない。
    pub fn mark_incomplete(&mut self, reason: impl Into<String>) {
        self.settlement_issues.push(reason.into());
        self.record_settlement();
    }

    /// キュー本体を複製せず、保持量を取得する。数量を持つlevelと生存注文を走査する。
    pub fn queue_diagnostics(&self) -> PaperQueueDiagnostics {
        self.queue_model.diagnostics()
    }

    /// 現在の口座照会用の確定状態を、割当やキューの走査なしで取得する。
    /// 過去の未確定照会も含めた評価全体の診断には [`Self::settlement_status`] を使う。
    pub fn settlement_observation(&self) -> PaperSettlementObservation<'_> {
        PaperSettlementObservation {
            watermark_ms: self.watermark_ms,
            unsettled: self.current_unsettled(),
            issues: &self.settlement_issues,
            model_issues: &self.queue_model.issues,
        }
    }

    /// 全キューの根拠を複製する詳細診断。終了時にも取得・保存する。
    /// 未確定残高を確定済み損益へ混ぜない。通常の照会には [`Self::settlement_observation`] を使う。
    pub fn settlement_status(&self) -> PaperSettlementStatus {
        PaperSettlementStatus {
            execution_model: self.execution_model(),
            simulation: None,
            book: self.queue_model.evidence(),
            book_age_ms: self
                .queue_model
                .evidence()
                .and_then(|b| self.exchange_now_ms().ok()?.checked_sub(b.exchange_at_ms)),
            queue: self.queue_model.state(),
            queue_diagnostics: self.queue_diagnostics(),
            model_issues: self.queue_model.issues.clone(),
            observed_market_mode: self.queue_model.observed_mode(),
            normal_mode_assumed: self.config.assume_normal_mode
                && self.queue_model.observed_mode().is_none(),
            time_model: self.config.time_model,
            watermark_ms: self.watermark_ms,
            max_trade_delay_ms: self.max_trade_delay_ms,
            buffered_trades: self.buffered_trades.len(),
            pending_cancellations: self.cancel_at.len(),
            pending_operations: 0,
            latency_model: PaperLatencyConfig::default(),
            pending_operation_details: Vec::new(),
            uncertain_query_observations: self.uncertain_query_observations,
            unsettled: self.current_unsettled()
                || self.uncertain_query_observations > 0
                || (self.simulation_active
                    && (!self.cancel_at.is_empty() || !self.buffered_trades.is_empty())),
            issues: self.settlement_issues.clone(),
        }
    }

    /// 現在の採取値の未確定性。評価全体の履歴とは分離する。
    fn current_unsettled(&self) -> bool {
        if self.simulation_active {
            return !self.settlement_issues.is_empty() || !self.queue_model.issues.is_empty();
        }
        let now = self.exchange_now_ms();
        !self.settlement_issues.is_empty()
            || !self.queue_model.issues.is_empty()
            || !self.buffered_trades.is_empty()
            || !self.cancel_at.is_empty()
            || self
                .watermark_ms
                .is_none_or(|w| now.map_or(true, |now| w < now.max(self.time_floor)))
    }

    /// 現在の全キューを明示的に保存する。Disabledでは構築も保存もしない。
    pub fn record_queue_snapshot(&self) {
        self.record_queue(true);
    }

    fn record_queue(&self, force: bool) {
        if let Some(r) = &self.recorder {
            r.queue_record(
                self.queue_recording_id,
                force,
                || {
                    self.queue_model
                        .state()
                        .iter()
                        .map(|level| serde_json::to_value(level).expect("queue serialization"))
                        .collect()
                },
                || serde_json::json!(self.queue_diagnostics()),
            );
        }
    }

    fn settlement_record_data(&self) -> serde_json::Value {
        serde_json::json!({"execution_model":self.execution_model(),
            "book":self.queue_model.evidence(),
            "book_age_ms":self.queue_model.evidence().and_then(|b| self.exchange_now_ms().ok()?.checked_sub(b.exchange_at_ms)),
            "model_issues":self.queue_model.issues,
            "observed_market_mode":self.queue_model.observed_mode(),
            "normal_mode_assumed":self.config.assume_normal_mode && self.queue_model.observed_mode().is_none(),
            "time_model":self.config.time_model,"clock":self.clock,"watermark_ms":self.watermark_ms,
            "max_trade_delay_ms":self.max_trade_delay_ms,
            "buffered_trades":self.buffered_trades.len(),"pending_cancellations":self.cancel_at.len(),
            "uncertain_query_observations":self.uncertain_query_observations,
            "unsettled":self.current_unsettled() || self.uncertain_query_observations > 0,
            "issues":self.settlement_issues,"balances":self.balances(),"open_orders":self.open_orders()})
    }

    /// 現在の口座・watermark・未確定理由をキュー本体の複製なしで保存する。
    /// エンジンを直接利用した場合も終了時に呼ぶ。キュー診断は設定した周期に従う。
    /// 終了時の全キューが必要なら [`Self::record_queue_snapshot`] も呼ぶ。
    pub fn record_settlement(&self) {
        self.record_queue(false);
        if let Some(r) = &self.recorder {
            r.record_lazy("paper_settlement", || self.settlement_record_data());
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

    pub fn execution_model(&self) -> &'static str {
        if self.simulation_active {
            "exchange_timeline_fifo_v2"
        } else {
            "observed_book_fifo_v1"
        }
    }
    pub fn config(&self) -> &PaperExecutionConfig {
        &self.config
    }

    fn lock_funds_for_order(&mut self, order: &DesiredLimitOrder) -> Result<(), PaperRejectReason> {
        let (asset, required) = match order.side {
            OrderSide::Buy => (
                self.quote_asset.clone(),
                required_quote(
                    order.price,
                    order.amount,
                    self.config.fee_schedule.maker_fee_rate_quote,
                )?,
            ),
            OrderSide::Sell => (self.base_asset.clone(), order.amount.to_amount()),
        };
        let balance = self.balances.get(&asset).expect("validated balance");
        if balance.free_amount < required {
            return Err(PaperRejectReason::InsufficientFunds {
                asset,
                required,
                free: balance.free_amount,
            });
        }
        let mut next = balance.clone();
        next.free_amount = next.free_amount.checked_sub(required)?;
        next.locked_amount = next.locked_amount.checked_add(required)?;
        next.onhand_amount = next.free_amount.checked_add(next.locked_amount)?;
        self.balances.insert(asset, next);
        Ok(())
    }

    /// ログ用の中間口座。未記録・Disabledでは複製しない。
    fn stage_event_account(
        &self,
        balances: &BTreeMap<String, BalanceSnapshot>,
        orders: &BTreeMap<OrderId, OpenOrder>,
    ) -> Option<AccountSnapshot> {
        self.recorder
            .as_ref()
            .filter(|r| r.enabled("paper_event"))
            .map(|_| AccountSnapshot {
                balances: balances.values().cloned().collect(),
                // open_orders()と同じく、取消確定待ちの注文は公開しない。
                open_orders: orders
                    .values()
                    .filter(|o| !self.cancel_at.contains_key(&o.order_id))
                    .cloned()
                    .collect(),
                query: None,
            })
    }

    fn record_event(&mut self, event: PaperEvent) {
        self.record_event_with_account(event, None);
    }

    fn record_event_with_account(&mut self, event: PaperEvent, account: Option<AccountSnapshot>) {
        self.record_queue(false);
        match &event {
            PaperEvent::OrderAccepted {
                order_id, order, ..
            } => {
                tracing::info!(event = "paper_order_accepted", order_id = order_id.0, pair = %crate::telemetry::safe(&order.pair), side = order.side.as_str(), price = %order.price, amount = %order.amount)
            }
            PaperEvent::OrderRejected { .. } => tracing::warn!(event = "paper_order_rejected"),
            PaperEvent::OrderCancelPending { order_id, .. } => {
                tracing::info!(event = "paper_cancel_pending", order_id = order_id.0)
            }
            PaperEvent::OrderCancelled { order_id, .. } => {
                tracing::info!(event = "paper_order_cancelled", order_id = order_id.0)
            }
            PaperEvent::OrderFilled {
                order_id,
                price,
                amount,
                fee_amount_quote,
                ..
            } => {
                tracing::info!(event = "paper_order_filled", order_id = order_id.0, price = %price, amount = %amount, fee_amount_quote = %fee_amount_quote)
            }
        }
        if let Some(r) = &self.recorder {
            r.record_lazy("paper_event", || {
                let account = account.unwrap_or_else(|| AccountSnapshot {
                    balances: self.balances(), open_orders: self.open_orders(), query: None,
                });
                serde_json::json!({"event":event,"clock":self.clock,"watermark_ms":self.watermark_ms,
                    "balances":account.balances,"open_orders":account.open_orders})
            });
        }
        if self.event_history.len() == PAPER_EVENT_HISTORY_LIMIT {
            self.event_history.pop_front();
            self.discarded_events = self.discarded_events.saturating_add(1);
        }
        self.event_history.push_back(event);
    }
}

mod latency;
pub use latency::{
    OwnedPaperExecution, PaperDiagnosticSnapshot, PaperDiagnostics, PaperLatencyConfig,
    PaperOperationLatency, PaperTradingVenue,
};
mod queue;
pub use queue::{
    PaperBookEvidence, PaperModelUnavailable, PaperQueueDiagnostics, PaperQueueLevel,
    PaperQueueSegment,
};

fn parse_jpy_pair(pair: &str) -> Result<(String, String), PaperExecutionError> {
    let Some(base) = pair.strip_suffix("_jpy") else {
        return Err(PaperExecutionError::UnsupportedPair(pair.to_owned()));
    };
    if base.is_empty() || base.contains('_') {
        return Err(PaperExecutionError::UnsupportedPair(pair.to_owned()));
    }

    Ok((base.to_owned(), "jpy".to_owned()))
}

fn required_quote(
    price: Price,
    quantity: Quantity,
    rate: Rate,
) -> Result<Amount, crate::book_units::NumericError> {
    price
        .notional(quantity)?
        .checked_add(price.fee(quantity, rate)?.max(Amount::ZERO))
}
fn locked_amount_for_open_order(
    order: &OpenOrder,
    rate: Rate,
) -> Result<Amount, crate::book_units::NumericError> {
    match order.side {
        OrderSide::Buy => required_quote(
            order.price.expect("limit price"),
            order.remaining_amount,
            rate,
        ),
        OrderSide::Sell => Ok(order.remaining_amount.to_amount()),
    }
}
fn move_locked_to_free(
    balances: &mut BTreeMap<String, BalanceSnapshot>,
    asset: &str,
    amount: Amount,
) -> Result<(), crate::book_units::NumericError> {
    decrease_locked(balances, asset, amount)?;
    add_free(balances, asset, amount)
}
fn decrease_locked(
    balances: &mut BTreeMap<String, BalanceSnapshot>,
    asset: &str,
    amount: Amount,
) -> Result<(), crate::book_units::NumericError> {
    let b = balances.get_mut(asset).expect("validated balance");
    b.locked_amount = b.locked_amount.checked_sub(amount)?;
    b.onhand_amount = b.free_amount.checked_add(b.locked_amount)?;
    Ok(())
}
fn add_free(
    balances: &mut BTreeMap<String, BalanceSnapshot>,
    asset: &str,
    amount: Amount,
) -> Result<(), crate::book_units::NumericError> {
    let b = balances.get_mut(asset).expect("validated balance");
    b.free_amount = b.free_amount.checked_add(amount)?;
    b.onhand_amount = b.free_amount.checked_add(b.locked_amount)?;
    Ok(())
}
fn settle_fill(
    balances: &mut BTreeMap<String, BalanceSnapshot>,
    base: &str,
    quote: &str,
    mut open: OpenOrder,
    trade: MarketTrade,
    quantity: Quantity,
    rate: Rate,
) -> Result<(OpenOrder, PaperEvent), crate::book_units::NumericError> {
    let mut order = open.to_desired_limit_order().expect("limit order");
    order.amount = quantity;
    let price = order.price;
    let notional = price.notional(quantity)?;
    let fee = price.fee(quantity, rate)?;
    match order.side {
        OrderSide::Buy => {
            decrease_locked(
                balances,
                quote,
                notional.checked_add(fee.max(Amount::ZERO))?,
            )?;
            add_free(balances, base, quantity.to_amount())?;
            if fee < Amount::ZERO {
                add_free(balances, quote, fee.checked_neg()?)?;
            }
        }
        OrderSide::Sell => {
            decrease_locked(balances, base, quantity.to_amount())?;
            add_free(balances, quote, notional.checked_sub(fee)?)?;
        }
    }
    open.remaining_amount = open.remaining_amount.checked_sub(quantity)?;
    let event = PaperEvent::OrderFilled {
        order_id: open.order_id,
        order,
        price,
        amount: quantity,
        fee_amount_quote: fee,
        trade,
    };
    Ok((open, event))
}
impl From<crate::book_units::NumericError> for PaperRejectReason {
    fn from(e: crate::book_units::NumericError) -> Self {
        Self::Numeric(e)
    }
}
impl From<crate::book_units::NumericError> for PaperExecutionError {
    fn from(e: crate::book_units::NumericError) -> Self {
        Self::Numeric(e)
    }
}

impl From<PaperRejectReason> for PaperExecutionError {
    fn from(value: PaperRejectReason) -> Self {
        match value {
            PaperRejectReason::Numeric(e) => Self::Numeric(e),
            PaperRejectReason::PostOnlyWouldTakeLiquidity => Self::PostOnlyWouldTakeLiquidity,
            PaperRejectReason::ExecutionModelUnavailable(reason) => {
                Self::ExecutionModelUnavailable(reason)
            }
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
    use crate::book_units::*;
    #[test]
    fn recording_without_diagnostics_does_not_clone_queue() {
        use crate::execution_recording::{ExecutionRecorder, JsonlSink, RecordingProfile};
        for profile in [
            None,
            Some(RecordingProfile::Disabled),
            Some(RecordingProfile::Financial),
        ] {
            let recorder = profile.map(|p| {
                ExecutionRecorder::with_profile(
                    "no-clone",
                    serde_json::json!({}),
                    p,
                    JsonlSink(std::io::sink()),
                )
            });
            let before = super::queue::STATE_CALLS.with(|n| n.get());
            let mut engine = super::PaperExecutionEngine::new(
                super::PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap(),
                super::PaperClock::Replay { now_ms: 0 },
                ["btc", "jpy"]
                    .into_iter()
                    .map(|asset| crate::order_domain::BalanceSnapshot {
                        asset: asset.into(),
                        free_amount: 0.into(),
                        locked_amount: 0.into(),
                        onhand_amount: 0.into(),
                    })
                    .collect(),
                recorder,
            )
            .unwrap();
            engine.mark_incomplete("test");
            let venue = super::PaperTradingVenue::new(engine);
            crate::trading_venue::TradingVenue::record_paper_settlement(&venue);
            assert_eq!(super::queue::STATE_CALLS.with(|n| n.get()), before);
        }
    }

    use super::*;

    fn balance(asset: &str, free_amount: Amount) -> BalanceSnapshot {
        BalanceSnapshot {
            asset: asset.to_owned(),
            free_amount,
            locked_amount: literal("0"),
            onhand_amount: free_amount,
        }
    }

    fn engine_with_balances(btc_free: Amount, jpy_free: Amount) -> PaperExecutionEngine {
        let mut engine = PaperExecutionEngine::new(
            PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap(),
            crate::paper_execution::PaperClock::Replay { now_ms: 0 },
            vec![balance("btc", btc_free), balance("jpy", jpy_free)],
            None,
        )
        .unwrap();
        seed_book(&mut engine);
        engine
    }

    fn seed_book(engine: &mut PaperExecutionEngine) {
        engine.apply_market_event(&MarketEvent::DepthUpdated {
            pair: engine.config.pair.clone(),
            depth: crate::market_event::MarketDepthSnapshot::new(
                [(literal("1000000000"), literal("1.0"))].into(),
                [(literal("0.00000001"), literal("1.0"))].into(),
                0,
            ),
        });
    }

    #[test]
    fn numeric_order_failure_does_not_lock_funds_or_advance_operation_state() {
        let mut e = engine_with_balances(Amount::ONE, Amount::MAX);
        let before = e.balances();
        let floor = e.time_floor;
        let id = e.config.next_order_id;
        let order = DesiredLimitOrder::limit(
            "btc_jpy".into(),
            OrderSide::Buy,
            literal("100000000"),
            literal("100000000"),
        );
        assert_eq!(
            e.place_order(order),
            Err(PaperExecutionError::Numeric(NumericError::Overflow))
        );
        assert_eq!(e.balances(), before);
        assert!(e.open_orders.is_empty());
        assert_eq!(e.time_floor, floor);
        assert_eq!(e.config.next_order_id, id);
        assert_eq!(e.queue_diagnostics().live_orders, 0);
    }
    #[test]
    fn numeric_fill_failure_keeps_account_orders_fifo_and_buffered_trade() {
        let mut e = engine_with_balances(Amount::ONE, Amount::MAX);
        e.config.fee_schedule.maker_fee_rate_quote = literal("-0.0002");
        e.place_order(DesiredLimitOrder::limit(
            "btc_jpy".into(),
            OrderSide::Sell,
            Quantity::ONE,
            100.into(),
        ))
        .unwrap();
        e.advance_replay_time(1000).unwrap();
        e.apply_market_event(&MarketEvent::Transactions {
            pair: "btc_jpy".into(),
            transactions: vec![MarketTrade {
                amount: Quantity::ONE,
                price: 100.into(),
                side: OrderSide::Buy,
                transaction_id: 1,
                executed_at: 1000,
            }],
        });
        let balances = e.balances();
        let orders = e.open_orders.clone();
        let queue = e.queue_model.state();
        let watermark = e.watermark_ms;
        assert_eq!(
            e.advance_trade_watermark(1000),
            Err(PaperExecutionError::Numeric(NumericError::Overflow))
        );
        assert_eq!(e.balances(), balances);
        assert_eq!(e.open_orders, orders);
        assert_eq!(e.queue_model.state(), queue);
        assert_eq!(e.watermark_ms, watermark);
        assert_eq!(e.buffered_trades.len(), 1);
    }
    #[test]
    fn numeric_cancel_failure_keeps_pending_cancellation_and_queue() {
        let mut e = engine_with_balances(Amount::ONE, Amount::ZERO);
        e.place_order(DesiredLimitOrder::limit(
            "btc_jpy".into(),
            OrderSide::Sell,
            Quantity::ONE,
            100.into(),
        ))
        .unwrap();
        e.balances.get_mut("btc").unwrap().free_amount = Amount::MAX;
        e.cancel_orders("btc_jpy", vec![OrderId(1)]);
        let balances = e.balances();
        let orders = e.open_orders.clone();
        let queue = e.queue_model.state();
        let pending = e.cancel_at.clone();
        assert_eq!(
            e.advance_trade_watermark(1000),
            Err(PaperExecutionError::Numeric(NumericError::Overflow))
        );
        assert_eq!(e.balances(), balances);
        assert_eq!(e.open_orders, orders);
        assert_eq!(e.queue_model.state(), queue);
        assert_eq!(e.cancel_at, pending);
        assert_eq!(e.watermark_ms, None);
    }

    #[test]
    fn later_cancellation_overflow_does_not_emit_or_apply_first_release() {
        use crate::execution_recording::{
            ExecutionRecorder, JsonlSink, read_records, tests::Memory,
        };
        let memory = Memory::default();
        let mut e = PaperExecutionEngine::new(
            PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap(),
            PaperClock::Replay { now_ms: 0 },
            vec![balance("btc", Amount::ONE), balance("jpy", 1000.into())],
            Some(ExecutionRecorder::new(
                "cancel-atomic",
                serde_json::json!({}),
                JsonlSink(memory.clone()),
            )),
        )
        .unwrap();
        seed_book(&mut e);
        e.place_order(DesiredLimitOrder::limit(
            "btc_jpy".into(),
            OrderSide::Sell,
            1.into(),
            110.into(),
        ))
        .unwrap();
        e.place_order(DesiredLimitOrder::limit(
            "btc_jpy".into(),
            OrderSide::Buy,
            1.into(),
            90.into(),
        ))
        .unwrap();
        // 最初のbase資金解放は成功し、2件目のquote資金解放だけが失敗する状態を注入。
        e.balances.get_mut("jpy").unwrap().free_amount = Amount::MAX;
        e.advance_replay_time(10).unwrap();
        e.cancel_orders("btc_jpy", vec![OrderId(1), OrderId(2)]);
        let balances = e.balances();
        let orders = e.open_orders.clone();
        let queue = e.queue_model.state();
        let pending = e.cancel_at.clone();
        assert_eq!(
            e.advance_trade_watermark(10),
            Err(PaperExecutionError::Numeric(NumericError::Overflow))
        );
        assert_eq!(e.balances(), balances);
        assert_eq!(e.open_orders, orders);
        assert_eq!(e.queue_model.state(), queue);
        assert_eq!(e.cancel_at, pending);
        assert_eq!(e.watermark_ms, None);
        let records = read_records(&memory.0.lock().unwrap()[..]).unwrap();
        assert!(!records.iter().any(|r| r.kind=="paper_event" && r.data["event"].get("OrderCancelled").is_some()));
    }

    #[test]
    fn utc_rollback_does_not_reverse_operation_floor_or_sealed_boundary() {
        let mut engine = engine_with_balances(literal("1"), literal("0"));
        // Realtimeの読み取り後と同じ境界へ、補正前後の取引所時計サンプルを渡す。
        assert_eq!(engine.observe_operation_time(1200), 1200);
        assert_eq!(engine.observe_operation_time(1100), 1200);
        engine.advance_trade_watermark(1300).unwrap();
        assert_eq!(engine.observe_operation_time(1150), 1300);
        // 逆行したローカル時計での取消も最初の未確定ms以降。
        engine
            .place_order(order(OrderSide::Sell, literal("1"), literal("1")))
            .unwrap();
        engine.cancel_orders("btc_jpy", vec![OrderId(1)]);
        assert_eq!(engine.cancel_at[&OrderId(1)], 1301);
    }

    fn order(side: OrderSide, amount: Quantity, price: Price) -> DesiredLimitOrder {
        DesiredLimitOrder::limit("btc_jpy".to_owned(), side, amount, price)
    }

    fn trade(side: OrderSide, amount: Quantity, price: Price, transaction_id: i64) -> MarketTrade {
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
            PaperFeeSchedule::new(literal("0"), literal("0.001"))
        );
        assert_eq!(
            PaperFeeSchedule::bitbank_spot_default("eth_jpy").unwrap(),
            PaperFeeSchedule::new(literal("-0.0002"), literal("0.0012"))
        );
    }

    #[test]
    fn place_buy_order_locks_quote_balance() {
        let mut engine = engine_with_balances(literal("0"), literal("1000000"));

        let placed = engine
            .place_order(order(OrderSide::Buy, literal("0.1"), literal("5000000")))
            .unwrap();

        assert_eq!(placed.order_id, Some(OrderId(1)));
        assert_eq!(balance_of(&engine, "jpy").free_amount, literal("500000"));
        assert_eq!(balance_of(&engine, "jpy").locked_amount, literal("500000"));
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
        let mut engine = engine_with_balances(literal("0.2"), literal("0"));

        engine
            .place_order(order(OrderSide::Sell, literal("0.1"), literal("5000000")))
            .unwrap();

        assert_eq!(balance_of(&engine, "btc").free_amount, literal("0.1"));
        assert_eq!(balance_of(&engine, "btc").locked_amount, literal("0.1"));
    }

    #[test]
    fn insufficient_funds_rejects_order_and_records_event() {
        let mut engine = engine_with_balances(literal("0"), literal("100000"));
        let order = order(OrderSide::Buy, literal("0.1"), literal("5000000"));

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
            order(OrderSide::Buy, literal("0"), literal("5000000")),
            order(OrderSide::Buy, literal("-0.1"), literal("5000000")),
            order(OrderSide::Buy, literal("0.1"), literal("0")),
            order(OrderSide::Buy, literal("0.1"), literal("-5000000")),
            order(OrderSide::Sell, literal("0"), literal("5000000")),
            order(OrderSide::Sell, literal("-0.1"), literal("5000000")),
            order(OrderSide::Sell, literal("0.1"), literal("0")),
            order(OrderSide::Sell, literal("0.1"), literal("-5000000")),
        ] {
            let mut engine = engine_with_balances(literal("1"), literal("1000000"));

            let result = engine.place_order(invalid_order);

            assert!(result.is_err());
            assert!(engine.open_orders().is_empty());
            assert_eq!(balance_of(&engine, "btc"), balance("btc", literal("1")));
            assert_eq!(
                balance_of(&engine, "jpy"),
                balance("jpy", literal("1000000"))
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
        let mut engine = engine_with_balances(literal("0"), literal("1000000"));
        engine
            .place_order(order(OrderSide::Buy, literal("0.1"), literal("5000000")))
            .unwrap();
        engine.drain_events();

        engine.cancel_orders("btc_jpy", vec![OrderId(1), OrderId(999)]);
        engine.advance_trade_watermark(0).unwrap();

        assert!(engine.open_orders().is_empty());
        assert_eq!(balance_of(&engine, "jpy").free_amount, literal("1000000"));
        assert_eq!(balance_of(&engine, "jpy").locked_amount, literal("0"));
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
        let mut engine = engine_with_balances(literal("0.1"), literal("0"));
        engine
            .place_order(order(OrderSide::Sell, literal("0.1"), literal("5000000")))
            .unwrap();
        engine.drain_events();

        engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Buy,
            literal("0.1"),
            literal("5100000"),
            10,
        )]));
        let events = engine.advance_trade_watermark(1_710_000_000_000).unwrap();

        assert!(engine.open_orders().is_empty());
        assert_eq!(balance_of(&engine, "btc").locked_amount, literal("0"));
        assert_eq!(balance_of(&engine, "jpy").free_amount, literal("500000"));
        assert!(matches!(
            events.as_slice(),
            [PaperEvent::OrderFilled {
                order_id: OrderId(1),
                price,
                amount,
                fee_amount_quote,
                ..
            }] if *price == literal("5000000")
                && *amount == literal("0.1")
                && *fee_amount_quote == literal("0")
        ));
    }

    #[test]
    fn sell_trade_fills_buy_order_and_updates_balances() {
        let mut engine = engine_with_balances(literal("0"), literal("1000000"));
        engine
            .place_order(order(OrderSide::Buy, literal("0.1"), literal("5000000")))
            .unwrap();
        engine.drain_events();

        engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Sell,
            literal("0.1"),
            literal("4900000"),
            11,
        )]));
        let events = engine.advance_trade_watermark(1_710_000_000_000).unwrap();

        assert!(engine.open_orders().is_empty());
        assert_eq!(balance_of(&engine, "btc").free_amount, literal("0.1"));
        assert_eq!(balance_of(&engine, "jpy").locked_amount, literal("0"));
        assert_eq!(balance_of(&engine, "jpy").free_amount, literal("500000"));
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn positive_maker_fee_for_buy_order_is_not_charged_twice() {
        let mut engine = PaperExecutionEngine::new(
            PaperExecutionConfig {
                pair: "btc_jpy".to_owned(),
                fee_schedule: PaperFeeSchedule::new(literal("0.001"), literal("0")),
                next_order_id: OrderId(1),
                time_model: PaperTimeModel::default(),
                assume_normal_mode: true,
            },
            crate::paper_execution::PaperClock::Replay { now_ms: 0 },
            vec![
                balance("btc", literal("0")),
                balance("jpy", literal("1000000")),
            ],
            None,
        )
        .unwrap();
        seed_book(&mut engine);
        engine
            .place_order(order(OrderSide::Buy, literal("0.1"), literal("5000000")))
            .unwrap();
        engine.drain_events();

        engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Sell,
            literal("0.1"),
            literal("5000000"),
            12,
        )]));
        let events = engine.advance_trade_watermark(1_710_000_000_000).unwrap();

        assert_eq!(balance_of(&engine, "btc").free_amount, literal("0.1"));
        assert_eq!(balance_of(&engine, "jpy").locked_amount, literal("0"));
        assert_eq!(balance_of(&engine, "jpy").free_amount, literal("499500"));
        assert!(matches!(
            events.as_slice(),
            [PaperEvent::OrderFilled {
                fee_amount_quote, ..
            }] if *fee_amount_quote == literal("500")
        ));
    }

    #[test]
    fn trade_does_not_fill_when_pair_side_or_price_do_not_match() {
        let mut engine = engine_with_balances(literal("0.1"), literal("1000000"));
        engine
            .place_order(order(OrderSide::Sell, literal("0.1"), literal("5000000")))
            .unwrap();
        engine.drain_events();

        assert!(
            engine
                .apply_market_event(&transactions(vec![trade(
                    OrderSide::Sell,
                    literal("0.1"),
                    literal("5100000"),
                    12,
                )]))
                .is_empty()
        );
        assert!(
            engine
                .apply_market_event(&transactions(vec![trade(
                    OrderSide::Buy,
                    literal("0.1"),
                    literal("4900000"),
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
                        literal("0.1"),
                        literal("5100000"),
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
            vec![balance("eth", literal("1")), balance("jpy", literal("0"))],
            None,
        )
        .unwrap();
        seed_book(&mut engine);
        engine
            .place_order(DesiredLimitOrder::limit(
                "eth_jpy".to_owned(),
                OrderSide::Sell,
                literal("1"),
                literal("500000"),
            ))
            .unwrap();
        engine.drain_events();

        engine.apply_market_event(&MarketEvent::Transactions {
            pair: "eth_jpy".to_owned(),
            transactions: vec![trade(OrderSide::Buy, literal("1"), literal("500000"), 20)],
        });

        let events = engine.advance_trade_watermark(1_710_000_000_000).unwrap();
        assert_eq!(balance_of(&engine, "jpy").free_amount, literal("500100"));
        assert!(matches!(
            events.as_slice(),
            [PaperEvent::OrderFilled {
                fee_amount_quote, ..
            }] if *fee_amount_quote == literal("-100")
        ));
    }

    #[test]
    fn fills_multiple_orders_by_price_then_order_id() {
        let mut engine = engine_with_balances(literal("0"), literal("2000000"));
        engine
            .place_order(order(OrderSide::Buy, literal("0.1"), literal("4900000")))
            .unwrap();
        seed_book(&mut engine);
        engine
            .place_order(order(OrderSide::Buy, literal("0.1"), literal("5000000")))
            .unwrap();
        seed_book(&mut engine);
        engine
            .place_order(order(OrderSide::Buy, literal("0.1"), literal("5000000")))
            .unwrap();
        engine.drain_events();

        engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Sell,
            literal("0.3"),
            literal("4800000"),
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
        let executor =
            PaperTradingVenue::new(engine_with_balances(literal("0"), literal("1000000")));

        let placed = executor
            .place_order(PlacementRequest::from(order(
                OrderSide::Buy,
                literal("0.1"),
                literal("5000000"),
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
        let mut engine = engine_with_balances(literal("0"), literal("1000000"));
        engine
            .place_order(order(OrderSide::Buy, literal("0.1"), literal("5000000")))
            .unwrap();

        assert_eq!(engine.drain_events().len(), 1);
        assert!(engine.drain_events().is_empty());
    }

    #[test]
    fn trade_amount_caps_fill_and_leaves_remaining_open_order_amount() {
        let mut engine = engine_with_balances(literal("0"), literal("1500000"));
        for _ in 0..3 {
            engine
                .place_order(order(OrderSide::Buy, literal("0.1"), literal("5000000")))
                .unwrap();
        }
        engine.drain_events();

        engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Sell,
            literal("0.08"),
            literal("5000000"),
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
                && *amount == literal("0.08")
                && *fee_amount_quote == literal("0")
        ));
        assert_eq!(
            engine.open_orders(),
            vec![
                OpenOrder {
                    order_id: OrderId(1),
                    pair: "btc_jpy".to_owned(),
                    side: OrderSide::Buy,
                    order_type: OrderType::Limit,
                    remaining_amount: literal("0.02"),
                    price: Some(literal("5000000")),
                    post_only: Some(true),
                },
                OpenOrder {
                    order_id: OrderId(2),
                    pair: "btc_jpy".to_owned(),
                    side: OrderSide::Buy,
                    order_type: OrderType::Limit,
                    remaining_amount: literal("0.1"),
                    price: Some(literal("5000000")),
                    post_only: Some(true),
                },
                OpenOrder {
                    order_id: OrderId(3),
                    pair: "btc_jpy".to_owned(),
                    side: OrderSide::Buy,
                    order_type: OrderType::Limit,
                    remaining_amount: literal("0.1"),
                    price: Some(literal("5000000")),
                    post_only: Some(true),
                },
            ]
        );
        assert_eq!(balance_of(&engine, "btc").free_amount, literal("0.08"));
        assert_eq!(balance_of(&engine, "jpy").locked_amount, literal("1100000"));
    }
}
