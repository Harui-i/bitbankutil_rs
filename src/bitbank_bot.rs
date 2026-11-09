use crate::bitbank_structs::{
    BitbankCircuitBreakInfo, BitbankDepth, BitbankDepthDiff, BitbankDepthWhole,
    BitbankTickerResponse, BitbankTransactionDatum,
};
use crate::market_event::{
    MarketDepthSnapshot, MarketEvent, MarketEventConversionError, MarketTrade,
};
use crate::trading_venue::{NoTradingVenue, TradingVenue};
use crate::websocket_handler::run_observed_websocket;
use crypto_botters::bitbank::BitbankOption;
use crypto_botters::websocket::WebSocketConfig;
use std::convert::Infallible;
use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::warn;
use tracing::{Instrument, instrument::WithSubscriber};

/// ボットが受け取る市場データ、またはユーザー定義のイベント。
/// 市場データはまずvenueに渡し（復旧経路では `on_market_event_recovering`）、その後で戦略に渡す。
/// paper tradingでは、この順序により戦略がwatermarkまで確定した注文・残高を参照できる。
#[derive(Debug, Clone, PartialEq)]
pub enum BotEvent<C = Infallible> {
    Market(MarketEvent),
    Custom(C),
}

/// 戦略が残高取得・発注・取消を行い、ボットへイベントを送るための窓口。
/// `venue()` でlive tradingまたはpaper tradingの注文・口座操作を利用できる。
/// クローンしても別口座は作らず、同じ口座と注文を操作する。
pub struct BotContext<C = Infallible, V = NoTradingVenue> {
    venue: V,
    received_at_ms: i64,
    time: Option<std::sync::Arc<std::sync::atomic::AtomicI64>>,
    pending: std::sync::Arc<std::sync::Mutex<Pending<C>>>,
}

// CにCloneを要求せず、同じ派生キューと注文・口座操作を共有する。
impl<C, V: Clone> Clone for BotContext<C, V> {
    fn clone(&self) -> Self {
        Self {
            venue: self.venue.clone(),
            received_at_ms: self.received_at_ms,
            time: self.time.clone(),
            pending: self.pending.clone(),
        }
    }
}

impl<C: Send + 'static, V: TradingVenue> BotContext<C, V> {
    /// このイベントが登録された時刻。API待機・配送滞留後も変わらない。
    pub fn received_at_ms(&self) -> i64 {
        self.received_at_ms
    }
    pub fn now_ms(&self) -> i64 {
        self.time
            .as_ref()
            .map(|t| t.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or_else(|| crate::execution_recording::timestamp_ms() as i64)
    }

    pub fn venue(&self) -> &V {
        &self.venue
    }

    /// ローカル仮想時刻にカスタムイベントを予約する。live経路・過去・閉じたcontextは拒否する。
    /// 同時刻は予約採番順。実行はreplayランナーが行う。模擬API待機中は時計が進む。
    pub fn schedule_at(&self, local_ms: i64, event: C) -> Result<(), ScheduleError> {
        self.time.as_ref().ok_or(ScheduleError::NotReplay)?;
        let now = self.now_ms();
        if local_ms < now {
            return Err(ScheduleError::Past);
        }
        let mut p = self.pending.lock().unwrap();
        if !p.open {
            return Err(ScheduleError::Closed);
        }
        if p.scheduled.len() >= p.schedule_limit {
            p.schedule_overflow = true;
            return Err(ScheduleError::Limit);
        }
        p.scheduled.push((local_ms, event));
        Ok(())
    }

    /// ローカル仮想時刻からの非負の遅延で予約する。加算の範囲外は拒否する。
    pub fn schedule_after(&self, delay_ms: i64, event: C) -> Result<(), ScheduleError> {
        self.time.as_ref().ok_or(ScheduleError::NotReplay)?;
        let now = self.now_ms();
        if delay_ms < 0 {
            return Err(ScheduleError::Past);
        }
        let at = now.checked_add(delay_ms).ok_or(ScheduleError::Overflow)?;
        self.schedule_at(at, event)
    }

    pub async fn emit(
        &self,
        event: BotEvent<C>,
    ) -> Result<(), mpsc::error::SendError<BotEvent<C>>> {
        {
            let pending = &self.pending;
            let mut guard = pending.lock().unwrap();
            if !guard.open {
                return Err(mpsc::error::SendError(event));
            }
            if guard.emitted >= guard.limit {
                guard.overflow = true;
                return Err(mpsc::error::SendError(event));
            }
            guard.emitted += 1;
            guard.queue.push_back(event);
            return Ok(());
        }
    }
}

/// 市場データなどを受け取り、発注や取消を判断する処理。
/// paper tradingの約定登録と、設定したwatermarkまでの残高更新は `handle_event()` の前に行う。
/// 戦略内でvenueへ市場データを再適用する必要はない。
/// ユーザー定義のイベントを使わない場合はCustomEventにInfallibleを指定する。
pub trait BotStrategy<V: TradingVenue = NoTradingVenue>: Send + 'static {
    type CustomEvent: Send + 'static;
    type Error: std::fmt::Debug + Send + 'static;

    /// Return a static, non-sensitive code for investigation. Error payloads are never formatted.
    fn error_code(_error: &Self::Error) -> &'static str {
        "strategy_error"
    }

    fn handle_event(
        &mut self,
        event: BotEvent<Self::CustomEvent>,
        ctx: &BotContext<Self::CustomEvent, V>,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send;
}

/// 送信開始時のローカル時刻を保持する入力窓口。
/// 遅延Paperでは外部入力もこの時刻まで戦略へ公開しない。
pub struct BotEventSender<C> {
    tx: mpsc::Sender<(i64, BotEvent<C>, tracing::Span)>,
}
impl<C> Clone for BotEventSender<C> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}
impl<C> BotEventSender<C> {
    pub async fn send(
        &self,
        event: BotEvent<C>,
    ) -> Result<(), mpsc::error::SendError<BotEvent<C>>> {
        let received = crate::execution_recording::timestamp_ms() as i64;
        self.send_at(received, event).await
    }
    /// 外部フィードのコールバックで採取済みのローカル受信時刻を維持する。
    pub async fn send_at(
        &self,
        received_at_ms: i64,
        event: BotEvent<C>,
    ) -> Result<(), mpsc::error::SendError<BotEvent<C>>> {
        self.tx
            .send((received_at_ms, event, tracing::Span::current()))
            .await
            .map_err(|e| mpsc::error::SendError(e.0.1))
    }
}

/// ボットアクターを生かし続け、基本的なライフサイクル制御を提供するハンドル。
pub struct BotHandle<E> {
    event_tx: BotEventSender<E>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<JoinHandle<Result<(), String>>>,
    span: tracing::Span,
}

impl<E: Send + 'static> BotHandle<E> {
    pub fn event_sender(&self) -> BotEventSender<E> {
        self.event_tx.clone()
    }

    pub async fn shutdown(mut self) -> Result<(), String> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        match self.join_handle.take() {
            Some(handle) => handle.await.map_err(|e| {
                tracing::error!(
                    parent: &self.span, event = "bot_join_failed",
                    canceled = e.is_cancelled(),
                    panicked = e.is_panic()
                );
                e.to_string()
            })?,
            _ => Ok(()),
        }
    }
}

impl<E> Drop for BotHandle<E> {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join_handle.take() {
            tracing::info!(parent: &self.span, event = "bot_abort_requested", reason_code = "handle_dropped", action = "abort_task");
            handle.abort();
        }
    }
}

fn spawn_bot_actor<S, V>(strategy: S, venue: V, buffer: usize) -> BotHandle<S::CustomEvent>
where
    V: TradingVenue,
    S: BotStrategy<V>,
{
    let (tx, mut event_rx) = mpsc::channel(buffer);
    let event_tx = BotEventSender { tx };
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let run_id = crate::telemetry::next_id();
    let run_span = tracing::info_span!("bot", run_id);
    let handle_span = run_span.clone();
    let join_handle = tokio::spawn(async move {
        let _lifetime = crate::telemetry::TaskLifetime("bot");
        tracing::info!(event = "bot_started", library_version = env!("CARGO_PKG_VERSION"));
        let mut runner = BotRunner::new(strategy, venue);
        let mut stopping = false;
        let wake = runner.wake.clone();
        loop {
            if let Err(err) = runner.poll_live() {
                runner
                    .venue()
                    .mark_market_incomplete("bot processing failed");
                runner.record_settlement();
                tracing::error!(event = "bot_failed", stage = "processing", reason_code = err.reason_code(), action = "stop_bot");
                return Err(format!("bot processing failed: {err:?}"));
            }
            select! {
                biased;
                _ = &mut shutdown_rx, if !stopping => { stopping = true; event_rx.close(); }
                maybe_event = event_rx.recv(), if !runner.handler_pending() || runner.venue().has_pending_operations() => {
                    let Some((received, event, source_span)) = maybe_event else {
                        // 将来の市場入力がなければwatermark待ちを完了できない。
                        // 送信済み操作はvenueに残し、shutdownを無期限に待たせない。
                        if runner.handler_pending() {
                            if runner.venue().has_pending_operations() {
                                runner.venue().mark_market_incomplete("shutdown with pending strategy operation");
                                runner.stop_pending_handler();
                            } else if let Err(err)=runner.drive_direct(false).await {
                                runner.venue().mark_market_incomplete("bot processing failed");
                                runner.record_settlement();
                                tracing::error!(event = "bot_failed", stage = "processing", reason_code = err.reason_code(), action = "stop_bot");
                return Err(format!("bot processing failed: {err:?}"));
                            }
                        }
                        break;
                    };
                    let source_span = if source_span.is_none() { tracing::Span::current() } else { source_span };
                    let delivery_span = tracing::info_span!(parent: &source_span, "bot_delivery", run_id);
                    let _source = delivery_span.enter();
                    tracing::debug!(event = "bot_input_received", received_at_ms = received,
                        processed_at_ms = crate::execution_recording::timestamp_ms());
                    if let Err(err) = runner.ingest(None, event, true, None, false) {
                        runner.venue().mark_market_incomplete("bot input failed");
                        runner.record_settlement();
                        tracing::error!(event = "bot_failed", stage = "input", reason_code = err.reason_code(), action = "stop_bot");
                        return Err(format!("bot input failed: {err:?}"));
                    }
                }
                _ = wake.notified() => {}
            }
        }
        runner.record_settlement();
        tracing::info!(event = "bot_stopped", requested = stopping, reason_code = if stopping { "shutdown_requested" } else { "input_closed" }, pending_operations = runner.venue().has_pending_operations(), pending_deliveries = runner.pending_delivery_count());
        Ok(())
    }.instrument(run_span).with_current_subscriber());

    BotHandle {
        event_tx,
        shutdown_tx: Some(shutdown_tx),
        join_handle: Some(join_handle),
        span: handle_span,
    }
}

