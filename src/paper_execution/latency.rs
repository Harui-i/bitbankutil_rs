//! 注文の取引所有効化と、ローカルへの応答・通知を駆動する。
use super::*;
use std::task::Waker;
use tokio::sync::oneshot;
mod query;
use query::{Endpoint, QueryEvent};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PaperOperationLatency {
    pub entry_ms: i64,
    pub response_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PaperLatencyConfig {
    pub placement: PaperOperationLatency,
    pub cancellation: PaperOperationLatency,
    pub orders_query: PaperOperationLatency,
    pub balances_query: PaperOperationLatency,
    /// 内部取引所の状態変化からローカル通知までの遅延。
    pub notification_ms: i64,
}

impl PaperLatencyConfig {
    fn validate(self) -> Result<(), PaperExecutionError> {
        if [
            self.placement.entry_ms,
            self.placement.response_ms,
            self.cancellation.entry_ms,
            self.cancellation.response_ms,
            self.orders_query.entry_ms,
            self.orders_query.response_ms,
            self.balances_query.entry_ms,
            self.balances_query.response_ms,
            self.notification_ms,
        ]
        .into_iter()
        .any(|v| v < 0)
        {
            return Err(PaperExecutionError::InvalidClock);
        }
        Ok(())
    }
}

#[derive(Debug)]
enum Internal {
    Query(QueryEvent),
    Place {
        id: u64,
        order: DesiredLimitOrder,
        response_at: i64,
        reply: Reply<PlacedOrder>,
    },
    Cancel {
        id: u64,
        pair: String,
        orders: Vec<OrderId>,
        response_at: i64,
        reply: Reply<()>,
    },
    PlaceResponse {
        id: u64,
        result: Result<PlacedOrder, TradingVenueError>,
        reply: Reply<PlacedOrder>,
    },
    CancelResponse {
        id: u64,
        result: Result<(), TradingVenueError>,
        reply: Reply<()>,
    },
    Publish {
        id: u64,
        snapshot: AccountSnapshot,
    },
}

#[derive(Debug)]
struct ScheduledInternal {
    context: crate::execution_recording::RecordingContext,
    event: Internal,
}

#[derive(Debug)]
enum PendingOperation {
    Place {
        id: u64,
        requested: i64,
        effective: i64,
        exchange: Option<i64>,
        response: i64,
        order: DesiredLimitOrder,
        accepted: Option<Option<OrderId>>,
    },
    Cancel {
        id: u64,
        requested: i64,
        effective: i64,
        exchange: Option<i64>,
        response: i64,
        orders: Vec<OrderId>,
    },
    Query {
        id: u64,
        kind: &'static str,
        pair: String,
        requested: i64,
        sample: i64,
        response: i64,
        sampled: Option<i64>,
    },
}
impl PendingOperation {
    fn json(&self) -> serde_json::Value {
        match self {
            Self::Place {
                id,
                requested,
                effective,
                exchange,
                response,
                order,
                accepted,
            } => {
                let mut value = serde_json::json!({"id":id,"kind":"place","requested_at_ms":requested,"effective_local_ms":effective,"effective_exchange_ms":exchange,"response_at_ms":response,"order":order});
                if let Some(accepted) = accepted {
                    value["order_id"] = serde_json::json!(accepted);
                    value["phase"] = serde_json::json!("awaiting_response");
                }
                value
            }
            Self::Cancel {
                id,
                requested,
                effective,
                exchange,
                response,
                orders,
            } => {
                serde_json::json!({"id":id,"kind":"cancel","requested_at_ms":requested,"effective_local_ms":effective,"effective_exchange_ms":exchange,"response_at_ms":response,"orders":orders})
            }
            Self::Query {
                id,
                kind,
                pair,
                requested,
                sample,
                response,
                sampled,
            } => {
                let mut value = serde_json::json!({"id":id,"kind":kind,"pair":pair,"requested_at_ms":requested,"sample_at_ms":sample,"response_at_ms":response,"phase":if sampled.is_some(){"awaiting_response"}else{"awaiting_sample"}});
                if let Some(sampled) = sampled {
                    value["sampled_at_ms"] = serde_json::json!(sampled);
                }
                value
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct State {
    latency: PaperLatencyConfig,
    simulation: crate::simulation::SimulationConfig,
    simulation_active: bool,
    local: AccountSnapshot,
    events: BTreeMap<(i64, u8, u64), ScheduledInternal>,
    sequence: u64,
    operation: u64,
    operations: BTreeMap<u64, PendingOperation>,
    completed: VecDeque<Completion>,
    /// ローカル口座は取引所の状態遷移の一貫したprefixを通知する。
    publication_floor: i64,
    waker: Option<Waker>,
    worker_running: bool,
    managed_replay: bool,
    notify: Arc<tokio::sync::Notify>,
}

impl State {
    fn new(engine: &PaperExecutionEngine, latency: PaperLatencyConfig) -> Self {
        Self {
            latency,
            simulation: Default::default(),
            simulation_active: false,
            local: snapshot(engine),
            events: BTreeMap::new(),
            sequence: 0,
            operation: 0,
            operations: BTreeMap::new(),
            completed: VecDeque::new(),
            publication_floor: i64::MIN,
            waker: None,
            worker_running: false,
            managed_replay: false,
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }
    fn settlement_status(&self, engine: &PaperExecutionEngine) -> PaperSettlementStatus {
        let mut status = engine.settlement_status();
        status.latency_model = self.latency;
        status.simulation = self.simulation_active.then_some(self.simulation);
        status.pending_operation_details = self
            .operations
            .values()
            .map(PendingOperation::json)
            .collect();
        status.pending_operations = self.operations.len()
            + self
                .events
                .values()
                .filter(|v| matches!(v.event, Internal::Publish { .. }))
                .count();
        status.unsettled |= status.pending_operations > 0;
        status
    }
    fn enqueue(&mut self, at: i64, event: Internal) -> Result<(), TradingVenueError> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(TradingVenueError::Paper(PaperExecutionError::InvalidClock))?;
        self.events.insert(
            (
                at,
                match &event {
                    Internal::Place { .. } | Internal::Cancel { .. } => 0,
                    Internal::Query(QueryEvent::Sample { .. }) => 1,
                    _ => 2,
                },
                self.sequence,
            ),
            ScheduledInternal {
                context: crate::execution_recording::current_context(),
                event,
            },
        );
        if let Some(w) = &self.waker {
            w.wake_by_ref();
        }
        self.notify.notify_one();
        Ok(())
    }
    fn publish(
        &mut self,
        engine: &PaperExecutionEngine,
        earliest: i64,
        id: u64,
    ) -> Result<(), TradingVenueError> {
        let at = earliest.max(self.publication_floor);
        self.publication_floor = at;
        self.enqueue(
            at,
            Internal::Publish {
                id,
                snapshot: snapshot(engine),
            },
        )
    }
    fn publish_market(
        &mut self,
        engine: &PaperExecutionEngine,
        now: i64,
    ) -> Result<(), TradingVenueError> {
        let at = add(now, self.latency.notification_ms)?;
        if at <= now
            && self.publication_floor <= now
            && !self
                .events
                .values()
                .any(|v| matches!(v.event, Internal::Publish { .. }))
        {
            self.publication_floor = now;
            self.local = snapshot(engine);
            record(
                engine,
                "paper_local_state",
                || serde_json::json!({"operation_id":0,"local_ms":now,"account":self.local}),
            );
            Ok(())
        } else {
            self.publish(engine, at, 0)
        }
    }
}

fn snapshot(engine: &PaperExecutionEngine) -> AccountSnapshot {
    AccountSnapshot {
        open_orders: engine.open_orders(),
        balances: engine.balances(),
        query: None,
    }
}
fn local_now(engine: &PaperExecutionEngine) -> Result<i64, TradingVenueError> {
    match engine.clock {
        PaperClock::Replay { now_ms } => Ok(now_ms),
        PaperClock::Realtime => i64::try_from(crate::execution_recording::timestamp_ms())
            .map_err(|_| TradingVenueError::Paper(PaperExecutionError::InvalidClock)),
    }
}
fn add(time: i64, delay: i64) -> Result<i64, TradingVenueError> {
    time.checked_add(delay)
        .ok_or(TradingVenueError::Paper(PaperExecutionError::InvalidClock))
}
fn record(engine: &PaperExecutionEngine, kind: &str, data: impl FnOnce() -> serde_json::Value) {
    if let Some(r) = &engine.recorder {
        r.record_lazy(kind, data);
    }
}

fn settle_simulation(
    s: &mut State,
    e: &mut PaperExecutionEngine,
    local: i64,
) -> Result<(), TradingVenueError> {
    if !e.settlement_issues.is_empty() {
        return Ok(());
    }
    let through = e
        .config
        .time_model
        .local_to_exchange_ms(local)
        .map_err(TradingVenueError::Paper)?;
    if e.watermark_ms.is_some_and(|w| through < w) {
        return Ok(());
    }
    if e.buffered_trades.is_empty() && e.cancel_at.is_empty() {
        e.watermark_ms = Some(through);
        return Ok(());
    }
    let before = snapshot(e);
    e.advance_trade_watermark(through)
        .map_err(TradingVenueError::Paper)?;
    if snapshot(e) != before {
        s.publish_market(e, local)?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct PaperTradingVenue {
    engine: Arc<Mutex<PaperExecutionEngine>>,
    state: Arc<Mutex<State>>,
    execution_access: bool,
}

// Debug経由でも戦略へ内部板・未通知口座を公開しない。
impl std::fmt::Debug for PaperTradingVenue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaperTradingVenue").finish_non_exhaustive()
    }
}

/// 戦略用venueには含めない、実行基盤が所有する診断窓口。
#[derive(Debug, Clone)]
pub struct PaperDiagnostics(Arc<Mutex<PaperExecutionEngine>>, Arc<Mutex<State>>);
#[derive(Debug, Clone)]
pub struct PaperDiagnosticSnapshot {
    account: AccountSnapshot,
    status: PaperSettlementStatus,
    config: PaperExecutionConfig,
    events: Vec<PaperEvent>,
}
impl PaperDiagnostics {
    pub fn snapshot(&self) -> PaperDiagnosticSnapshot {
        let state = self.1.lock().unwrap();
        let e = self.0.lock().unwrap();
        PaperDiagnosticSnapshot {
            account: snapshot(&e),
            status: state.settlement_status(&e),
            config: e.config.clone(),
            events: e.event_history.iter().cloned().collect(),
        }
    }
}
impl PaperDiagnosticSnapshot {
    pub fn balances(&self) -> Vec<BalanceSnapshot> {
        self.account.balances.clone()
    }
    pub fn open_orders(&self) -> Vec<OpenOrder> {
        self.account.open_orders.clone()
    }
    pub fn settlement_status(&self) -> PaperSettlementStatus {
        self.status.clone()
    }
    pub fn config(&self) -> &PaperExecutionConfig {
        &self.config
    }
    pub fn events(&self) -> &[PaperEvent] {
        &self.events
    }
    /// snapshot自身のイベントだけを取り出す。内部取引所の履歴は変更しない。
    pub fn drain_events(&mut self) -> Vec<PaperEvent> {
        std::mem::take(&mut self.events)
    }
}

impl PaperTradingVenue {
    /// 実行基盤が保有する読み取り専用診断ハンドルを同時に作る。
    pub fn with_diagnostics(engine: PaperExecutionEngine) -> (Self, PaperDiagnostics) {
        let venue = Self::new(engine);
        let diagnostics = PaperDiagnostics(venue.engine.clone(), venue.state.clone());
        (venue, diagnostics)
    }
    pub(crate) fn diagnostics(&self) -> Option<PaperDiagnostics> {
        self.execution_access
            .then(|| PaperDiagnostics(self.engine.clone(), self.state.clone()))
    }
    pub fn new(engine: PaperExecutionEngine) -> Self {
        Self::with_latency(engine, PaperLatencyConfig::default()).expect("zero latency is valid")
    }
    pub fn with_latency(
        mut engine: PaperExecutionEngine,
        latency: PaperLatencyConfig,
    ) -> Result<Self, PaperExecutionError> {
        latency.validate()?;
        let state = Arc::new(Mutex::new(State::new(&engine, latency)));
        engine.venue_state = Some(state.clone());
        record(&engine, "paper_latency_model", || {
            serde_json::json!({"latency":latency,
            "local_view":"ordered_notifications", "cancel_response":"acceptance_ack"})
        });
        Ok(Self {
            engine: Arc::new(Mutex::new(engine)),
            state,
            execution_access: true,
        })
    }
    /// 同じengineから作るハンドルも操作・通知キューを共有する。
    #[cfg(test)]
    pub(crate) fn from_shared(engine: Arc<Mutex<PaperExecutionEngine>>) -> Self {
        let state = {
            let mut e = engine.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(s) = &e.venue_state {
                s.clone()
            } else {
                let s = Arc::new(Mutex::new(State::new(&e, PaperLatencyConfig::default())));
                e.venue_state = Some(s.clone());
                s
            }
        };
        Self {
            engine,
            state,
            execution_access: true,
        }
    }
    /// 診断用の取引所状態。直接の変更はローカル通知経路を迂回する。
    #[cfg(test)]
    pub(crate) fn engine(&self) -> Arc<Mutex<PaperExecutionEngine>> {
        self.engine.clone()
    }

    pub fn with_simulation_config(
        self,
        config: crate::simulation::SimulationConfig,
    ) -> Result<Self, PaperExecutionError> {
        config.validate()?;
        let mut state = self.state.lock().unwrap();
        if !self.execution_access || (state.simulation_active && state.simulation != config) {
            return Err(PaperExecutionError::InvalidClock);
        }
        state.simulation = config;
        drop(state);
        Ok(self)
    }

    pub fn latency_config(&self) -> PaperLatencyConfig {
        self.state.lock().unwrap().latency
    }

    /// 応答・通知で既知となった口座キャッシュ。REST照会とは別の読み出し。
    /// 照会結果をこのキャッシュへ書き戻さず、古い応答による巻き戻しを防ぐ。
    pub fn known_account_snapshot(&self) -> AccountSnapshot {
        self.state.lock().unwrap().local.clone()
    }

    pub fn pending_operations(&self) -> Vec<serde_json::Value> {
        if !self.execution_access {
            return Vec::new();
        }
        self.state
            .lock()
            .unwrap()
            .operations
            .values()
            .map(PendingOperation::json)
            .collect()
    }

    pub fn advance_trade_watermark(
        &self,
        through_ms: i64,
    ) -> Result<Vec<PaperEvent>, TradingVenueError> {
        if !self.execution_access {
            return Err(TradingVenueError::Unavailable);
        }
        let events = {
            let mut s = self
                .state
                .lock()
                .map_err(|_| TradingVenueError::StatePoisoned)?;
            let mut e = self
                .engine
                .lock()
                .map_err(|_| TradingVenueError::StatePoisoned)?;
            let before = snapshot(&e);
            let events = e
                .advance_trade_watermark(through_ms)
                .map_err(TradingVenueError::Paper)?;
            let now = local_now(&e)?;
            if snapshot(&e) != before {
                s.publish_market(&e, now)?;
            }
            events
        };
        self.drive_now()?;
        Ok(events)
    }

    fn drive_now(&self) -> Result<(), TradingVenueError> {
        // 管理された再生では、同msの全市場入力の後でランナーが期限を処理する。
        if self
            .state
            .lock()
            .map_err(|_| TradingVenueError::StatePoisoned)?
            .managed_replay
            && self.current_time_ms().is_some()
        {
            return Ok(());
        }
        let now = local_now(
            &*self
                .engine
                .lock()
                .map_err(|_| TradingVenueError::StatePoisoned)?,
        )?;
        self.process_internal_events(now)?;
        self.start_worker();
        Ok(())
    }

    fn start_worker(&self) {
        let realtime = matches!(self.engine.lock().unwrap().clock, PaperClock::Realtime);
        if !realtime {
            return;
        }
        {
            let mut s = self.state.lock().unwrap();
            if s.worker_running || s.events.is_empty() {
                return;
            }
            s.worker_running = true;
        }
        let venue = self.clone();
        tokio::spawn(async move {
            let notify = venue.state.lock().unwrap().notify.clone();
            loop {
                let next = {
                    let mut s = venue.state.lock().unwrap();
                    match s.events.first_key_value() {
                        Some((key, _)) => Some(key.0),
                        None => {
                            s.worker_running = false;
                            None
                        }
                    }
                };
                let Some(next) = next else {
                    break;
                };
                let now = crate::execution_recording::timestamp_ms() as i64;
                if next > now {
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis((next - now) as u64)) => {},
                        _ = notify.notified() => continue,
                    }
                }
                let now = crate::execution_recording::timestamp_ms() as i64;
                if venue.process_internal_events(now).is_err() {
                    venue.mark_market_incomplete("paper timer processing failed");
                    venue.state.lock().unwrap().worker_running = false;
                    break;
                }
            }
        });
    }

    fn submit_place(
        &self,
        order: DesiredLimitOrder,
    ) -> Result<oneshot::Receiver<Result<PlacedOrder, TradingVenueError>>, TradingVenueError> {
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| TradingVenueError::StatePoisoned)?;
            let engine = self
                .engine
                .lock()
                .map_err(|_| TradingVenueError::StatePoisoned)?;
            enqueue_place(&mut state, &engine, order, Reply::Async(tx))?;
        }
        self.drive_now()?;
        Ok(rx)
    }

    fn submit_cancel(
        &self,
        pair: String,
        orders: Vec<OrderId>,
    ) -> Result<oneshot::Receiver<Result<(), TradingVenueError>>, TradingVenueError> {
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| TradingVenueError::StatePoisoned)?;
            let engine = self
                .engine
                .lock()
                .map_err(|_| TradingVenueError::StatePoisoned)?;
            enqueue_cancel(&mut state, &engine, pair, orders, Reply::Async(tx))?;
        }
        self.drive_now()?;
        Ok(rx)
    }
}

