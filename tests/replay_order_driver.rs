use bitbankutil_rs::book_units::literal;
mod support;
use bitbankutil_rs::{
    bitbank_bot::{BotContext, BotEvent, BotRunner, BotStrategy},
    market_event::MarketEvent,
    order_domain::{DesiredLimitOrder, OrderId, OrderSide},
    replay::{CapturePayload, CaptureRecord, ReplayOptions, run_replay_with_options},
    trading_venue::{
        AccountSnapshot, PlacedOrder, PlacementRequest, TradingVenue, TradingVenueError,
        TradingVenueFuture,
    },
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, atomic::AtomicBool},
    task::{Poll, Waker},
};

#[derive(Default)]
struct State {
    now: i64,
    next_id: u64,
    requests: BTreeMap<u64, (i64, bool, Option<Waker>)>,
    driver: Option<Waker>,
    markets: usize,
}
#[derive(Clone, Default)]
struct Venue(Arc<Mutex<State>>);
impl TradingVenue for Venue {
    fn advance_replay_time(&self, time: i64) -> Result<(), TradingVenueError> {
        self.0.lock().unwrap().now = time;
        Ok(())
    }
    fn current_time_ms(&self) -> Option<i64> {
        Some(self.0.lock().unwrap().now)
    }
    fn set_replay_waker(&self, waker: Option<Waker>) {
        self.0.lock().unwrap().driver = waker;
    }
    fn next_internal_time(&self) -> Option<i64> {
        self.0
            .lock()
            .unwrap()
            .requests
            .values()
            .filter(|r| !r.1)
            .map(|r| r.0)
            .min()
    }
    fn process_internal_events(&self, time: i64) -> Result<(), TradingVenueError> {
        let mut s = self.0.lock().unwrap();
        for r in s.requests.values_mut() {
            if r.0 <= time {
                r.1 = true;
                if let Some(w) = r.2.take() {
                    w.wake();
                }
            }
        }
        Ok(())
    }
    fn on_market_event(&self, _: &MarketEvent) -> Result<(), TradingVenueError> {
        self.0.lock().unwrap().markets += 1;
        Ok(())
    }
    fn account_snapshot<'a>(&'a self, _: &'a str) -> TradingVenueFuture<'a, AccountSnapshot> {
        Box::pin(async {
            Ok(AccountSnapshot {
                query: None,
                open_orders: vec![],
                balances: vec![],
            })
        })
    }
    fn place_order(&self, _: PlacementRequest) -> TradingVenueFuture<'_, PlacedOrder> {
        let mut id = None;
        Box::pin(std::future::poll_fn(move |cx| {
            let mut s = self.0.lock().unwrap();
            let key = *id.get_or_insert_with(|| {
                s.next_id += 1;
                let key = s.next_id;
                let deadline = s.now + 100;
                s.requests.insert(key, (deadline, false, None));
                if let Some(w) = &s.driver {
                    w.wake_by_ref();
                }
                key
            });
            let r = s.requests.get_mut(&key).unwrap();
            if r.1 {
                Poll::Ready(Ok(PlacedOrder {
                    order_id: Some(OrderId(key)),
                }))
            } else {
                r.2 = Some(cx.waker().clone());
                Poll::Pending
            }
        }))
    }
    fn cancel_orders<'a>(&'a self, _: &'a str, _: Vec<OrderId>) -> TradingVenueFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