/// BitbankのWebSocket接続から転送された生メッセージ。
#[derive(Debug, Clone)]
pub enum BitbankInboundMessage {
    Ticker(BitbankTickerResponse),
    Transactions(Vec<BitbankTransactionDatum>),
    DepthDiff(BitbankDepthDiff),
    DepthWhole(BitbankDepthWhole),
    CircuitBreakInfo(BitbankCircuitBreakInfo),
}

impl BitbankInboundMessage {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Ticker(_) => "ticker",
            Self::Transactions(_) => "transactions",
            Self::DepthDiff(_) => "depth_diff",
            Self::DepthWhole(_) => "depth_whole",
            Self::CircuitBreakInfo(_) => "circuit_break_info",
        }
    }
    fn observation(&self) -> (Option<i64>, Option<String>) {
        match self {
            Self::Ticker(v) => (v.timestamp.as_i64(), None),
            Self::Transactions(v) => (v.last().map(|t| t.executed_at), None),
            Self::DepthDiff(v) => (Some(v.t), Some(crate::telemetry::safe(&v.s))),
            Self::DepthWhole(v) => (
                Some(v.timestamp),
                Some(crate::telemetry::safe(&v.sequenceId)),
            ),
            Self::CircuitBreakInfo(v) => (v.timestamp.as_i64(), None),
        }
    }
}

#[derive(Debug)]
pub struct BitbankMarketEventConverter {
    pair: String,
    depth: BitbankDepth,
    failures: crate::telemetry::Failures,
    depth_degraded: bool,
    waiting_logged: bool,
}

impl BitbankMarketEventConverter {
    pub fn new(pair: String) -> Self {
        let failures = crate::telemetry::Failures::new("market_conversion", &pair);
        Self {
            pair,
            depth: BitbankDepth::new(),
            failures,
            depth_degraded: false,
            waiting_logged: false,
        }
    }

    pub fn convert(
        &mut self,
        message: BitbankInboundMessage,
    ) -> Result<Option<MarketEvent>, MarketEventConversionError> {
        let kind = message.kind();
        let (event_time_ms, sequence_id) = message.observation();
        let was_complete = self.depth.is_complete();
        let previous_whole = self.depth.whole_sequence();
        let transaction_index = match &message {
            BitbankInboundMessage::Transactions(v) => {
                v.iter().position(|t| t.side != "buy" && t.side != "sell")
            }
            _ => None,
        };
        let transaction_id = match &message {
            BitbankInboundMessage::Transactions(v) => v
                .iter()
                .find(|t| t.side != "buy" && t.side != "sell")
                .map(|t| t.transaction_id),
            _ => None,
        };
        let result = self.convert_inner(message);
        match &result {
            Err(error) => {
                let (error_kind, reason_code, field, side, level_index) = match error {
                    MarketEventConversionError::InvalidDepth(e) => (
                        "invalid_depth",
                        e.reason_code,
                        e.field,
                        e.side,
                        e.level_index,
                    ),
                    MarketEventConversionError::InvalidTradeSide(_) => {
                        ("invalid_trade_side", "unknown_side", "side", None, None)
                    }
                };
                if self.failures.record_for(kind, reason_code) {
                    tracing::warn!(event = "market_conversion_failed", pair = %crate::telemetry::safe(&self.pair),
                        message_type = kind, error_kind, reason_code, field, side, level_index, transaction_index, transaction_id,
                        event_time_ms, sequence_id, action = "drop_message", failure_count = self.failures.total,
                        suppressed_count = self.failures.suppressed, book_complete = self.depth.is_complete(),
                        last_book_event_time_ms = self.depth.last_timestamp());
                }
                if error_kind == "invalid_depth" && !self.depth_degraded {
                    self.depth_degraded = true;
                    tracing::warn!(event = "market_feed_state", pair = %crate::telemetry::safe(&self.pair),
                        state = "degraded", reason_code, action = "retain_previous_book", book_complete = self.depth.is_complete());
                }
            }
            Ok(_) => {
                if let Some(failure_count) = self.failures.recover(kind) {
                    tracing::info!(event = "market_input_recovered", pair = %crate::telemetry::safe(&self.pair), message_type = kind,
                        failure_count, event_time_ms, sequence_id);
                }
                if kind == "depth_whole"
                    && self.depth.whole_sequence() != previous_whole
                    && self.depth.is_complete()
                    && (!was_complete || self.depth_degraded)
                {
                    self.depth_degraded = false;
                    tracing::info!(event = "market_feed_state", pair = %crate::telemetry::safe(&self.pair), state = "ready",
                        reason_code = "snapshot_applied", event_time_ms, sequence_id, book_complete = true);
                } else if kind == "depth_diff" && !self.depth.is_complete() && !self.waiting_logged
                {
                    self.waiting_logged = true;
                    // First wait is observable without logging every buffered diff.
                    tracing::info!(event = "market_snapshot_pending", pair = %crate::telemetry::safe(&self.pair), event_time_ms, sequence_id);
                }
            }
        }
        result
    }

    fn convert_inner(
        &mut self,
        message: BitbankInboundMessage,
    ) -> Result<Option<MarketEvent>, MarketEventConversionError> {
        let event = match message {
            BitbankInboundMessage::Ticker(ticker) => Some(MarketEvent::Ticker {
                pair: self.pair.clone(),
                ticker: ticker.into(),
            }),
            BitbankInboundMessage::Transactions(transactions) => {
                let transactions = transactions
                    .into_iter()
                    .map(MarketTrade::try_from)
                    .collect::<Result<Vec<_>, _>>()?;
                Some(MarketEvent::Transactions {
                    pair: self.pair.clone(),
                    transactions,
                })
            }
            BitbankInboundMessage::DepthDiff(depth_diff) => {
                self.depth
                    .insert_diff(depth_diff)
                    .map_err(MarketEventConversionError::InvalidDepth)?;
                if self.depth.is_complete() {
                    Some(MarketEvent::DepthUpdated {
                        pair: self.pair.clone(),
                        depth: MarketDepthSnapshot::from(&self.depth),
                    })
                } else {
                    None
                }
            }
            BitbankInboundMessage::DepthWhole(depth_whole) => {
                self.depth
                    .update_whole(depth_whole)
                    .map_err(MarketEventConversionError::InvalidDepth)?;
                if self.depth.is_complete() {
                    Some(MarketEvent::DepthUpdated {
                        pair: self.pair.clone(),
                        depth: MarketDepthSnapshot::from(&self.depth),
                    })
                } else {
                    None
                }
            }
            BitbankInboundMessage::CircuitBreakInfo(info) => Some(MarketEvent::CircuitBreakInfo {
                pair: self.pair.clone(),
                info: info.into(),
            }),
        };

        Ok(event)
    }
}