impl TradingVenue for PaperTradingVenue {
    fn mode(&self) -> &'static str {
        if self.simulation_config().is_some() {
            "simulation"
        } else {
            "paper"
        }
    }
    fn strategy_view(&self) -> Self {
        let mut view = self.clone();
        view.execution_access = false;
        view
    }

    fn simulation_config(&self) -> Option<crate::simulation::SimulationConfig> {
        Some(self.state.lock().unwrap().simulation)
    }
    fn simulation_time_model(&self) -> PaperTimeModel {
        self.engine.lock().unwrap().config.time_model
    }
    fn begin_simulation(&self) -> Result<(), TradingVenueError> {
        if !self.execution_access {
            return Err(TradingVenueError::Unavailable);
        }
        let mut s = self.state.lock().unwrap();
        let mut e = self.engine.lock().unwrap();
        s.simulation.validate().map_err(TradingVenueError::Paper)?;
        if !s.operations.is_empty() || !s.events.is_empty() {
            return Err(TradingVenueError::Unavailable);
        }
        if matches!(e.clock, PaperClock::Realtime) {
            e.clock = PaperClock::Replay { now_ms: 0 };
            e.time_floor = i64::MIN;
        }
        e.max_trade_delay_ms = None;
        e.simulation_active = true;
        s.simulation_active = true;
        s.managed_replay = true;
        record(&e, "simulation_model", || {
            serde_json::json!({"config":s.simulation,"time_model":e.config.time_model,
            "execution_model":"exchange_timeline_fifo_v2","ordering":"market,operations,settlement,query,responses,delivery"})
        });
        Ok(())
    }

    fn next_internal_time(&self) -> Option<i64> {
        if !self.execution_access {
            return None;
        }
        let s = self.state.lock().unwrap();
        let e = self.engine.lock().unwrap();
        let cancellation = if s.simulation_active && e.settlement_issues.is_empty() {
            e.cancel_at
                .values()
                .min()
                .and_then(|t| e.config.time_model.exchange_to_local_ms(*t).ok())
        } else {
            None
        };
        s.events
            .first_key_value()
            .map(|(k, _)| k.0)
            .into_iter()
            .chain(cancellation)
            .min()
    }
    fn current_time_ms(&self) -> Option<i64> {
        match self.engine.lock().unwrap().clock {
            PaperClock::Replay { now_ms } => Some(now_ms),
            _ => None,
        }
    }
    fn set_replay_waker(&self, waker: Option<Waker>) {
        if !self.execution_access {
            return;
        }
        self.state.lock().unwrap().waker = waker;
    }
    fn set_replay_driver(&self, active: bool) {
        if !self.execution_access {
            return;
        }
        self.state.lock().unwrap().managed_replay = active;
    }
    fn advance_replay_time(&self, time_ms: i64) -> Result<(), TradingVenueError> {
        if !self.execution_access {
            return Err(TradingVenueError::Unavailable);
        }
        self.engine
            .lock()
            .map_err(|_| TradingVenueError::StatePoisoned)?
            .advance_replay_time(time_ms)
            .map_err(TradingVenueError::Paper)
    }
    fn process_internal_events(&self, local_ms: i64) -> Result<(), TradingVenueError> {
        if !self.execution_access {
            return Err(TradingVenueError::Unavailable);
        }
        let mut s = self
            .state
            .lock()
            .map_err(|_| TradingVenueError::StatePoisoned)?;
        let mut e = self
            .engine
            .lock()
            .map_err(|_| TradingVenueError::StatePoisoned)?;
        process_events(&mut s, &mut e, local_ms)
    }
    fn recorder(&self) -> Option<crate::execution_recording::ExecutionRecorder> {
        self.engine.lock().unwrap().recorder.clone()
    }
    fn has_pending_operations(&self) -> bool {
        if !self.execution_access {
            return false;
        }
        let s = self.state.lock().unwrap();
        !s.operations.is_empty()
            || s.events
                .values()
                .any(|v| matches!(v.event, Internal::Publish { .. }))
    }
    fn paper_settlement_status(&self) -> Option<PaperSettlementStatus> {
        if !self.execution_access {
            return None;
        }
        let s = self.state.lock().unwrap();
        let status = s.settlement_status(&self.engine.lock().unwrap());
        Some(status)
    }
    fn record_paper_settlement(&self) {
        if !self.execution_access {
            return;
        }
        let s = self.state.lock().unwrap();
        let engine = self.engine.lock().unwrap();
        let force = engine.recorder.as_ref().is_some_and(|r| {
            matches!(
                r.profile(),
                crate::execution_recording::RecordingProfile::Diagnostic {
                    queue: crate::execution_recording::QueueRecording::Full
                        | crate::execution_recording::QueueRecording::Periodic { .. }
                        | crate::execution_recording::QueueRecording::Delta { .. }
                }
            )
        });
        engine.record_queue(force);
        record(&engine, "paper_settlement", || {
            let mut data = engine.settlement_record_data();
            let pending = s.operations.len()
                + s.events
                    .values()
                    .filter(|v| matches!(v.event, Internal::Publish { .. }))
                    .count();
            data["latency_model"] = serde_json::json!(s.latency);
            data["simulation"] = serde_json::json!(s.simulation_active.then_some(s.simulation));
            data["pending_operations"] = serde_json::json!(pending);
            data["pending_operation_details"] = serde_json::json!(
                s.operations
                    .values()
                    .map(PendingOperation::json)
                    .collect::<Vec<_>>()
            );
            data["unsettled"] = serde_json::json!(data["unsettled"] == true || pending > 0);
            data
        });
    }
    fn mark_market_incomplete(&self, reason: &str) {
        if !self.execution_access {
            return;
        }
        self.engine.lock().unwrap().mark_incomplete(reason);
    }
    fn account_snapshot<'a>(&'a self, pair: &'a str) -> TradingVenueFuture<'a, AccountSnapshot> {
        Box::pin(async move {
            let recorder = self.recorder();
            if recorder.is_none() {
                return self.query_account(pair).await;
            }
            crate::execution_recording::operation(
                recorder.as_ref(),
                "account_snapshot",
                serde_json::json!({"pair":pair}),
                async move { self.query_account(pair).await },
            )
            .await
        })
    }
    fn place_order(&self, request: PlacementRequest) -> TradingVenueFuture<'_, PlacedOrder> {
        Box::pin(async move {
            let recorder = self.recorder();
            if recorder.is_none() {
                return self
                    .submit_place(request.order)?
                    .await
                    .map_err(|_| TradingVenueError::Unavailable)?;
            }
            crate::execution_recording::operation(
                recorder.as_ref(),
                "place_order",
                serde_json::json!(&request),
                async move {
                    self.submit_place(request.order)?
                        .await
                        .map_err(|_| TradingVenueError::Unavailable)?
                },
            )
            .await
        })
    }
    fn cancel_orders<'a>(
        &'a self,
        pair: &'a str,
        orders: Vec<OrderId>,
    ) -> TradingVenueFuture<'a, ()> {
        Box::pin(async move {
            let recorder = self.recorder();
            if recorder.is_none() {
                return self
                    .submit_cancel(pair.into(), orders)?
                    .await
                    .map_err(|_| TradingVenueError::Unavailable)?;
            }
            crate::execution_recording::operation(
                recorder.as_ref(),
                "cancel_orders",
                serde_json::json!({"pair":pair,"order_ids":orders}),
                async move {
                    self.submit_cancel(pair.into(), orders)?
                        .await
                        .map_err(|_| TradingVenueError::Unavailable)?
                },
            )
            .await
        })
    }
    fn on_market_event(&self, event: &MarketEvent) -> Result<(), TradingVenueError> {
        self.on_market_event_recovering(event)?;
        if !self.engine.lock().unwrap().settlement_issues.is_empty() {
            return Err(TradingVenueError::Paper(
                PaperExecutionError::InvalidTradeStream,
            ));
        }
        Ok(())
    }
    fn on_market_event_recovering(&self, event: &MarketEvent) -> Result<(), TradingVenueError> {
        self.apply_market(event, None)
    }
    fn on_simulation_market(
        &self,
        event: &MarketEvent,
        received_at_ms: i64,
    ) -> Result<(), TradingVenueError> {
        self.apply_market(event, Some(received_at_ms))
    }
}
impl PaperTradingVenue {
    fn apply_market(
        &self,
        event: &MarketEvent,
        received_at_ms: Option<i64>,
    ) -> Result<(), TradingVenueError> {
        if !self.execution_access {
            return Err(TradingVenueError::Unavailable);
        }
        {
            let mut s = self
                .state
                .lock()
                .map_err(|_| TradingVenueError::StatePoisoned)?;
            let mut e = self
                .engine
                .lock()
                .map_err(|_| TradingVenueError::StatePoisoned)?;
            // delay境界を自動確定しない入力登録では口座は変わらない。
            // simulationの口座通知はsettle_simulationが確定時に生成する。
            let before = e.max_trade_delay_ms.map(|_| snapshot(&e));
            e.apply_timed_market_event(event, received_at_ms);
            if before.is_some_and(|before| snapshot(&e) != before) {
                s.publish_market(&e, local_now(&e)?)?;
            }
        }
        self.drive_now()
    }
}

