use bitbankutil_rs::{
    bitbank_bot::*,
    book_units::literal,
    event_strategy::*,
    market_event::*,
    order_domain::*,
    paper_execution::*,
    replay::*,
    simulation::SimulationConfig,
    trading_venue::{AccountSnapshot, PlacementRequest, TradingVenue},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, atomic::AtomicBool};

type Trace = Arc<Mutex<Vec<Value>>>;
fn log(trace: &Trace, kind: &str, time: i64, value: Value) {
    trace.lock().unwrap().push(json!([kind, time, value]));
}
fn sell() -> DesiredLimitOrder {
    DesiredLimitOrder::limit(
        "btc_jpy".into(),
        OrderSide::Sell,
        literal("1"),
        literal("101"),
    )
}
fn engine(delta: i64) -> PaperExecutionEngine {
    let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
    config.time_model = PaperTimeModel::FixedOffset {
        exchange_minus_local_ms: delta,
    };
    PaperExecutionEngine::new(
        config,
        PaperClock::Replay { now_ms: 0 },
        [("btc", 3), ("jpy", 10000)]
            .into_iter()
            .map(|(asset, n)| BalanceSnapshot {
                asset: asset.into(),
                free_amount: n.into(),
                locked_amount: 0.into(),
                onhand_amount: n.into(),
            })
            .collect(),
        None,
    )
    .unwrap()
}
fn inputs(delta: i64, disconnect: bool) -> PreparedReplay {
    let mut records = vec![
        (
            90,
            "bitbank",
            CapturePayload::Connected {
                endpoint: "fixture".into(),
                subscriptions: vec!["depth".into(), "trades".into()],
            },
        ),
        (
            100,
            "bitbank",
            CapturePayload::Message {
                message: json!(["message",{"room_name":"depth_whole_btc_jpy","message":{"data":{"asks":[["110","10"]],"bids":[["90","10"]],"asks_over":"0","bids_under":"0","asks_under":"0","bids_over":"0","ask_market":"0","bid_market":"0","timestamp":100+delta,"sequenceId":"1"}}}]),
            },
        ),
        (
            105,
            "bybit",
            CapturePayload::Connected {
                endpoint: "fixture".into(),
                subscriptions: vec!["bybit".into()],
            },
        ),
        (
            110,
            "bybit",
            CapturePayload::Message {
                message: json!("start"),
            },
        ),
        (
            155,
            "bitbank",
            CapturePayload::Message {
                message: json!(["message",{"room_name":"transactions_btc_jpy","message":{"data":{"transactions":[{"executed_at":150+delta,"transaction_id":1,"side":"buy","amount":"0.5","price":"101"}]}}}]),
            },
        ),
        (
            170,
            "bitbank",
            CapturePayload::Message {
                message: json!(["message",{"room_name":"depth_diff_btc_jpy","message":{"data":{"a":[["110","9"]],"b":[],"t":165+delta,"s":"2"}}}]),
            },
        ),
    ];
    if disconnect {
        records.push((
            210,
            "bitbank",
            CapturePayload::Disconnected {
                reason: "capture_gap".into(),
            },
        ));
    }
    if disconnect {
        records.push((
            220,
            "bitbank",
            CapturePayload::Connected {
                endpoint: "fixture".into(),
                subscriptions: vec!["depth".into(), "trades".into()],
            },
        ));
    }
    // Each source must cover the full evaluation horizon.
    for exchange in ["bitbank", "bybit"] {
        records.push((
            1000,
            exchange,
            CapturePayload::Message {
                message: json!(["heartbeat"]),
            },
        ));
    }
    let mut sequences = [0_u64; 2];
    PreparedReplay::new(records.into_iter().map(|(time, exchange, payload)| {
        let source = if exchange == "bybit" { 1 } else { 0 };
        sequences[source] += 1;
        Ok((
            if exchange == "bybit" { 1 } else { 0 },
            CaptureRecord {
                format_version: 1,
                exchange: exchange.into(),
                stream: if exchange == "bitbank" {
                    "public"
                } else {
                    "linear"
                }
                .into(),
                session_id: exchange.into(),
                connection_id: if disconnect && exchange == "bitbank" && time >= 220 {
                    "reconnected".into()
                } else {
                    exchange.into()
                },
                sequence: sequences[source],
                received_at_micros: time * 1000,
                payload,
            },
        ))
    }))
    .unwrap()
}
fn market(trace: &Trace, event: &MarketEvent, now: i64) {
    log(
        trace,
        "market",
        now,
        json!(match event {
            MarketEvent::DepthUpdated { .. } => "depth",
            MarketEvent::Transactions { .. } => "trade",
            _ => "other",
        }),
    );
}
struct AsyncStrategy {
    trace: Trace,
    bad_pair: bool,
}
impl BotStrategy<PaperTradingVenue> for AsyncStrategy {
    type CustomEvent = ();
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<()>,
        ctx: &BotContext<(), PaperTradingVenue>,
    ) -> Result<(), String> {
        match event {
            BotEvent::Market(e) => market(&self.trace, &e, ctx.now_ms()),
            BotEvent::Custom(()) => {
                let pair = if self.bad_pair { "bad_jpy" } else { "btc_jpy" };
                let account = ctx.venue().account_snapshot(pair).await;
                log(
                    &self.trace,
                    "account",
                    ctx.now_ms(),
                    result_account(&account),
                );
                if self.bad_pair {
                    return Ok(());
                }
                let placed = ctx
                    .venue()
                    .place_order(PlacementRequest::from(sell()))
                    .await;
                let placed = match placed {
                    Ok(p) => p,
                    Err(e) => {
                        log(
                            &self.trace,
                            "place_error",
                            ctx.now_ms(),
                            json!(format!("{e:?}")),
                        );
                        return Ok(());
                    }
                };
                log(&self.trace, "place", ctx.now_ms(), json!(placed));
                let account = ctx.venue().account_snapshot(pair).await;
                log(
                    &self.trace,
                    "account",
                    ctx.now_ms(),
                    result_account(&account),
                );
                let cancel = ctx
                    .venue()
                    .cancel_orders(pair, vec![placed.order_id.unwrap()])
                    .await;
                log(
                    &self.trace,
                    "cancel",
                    ctx.now_ms(),
                    json!(format!("{cancel:?}")),
                );
                let account = ctx.venue().account_snapshot(pair).await;
                log(
                    &self.trace,
                    "account",
                    ctx.now_ms(),
                    result_account(&account),
                );
            }
        }
        Ok(())
    }
}
fn result_account(
    account: &Result<AccountSnapshot, bitbankutil_rs::trading_venue::TradingVenueError>,
) -> Value {
    match account {
        Ok(a) => json!(a),
        Err(e) => json!(format!("{e:?}")),
    }
}
struct Commands {
    trace: Trace,
    bad_pair: bool,
    stage: u8,
    order: Option<OrderId>,
}
impl EventStrategy for Commands {
    type CustomEvent = ();
    fn on_event(
        &mut self,
        event: StrategyEvent<()>,
        now: i64,
        out: &mut Vec<Request>,
    ) -> Result<bool, String> {
        let pair = if self.bad_pair { "bad_jpy" } else { "btc_jpy" };
        let command = match event {
            StrategyEvent::Input(BotEvent::Market(e)) => {
                market(&self.trace, &e, now);
                return Ok(false);
            }
            StrategyEvent::Input(BotEvent::Custom(())) => {
                self.stage = 1;
                Command::Account { pair: pair.into() }
            }
            StrategyEvent::Response(r) => {
                match r.value {
                    ResponseValue::Account(a) => {
                        log(&self.trace, "account", now, result_account(&a))
                    }
                    ResponseValue::Placed(p) => {
                        let p = match p {
                            Ok(p) => p,
                            Err(e) => {
                                log(&self.trace, "place_error", now, json!(format!("{e:?}")));
                                return Ok(false);
                            }
                        };
                        self.order = p.order_id;
                        log(&self.trace, "place", now, json!(p));
                    }
                    ResponseValue::Cancelled(c) => {
                        log(&self.trace, "cancel", now, json!(format!("{c:?}")))
                    }
                }
                if self.bad_pair {
                    return Ok(false);
                }
                self.stage += 1;
                match self.stage {
                    2 => Command::Place(sell()),
                    3 | 5 => Command::Account { pair: pair.into() },
                    4 => Command::Cancel {
                        pair: pair.into(),
                        orders: vec![self.order.unwrap()],
                    },
                    _ => return Ok(false),
                }
            }
        };
        out.push(Request {
            id: self.stage as u64,
            command,
        });
        Ok(true)
    }
}