async fn run_bitbank_pair_feed<E>(
    pair: String,
    client_options: Vec<BitbankOption>,
    websocket_config: WebSocketConfig,
    event_tx: BotEventSender<E>,
    observation: Option<crate::execution_recording::ExecutionRecorder>,
    stale_after: std::time::Duration,
) where
    E: Send + 'static,
{
    let (inbound_tx, mut inbound_rx) =
        mpsc::channel::<crate::websocket_handler::ObservedMessage>(128);
    let ws_task = tokio::spawn(
        run_observed_websocket(
            pair.clone(),
            client_options,
            websocket_config,
            inbound_tx,
            observation,
            stale_after,
        )
        .in_current_span()
        .with_current_subscriber(),
    );

    let mut converter = BitbankMarketEventConverter::new(pair.clone());
    let mut connection_epoch = 0;
    while let Some(observed) = inbound_rx.recv().await {
        if observed.connection_epoch != connection_epoch {
            converter.depth_degraded = true;
            connection_epoch = observed.connection_epoch;
        }
        let message = observed.message;
        let event = match observed.span.in_scope(|| converter.convert(message)) {
            Ok(event) => event,
            Err(_) => continue,
        };

        if let Some(event) = event {
            if event_tx
                .send_at(observed.received_at_ms, BotEvent::Market(event))
                .instrument(observed.span)
                .await
                .is_err()
            {
                tracing::debug!(event = "feed_stopped", pair = %crate::telemetry::safe(&pair), reason = "downstream_closed");
                break;
            }
        }
    }

    if ws_task.is_finished() {
        let reason_code = match ws_task.await {
            Ok(()) => "websocket_task_returned",
            Err(error) if error.is_panic() => "websocket_task_panicked",
            Err(_) => "websocket_task_cancelled",
        };
        tracing::error!(event = "feed_terminated", pair = %crate::telemetry::safe(&pair), reason_code, action = "stop_feed");
    } else {
        ws_task.abort();
    }
}

fn duplicate_bitbank_options(options: &[BitbankOption]) -> Vec<BitbankOption> {
    options
        .iter()
        .map(|option| match option {
            BitbankOption::Default => BitbankOption::Default,
            BitbankOption::Key(key) => BitbankOption::Key(key.clone()),
            BitbankOption::Secret(secret) => BitbankOption::Secret(secret.clone()),
            BitbankOption::HttpUrl(url) => BitbankOption::HttpUrl(*url),
            BitbankOption::HttpAuth(auth) => BitbankOption::HttpAuth(*auth),
            BitbankOption::RequestConfig(cfg) => BitbankOption::RequestConfig(cfg.clone()),
            BitbankOption::WebSocketUrl(url) => BitbankOption::WebSocketUrl(*url),
            BitbankOption::WebSocketChannels(channels) => {
                BitbankOption::WebSocketChannels(channels.clone())
            }
            BitbankOption::WebSocketConfig(config) => {
                BitbankOption::WebSocketConfig(config.clone())
            }
        })
        .collect()
}

/// BitbankのWebSocketフィードを[`BotStrategy`]に接続するビルダー。
pub struct BitbankBotBuilder<S, V = NoTradingVenue>
where
    V: TradingVenue,
    S: BotStrategy<V>,
{
    strategy: S,
    pairs: Vec<String>,
    default_options: Vec<BitbankOption>,
    websocket_config: WebSocketConfig,
    websocket_observation: Option<crate::execution_recording::ExecutionRecorder>,
    buffer_size: usize,
    stale_after: std::time::Duration,
    run_metadata: crate::telemetry::RunMetadata,
    venue: V,
}

impl<S, V> BitbankBotBuilder<S, V>
where
    V: TradingVenue,
    S: BotStrategy<V>,
{
    pub fn new(strategy: S, venue: V) -> Self {
        Self {
            strategy,
            pairs: Vec::new(),
            default_options: Vec::new(),
            websocket_config: WebSocketConfig::default(),
            websocket_observation: None,
            buffer_size: 128,
            stale_after: std::time::Duration::from_secs(30),
            run_metadata: Default::default(),
            venue,
        }
    }

    /// Non-sensitive application identity propagated to all runtime tasks.
    pub fn run_metadata(mut self, metadata: crate::telemetry::RunMetadata) -> Self {
        self.run_metadata = metadata;
        self
    }
    /// Diagnostic threshold for absence of valid WS input. Does not alter trading policy.
    pub fn feed_stale_after(mut self, duration: std::time::Duration) -> Self {
        self.stale_after = duration;
        self
    }

    pub fn add_pair(mut self, pair: impl Into<String>) -> Self {
        self.pairs.push(pair.into());
        self
    }

    pub fn with_pairs(mut self, pairs: Vec<String>) -> Self {
        self.pairs = pairs;
        self
    }

    pub fn websocket_config(mut self, websocket_config: WebSocketConfig) -> Self {
        self.websocket_config = websocket_config;
        self
    }

    /// WSコールバック冒頭の時計観測を指定したrecorderへ保存する。
    /// venueにも同じrecorderを渡すと、HTTPと同じrun_idに記録できる。
    pub fn websocket_observation(
        mut self,
        recorder: crate::execution_recording::ExecutionRecorder,
    ) -> Self {
        self.websocket_observation = Some(recorder);
        self
    }

    pub fn default_options(mut self, options: Vec<BitbankOption>) -> Self {
        self.default_options = options;
        self
    }

    pub fn buffer_size(mut self, size: usize) -> Self {
        self.buffer_size = size;
        self
    }

    pub fn spawn(self) -> BitbankBotRuntime<S::CustomEvent> {
        let application_span = self.run_metadata.span();
        let runtime_span = tracing::info_span!(parent: &application_span, "bot_runtime",
            runtime_id = crate::telemetry::next_id(), mode = self.venue.mode(),
            strategy_type = std::any::type_name::<S>(), venue_type = std::any::type_name::<V>());
        let _entered = runtime_span.enter();
        if self.pairs.is_empty() {
            warn!(event = "bot_no_subscriptions");
        }

        tracing::info!(event = "bot_configured", library_version = env!("CARGO_PKG_VERSION"),
            subscriptions = ?self.pairs.iter().take(32).map(|p| crate::telemetry::safe(p)).collect::<Vec<_>>(),
            subscription_count = self.pairs.len(), subscriptions_truncated = self.pairs.len() > 32,
            buffer_capacity = self.buffer_size, feed_stale_after_ms = self.stale_after.as_millis() as u64);
        if self.buffer_size == 0 {
            tracing::error!(
                event = "configuration_invalid",
                field = "buffer_size",
                reason_code = "non_positive_capacity",
                action = "panic"
            );
        }
        let simulation = self.venue.simulation_config().is_some();
        let (raw_tx, raw_rx) = mpsc::channel(self.buffer_size);
        let input_failed = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0));
        let actor = if simulation {
            spawn_simulation_actor(
                self.strategy,
                self.venue,
                self.buffer_size,
                raw_rx,
                input_failed.clone(),
            )
        } else {
            spawn_bot_actor(self.strategy, self.venue, self.buffer_size)
        };
        let event_tx = actor.event_sender();
        let mut feed_handles = Vec::new();

        for pair in self.pairs {
            let pair_options = duplicate_bitbank_options(&self.default_options);
            let tx = event_tx.clone();
            let config = self.websocket_config.clone();
            let observation = self.websocket_observation.clone();
            let raw_tx = raw_tx.clone();
            let failed = input_failed.clone();
            let stale_after = self.stale_after;
            let handle = tokio::spawn(
                async move {
                    if simulation {
                        crate::websocket_handler::run_simulation_websocket(
                            pair,
                            pair_options,
                            config,
                            raw_tx,
                            failed,
                            observation,
                            stale_after,
                        )
                        .await;
                    } else {
                        run_bitbank_pair_feed(
                            pair,
                            pair_options,
                            config,
                            tx,
                            observation,
                            stale_after,
                        )
                        .await;
                    }
                }
                .in_current_span()
                .with_current_subscriber(),
            );
            feed_handles.push(handle);
        }

        BitbankBotRuntime {
            bot_handle: Some(actor),
            feed_handles,
        }
    }
}

impl<S: BotStrategy> BitbankBotBuilder<S, NoTradingVenue> {
    /// 注文・口座取得を使わない市場データ閲覧用の起動設定。
    pub fn market_data(strategy: S) -> Self {
        Self::new(strategy, NoTradingVenue)
    }
}

/// アクターとフィードタスクを生かし続けるランタイム。ランタイムをドロップすると
/// フィードタスクが中止される。正常に停止する必要がある場合は[`Self::shutdown`]を呼び出す。
pub struct BitbankBotRuntime<E> {
    bot_handle: Option<BotHandle<E>>,
    feed_handles: Vec<JoinHandle<()>>,
}

impl<E: Send + 'static> BitbankBotRuntime<E> {
    pub fn event_sender(&self) -> BotEventSender<E> {
        self.bot_handle
            .as_ref()
            .expect("bitbank bot runtime missing actor handle")
            .event_sender()
    }

    pub async fn shutdown(mut self) -> Result<(), String> {
        for handle in &self.feed_handles {
            handle.abort();
        }
        match self.bot_handle.take() {
            Some(bot_handle) => bot_handle.shutdown().await,
            _ => Ok(()),
        }
    }
}

impl<E> Drop for BitbankBotRuntime<E> {
    fn drop(&mut self) {
        for handle in &self.feed_handles {
            handle.abort();
        }
        if let Some(bot_handle) = self.bot_handle.take() {
            drop(bot_handle);
        }
    }
}

