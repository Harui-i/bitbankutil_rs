//! 注文の取引所有効化と、ローカルへの応答・通知を駆動する。
use super::*;
use std::task::Waker;
use tokio::sync::oneshot;
mod query;
use query::QueryEvent;

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
    /// watermarkで確定できた状態変化からローカル通知までの遅延。
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
        reply: oneshot::Sender<Result<PlacedOrder, TradingVenueError>>,
    },
    Cancel {
        id: u64,
        pair: String,
        orders: Vec<OrderId>,
        response_at: i64,
        reply: oneshot::Sender<Result<(), TradingVenueError>>,
    },
    PlaceResponse {
        id: u64,
        result: Result<PlacedOrder, TradingVenueError>,
        reply: oneshot::Sender<Result<PlacedOrder, TradingVenueError>>,
    },
    CancelResponse {
        id: u64,
        result: Result<(), TradingVenueError>,
        reply: oneshot::Sender<Result<(), TradingVenueError>>,
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
pub(super) struct State {
    latency: PaperLatencyConfig,
    local: AccountSnapshot,
    events: BTreeMap<(i64, u64), ScheduledInternal>,
    sequence: u64,
    operation: u64,
    operations: BTreeMap<u64, serde_json::Value>,
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
            local: snapshot(engine),
            events: BTreeMap::new(),
            sequence: 0,
            operation: 0,
            operations: BTreeMap::new(),
            publication_floor: i64::MIN,
            waker: None,
            worker_running: false,
            managed_replay: false,
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }
    fn enqueue(&mut self, at: i64, event: Internal) -> Result<(), TradingVenueError> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(TradingVenueError::Paper(PaperExecutionError::InvalidClock))?;
        self.events.insert(
            (at, self.sequence),
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
                serde_json::json!({"operation_id":0,"local_ms":now,"account":self.local}),
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
fn record(engine: &PaperExecutionEngine, kind: &str, data: serde_json::Value) {
    if let Some(r) = &engine.recorder {
        r.record(kind, data);
    }
}

#[derive(Debug, Clone)]
pub struct PaperTradingVenue {
    engine: Arc<Mutex<PaperExecutionEngine>>,
    state: Arc<Mutex<State>>,
}

impl PaperTradingVenue {
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
        record(
            &engine,
            "paper_latency_model",
            serde_json::json!({"latency":latency,
            "local_view":"ordered_notifications", "cancel_response":"acceptance_ack"}),
        );
        Ok(Self {
            engine: Arc::new(Mutex::new(engine)),
            state,
        })
    }
    /// 同じengineから作るハンドルも操作・通知キューを共有する。
    pub fn from_shared(engine: Arc<Mutex<PaperExecutionEngine>>) -> Self {
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
        Self { engine, state }
    }
    /// 診断用の取引所状態。直接の変更はローカル通知経路を迂回する。
    pub fn engine(&self) -> Arc<Mutex<PaperExecutionEngine>> {
        self.engine.clone()
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
        self.state
            .lock()
            .unwrap()
            .operations
            .values()
            .cloned()
            .collect()
    }

    pub fn advance_trade_watermark(
        &self,
        through_ms: i64,
    ) -> Result<Vec<PaperEvent>, TradingVenueError> {
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

    fn request_times(
        &self,
        latency: PaperOperationLatency,
    ) -> Result<(i64, i64, i64), TradingVenueError> {
        let e = self
            .engine
            .lock()
            .map_err(|_| TradingVenueError::StatePoisoned)?;
        let now = local_now(&e)?;
        let entry = add(now, latency.entry_ms)?;
        let response = add(entry, latency.response_ms)?;
        e.config
            .time_model
            .local_to_exchange_ms(entry)
            .map_err(TradingVenueError::Paper)?;
        Ok((now, entry, response))
    }

    fn submit_place(
        &self,
        order: DesiredLimitOrder,
    ) -> Result<oneshot::Receiver<Result<PlacedOrder, TradingVenueError>>, TradingVenueError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| TradingVenueError::StatePoisoned)?;
        let (now, entry, response) = self.request_times(s.latency.placement)?;
        let id = s
            .operation
            .checked_add(1)
            .ok_or(TradingVenueError::Paper(PaperExecutionError::InvalidClock))?;
        let (tx, rx) = oneshot::channel();
        s.operation = id;
        let info = serde_json::json!({"id":id,"kind":"place","requested_at_ms":now,
            "effective_local_ms":entry,"effective_exchange_ms":self.engine.lock().unwrap().config.time_model.local_to_exchange_ms(entry).ok(),"response_at_ms":response,"order":order});
        record(
            &self.engine.lock().unwrap(),
            "paper_operation_requested",
            info.clone(),
        );
        s.operations.insert(id, info);
        s.enqueue(
            entry,
            Internal::Place {
                id,
                order,
                response_at: response,
                reply: tx,
            },
        )?;
        drop(s);
        self.drive_now()?;
        Ok(rx)
    }

    fn submit_cancel(
        &self,
        pair: String,
        orders: Vec<OrderId>,
    ) -> Result<oneshot::Receiver<Result<(), TradingVenueError>>, TradingVenueError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| TradingVenueError::StatePoisoned)?;
        let (now, entry, response) = self.request_times(s.latency.cancellation)?;
        let id = s
            .operation
            .checked_add(1)
            .ok_or(TradingVenueError::Paper(PaperExecutionError::InvalidClock))?;
        let (tx, rx) = oneshot::channel();
        s.operation = id;
        let info = serde_json::json!({"id":id,"kind":"cancel","requested_at_ms":now,
            "effective_local_ms":entry,"effective_exchange_ms":self.engine.lock().unwrap().config.time_model.local_to_exchange_ms(entry).ok(),"response_at_ms":response,"orders":orders});
        record(
            &self.engine.lock().unwrap(),
            "paper_operation_requested",
            info.clone(),
        );
        s.operations.insert(id, info);
        s.enqueue(
            entry,
            Internal::Cancel {
                id,
                pair,
                orders,
                response_at: response,
                reply: tx,
            },
        )?;
        drop(s);
        self.drive_now()?;
        Ok(rx)
    }
}