fn process_events(
    s: &mut State,
    e: &mut PaperExecutionEngine,
    local_ms: i64,
) -> Result<(), TradingVenueError> {
    if let PaperClock::Replay { now_ms } = e.clock {
        if local_ms < now_ms {
            return Err(TradingVenueError::Paper(PaperExecutionError::InvalidClock));
        }
    }
    while s
        .events
        .first_key_value()
        .is_some_and(|(key, _)| key.0 <= local_ms)
    {
        let ((at, priority, _), scheduled) = s.events.pop_first().unwrap();
        let mut context = scheduled.context;
        let event = scheduled.event;
        if let PaperClock::Replay { now_ms } = e.clock {
            e.advance_replay_time(at.max(now_ms))
                .map_err(TradingVenueError::Paper)?;
        }
        let executed = local_now(e)?;
        if s.simulation_active && priority > 0 {
            settle_simulation(s, e, executed)?;
        }
        context.event_time_ms = Some(executed);
        // 内部イベントはその実行時刻を明示する。元要求の共有時計に上書きさせない。
        context.replay_clock = None;
        crate::execution_recording::sync_scope(context, || -> Result<(), TradingVenueError> {
            match event {
                Internal::Query(event) => query::process(s, e, event, executed)?,
                Internal::Place {
                    id,
                    order,
                    response_at,
                    reply,
                } => {
                    let result = e.place_order(order).map_err(TradingVenueError::Paper);
                    let accepted_id = result.as_ref().ok().and_then(|v| v.order_id);
                    if let Some(PendingOperation::Place { accepted, .. }) =
                        s.operations.get_mut(&id)
                    {
                        *accepted = Some(accepted_id);
                    }
                    record(e, "paper_operation_effective", || {
                        serde_json::json!({"id":id,"local_ms":executed,"scheduled_local_ms":at,
                                    "exchange_ms":e.config.time_model.local_to_exchange_ms(executed).ok(),
                                    "order_id":accepted_id,"effective_exchange_ms":accepted_id.and_then(|id|e.effective_after.get(&id).copied()),"success":result.is_ok()})
                    });
                    let response_at =
                        response_at.max(add(executed, s.latency.placement.response_ms)?);
                    if result.is_ok() {
                        s.publish(e, response_at, id)?;
                    }
                    s.enqueue(response_at, Internal::PlaceResponse { id, result, reply })?;
                }
                Internal::Cancel {
                    id,
                    pair,
                    orders,
                    response_at,
                    reply,
                } => {
                    let result = if pair != e.config.pair {
                        Err(TradingVenueError::Paper(
                            PaperExecutionError::PairMismatch {
                                expected: e.config.pair.clone(),
                                actual: pair,
                            },
                        ))
                    } else {
                        e.cancel_orders(&pair, orders);
                        Ok(())
                    };
                    record(e, "paper_operation_effective", || {
                        serde_json::json!({"id":id,"local_ms":executed,"scheduled_local_ms":at,
                        "exchange_ms":e.config.time_model.local_to_exchange_ms(executed).ok(),"success":result.is_ok()})
                    });
                    let response_at =
                        response_at.max(add(executed, s.latency.cancellation.response_ms)?);
                    if result.is_ok() {
                        s.publish(e, response_at, id)?;
                    }
                    s.enqueue(response_at, Internal::CancelResponse { id, result, reply })?;
                }
                Internal::PlaceResponse { id, result, reply } => {
                    record(
                        e,
                        "paper_operation_response",
                        || serde_json::json!({"id":id,"local_ms":executed,"scheduled_local_ms":at,"success":result.is_ok()}),
                    );
                    s.operations.remove(&id);
                    reply.send(result, s);
                }
                Internal::CancelResponse { id, result, reply } => {
                    record(e, "paper_operation_response", || {
                        serde_json::json!({"id":id,"local_ms":executed,"scheduled_local_ms":at,
                        "success":result.is_ok(),"meaning":"acceptance_ack","settlement_pending":!e.cancel_at.is_empty()})
                    });
                    s.operations.remove(&id);
                    reply.send(result, s);
                }
                Internal::Publish { id, snapshot } => {
                    s.local = snapshot;
                    record(
                        e,
                        "paper_local_state",
                        || serde_json::json!({"operation_id":id,"local_ms":executed,"scheduled_local_ms":at,"account":s.local}),
                    );
                }
            }
            Ok(())
        })?;
    }
    if s.simulation_active {
        settle_simulation(s, e, local_ms)?;
    }
    if matches!(e.clock, PaperClock::Replay { .. }) {
        e.advance_replay_time(local_ms)
            .map_err(TradingVenueError::Paper)?;
    }
    Ok(())
}