/// 呼び出し元が独自の[`BitbankInboundMessage`]値のストリーム（たとえば、ログ再生）を提供する
/// 高度なシナリオ向けのヘルパー。この関数は`inbound_rx`からのメッセージを消費し、
/// 準備完了イベントを`event_tx`に転送する。
pub async fn forward_bitbank_messages<E>(
    pair: String,
    inbound_rx: &mut mpsc::Receiver<BitbankInboundMessage>,
    event_tx: &BotEventSender<E>,
) where
    E: Send + 'static,
{
    let mut converter = BitbankMarketEventConverter::new(pair.clone());
    while let Some(message) = inbound_rx.recv().await {
        let event = match converter.convert(message) {
            Ok(event) => event,
            Err(_) => continue,
        };

        if let Some(event) = event {
            if event_tx.send(BotEvent::Market(event)).await.is_err() {
                tracing::debug!(event = "feed_stopped", pair = %crate::telemetry::safe(&pair), reason = "downstream_closed");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // 合成の時刻/資金fixture用。注文をspread内に置き、先行数量を0にする。
    fn seeded_engine(
        mut engine: crate::paper_execution::PaperExecutionEngine,
        exchange_ms: i64,
    ) -> crate::paper_execution::PaperExecutionEngine {
        engine.apply_market_event(&crate::market_event::MarketEvent::DepthUpdated {
            pair: "btc_jpy".into(),
            depth: crate::market_event::MarketDepthSnapshot::new(
                std::collections::BTreeMap::from([(1_000_000.into(), literal("1.0"))]),
                std::collections::BTreeMap::from([(1.into(), literal("1.0"))]),
                exchange_ms,
            ),
        });
        engine
    }

    use super::*;
    use crate::book_units::{Quantity, literal};
    use crate::depth::Depth;
    use crate::order_domain::OrderSide;
    use serde_json::Number;

    use crate::order_domain::OrderId;
    use crate::trading_venue::{
        AccountSnapshot, PlacedOrder, PlacementRequest, TradingVenueError, TradingVenueFuture,
    };
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct ProbeVenue {
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    impl TradingVenue for ProbeVenue {
        fn account_snapshot<'a>(
            &'a self,
            _pair: &'a str,
        ) -> TradingVenueFuture<'a, AccountSnapshot> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("snapshot");
                Ok(AccountSnapshot {
                    query: None,
                    open_orders: vec![],
                    balances: vec![],
                })
            })
        }
        fn place_order(&self, request: PlacementRequest) -> TradingVenueFuture<'_, PlacedOrder> {
            NoTradingVenue.place_order(request)
        }
        fn cancel_orders<'a>(
            &'a self,
            pair: &'a str,
            ids: Vec<OrderId>,
        ) -> TradingVenueFuture<'a, ()> {
            NoTradingVenue.cancel_orders(pair, ids)
        }
        fn on_market_event(&self, event: &MarketEvent) -> Result<(), TradingVenueError> {
            self.calls.lock().unwrap().push("apply");
            if matches!(event, MarketEvent::Transactions { pair, .. } if pair == "fail") {
                return Err(TradingVenueError::StatePoisoned);
            }
            Ok(())
        }
    }

    struct ProbeStrategy {
        handled: mpsc::UnboundedSender<()>,
        gate: Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>,
    }

    impl BotStrategy<ProbeVenue> for ProbeStrategy {
        type CustomEvent = ();
        type Error = String;
        async fn handle_event(
            &mut self,
            event: BotEvent<()>,
            ctx: &BotContext<(), ProbeVenue>,
        ) -> Result<(), Self::Error> {
            if let Some((entered, release)) = self.gate.take() {
                entered.send(()).unwrap();
                release.await.unwrap();
            }
            ctx.venue().calls.lock().unwrap().push(match event {
                BotEvent::Market(_) => "market",
                BotEvent::Custom(()) => "custom",
            });
            ctx.venue().account_snapshot("btc_jpy").await.unwrap();
            self.handled.send(()).unwrap();
            Ok(())
        }
    }

    fn transaction_event(pair: &str) -> BotEvent<()> {
        BotEvent::Market(MarketEvent::Transactions {
            pair: pair.to_owned(),
            transactions: vec![],
        })
    }

    async fn wait_handled(receiver: &mut mpsc::UnboundedReceiver<()>) {
        tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn actor_applies_market_once_before_strategy() {
        let venue = ProbeVenue::default();
        let (handled, mut receiver) = mpsc::unbounded_channel();
        let runtime = BitbankBotBuilder::new(
            ProbeStrategy {
                handled,
                gate: None,
            },
            venue.clone(),
        )
        .spawn();
        let sender = runtime.event_sender();
        sender.send(transaction_event("btc_jpy")).await.unwrap();
        sender.send(BotEvent::Custom(())).await.unwrap();
        wait_handled(&mut receiver).await;
        wait_handled(&mut receiver).await;
        runtime.shutdown().await.unwrap();
        assert_eq!(
            *venue.calls.lock().unwrap(),
            ["apply", "market", "snapshot", "custom", "snapshot"]
        );
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn next_market_event_cannot_apply_while_strategy_is_running() {
        let venue = ProbeVenue::default();
        let (handled, mut receiver) = mpsc::unbounded_channel();
        let (entered, entered_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let runtime = BitbankBotBuilder::new(
            ProbeStrategy {
                handled,
                gate: Some((entered, release_rx)),
            },
            venue.clone(),
        )
        .spawn();
        let sender = runtime.event_sender();
        sender.send(BotEvent::Custom(())).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), entered_rx)
            .await
            .unwrap()
            .unwrap();
        sender.send(transaction_event("btc_jpy")).await.unwrap();
        assert!(venue.calls.lock().unwrap().is_empty());
        release.send(()).unwrap();
        wait_handled(&mut receiver).await;
        wait_handled(&mut receiver).await;
        runtime.shutdown().await.unwrap();
        assert_eq!(
            *venue.calls.lock().unwrap(),
            ["custom", "snapshot", "apply", "market", "snapshot"]
        );
    }

    #[tokio::test]
    async fn market_data_runtime_delivers_events_without_credentials() {
        struct Strategy(Option<oneshot::Sender<()>>);
        impl BotStrategy for Strategy {
            type CustomEvent = Infallible;
            type Error = String;
            async fn handle_event(
                &mut self,
                event: BotEvent,
                ctx: &BotContext,
            ) -> Result<(), Self::Error> {
                assert!(matches!(event, BotEvent::Market(_)));
                assert!(matches!(
                    ctx.venue().account_snapshot("btc_jpy").await,
                    Err(TradingVenueError::Unavailable)
                ));
                self.0.take().unwrap().send(()).unwrap();
                Ok(())
            }
        }
        let (done, done_rx) = oneshot::channel();
        let runtime = BitbankBotBuilder::market_data(Strategy(Some(done))).spawn();
        runtime
            .event_sender()
            .send(BotEvent::Market(MarketEvent::Transactions {
                pair: "btc_jpy".into(),
                transactions: vec![],
            }))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), done_rx)
            .await
            .unwrap()
            .unwrap();
        runtime.shutdown().await.unwrap();
    }

    struct PaperProbe {
        snapshots: mpsc::UnboundedSender<AccountSnapshot>,
    }

    impl BotStrategy<crate::paper_execution::PaperTradingVenue> for PaperProbe {
        type CustomEvent = Infallible;
        type Error = String;
        async fn handle_event(
            &mut self,
            _event: BotEvent,
            ctx: &BotContext<Infallible, crate::paper_execution::PaperTradingVenue>,
        ) -> Result<(), Self::Error> {
            self.snapshots
                .send(ctx.venue().account_snapshot("btc_jpy").await.unwrap())
                .unwrap();
            Ok(())
        }
    }

    #[tokio::test]
    async fn manual_and_replayed_trades_update_paper_before_strategy_reads_account() {
        use crate::order_domain::{BalanceSnapshot, DesiredLimitOrder};
        use crate::paper_execution::{
            PaperExecutionConfig, PaperExecutionEngine, PaperTradingVenue,
        };
        let engine = PaperExecutionEngine::new(
            PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap(),
            crate::paper_execution::PaperClock::Replay { now_ms: 0 },
            vec![
                BalanceSnapshot {
                    asset: "btc".into(),
                    free_amount: literal("0"),
                    locked_amount: literal("0"),
                    onhand_amount: literal("0"),
                },
                BalanceSnapshot {
                    asset: "jpy".into(),
                    free_amount: literal("1000"),
                    locked_amount: literal("0"),
                    onhand_amount: literal("1000"),
                },
            ],
            None,
        )
        .map(|engine| seeded_engine(engine, 0))
        .unwrap();
        let venue = PaperTradingVenue::new(engine)
            .with_simulation_config(crate::simulation::SimulationConfig { lookahead_ms: 200 })
            .unwrap();
        let occurred = crate::execution_recording::timestamp_ms() as i64 - 10;
        venue
            .place_order(
                DesiredLimitOrder::limit(
                    "btc_jpy".into(),
                    OrderSide::Buy,
                    literal("1"),
                    literal("100"),
                )
                .into(),
            )
            .await
            .unwrap();
        let (snapshots, mut receiver) = mpsc::unbounded_channel();
        let runtime = BitbankBotBuilder::new(PaperProbe { snapshots }, venue).spawn();
        let sender = runtime.event_sender();
        sender
            .send(BotEvent::Market(MarketEvent::Transactions {
                pair: "btc_jpy".into(),
                transactions: vec![MarketTrade {
                    amount: literal("0.25"),
                    executed_at: occurred,
                    price: literal("100"),
                    side: OrderSide::Sell,
                    transaction_id: 1,
                }],
            }))
            .await
            .unwrap();
        let (inbound, mut inbound_rx) = mpsc::channel(1);
        inbound
            .send(BitbankInboundMessage::Transactions(vec![
                BitbankTransactionDatum {
                    amount: literal("0.25"),
                    executed_at: occurred + 1,
                    price: literal("100"),
                    side: "sell".into(),
                    transaction_id: 2,
                },
            ]))
            .await
            .unwrap();
        drop(inbound);
        forward_bitbank_messages("btc_jpy".into(), &mut inbound_rx, &sender).await;
        for remaining in [literal("0.5"), literal("0.5")] {
            let snapshot = tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(snapshot.open_orders[0].remaining_amount, remaining);
            assert_eq!(
                snapshot
                    .balances
                    .iter()
                    .find(|b| b.asset == "btc")
                    .unwrap()
                    .free_amount,
                Quantity::ONE.checked_sub(remaining).unwrap().to_amount()
            );
        }
        runtime.shutdown().await.unwrap();
    }

    fn depth_whole() -> BitbankDepthWhole {
        serde_json::from_value(serde_json::json!({
            "asks": [["101", "1.5"]],
            "bids": [["100", "2.0"]],
            "asks_over": "0",
            "bids_under": "0",
            "asks_under": "0",
            "bids_over": "0",
            "ask_market": "0",
            "bid_market": "0",
            "timestamp": 1234,
            "sequenceId": "10"
        }))
        .unwrap()
    }

    #[test]
    fn bitbank_converter_waits_for_complete_depth() {
        let mut converter = BitbankMarketEventConverter::new("btc_jpy".to_owned());
        let diff = BitbankDepthDiff {
            a: vec![vec!["101".to_owned(), "0.5".to_owned()]],
            b: vec![vec!["100".to_owned(), "1.0".to_owned()]],
            ao: None,
            bu: None,
            au: None,
            bo: None,
            am: None,
            bm: None,
            t: 1200,
            s: "9".to_owned(),
        };

        let event = converter
            .convert(BitbankInboundMessage::DepthDiff(diff))
            .unwrap();
        assert!(event.is_none());

        let event = converter
            .convert(BitbankInboundMessage::DepthWhole(depth_whole()))
            .unwrap();

        let Some(MarketEvent::DepthUpdated { pair, depth }) = event else {
            panic!("expected depth update");
        };
        assert_eq!(pair, "btc_jpy");
        assert!(depth.is_complete());
        assert_eq!(depth.best_ask().unwrap().0, &literal("101"));
        assert_eq!(depth.best_bid().unwrap().0, &literal("100"));
        assert_eq!(depth.last_timestamp(), 1234);
    }

    #[test]
    fn bitbank_converter_maps_transactions_to_domain_side() {
        let mut converter = BitbankMarketEventConverter::new("btc_jpy".to_owned());
        let trade = BitbankTransactionDatum {
            amount: literal("2.5"),
            executed_at: 1000,
            price: literal("100"),
            side: "buy".to_owned(),
            transaction_id: 42,
        };

        let event = converter
            .convert(BitbankInboundMessage::Transactions(vec![trade]))
            .unwrap();

        let Some(MarketEvent::Transactions { pair, transactions }) = event else {
            panic!("expected transactions");
        };
        assert_eq!(pair, "btc_jpy");
        assert_eq!(transactions.len(), 1);
        assert_eq!(transactions[0].side, OrderSide::Buy);
    }

    #[test]
    fn bitbank_converter_preserves_notification_transaction_order() {
        let mut converter = BitbankMarketEventConverter::new("btc_jpy".to_owned());
        let newest = BitbankTransactionDatum {
            amount: literal("1"),
            executed_at: 3000,
            price: literal("90"),
            side: "sell".to_owned(),
            transaction_id: 3,
        };
        let middle = BitbankTransactionDatum {
            amount: literal("1"),
            executed_at: 2000,
            price: literal("100"),
            side: "sell".to_owned(),
            transaction_id: 2,
        };
        let oldest = BitbankTransactionDatum {
            amount: literal("1"),
            executed_at: 1000,
            price: literal("110"),
            side: "sell".to_owned(),
            transaction_id: 1,
        };

        let event = converter
            .convert(BitbankInboundMessage::Transactions(vec![
                newest, middle, oldest,
            ]))
            .unwrap();

        let Some(MarketEvent::Transactions { transactions, .. }) = event else {
            panic!("expected transactions");
        };
        let ids = transactions
            .iter()
            .map(|trade| trade.transaction_id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![3, 2, 1]);
    }

    #[test]
    fn bitbank_converter_rejects_unknown_transaction_side() {
        let mut converter = BitbankMarketEventConverter::new("btc_jpy".to_owned());
        let trade = BitbankTransactionDatum {
            amount: literal("2.5"),
            executed_at: 1000,
            price: literal("100"),
            side: "unknown".to_owned(),
            transaction_id: 42,
        };

        let err = converter
            .convert(BitbankInboundMessage::Transactions(vec![trade]))
            .unwrap_err();

        assert!(matches!(
            err,
            MarketEventConversionError::InvalidTradeSide(_)
        ));
    }

    #[tokio::test]
    async fn forward_bitbank_messages_emits_market_events() {
        let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
        let (tx, mut event_rx) = mpsc::channel::<(i64, BotEvent, tracing::Span)>(4);
        let event_tx = BotEventSender { tx };

        inbound_tx
            .send(BitbankInboundMessage::Ticker(BitbankTickerResponse {
                sell: Some("101".to_owned()),
                buy: Some("100".to_owned()),
                high: "110".to_owned(),
                low: "90".to_owned(),
                open: "95".to_owned(),
                last: "100".to_owned(),
                vol: "12.5".to_owned(),
                timestamp: Number::from(1234),
            }))
            .await
            .unwrap();
        drop(inbound_tx);

        forward_bitbank_messages("btc_jpy".to_owned(), &mut inbound_rx, &event_tx).await;

        let (_, event, _) = event_rx.recv().await.unwrap();
        let BotEvent::Market(MarketEvent::Ticker { pair, ticker }) = event else {
            panic!("expected ticker");
        };
        assert_eq!(pair, "btc_jpy");
        assert_eq!(ticker.last, "100");
    }
}

