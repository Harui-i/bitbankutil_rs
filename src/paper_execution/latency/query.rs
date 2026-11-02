//! 注文一覧・残高の採取値を凍結し、別々の応答として返す。
use super::*;
use crate::trading_venue::{AccountObservation, AccountQueryMetadata};

#[derive(Debug, Clone, Copy)]
pub(super) enum Endpoint {
    Orders,
    Balances,
}
impl Endpoint {
    fn name(self) -> &'static str {
        match self {
            Self::Orders => "orders_query",
            Self::Balances => "balances_query",
        }
    }
}

#[derive(Debug)]
enum Values {
    Orders(Vec<OpenOrder>),
    Balances(Vec<BalanceSnapshot>),
}

#[derive(Debug)]
pub(super) struct Part {
    values: Values,
    observation: AccountObservation,
}

#[derive(Debug)]
pub(super) enum QueryEvent {
    Sample {
        id: u64,
        pair: String,
        endpoint: Endpoint,
        requested: i64,
        response_delay: i64,
        reply: oneshot::Sender<Result<Part, TradingVenueError>>,
    },
    Response {
        id: u64,
        result: Result<Part, TradingVenueError>,
        reply: oneshot::Sender<Result<Part, TradingVenueError>>,
    },
}

impl PaperTradingVenue {
    fn submit_query(
        &self,
        pair: &str,
        endpoint: Endpoint,
    ) -> Result<oneshot::Receiver<Result<Part, TradingVenueError>>, TradingVenueError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| TradingVenueError::StatePoisoned)?;
        let latency = match endpoint {
            Endpoint::Orders => s.latency.orders_query,
            Endpoint::Balances => s.latency.balances_query,
        };
        let (now, sample_at, response_at) = self.request_times(latency)?;
        let id = s
            .operation
            .checked_add(1)
            .ok_or(TradingVenueError::Paper(PaperExecutionError::InvalidClock))?;
        let (tx, rx) = oneshot::channel();
        s.operation = id;
        let info = serde_json::json!({"id":id,"kind":endpoint.name(),"pair":pair,
            "requested_at_ms":now,"sample_at_ms":sample_at,"response_at_ms":response_at,
            "phase":"awaiting_sample"});
        record(
            &self.engine.lock().unwrap(),
            "paper_query_requested",
            info.clone(),
        );
        s.operations.insert(id, info);
        let enqueued = s.enqueue(
            sample_at,
            Internal::Query(QueryEvent::Sample {
                id,
                pair: pair.into(),
                endpoint,
                requested: now,
                response_delay: latency.response_ms,
                reply: tx,
            }),
        );
        if let Err(error) = enqueued {
            s.operations.remove(&id);
            return Err(error);
        }
        drop(s);
        self.drive_now()?;
        Ok(rx)
    }

    async fn query_part(&self, pair: &str, endpoint: Endpoint) -> Result<Part, TradingVenueError> {
        self.submit_query(pair, endpoint)?
            .await
            .map_err(|_| TradingVenueError::Unavailable)?
    }

    pub(super) async fn query_account(
        &self,
        pair: &str,
    ) -> Result<AccountSnapshot, TradingVenueError> {
        // 実venueと同じく並行開始。片方の失敗でももう片方の要求を無かったことにしない。
        let (orders, balances) = tokio::join!(
            self.query_part(pair, Endpoint::Orders),
            self.query_part(pair, Endpoint::Balances),
        );
        let orders = orders?;
        let balances = balances?;
        let completed = local_now(
            &*self
                .engine
                .lock()
                .map_err(|_| TradingVenueError::StatePoisoned)?,
        )?;
        let Values::Orders(open_orders) = orders.values else {
            unreachable!()
        };
        let Values::Balances(balances_values) = balances.values else {
            unreachable!()
        };
        let result = AccountSnapshot {
            open_orders,
            balances: balances_values,
            query: Some(AccountQueryMetadata {
                orders: orders.observation,
                balances: balances.observation,
                completed_local_ms: completed,
            }),
        };
        record(
            &self.engine.lock().unwrap(),
            "paper_query_completed",
            serde_json::json!({"local_ms":completed,"snapshot":result}),
        );
        Ok(result)
    }
}

pub(super) fn process(
    state: &mut State,
    engine: &PaperExecutionEngine,
    event: QueryEvent,
    executed: i64,
) -> Result<(), TradingVenueError> {
    match event {
        QueryEvent::Sample {
            id,
            pair,
            endpoint,
            requested,
            response_delay,
            reply,
        } => {
            let result = (|| {
                if pair != engine.config.pair {
                    Err(TradingVenueError::Paper(
                        PaperExecutionError::PairMismatch {
                            expected: engine.config.pair.clone(),
                            actual: pair,
                        },
                    ))
                } else {
                    let status = engine.settlement_status();
                    let exchange = engine
                        .config
                        .time_model
                        .local_to_exchange_ms(executed)
                        .map_err(TradingVenueError::Paper)?;
                    Ok(Part {
                        values: match endpoint {
                            Endpoint::Orders => Values::Orders(engine.open_orders()),
                            Endpoint::Balances => Values::Balances(engine.balances()),
                        },
                        observation: AccountObservation {
                            request_id: id,
                            requested_local_ms: requested,
                            sampled_local_ms: executed,
                            sampled_exchange_ms: exchange,
                            received_local_ms: None,
                            watermark_ms: status.watermark_ms,
                            unsettled: status.unsettled,
                            issues: status.issues,
                        },
                    })
                }
            })();
            record(
                engine,
                "paper_query_sampled",
                serde_json::json!({"id":id,
                "local_ms":executed,"success":result.is_ok(),
                "observation":result.as_ref().ok().map(|p|&p.observation)}),
            );
            if let Some(info) = state.operations.get_mut(&id) {
                info["phase"] = serde_json::json!("awaiting_response");
                info["sampled_at_ms"] = serde_json::json!(executed);
            }
            match add(executed, response_delay) {
                Ok(at) => {
                    if let Err(error) = state.enqueue(
                        at,
                        Internal::Query(QueryEvent::Response { id, result, reply }),
                    ) {
                        state.operations.remove(&id);
                        return Err(error);
                    }
                }
                Err(error) => {
                    // 遅れた実時計採取の後に応答期限が表現不能になった場合も待機を残さない。
                    process(
                        state,
                        engine,
                        QueryEvent::Response {
                            id,
                            result: Err(error),
                            reply,
                        },
                        executed,
                    )?;
                }
            }
        }
        QueryEvent::Response {
            id,
            mut result,
            reply,
        } => {
            if let Ok(part) = &mut result {
                part.observation.received_local_ms = Some(executed);
                if part.observation.unsettled {
                    state.uncertain_query_observations =
                        state.uncertain_query_observations.saturating_add(1);
                }
            }
            record(
                engine,
                "paper_query_response",
                serde_json::json!({"id":id,
                "local_ms":executed,"success":result.is_ok(),
                "observation":result.as_ref().ok().map(|p|&p.observation)}),
            );
            state.operations.remove(&id);
            let _ = reply.send(result);
        }
    }
    Ok(())
}
