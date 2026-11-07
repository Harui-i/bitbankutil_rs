//! 実験用の同期イベント・コマンドAPI。要求の完了は後続のResponseで通知する。
//! ランナーは取引所を単独所有し、戦略へ内部板や未応答口座を公開しない。
use crate::{
    bitbank_bot::{BotEvent, InputId, ScheduledEventInfo},
    market_event::MarketEvent,
    order_domain::{DesiredLimitOrder, OrderId},
    paper_execution::{OwnedPaperExecution, PaperSettlementStatus, PaperTimeModel},
    simulation::{ReplayRuntime, SimulationConfig},
    trading_venue::{AccountSnapshot, PlacedOrder, TradingVenueError},
};
use std::collections::{BTreeSet, VecDeque};

#[derive(Debug)]
pub enum Command {
    Account { pair: String },
    Place(DesiredLimitOrder),
    Cancel { pair: String, orders: Vec<OrderId> },
}
/// idは未完了要求間で一意。応答と対応づけるために戦略が指定する。
#[derive(Debug)]
pub struct Request {
    pub id: u64,
    pub command: Command,
}
#[derive(Debug)]
pub enum ResponseValue {
    Account(Result<AccountSnapshot, TradingVenueError>),
    Placed(Result<PlacedOrder, TradingVenueError>),
    Cancelled(Result<(), TradingVenueError>),
}
#[derive(Debug)]
pub struct Response {
    pub id: u64,
    pub value: ResponseValue,
}
pub enum StrategyEvent<C> {
    Input(BotEvent<C>),
    Response(Response),
}
/// trueを返す間は現在の入力処理が継続中となり、新しい市場配送は保留される。
/// 応答だけを先に受け取り、falseを返すと保留した配送を受け付ける。
pub trait EventStrategy {
    type CustomEvent: Send + 'static;
    fn on_event(
        &mut self,
        event: StrategyEvent<Self::CustomEvent>,
        now_ms: i64,
        commands: &mut Vec<Request>,
    ) -> Result<bool, String>;
}

pub struct EventRunner<S: EventStrategy> {
    strategy: S,
    execution: OwnedPaperExecution,
    now: Option<i64>,
    busy: bool,
    deliveries: VecDeque<BotEvent<S::CustomEvent>>,
    commands: Vec<Request>,
    requests: BTreeSet<u64>,
}
impl<S: EventStrategy> EventRunner<S> {
    pub fn new(strategy: S, execution: OwnedPaperExecution) -> Self {
        Self {
            strategy,
            execution,
            now: None,
            busy: false,
            deliveries: VecDeque::new(),
            commands: Vec::new(),
            requests: BTreeSet::new(),
        }
    }
    pub fn execution(&self) -> &OwnedPaperExecution {
        &self.execution
    }
    fn dispatch(&mut self, event: StrategyEvent<S::CustomEvent>) -> Result<(), String> {
        self.busy = self
            .strategy
            .on_event(event, self.now.unwrap(), &mut self.commands)?;
        for request in self.commands.drain(..) {
            if !self.requests.insert(request.id) {
                return Err("duplicate pending command id".into());
            }
            self.execution
                .submit(request)
                .map_err(|e| format!("{e:?}"))?;
        }
        Ok(())
    }
    fn drive(&mut self, time: i64) -> Result<(), String> {
        loop {
            // 同msの操作→確定→照会採取→応答を処理してから戦略へ戻す。
            self.execution.process(time).map_err(|e| format!("{e:?}"))?;
            if let Some(response) = self.execution.response() {
                self.requests.remove(&response.id);
                self.dispatch(StrategyEvent::Response(response))?;
            } else if !self.busy {
                if let Some(event) = self.deliveries.pop_front() {
                    self.dispatch(StrategyEvent::Input(event))?;
                } else {
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        }
    }
}
impl<S: EventStrategy> ReplayRuntime for EventRunner<S> {
    type CustomEvent = S::CustomEvent;
    fn simulation_config(&self) -> Option<SimulationConfig> {
        Some(self.execution.config())
    }
    fn simulation_time_model(&self) -> PaperTimeModel {
        self.execution.time_model()
    }
    fn begin_simulation(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn mark_market_incomplete(&mut self, reason: &str) {
        self.execution.incomplete(reason)
    }
    fn recorder(&self) -> Option<crate::execution_recording::ExecutionRecorder> {
        None
    }
    fn paper_settlement_status(&self) -> Option<PaperSettlementStatus> {
        Some(self.execution.status())
    }
    fn record_paper_settlement(&self) {}
    fn on_simulation_market(&mut self, event: &MarketEvent, received: i64) -> Result<(), String> {
        self.execution.market(event, received);
        Ok(())
    }
    fn process_internal_events(&mut self, time: i64) -> Result<(), String> {
        self.execution.process(time).map_err(|e| format!("{e:?}"))
    }
    fn advance_time(&mut self, time: i64) -> Result<(), String> {
        if self.now.is_some_and(|now| time < now) {
            return Err("time reversed".into());
        }
        self.execution.advance(time).map_err(|e| format!("{e:?}"))?;
        self.now = Some(time);
        Ok(())
    }
    fn next_internal_time(&self) -> Option<i64> {
        self.execution
            .next_time()
            .map(|t| self.now.map_or(t, |now| now.max(t)))
    }
    fn now_ms(&self) -> Option<i64> {
        self.now
    }
    fn pending_scheduled(&self) -> Vec<ScheduledEventInfo> {
        Vec::new()
    }
    fn handler_pending(&self) -> bool {
        self.busy || !self.requests.is_empty()
    }
    fn pending_delivery_count(&self) -> usize {
        self.deliveries.len()
    }
    fn stop_pending_handler(&mut self) {
        self.busy = false;
    }
    fn enqueue_simulation_delivery(
        &mut self,
        _time: i64,
        _id: Option<InputId>,
        _market_id: Option<u64>,
        event: BotEvent<Self::CustomEvent>,
        _recovery: bool,
    ) -> Result<(), String> {
        if self.deliveries.len() >= 10_000 {
            return Err("delivery limit".into());
        }
        self.deliveries.push_back(event);
        Ok(())
    }
    async fn tick(&mut self, time: i64) -> Result<(), String> {
        self.drive(time)
    }
}