/// 記録セッション内の入力を識別するセッションIDとsequence。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InputId {
    pub session_id: String,
    pub sequence: u64,
}

/// 予約APIの入力・ライフサイクルエラー。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    NotReplay,
    Past,
    Closed,
    Overflow,
    Limit,
}

/// 予約の識別子はレコーダーとは独立したランナー内の単調採番。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduledEventInfo {
    pub id: u64,
    pub scheduled_at_ms: i64,
    pub execute_at_ms: i64,
    pub input_id: Option<InputId>,
}
struct ScheduledEvent<C> {
    info: ScheduledEventInfo,
    event: C,
}

#[derive(Debug)]
pub enum BotRunError<E> {
    TimeReversed,
    Venue(crate::trading_venue::TradingVenueError),
    Strategy(E),
    DerivedLimit,
    ScheduledLimit,
    ScheduleIdOverflow,
    DeliveryLimit,
    Stopped,
}

impl<E> BotRunError<E> {
    fn reason_code(&self) -> &'static str {
        match self {
            Self::TimeReversed => "time_reversed",
            Self::Venue(e) => crate::error_diagnostics::venue(e).0,
            Self::Strategy(_) => "strategy_error",
            Self::DerivedLimit => "derived_limit",
            Self::ScheduledLimit => "scheduled_limit",
            Self::ScheduleIdOverflow => "schedule_id_overflow",
            Self::DeliveryLimit => "delivery_limit",
            Self::Stopped => "stopped",
        }
    }
}

/// 市場受信、venue内部期限、逐次の戦略配送を管理するランナー。
/// 模擬操作を待つ間も市場はvenueへ登録するが、戦略ハンドラーは同時実行しない。
/// 詳しい実行手順は `docs/offline-replay.md` を参照。
struct QueuedEvent<C> {
    source_span: tracing::Span,
    received_at_ms: i64,
    event: BotEvent<C>,
    input_id: Option<InputId>,
    market_id: Option<u64>,
    derived: bool,
    recovery: bool,
}

struct ActiveHandler<S: BotStrategy<V>, V: TradingVenue> {
    future: std::pin::Pin<Box<dyn std::future::Future<Output = (S, Result<(), S::Error>)> + Send>>,
    pending: std::sync::Arc<std::sync::Mutex<Pending<S::CustomEvent>>>,
    input_id: Option<InputId>,
    market_id: Option<u64>,
    recovery: bool,
    replay: bool,
}

struct RunnerWake(std::sync::Arc<tokio::sync::Notify>);
impl std::task::Wake for RunnerWake {
    fn wake(self: std::sync::Arc<Self>) {
        self.0.notify_one();
    }
    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        self.0.notify_one();
    }
}