impl TradingVenue for PaperTradingVenue {
    fn next_internal_time(&self) -> Option<i64> {
        self.state
            .lock()
            .unwrap()
            .events
            .first_key_value()
            .map(|(k, _)| k.0)
    }
    fn current_time_ms(&self) -> Option<i64> {
        match self.engine.lock().unwrap().clock {
            PaperClock::Replay { now_ms } => Some(now_ms),
            _ => None,
        }
    }
    fn set_replay_waker(&self, waker: Option<Waker>) {
        self.state.lock().unwrap().waker = waker;
    }
    fn set_replay_driver(&self, active: bool) {
        self.state.lock().unwrap().managed_replay = active;
    }
    fn advance_replay_time(&self, time_ms: i64) -> Result<(), TradingVenueError> {
        self.engine
            .lock()
            .map_err(|_| TradingVenueError::StatePoisoned)?
            .advance_replay_time(time_ms)
            .map_err(TradingVenueError::Paper)
    }
    fn process_internal_events(&self, local_ms: i64) -> Result<(), TradingVenueError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| TradingVenueError::StatePoisoned)?;
        let mut e = self
            .engine
            .lock()
            .map_err(|_| TradingVenueError::StatePoisoned)?;
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
            let ((at, _), scheduled) = s.events.pop_first().unwrap();
            let mut context = scheduled.context;
            let event = scheduled.event;
            if let PaperClock::Replay { now_ms } = e.clock {
                e.advance_replay_time(at.max(now_ms))
                    .map_err(TradingVenueError::Paper)?;
            }
            let executed = local_now(&e)?;
            context.event_time_ms = Some(executed);
            // 内部イベントはその実行時刻を明示する。元要求の共有時計に上書きさせない。
            context.replay_clock = None;
            crate::execution_recording::sync_scope(
                context,
                || -> Result<(), TradingVenueError> {
                    match event {
                        Internal::Query(event) => query::process(&mut s, &mut e, event, executed)?,
                        Internal::Place {
                            id,
                            order,
                            response_at,
                            reply,
                        } => {
                            let result = e.place_order(order).map_err(TradingVenueError::Paper);
                            let accepted_id = result.as_ref().ok().and_then(|v| v.order_id);
                            if let Some(info) = s.operations.get_mut(&id) {
                                info["order_id"] = serde_json::json!(accepted_id);
                                info["phase"] = serde_json::json!("awaiting_response");
                            }
                            record(
                                &e,
                                "paper_operation_effective",
                                serde_json::json!({"id":id,"local_ms":executed,"scheduled_local_ms":at,
                                    "exchange_ms":e.config.time_model.local_to_exchange_ms(executed).ok(),
                                    "order_id":accepted_id,"effective_exchange_ms":accepted_id.and_then(|id|e.effective_after.get(&id).copied()),"success":result.is_ok()}),
                            );
                            let response_at =
                                response_at.max(add(executed, s.latency.placement.response_ms)?);
                            if result.is_ok() {
                                s.publish(&e, response_at, id)?;
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
                            record(
                                &e,
                                "paper_operation_effective",
                                serde_json::json!({"id":id,"local_ms":executed,"scheduled_local_ms":at,
                        "exchange_ms":e.config.time_model.local_to_exchange_ms(executed).ok(),"success":result.is_ok()}),
                            );
                            let response_at =
                                response_at.max(add(executed, s.latency.cancellation.response_ms)?);
                            if result.is_ok() {
                                s.publish(&e, response_at, id)?;
                            }
                            s.enqueue(response_at, Internal::CancelResponse { id, result, reply })?;
                        }
                        Internal::PlaceResponse { id, result, reply } => {
                            record(
                                &e,
                                "paper_operation_response",
                                serde_json::json!({"id":id,"local_ms":executed,"scheduled_local_ms":at,"success":result.is_ok()}),
                            );
                            s.operations.remove(&id);
                            let _ = reply.send(result);
                        }
                        Internal::CancelResponse { id, result, reply } => {
                            record(
                                &e,
                                "paper_operation_response",
                                serde_json::json!({"id":id,"local_ms":executed,"scheduled_local_ms":at,
                        "success":result.is_ok(),"meaning":"acceptance_ack","settlement_pending":!e.cancel_at.is_empty()}),
                            );
                            s.operations.remove(&id);
                            let _ = reply.send(result);
                        }
                        Internal::Publish { id, snapshot } => {
                            s.local = snapshot;
                            record(
                                &e,
                                "paper_local_state",
                                serde_json::json!({"operation_id":id,"local_ms":executed,"scheduled_local_ms":at,"account":s.local}),
                            );
                        }
                    }
                    Ok(())
                },
            )?;
        }
        if matches!(e.clock, PaperClock::Replay { .. }) {
            e.advance_replay_time(local_ms)
                .map_err(TradingVenueError::Paper)?;
        }
        Ok(())
    }
    fn recorder(&self) -> Option<crate::execution_recording::ExecutionRecorder> {
        self.engine.lock().unwrap().recorder.clone()
    }
    fn paper_settlement_status(&self) -> Option<PaperSettlementStatus> {
        let s = self.state.lock().unwrap();
        let mut status = self.engine.lock().unwrap().settlement_status();
        status.latency_model = s.latency;
        status.pending_operation_details = s.operations.values().cloned().collect();
        status.pending_operations = s.operations.len()
            + s.events
                .values()
                .filter(|v| matches!(v.event, Internal::Publish { .. }))
                .count();
        status.unsettled |= status.pending_operations > 0;
        Some(status)
    }
    fn mark_market_incomplete(&self, reason: &str) {
        self.engine.lock().unwrap().mark_incomplete(reason);
    }
    fn account_snapshot<'a>(&'a self, pair: &'a str) -> TradingVenueFuture<'a, AccountSnapshot> {
        Box::pin(async move {
            let recorder = self.recorder();
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
        {
            let mut s = self
                .state
                .lock()
                .map_err(|_| TradingVenueError::StatePoisoned)?;
            let mut e = self
                .engine
                .lock()
                .map_err(|_| TradingVenueError::StatePoisoned)?;
            let before = snapshot(&e);
            e.apply_market_event(event);
            if snapshot(&e) != before {
                s.publish_market(&e, local_now(&e)?)?;
            }
        }
        self.drive_now()
    }
}