#[derive(Debug)]
enum Reply<T> {
    Async(oneshot::Sender<Result<T, TradingVenueError>>),
    Command(u64),
}
#[derive(Debug)]
enum Completion {
    Query(u64, Result<query::Part, TradingVenueError>),
    Place(u64, Result<PlacedOrder, TradingVenueError>),
    Cancel(u64, Result<(), TradingVenueError>),
}
trait Complete {
    fn completion(id: u64, result: Result<Self, TradingVenueError>) -> Completion
    where
        Self: Sized;
}
impl Complete for PlacedOrder {
    fn completion(id: u64, result: Result<Self, TradingVenueError>) -> Completion {
        Completion::Place(id, result)
    }
}
impl Complete for () {
    fn completion(id: u64, result: Result<Self, TradingVenueError>) -> Completion {
        Completion::Cancel(id, result)
    }
}
impl Complete for query::Part {
    fn completion(id: u64, result: Result<Self, TradingVenueError>) -> Completion {
        Completion::Query(id, result)
    }
}
impl<T: Complete> Reply<T> {
    fn send(self, result: Result<T, TradingVenueError>, state: &mut State) {
        match self {
            Self::Async(tx) => {
                let _ = tx.send(result);
            }
            Self::Command(id) => state.completed.push_back(T::completion(id, result)),
        }
    }
}
mod owned;
pub use owned::OwnedPaperExecution;