pub struct BotRunner<S: BotStrategy<V>, V: TradingVenue> {
    strategy: Option<S>,
    active: Option<ActiveHandler<S, V>>,
    delivery: std::collections::VecDeque<QueuedEvent<S::CustomEvent>>,
    venue: V,
    last_time: Option<i64>,
    simulation_mode: bool,
    replay_clock: std::sync::Arc<std::sync::atomic::AtomicI64>,
    wake: std::sync::Arc<tokio::sync::Notify>,
    derived_count: usize,
    pub derived_limit: usize,
    /// 戦略待機中に配送を保留する入力の最大件数。
    pub delivery_limit: usize,
    /// 未実行予約数と、同一時刻での内部実行数の上限。
    pub scheduled_limit: usize,
    scheduled: std::collections::BTreeMap<(i64, u64), ScheduledEvent<S::CustomEvent>>,
    next_scheduled_id: u64,
    scheduled_tick: Option<(i64, usize)>,
}
impl<S: BotStrategy<V>, V: TradingVenue> Drop for BotRunner<S, V> {
    fn drop(&mut self) {
        self.venue.set_replay_waker(None);
        self.venue.set_replay_driver(false);
    }
}
impl<S: BotStrategy<V>, V: TradingVenue> BotRunner<S, V> {
    pub fn new(strategy: S, venue: V) -> Self {
        Self {
            strategy: Some(strategy),
            active: None,
            delivery: Default::default(),
            venue,
            last_time: None,
            simulation_mode: false,
            replay_clock: std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0)),
            wake: std::sync::Arc::new(tokio::sync::Notify::new()),
            derived_count: 0,
            derived_limit: 10_000,
            delivery_limit: 10_000,
            scheduled_limit: 10_000,
            scheduled: Default::default(),
            next_scheduled_id: 0,
            scheduled_tick: None,
        }
    }
    pub fn venue(&self) -> &V {
        &self.venue
    }
    pub fn pending_scheduled(&self) -> Vec<ScheduledEventInfo> {
        self.scheduled.values().map(|e| e.info.clone()).collect()
    }
    pub fn pending_delivery_count(&self) -> usize {
        self.delivery.len()
    }
    /// 全追加経路で、退避・復元分を含む最終保留数を事前検証する。
    fn ensure_delivery_capacity(&self, additional: usize) -> Result<(), BotRunError<S::Error>> {
        if self
            .delivery
            .len()
            .checked_add(additional)
            .is_none_or(|len| len > self.delivery_limit)
        {
            return Err(BotRunError::DeliveryLimit);
        }
        Ok(())
    }
    pub fn handler_pending(&self) -> bool {
        self.active.is_some()
    }
    pub fn now_ms(&self) -> Option<i64> {
        self.last_time
    }
    pub fn next_scheduled_time(&self) -> Option<i64> {
        self.scheduled.first_key_value().map(|(key, _)| key.0)
    }
    pub fn next_internal_time(&self) -> Option<i64> {
        self.next_scheduled_time()
            .into_iter()
            .chain(self.venue.next_internal_time())
            .min()
            .map(|t| self.last_time.map_or(t, |now| now.max(t)))
    }
    /// 現時刻で戦略をpollし、模擬注文待ちの場合だけ呼出元へ制御を戻す。
    /// 通常のasync処理は従来どおり時刻を固定して完了を待つ。
    pub async fn poll_replay(&mut self) -> Result<(), BotRunError<S::Error>> {
        self.venue.set_replay_driver(true);
        if self.active.is_none() && self.delivery.is_empty() {
            return Ok(());
        }
        let waker = std::task::Waker::from(std::sync::Arc::new(RunnerWake(self.wake.clone())));
        self.venue.set_replay_waker(Some(waker.clone()));
        loop {
            self.start_handler(true)?;
            if self.active.is_none() {
                return Ok(());
            }
            let polled = self.poll_active(&mut std::task::Context::from_waker(&waker));
            if let std::task::Poll::Ready((strategy, result)) = polled {
                self.strategy = Some(strategy);
                self.finish_handler(result)?;
                continue;
            }
            // 模擬操作が登録済みでも、戦略の子タスクが未実行の場合がある。
            // 時計を進める前に実行機会を与え、同時刻の要求を登録させる。
            tokio::task::yield_now().await;
            if let std::task::Poll::Ready((strategy, result)) =
                self.poll_active(&mut std::task::Context::from_waker(&waker))
            {
                self.strategy = Some(strategy);
                self.finish_handler(result)?;
                continue;
            }
            if self.venue.next_internal_time().is_some() || self.venue.has_pending_operations() {
                return Ok(());
            }
            self.wake.notified().await;
        }
    }
    pub async fn process_next_internal(
        &mut self,
        recovery: bool,
    ) -> Result<(), BotRunError<S::Error>> {
        let Some(time) = self.next_internal_time() else {
            return Ok(());
        };
        self.advance_time(time)?;
        // 同時刻はvenue内部処理、戦略予約の順。市場入力は呼出元で先に処理する。
        self.venue
            .process_internal_events(time)
            .map_err(BotRunError::Venue)?;
        if self.next_scheduled_time().is_some_and(|t| t <= time) {
            self.enqueue_next_scheduled(recovery)?;
        }
        self.poll_replay().await
    }
    fn enqueue_next_scheduled(&mut self, recovery: bool) -> Result<(), BotRunError<S::Error>> {
        let Some((&(time, _), _)) = self.scheduled.first_key_value() else {
            return Ok(());
        };
        // 容量不足なら予約を消費せず、終了結果で未実行として追跡できるようにする。
        self.ensure_delivery_capacity(1)?;
        let time = self.last_time.map_or(time, |now| now.max(time));
        let count = match self.scheduled_tick {
            Some((tick, count)) if tick == time => count,
            _ => 0,
        };
        if count >= self.scheduled_limit {
            return Err(BotRunError::ScheduledLimit);
        }
        self.scheduled_tick = Some((time, count + 1));
        let (_, entry) = self.scheduled.pop_first().unwrap();
        if let Some(r) = self.venue.recorder() {
            r.record_lazy(
                "scheduled_event_executed",
                || serde_json::json!({"event":entry.info,"executed_at_ms":time}),
            );
        }
        self.delivery.push_back(QueuedEvent {
            source_span: tracing::Span::current(),
            received_at_ms: entry.info.execute_at_ms,
            event: BotEvent::Custom(entry.event),
            input_id: entry.info.input_id,
            market_id: None,
            derived: false,
            recovery,
        });
        Ok(())
    }
    pub async fn process_next_scheduled(
        &mut self,
        recovery: bool,
    ) -> Result<(), BotRunError<S::Error>> {
        let Some(time) = self.next_scheduled_time() else {
            return Ok(());
        };
        self.advance_time(self.last_time.map_or(time, |now| now.max(time)))?;
        self.enqueue_next_scheduled(recovery)?;
        self.drive_direct(true).await
    }
    fn record_settlement(&self) {
        self.venue.record_paper_settlement();
    }
    pub fn advance_time(&mut self, time_ms: i64) -> Result<(), BotRunError<S::Error>> {
        if self.last_time.is_some_and(|t| time_ms < t) {
            return Err(BotRunError::TimeReversed);
        }
        self.venue
            .advance_replay_time(time_ms)
            .map_err(BotRunError::Venue)?;
        self.last_time = Some(time_ms);
        self.replay_clock
            .store(time_ms, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn ingest(
        &mut self,
        input_id: Option<InputId>,
        event: BotEvent<S::CustomEvent>,
        recovery: bool,
        inherited_market_id: Option<u64>,
        derived: bool,
    ) -> Result<(), BotRunError<S::Error>> {
        self.ensure_delivery_capacity(1)?;
        let recorder = self.venue.recorder();
        let market_id = if matches!(event, BotEvent::Market(_)) {
            recorder.as_ref().map(|r| r.next_id())
        } else {
            inherited_market_id
        };
        let received_at_ms = self
            .last_time
            .unwrap_or_else(|| crate::execution_recording::timestamp_ms() as i64);
        let recording_context = crate::execution_recording::RecordingContext {
            event_time_ms: Some(received_at_ms),
            market_event_id: market_id,
            input_id: input_id.clone(),
            ..Default::default()
        };
        crate::execution_recording::sync_scope(recording_context, || {
            if !self.simulation_mode
                && let BotEvent::Market(market) = &event
            {
                if recovery {
                    self.venue.on_market_event_recovering(market)
                } else {
                    self.venue.on_market_event(market)
                }
                .map_err(BotRunError::Venue)?;
            }
            if let Some(r) = recorder {
                r.record_lazy("input_registered", || serde_json::json!({"received_at_ms":received_at_ms,"input_id":input_id,"market_event_id":market_id}));
            }
            Ok::<(), BotRunError<S::Error>>(())
        })?;
        self.delivery.push_back(QueuedEvent {
            source_span: tracing::Span::current(),
            received_at_ms,
            event,
            input_id,
            market_id,
            derived,
            recovery,
        });
        Ok(())
    }
    /// シミュレータで適用済みの市場情報を、元の受信時刻で配送する。
    pub(crate) fn enqueue_simulation_delivery(
        &mut self,
        received_at_ms: i64,
        input_id: Option<InputId>,
        market_id: Option<u64>,
        event: BotEvent<S::CustomEvent>,
        recovery: bool,
    ) -> Result<(), BotRunError<S::Error>> {
        self.ensure_delivery_capacity(1)?;
        self.simulation_mode = true;
        if let Some(recorder) = self.venue.recorder() {
            let context = crate::execution_recording::RecordingContext {
                event_time_ms: self.last_time,
                input_id: input_id.clone(),
                market_event_id: market_id,
                ..Default::default()
            };
            crate::execution_recording::sync_scope(context, || {
                recorder.record_lazy("input_registered", || serde_json::json!({
                "received_at_ms":received_at_ms,"input_id":input_id,"market_event_id":market_id
            }))
            });
        }
        self.delivery.push_back(QueuedEvent {
            source_span: tracing::Span::current(),
            received_at_ms,
            input_id,
            event,
            market_id,
            derived: false,
            recovery,
        });
        Ok(())
    }

    /// 市場を現在の受信時刻で登録し、戦略が模擬操作待ちなら配送を保留する。
    /// このメソッド自身は将来の期限へ時計を進めない。
    pub async fn submit_replay(
        &mut self,
        time_ms: i64,
        input_id: Option<InputId>,
        event: BotEvent<S::CustomEvent>,
        recovery: bool,
    ) -> Result<(), BotRunError<S::Error>> {
        self.advance_time(time_ms)?;
        self.ingest(input_id, event, recovery, None, false)?;
        self.poll_replay().await
    }
    /// 直接入力を処理する。待機する模擬操作の期限も駆動するため、途中の入力が
    /// あり得る収録データにはsubmit_replayを使うrun_replayを選ぶ。
    pub async fn process(
        &mut self,
        time_ms: i64,
        input_id: Option<InputId>,
        event: BotEvent<S::CustomEvent>,
    ) -> Result<(), BotRunError<S::Error>> {
        self.submit_replay(time_ms, input_id, event, false).await?;
        self.drive_direct(true).await
    }
    pub async fn process_recovering(
        &mut self,
        time_ms: i64,
        input_id: Option<InputId>,
        event: BotEvent<S::CustomEvent>,
    ) -> Result<(), BotRunError<S::Error>> {
        self.submit_replay(time_ms, input_id, event, true).await?;
        self.drive_direct(true).await
    }
    fn poll_live(&mut self) -> Result<(), BotRunError<S::Error>> {
        self.venue.set_replay_driver(false);
        let waker = std::task::Waker::from(std::sync::Arc::new(RunnerWake(self.wake.clone())));
        self.venue.set_replay_waker(Some(waker.clone()));
        loop {
            self.start_handler(false)?;
            if self.active.is_none() {
                return Ok(());
            }
            match self.poll_active(&mut std::task::Context::from_waker(&waker)) {
                std::task::Poll::Ready((strategy, result)) => {
                    self.strategy = Some(strategy);
                    self.finish_handler(result)?;
                }
                std::task::Poll::Pending => return Ok(()),
            }
        }
    }
    async fn drive_direct(&mut self, replay: bool) -> Result<(), BotRunError<S::Error>> {
        struct ReleaseDriver<V: TradingVenue>(V);
        impl<V: TradingVenue> Drop for ReleaseDriver<V> {
            fn drop(&mut self) {
                self.0.set_replay_driver(false);
                self.0.set_replay_waker(None);
            }
        }
        let _release = ReleaseDriver(self.venue.clone());
        if replay {
            self.poll_replay().await?;
            while self.active.is_some() {
                let Some(t) = self.venue.next_internal_time() else {
                    break;
                };
                self.advance_time(t)?;
                self.venue
                    .process_internal_events(t)
                    .map_err(BotRunError::Venue)?;
                self.poll_replay().await?;
            }
            return Ok(());
        }
        loop {
            self.start_handler(false)?;
            if self.active.is_none() {
                return Ok(());
            }
            let (strategy, result) = std::future::poll_fn(|cx| self.poll_active(cx)).await;
            self.strategy = Some(strategy);
            self.finish_handler(result)?;
        }
    }
    fn poll_active(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<(S, Result<(), S::Error>)> {
        let active = self.active.as_mut().expect("active handler");
        let context = crate::execution_recording::RecordingContext {
            event_time_ms: Some(
                self.last_time
                    .unwrap_or_else(|| crate::execution_recording::timestamp_ms() as i64),
            ),
            market_event_id: active.market_id,
            input_id: active.input_id.clone(),
            replay_clock: self.last_time.map(|_| self.replay_clock.clone()),
            ..Default::default()
        };
        crate::execution_recording::sync_scope(context, || {
            // Tokioの協調実行budget切れを、模擬操作待ちと取り違えて時計を進めない。
            // backtestの同時刻処理は実時間スケジューラのbudgetに依存させない。
            if active.replay {
                use std::future::Future;
                std::pin::pin!(tokio::task::unconstrained(active.future.as_mut())).poll(cx)
            } else {
                active.future.as_mut().poll(cx)
            }
        })
    }
    fn start_handler(&mut self, replay: bool) -> Result<(), BotRunError<S::Error>> {
        if self.active.is_some() {
            return Ok(());
        }
        let Some(entry) = self.delivery.front() else {
            return Ok(());
        };
        let next_count = if entry.derived {
            self.derived_count
                .checked_add(1)
                .ok_or(BotRunError::DerivedLimit)?
        } else {
            1
        };
        if next_count > self.derived_limit.saturating_add(1) {
            return Err(BotRunError::DerivedLimit);
        }
        if self.strategy.is_none() {
            return Err(BotRunError::Stopped);
        }
        let entry = self.delivery.pop_front().unwrap();
        self.derived_count = next_count;
        let pending = std::sync::Arc::new(std::sync::Mutex::new(Pending {
            queue: Default::default(),
            scheduled: Vec::new(),
            schedule_limit: self.scheduled_limit.saturating_sub(self.scheduled.len()),
            schedule_overflow: false,
            open: true,
            emitted: 0,
            limit: self.derived_limit.saturating_sub(
                self.derived_count.saturating_sub(1)
                    + self.delivery.iter().filter(|e| e.derived).count(),
            ),
            overflow: false,
        }));
        let ctx = BotContext {
            venue: self.venue.strategy_view(),
            received_at_ms: entry.received_at_ms,
            time: replay.then(|| self.replay_clock.clone()),
            pending: pending.clone(),
        };
        let mut strategy = self.strategy.take().ok_or(BotRunError::Stopped)?;
        let recorder = self.venue.recorder();
        let input_id = entry.input_id.clone();
        let market_id = entry.market_id;
        let close = ClosePending(pending.clone());
        let event_time_ms = match &entry.event {
            BotEvent::Market(MarketEvent::DepthUpdated { depth, .. }) => {
                Some(depth.last_timestamp())
            }
            BotEvent::Market(MarketEvent::Ticker { ticker, .. }) => ticker.timestamp.as_i64(),
            BotEvent::Market(MarketEvent::Transactions { transactions, .. }) => {
                transactions.last().map(|t| t.executed_at)
            }
            BotEvent::Market(MarketEvent::CircuitBreakInfo { info, .. }) => info.timestamp.as_i64(),
            BotEvent::Custom(_) => None,
        };
        let (event_type, pair) = match &entry.event {
            BotEvent::Market(MarketEvent::Ticker { pair, .. }) => {
                ("ticker", Some(crate::telemetry::safe(pair)))
            }
            BotEvent::Market(MarketEvent::Transactions { pair, .. }) => {
                ("transactions", Some(crate::telemetry::safe(pair)))
            }
            BotEvent::Market(MarketEvent::DepthUpdated { pair, .. }) => {
                ("depth", Some(crate::telemetry::safe(pair)))
            }
            BotEvent::Market(MarketEvent::CircuitBreakInfo { pair, .. }) => {
                ("circuit_break_info", Some(crate::telemetry::safe(pair)))
            }
            BotEvent::Custom(_) => ("custom", None),
        };
        let decision_span = tracing::info_span!(
            parent: &entry.source_span,
            "strategy_decision",
            decision_id = crate::telemetry::next_id(), event_type, pair,
            market_event_id = market_id,
            received_at_ms = entry.received_at_ms,
            event_time_ms
        );
        let future = Box::pin(
            async move {
                let _close = close;
                if let Some(r) = recorder {
                    let kind = if matches!(entry.event, BotEvent::Market(_)) {
                        "market_event"
                    } else {
                        "custom_event"
                    };
                    let event_type = match &entry.event {
                        BotEvent::Market(MarketEvent::Ticker { .. }) => "ticker",
                        BotEvent::Market(MarketEvent::Transactions { .. }) => "transactions",
                        BotEvent::Market(MarketEvent::DepthUpdated { .. }) => "depth",
                        BotEvent::Market(MarketEvent::CircuitBreakInfo { .. }) => {
                            "circuit_break_info"
                        }
                        BotEvent::Custom(_) => "custom",
                    };
                    r.record_lazy(kind, || {
                        serde_json::json!({"id":market_id,"input_id":input_id,"type":event_type,
                        "received_at_ms":ctx.received_at_ms(),"processed_at_ms":ctx.now_ms()})
                    });
                }
                tracing::debug!(event = "strategy_started");
                let started = std::time::Instant::now();
                let result = strategy.handle_event(entry.event, &ctx).await;
                if let Err(error) = &result {
                    tracing::error!(
                        event = "strategy_failed",
                        reason_code = S::error_code(error),
                        error_type = std::any::type_name::<S::Error>(),
                        action = "stop_bot"
                    );
                }
                tracing::debug!(
                    event = "strategy_finished",
                    success = result.is_ok(),
                    elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
                );
                (strategy, result)
            }
            .instrument(decision_span),
        );
        self.active = Some(ActiveHandler {
            future,
            pending,
            input_id: entry.input_id.clone(),
            market_id,
            recovery: entry.recovery,
            replay,
        });
        Ok(())
    }
    fn finish_handler(
        &mut self,
        result: Result<(), S::Error>,
    ) -> Result<(), BotRunError<S::Error>> {
        let active = self.active.take().unwrap();
        let (emitted, scheduled) = {
            let mut p = active.pending.lock().unwrap();
            p.open = false;
            if p.schedule_overflow {
                return Err(BotRunError::ScheduledLimit);
            }
            if p.overflow {
                return Err(BotRunError::DerivedLimit);
            }
            (
                p.queue.drain(..).collect::<Vec<_>>(),
                p.scheduled.drain(..).collect::<Vec<_>>(),
            )
        };
        result.map_err(BotRunError::Strategy)?;
        // FIFO優先のため外部入力を退避する前に、復元後の全件数を検証する。
        self.ensure_delivery_capacity(emitted.len())?;
        // 派生イベントは待機中に受信した市場入力より先にFIFOで配送する。
        let external_start = self
            .delivery
            .iter()
            .position(|e| !e.derived)
            .unwrap_or(self.delivery.len());
        let queued = self.delivery.split_off(external_start);
        let ingestion = emitted.into_iter().try_for_each(|event| {
            self.ingest(
                active.input_id.clone(),
                event,
                active.recovery,
                active.market_id,
                true,
            )
        });
        self.delivery.extend(queued);
        ingestion?;
        for (execute_at_ms, event) in scheduled {
            if self.scheduled.len() >= self.scheduled_limit {
                return Err(BotRunError::ScheduledLimit);
            }
            self.next_scheduled_id = self
                .next_scheduled_id
                .checked_add(1)
                .ok_or(BotRunError::ScheduleIdOverflow)?;
            let info = ScheduledEventInfo {
                id: self.next_scheduled_id,
                scheduled_at_ms: self.last_time.expect("replay schedule"),
                execute_at_ms,
                input_id: active.input_id.clone(),
            };
            if let Some(r) = self.venue.recorder() {
                r.record_lazy("scheduled_event_reserved", || serde_json::json!(&info));
            }
            self.scheduled
                .insert((execute_at_ms, info.id), ScheduledEvent { info, event });
        }
        Ok(())
    }
    /// 評価終了時に戦略Futureを破棄する。送信済みvenue操作は取り消さない。
    pub fn stop_pending_handler(&mut self) {
        self.active.take();
        self.venue.set_replay_waker(None);
        self.venue.set_replay_driver(false);
    }
}

struct Pending<C> {
    scheduled: Vec<(i64, C)>,
    schedule_limit: usize,
    schedule_overflow: bool,
    queue: std::collections::VecDeque<BotEvent<C>>,
    open: bool,
    emitted: usize,
    limit: usize,
    overflow: bool,
}
struct ClosePending<C>(std::sync::Arc<std::sync::Mutex<Pending<C>>>);
impl<C> Drop for ClosePending<C> {
    fn drop(&mut self) {
        if let Ok(mut p) = self.0.lock() {
            p.open = false;
        }
    }
}

impl<E: std::fmt::Debug> std::fmt::Display for BotRunError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl<E: std::fmt::Debug> std::error::Error for BotRunError<E> {}

pub(crate) type TimedBitbankMessage = (i64, String, BitbankInboundMessage, tracing::Span);

fn spawn_simulation_actor<S: BotStrategy<V>, V: TradingVenue>(
    strategy: S,
    venue: V,
    buffer: usize,
    mut raw_rx: mpsc::Receiver<TimedBitbankMessage>,
    input_failed: std::sync::Arc<std::sync::atomic::AtomicU8>,
) -> BotHandle<S::CustomEvent> {
    let (tx, mut event_rx) = mpsc::channel(buffer);
    let event_tx = BotEventSender { tx };
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let run_id = crate::telemetry::next_id();
    let span = tracing::info_span!("bot", run_id, mode = "simulation");
    let handle_span = span.clone();
    let join_handle = tokio::spawn(async move {
        let _lifetime = crate::telemetry::TaskLifetime("bot");
        tracing::info!(event = "bot_started", library_version = env!("CARGO_PKG_VERSION"));
        let mut driver = crate::simulation::SimulationDriver::new(
            venue.simulation_config().unwrap(),
            venue.simulation_time_model(),
        )
        .map_err(|e| {
            tracing::error!(event = "bot_failed", stage = "initialization", reason_code = crate::error_diagnostics::venue(&crate::trading_venue::TradingVenueError::Paper(e.clone())).0, action = "stop_bot");
            format!("{e:?}")
        })?;
        venue.begin_simulation().map_err(|e| {
            tracing::error!(event = "bot_failed", stage = "initialization", reason_code = crate::error_diagnostics::venue(&e).0, action = "stop_bot");
            format!("{e:?}")
        })?;
        let mut runner = BotRunner::new(strategy, venue);
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(10));
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => break,
                _ = tick.tick() => {}
            }
            // 時刻を先に採取し、そこで受信済みの入力を全て登録してから進める。
            let through = crate::execution_recording::timestamp_ms() as i64;
            enum Input<C> {
                Raw(String, BitbankInboundMessage, tracing::Span),
                Event(BotEvent<C>, tracing::Span),
            }
            let mut batch = Vec::new();
            while let Ok((time, pair, message, span)) = raw_rx.try_recv() {
                batch.push((time, Input::Raw(pair, message, span)));
            }
            while let Ok((time, event, span)) = event_rx.try_recv() {
                batch.push((time, Input::Event(event, span)));
            }
            batch.sort_by_key(|(time, _)| *time);
            let mut result = Ok(());
            let failures = input_failed.swap(0, std::sync::atomic::Ordering::Relaxed);
            for (flag, reason) in [
                (crate::websocket_handler::INPUT_OVERFLOW, "input_buffer_full"),
                (crate::websocket_handler::INPUT_MALFORMED, "malformed_input"),
                (crate::websocket_handler::INPUT_RECONNECT, "feed_reconnecting"),
                (crate::websocket_handler::INPUT_TERMINATED, "feed_terminated"),
            ] { if failures & flag != 0 { driver.mark_incomplete(reason); } }
            for (received, input) in batch {
                result = match input {
                    Input::Raw(pair, message, span) => {
                        let parent = if span.is_none() { tracing::Span::current() } else { span };
                        let delivery_span = tracing::info_span!(parent: &parent, "bot_delivery", run_id);
                        delivery_span.in_scope(|| driver.admit_bitbank(received, None, pair.clone(), pair, message))
                    }
                    Input::Event(event, span) => {
                        let parent = if span.is_none() { tracing::Span::current() } else { span };
                        let delivery_span = tracing::info_span!(parent: &parent, "bot_delivery", run_id);
                        delivery_span.in_scope(|| driver.admit_event(received, None, event))
                    },
                };
                if result.is_err() {
                    break;
                }
            }
            if result.is_ok() {
                result = driver.advance_to(&mut runner, through, None, &cancel).await;
            }
            if let Err(error) = result {
                runner.venue().mark_market_incomplete(&error);
                runner.record_settlement();
                if let Some(recorder) = runner.venue().recorder() {
                    recorder.record_lazy(
                        "simulation_outcome",
                        || serde_json::json!({"simulation":driver.report(),"error":error}),
                    );
                }
                tracing::error!(event = "bot_failed", stage = "simulation", reason_code = crate::telemetry::simulation_reason(&error), action = "stop_bot");
                return Err(error);
            }
        }
        // 末尾の先読み分や操作期限を強制完了しない。
        let pending_handler = runner.handler_pending();
        runner.stop_pending_handler();
        runner.record_settlement();
        if let Some(recorder) = runner.venue().recorder() {
            recorder.record_lazy(
                "simulation_outcome",
                || serde_json::json!({"simulation":driver.report(),"shutdown":true,
                    "pending_handler":pending_handler,"pending_deliveries":runner.pending_delivery_count(),
                    "pending_scheduled":runner.pending_scheduled(),"final_time_ms":runner.now_ms()}),
            );
        }
        tracing::info!(event = "bot_stopped", requested = true, reason_code = "shutdown_requested", pending_handler, pending_deliveries = runner.pending_delivery_count(), pending_scheduled = runner.pending_scheduled().len());
        Ok(())
    }.instrument(span).with_current_subscriber());
    BotHandle {
        event_tx,
        shutdown_tx: Some(shutdown_tx),
        join_handle: Some(join_handle),
        span: handle_span,
    }
}

impl<S: BotStrategy<crate::paper_execution::PaperTradingVenue>>
    BotRunner<S, crate::paper_execution::PaperTradingVenue>
{
    pub fn paper_diagnostics(&self) -> Option<crate::paper_execution::PaperDiagnosticSnapshot> {
        self.venue.diagnostics().map(|d| d.snapshot())
    }
}