#[tokio::test]
async fn commands_match_async_delivery_query_samples_orders_fills_and_reports() {
    for delta in [0, 25] {
        for delayed in [false, true] {
            for bad_pair in [false, true] {
                for disconnect in [false, true] {
                    let prepared = inputs(delta, disconnect);
                    let config = SimulationConfig { lookahead_ms: 100 };
                    let latency = if delayed {
                        PaperLatencyConfig {
                            placement: PaperOperationLatency {
                                entry_ms: 7,
                                response_ms: 11,
                            },
                            cancellation: PaperOperationLatency {
                                entry_ms: 13,
                                response_ms: 17,
                            },
                            orders_query: PaperOperationLatency {
                                entry_ms: 3,
                                response_ms: 5,
                            },
                            balances_query: PaperOperationLatency {
                                entry_ms: 9,
                                response_ms: 19,
                            },
                            notification_ms: 23,
                        }
                    } else {
                        Default::default()
                    };
                    let compiled = prepared
                        .compile(
                            config,
                            PaperTimeModel::FixedOffset {
                                exchange_minus_local_ms: delta,
                            },
                            |r| {
                                Ok(if r.payload.message_is_start() {
                                    Some(())
                                } else {
                                    None
                                })
                            },
                        )
                        .unwrap();
                    for end_time_ms in [None, Some(140)] {
                        let expected = Trace::default();
                        let actual = Trace::default();
                        let venue = PaperTradingVenue::with_latency(engine(delta), latency)
                            .unwrap()
                            .with_simulation_config(config)
                            .unwrap();
                        let mut a = BotRunner::new(
                            AsyncStrategy {
                                trace: expected.clone(),
                                bad_pair,
                            },
                            venue,
                        );
                        let mut b = EventRunner::new(
                            Commands {
                                trace: actual.clone(),
                                bad_pair,
                                stage: 0,
                                order: None,
                            },
                            OwnedPaperExecution::new(engine(delta), latency, config).unwrap(),
                        );
                        let options = ReplayOptions {
                            recovery: true,
                            end_time_ms,
                        };
                        let ar = compiled.run(&mut a, options, &AtomicBool::new(false)).await;
                        let br = compiled
                            .run_commands(&mut b, options, &AtomicBool::new(false))
                            .await;
                        assert_eq!(
                            *actual.lock().unwrap(),
                            *expected.lock().unwrap(),
                            "delta={delta} delayed={delayed} bad_pair={bad_pair} gap={disconnect} end={end_time_ms:?}"
                        );
                        assert_eq!(
                            serde_json::to_value(br).unwrap(),
                            serde_json::to_value(ar).unwrap()
                        );
                        let diagnostics = a.paper_diagnostics().unwrap();
                        assert_eq!(b.execution().balances(), diagnostics.balances());
                        assert_eq!(b.execution().open_orders(), diagnostics.open_orders());
                    }
                }
            }
        }
    }
}
trait Start {
    fn message_is_start(&self) -> bool;
}
impl Start for CapturePayload {
    fn message_is_start(&self) -> bool {
        matches!(self,CapturePayload::Message{message} if message=="start")
    }
}