fn enqueue_place(
    s: &mut State,
    e: &PaperExecutionEngine,
    order: DesiredLimitOrder,
    reply: Reply<PlacedOrder>,
) -> Result<(), TradingVenueError> {
    let (now, entry, response) = request_times(e, s.latency.placement)?;
    let id = s
        .operation
        .checked_add(1)
        .ok_or(TradingVenueError::Paper(PaperExecutionError::InvalidClock))?;
    s.operation = id;
    let info = PendingOperation::Place {
        id,
        requested: now,
        effective: entry,
        exchange: e.config.time_model.local_to_exchange_ms(entry).ok(),
        response,
        order: order.clone(),
        accepted: None,
    };
    record(e, "paper_operation_requested", || info.json());
    s.operations.insert(id, info);
    s.enqueue(
        entry,
        Internal::Place {
            id,
            order,
            response_at: response,
            reply,
        },
    )?;
    Ok(())
}
fn enqueue_cancel(
    s: &mut State,
    e: &PaperExecutionEngine,
    pair: String,
    orders: Vec<OrderId>,
    reply: Reply<()>,
) -> Result<(), TradingVenueError> {
    let (now, entry, response) = request_times(e, s.latency.cancellation)?;
    let id = s
        .operation
        .checked_add(1)
        .ok_or(TradingVenueError::Paper(PaperExecutionError::InvalidClock))?;
    s.operation = id;
    let info = PendingOperation::Cancel {
        id,
        requested: now,
        effective: entry,
        exchange: e.config.time_model.local_to_exchange_ms(entry).ok(),
        response,
        orders: orders.clone(),
    };
    record(e, "paper_operation_requested", || info.json());
    s.operations.insert(id, info);
    s.enqueue(
        entry,
        Internal::Cancel {
            id,
            pair,
            orders,
            response_at: response,
            reply,
        },
    )?;
    Ok(())
}
fn enqueue_query(
    s: &mut State,
    e: &PaperExecutionEngine,
    pair: &str,
    endpoint: query::Endpoint,
    reply: impl FnOnce(u64) -> Reply<query::Part>,
) -> Result<u64, TradingVenueError> {
    let latency = match endpoint {
        Endpoint::Orders => s.latency.orders_query,
        Endpoint::Balances => s.latency.balances_query,
    };
    let (now, sample_at, response_at) = request_times(e, latency)?;
    let id = s
        .operation
        .checked_add(1)
        .ok_or(TradingVenueError::Paper(PaperExecutionError::InvalidClock))?;
    s.operation = id;
    let info = PendingOperation::Query {
        id,
        kind: endpoint.name(),
        pair: pair.into(),
        requested: now,
        sample: sample_at,
        response: response_at,
        sampled: None,
    };
    record(e, "paper_query_requested", || info.json());
    s.operations.insert(id, info);
    let enqueued = s.enqueue(
        sample_at,
        Internal::Query(QueryEvent::Sample {
            id,
            pair: pair.into(),
            endpoint,
            requested: now,
            response_delay: latency.response_ms,
            reply: reply(id),
        }),
    );
    if let Err(error) = enqueued {
        s.operations.remove(&id);
        return Err(error);
    }
    Ok(id)
}

fn request_times(
    e: &PaperExecutionEngine,
    latency: PaperOperationLatency,
) -> Result<(i64, i64, i64), TradingVenueError> {
    let now = local_now(e)?;
    let entry = add(now, latency.entry_ms)?;
    let response = add(entry, latency.response_ms)?;
    e.config
        .time_model
        .local_to_exchange_ms(entry)
        .map_err(TradingVenueError::Paper)?;
    Ok((now, entry, response))
}
