//! シミュレーション実行ループだけが所有する注文・照会キュー。
use super::*;
use crate::event_strategy::{Command, Request, Response, ResponseValue};
use crate::trading_venue::AccountQueryMetadata;
#[derive(Default)]
struct AccountParts {
    orders: Option<Result<query::Part, TradingVenueError>>,
    balances: Option<Result<query::Part, TradingVenueError>>,
}
/// Cloneしない単独所有のバックテスト用実行器。戦略には公開しない。
/// 記録プロファイルと実時計運用は、この実験APIではサポートしない。
pub struct OwnedPaperExecution {
    engine: PaperExecutionEngine,
    state: State,
    accounts: BTreeMap<u64, AccountParts>,
    query_tokens: BTreeMap<u64, (u64, query::Endpoint)>,
    responses: VecDeque<Response>,
}
impl OwnedPaperExecution {
    pub fn new(
        mut engine: PaperExecutionEngine,
        latency: PaperLatencyConfig,
        config: crate::simulation::SimulationConfig,
    ) -> Result<Self, PaperExecutionError> {
        latency.validate()?;
        config.validate()?;
        if engine.recorder.is_some() || engine.venue_state.is_some() {
            return Err(PaperExecutionError::InvalidClock);
        }
        if matches!(engine.clock, PaperClock::Realtime) {
            engine.clock = PaperClock::Replay { now_ms: 0 };
            engine.time_floor = i64::MIN;
        }
        engine.max_trade_delay_ms = None;
        engine.simulation_active = true;
        let mut state = State::new(&engine, latency);
        state.simulation = config;
        state.simulation_active = true;
        state.managed_replay = true;
        Ok(Self {
            engine,
            state,
            accounts: BTreeMap::new(),
            query_tokens: BTreeMap::new(),
            responses: VecDeque::new(),
        })
    }
    pub fn balances(&self) -> Vec<BalanceSnapshot> {
        self.engine.balances()
    }
    pub fn open_orders(&self) -> Vec<OpenOrder> {
        self.engine.open_orders()
    }
    pub(crate) fn config(&self) -> crate::simulation::SimulationConfig {
        self.state.simulation
    }
    pub(crate) fn time_model(&self) -> PaperTimeModel {
        self.engine.config.time_model
    }
    pub(crate) fn now(&self) -> i64 {
        local_now(&self.engine).expect("replay clock")
    }
    pub(crate) fn advance(&mut self, time: i64) -> Result<(), TradingVenueError> {
        self.engine
            .advance_replay_time(time)
            .map_err(TradingVenueError::Paper)
    }
    pub(crate) fn next_time(&self) -> Option<i64> {
        let cancellation = if self.engine.settlement_issues.is_empty() {
            self.engine
                .cancel_at
                .values()
                .min()
                .and_then(|t| self.time_model().exchange_to_local_ms(*t).ok())
        } else {
            None
        };
        self.state
            .events
            .first_key_value()
            .map(|(k, _)| k.0)
            .into_iter()
            .chain(cancellation)
            .min()
    }
    pub(crate) fn process(&mut self, time: i64) -> Result<(), TradingVenueError> {
        process_events(&mut self.state, &mut self.engine, time)
    }
    pub(crate) fn market(&mut self, event: &MarketEvent, received: i64) {
        self.engine.apply_timed_market_event(event, Some(received));
    }
    pub(crate) fn incomplete(&mut self, reason: &str) {
        self.engine.mark_incomplete(reason)
    }
    pub(crate) fn status(&self) -> PaperSettlementStatus {
        self.state.settlement_status(&self.engine)
    }
    pub(crate) fn submit(&mut self, request: Request) -> Result<(), TradingVenueError> {
        let token = request.id;
        match request.command {
            Command::Account { pair } => {
                if self.accounts.contains_key(&token) {
                    return Err(TradingVenueError::Unavailable);
                }
                self.accounts.insert(token, AccountParts::default());
                // 両エンドポイントを同時に登録する。片方失敗でももう片方は実行する。
                let orders = self.submit_query(token, &pair, query::Endpoint::Orders);
                let balances = self.submit_query(token, &pair, query::Endpoint::Balances);
                if let Err(e) = orders {
                    self.accounts.get_mut(&token).unwrap().orders = Some(Err(e));
                }
                if let Err(e) = balances {
                    self.accounts.get_mut(&token).unwrap().balances = Some(Err(e));
                }
                self.finish_account(token);
            }
            Command::Place(order) => {
                if let Err(e) = self.submit_place(token, order) {
                    self.responses.push_back(Response {
                        id: token,
                        value: ResponseValue::Placed(Err(e)),
                    });
                }
            }
            Command::Cancel { pair, orders } => {
                if let Err(e) = self.submit_cancel(token, pair, orders) {
                    self.responses.push_back(Response {
                        id: token,
                        value: ResponseValue::Cancelled(Err(e)),
                    });
                }
            }
        }
        Ok(())
    }
    fn finish_account(&mut self, token: u64) {
        let p = &self.accounts[&token];
        if p.orders.is_none() || p.balances.is_none() {
            return;
        }
        let mut parts = self.accounts.remove(&token).unwrap();
        let result = (|| {
            let orders = parts.orders.take().unwrap()?;
            let balances = parts.balances.take().unwrap()?;
            let query::Values::Orders(open_orders) = orders.values else {
                unreachable!()
            };
            let query::Values::Balances(values) = balances.values else {
                unreachable!()
            };
            Ok(AccountSnapshot {
                open_orders,
                balances: values,
                query: Some(AccountQueryMetadata {
                    orders: orders.observation,
                    balances: balances.observation,
                    completed_local_ms: self.now(),
                }),
            })
        })();
        self.responses.push_back(Response {
            id: token,
            value: ResponseValue::Account(result),
        });
    }
    pub(crate) fn response(&mut self) -> Option<Response> {
        while let Some(c) = self.state.completed.pop_front() {
            match c {
                Completion::Query(part_id, result) => {
                    let (token, kind) = self
                        .query_tokens
                        .remove(&part_id)
                        .expect("registered query endpoint");
                    let p = self
                        .accounts
                        .get_mut(&token)
                        .expect("registered account query");
                    let endpoint = matches!(kind, query::Endpoint::Orders);
                    if endpoint {
                        p.orders = Some(result)
                    } else {
                        p.balances = Some(result)
                    };
                    self.finish_account(token);
                }
                Completion::Place(id, result) => self.responses.push_back(Response {
                    id,
                    value: ResponseValue::Placed(result),
                }),
                Completion::Cancel(id, result) => self.responses.push_back(Response {
                    id,
                    value: ResponseValue::Cancelled(result),
                }),
            }
        }
        self.responses.pop_front()
    }
    fn submit_place(
        &mut self,
        token: u64,
        order: DesiredLimitOrder,
    ) -> Result<(), TradingVenueError> {
        enqueue_place(&mut self.state, &self.engine, order, Reply::Command(token))
    }

    fn submit_cancel(
        &mut self,
        token: u64,
        pair: String,
        orders: Vec<OrderId>,
    ) -> Result<(), TradingVenueError> {
        enqueue_cancel(
            &mut self.state,
            &self.engine,
            pair,
            orders,
            Reply::Command(token),
        )
    }
    fn submit_query(
        &mut self,
        token: u64,
        pair: &str,
        endpoint: query::Endpoint,
    ) -> Result<(), TradingVenueError> {
        let id = enqueue_query(&mut self.state, &self.engine, pair, endpoint, |id| {
            Reply::Command(id)
        })?;
        self.query_tokens.insert(id, (token, endpoint));
        Ok(())
    }
}