type Seen = Arc<Mutex<Vec<(u8, i64)>>>;
struct Strategy {
    seen: Seen,
    concurrent: bool,
}
fn order() -> PlacementRequest {
    DesiredLimitOrder::limit("btc_jpy".into(), OrderSide::Buy, 1.into(), 100.into()).into()
}
impl BotStrategy<Venue> for Strategy {
    type CustomEvent = u8;
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<u8>,
        ctx: &BotContext<u8, Venue>,
    ) -> Result<(), String> {
        let n = match event {
            BotEvent::Custom(n) => n,
            BotEvent::Market(_) => 9,
        };
        if n == 0 {
            if self.concurrent {
                let mut tasks = tokio::task::JoinSet::new();
                for _ in 0..2 {
                    let v = ctx.venue().clone();
                    tasks.spawn(async move { v.place_order(order()).await });
                }
                while let Some(result) = tasks.join_next().await {
                    result.unwrap().unwrap();
                }
            } else {
                ctx.venue().place_order(order()).await.unwrap();
            }
        }
        self.seen.lock().unwrap().push((n, ctx.now_ms()));
        Ok(())
    }
}
#[tokio::test]
async fn waiting_order_registers_market_once_and_defers_strategy_delivery() {
    let venue = Venue::default();
    let seen = Seen::default();
    let mut runner = BotRunner::new(
        Strategy {
            seen: seen.clone(),
            concurrent: false,
        },
        venue.clone(),
    );
    runner
        .submit_replay(1000, None, BotEvent::Custom(0), false)
        .await
        .unwrap();
    assert!(runner.handler_pending());
    runner
        .submit_replay(
            1050,
            None,
            BotEvent::Market(MarketEvent::Transactions {
                pair: "btc_jpy".into(),
                transactions: vec![],
            }),
            false,
        )
        .await
        .unwrap();
    assert_eq!(venue.0.lock().unwrap().markets, 1);
    assert!(seen.lock().unwrap().is_empty());
    runner.process_next_internal(false).await.unwrap();
    assert_eq!(*seen.lock().unwrap(), [(0, 1100), (9, 1100)]);
    assert_eq!(venue.0.lock().unwrap().markets, 1);
}
fn capture(
    seq: u64,
    time: u64,
    payload: CapturePayload,
) -> std::io::Result<(usize, CaptureRecord)> {
    Ok((
        0,
        CaptureRecord {
            format_version: 1,
            exchange: "bybit".into(),
            stream: "linear".into(),
            session_id: "s".into(),
            connection_id: "c".into(),
            sequence: seq,
            received_at_micros: time * 1000,
            payload,
        },
    ))
}
#[tokio::test]
async fn joinset_orders_do_not_skip_intervening_input_or_sleep_in_realtime() {
    let seen = Seen::default();
    let venue = Venue::default();
    let mut runner = BotRunner::new(
        Strategy {
            seen: seen.clone(),
            concurrent: true,
        },
        venue.clone(),
    );
    let input = [
        capture(
            1,
            900,
            CapturePayload::Connected {
                endpoint: "fake".into(),
                subscriptions: vec!["x".into()],
            },
        ),
        capture(
            2,
            1000,
            CapturePayload::Message {
                message: serde_json::json!(0),
            },
        ),
        capture(
            3,
            1050,
            CapturePayload::Message {
                message: serde_json::json!(1),
            },
        ),
        capture(
            4,
            1200,
            CapturePayload::Message {
                message: serde_json::json!(2),
            },
        ),
    ];
    support::configure(runner.venue(), 0);
    let report = run_replay_with_options(
        &mut runner,
        input,
        ReplayOptions::default(),
        &AtomicBool::new(false),
        |r| match &r.payload {
            CapturePayload::Message { message } => Ok(Some(message.as_u64().unwrap() as u8)),
            _ => Ok(None),
        },
    )
    .await;
    assert!(report.error.is_none(), "{:?}", report.error);
    assert_eq!(report.records, 4);
    assert_eq!(*seen.lock().unwrap(), [(0, 1100), (1, 1100), (2, 1200)]);
    assert_eq!(venue.0.lock().unwrap().next_id, 2);
}
#[tokio::test]
async fn evaluation_boundary_reports_unfinished_handler_without_finishing_order() {
    let seen = Seen::default();
    let venue = Venue::default();
    let mut runner = BotRunner::new(
        Strategy {
            seen: seen.clone(),
            concurrent: false,
        },
        venue.clone(),
    );
    let input = [
        capture(
            1,
            900,
            CapturePayload::Connected {
                endpoint: "fake".into(),
                subscriptions: vec!["x".into()],
            },
        ),
        capture(
            2,
            1000,
            CapturePayload::Message {
                message: serde_json::json!(0),
            },
        ),
    ];
    support::configure(runner.venue(), 0);
    let report = run_replay_with_options(
        &mut runner,
        input,
        ReplayOptions {
            recovery: false,
            end_time_ms: Some(1050),
        },
        &AtomicBool::new(false),
        |r| match &r.payload {
            CapturePayload::Message { .. } => Ok(Some(0)),
            _ => Ok(None),
        },
    )
    .await;
    assert!(report.error.is_none());
    assert!(report.pending_handler);
    assert!(report.incomplete);
    assert_eq!(report.final_time_ms, Some(1050));
    assert!(!venue.0.lock().unwrap().requests[&1].1);
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn same_ms_market_inputs_register_before_due_order_activation() {
    use bitbankutil_rs::{
        execution_recording::{EventSink, ExecutionRecord, ExecutionRecorder},
        order_domain::BalanceSnapshot,
        paper_execution::{
            PaperClock, PaperExecutionConfig, PaperExecutionEngine, PaperLatencyConfig,
            PaperOperationLatency, PaperTradingVenue,
        },
    };
    struct Sink(Arc<Mutex<Vec<ExecutionRecord>>>);
    impl EventSink for Sink {
        fn append(&mut self, r: &ExecutionRecord) -> std::io::Result<()> {
            self.0.lock().unwrap().push(r.clone());
            Ok(())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    struct Orders;
    impl BotStrategy<PaperTradingVenue> for Orders {
        type CustomEvent = ();
        type Error = String;
        async fn handle_event(
            &mut self,
            e: BotEvent<()>,
            ctx: &BotContext<(), PaperTradingVenue>,
        ) -> Result<(), String> {
            if matches!(e, BotEvent::Custom(())) {
                ctx.venue().place_order(order()).await.unwrap();
            }
            Ok(())
        }
    }
    let records = Arc::new(Mutex::new(Vec::new()));
    let recorder = ExecutionRecorder::new("ties", serde_json::json!({}), Sink(records.clone()));
    let engine = PaperExecutionEngine::new(
        PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap(),
        PaperClock::Replay { now_ms: 0 },
        [("btc", 10), ("jpy", 1000)]
            .into_iter()
            .map(|(asset, n)| BalanceSnapshot {
                asset: asset.into(),
                free_amount: n.into(),
                locked_amount: 0.into(),
                onhand_amount: n.into(),
            })
            .collect(),
        Some(recorder),
    )
    .map(|engine| seeded_engine(engine, 0))
    .unwrap();
    let venue = PaperTradingVenue::with_latency(
        engine,
        PaperLatencyConfig {
            placement: PaperOperationLatency {
                entry_ms: 100,
                response_ms: 0,
            },
            ..Default::default()
        },
    )
    .unwrap();
    let mut runner = BotRunner::new(Orders, venue);
    let mut market_connected = capture(
        1,
        900,
        CapturePayload::Connected {
            endpoint: "fake".into(),
            subscriptions: vec!["transactions_btc_jpy".into()],
        },
    )
    .unwrap();
    market_connected.0 = 1;
    market_connected.1.exchange = "bitbank".into();
    market_connected.1.stream = "public".into();
    market_connected.1.session_id = "market".into();
    let mut markets = Vec::new();
    for (seq, id) in [(2, 10), (3, 11)] {
        let mut record=capture(seq,1100,CapturePayload::Message{message:serde_json::json!(["message",{"room_name":"transactions_btc_jpy","message":{"data":{"transactions":[{"transaction_id":id,"side":"sell","price":"100","amount":"0.1","executed_at":1050}]}}}])}).unwrap();
        record.0 = 1;
        record.1.exchange = "bitbank".into();
        record.1.stream = "public".into();
        record.1.session_id = "market".into();
        markets.push(Ok(record));
    }
    let mut input = vec![
        capture(
            1,
            900,
            CapturePayload::Connected {
                endpoint: "fake".into(),
                subscriptions: vec!["x".into()],
            },
        ),
        Ok(market_connected),
        capture(
            2,
            1000,
            CapturePayload::Message {
                message: serde_json::json!(0),
            },
        ),
    ];
    input.extend(markets);
    let mut heartbeat = capture(
        3,
        1150,
        CapturePayload::Message {
            message: serde_json::Value::Null,
        },
    )
    .unwrap();
    input.push(Ok(heartbeat.clone()));
    heartbeat.0 = 1;
    heartbeat.1.sequence = 4;
    heartbeat.1.exchange = "bitbank".into();
    heartbeat.1.stream = "public".into();
    heartbeat.1.session_id = "market".into();
    heartbeat.1.payload = CapturePayload::Message {
        message: serde_json::json!(["heartbeat"]),
    };
    input.push(Ok(heartbeat));
    support::configure(runner.venue(), 50);
    let report = run_replay_with_options(
        &mut runner,
        input,
        ReplayOptions::default(),
        &AtomicBool::new(false),
        |_| Ok(Some(())),
    )
    .await;
    assert!(report.error.is_none(), "{:?}", report.error);
    let records = records.lock().unwrap();
    let second_market = records
        .iter()
        .position(|r| {
            r.kind == "simulation_market"
                && r.input_id
                    .as_ref()
                    .is_some_and(|i| i.session_id == "market" && i.sequence == 3)
        })
        .unwrap();
    let activation = records
        .iter()
        .position(|r| r.kind == "paper_operation_effective")
        .unwrap();
    assert!(
        second_market < activation,
        "all same-ms market inputs must register before the due order activation"
    );
}

// 合成の時刻/資金fixture用。注文をspread内に置き、先行数量を0にする。
fn seeded_engine(
    mut engine: bitbankutil_rs::paper_execution::PaperExecutionEngine,
    exchange_ms: i64,
) -> bitbankutil_rs::paper_execution::PaperExecutionEngine {
    engine.apply_market_event(&bitbankutil_rs::market_event::MarketEvent::DepthUpdated {
        pair: "btc_jpy".into(),
        depth: bitbankutil_rs::market_event::MarketDepthSnapshot::new(
            std::collections::BTreeMap::from([(1_000_000.into(), literal("1.0"))]),
            std::collections::BTreeMap::from([(1.into(), literal("1.0"))]),
            exchange_ms,
        ),
    });
    engine
}
