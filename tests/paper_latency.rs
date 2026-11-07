use bitbankutil_rs::book_units::literal;
mod support;
use bitbankutil_rs::{
    bitbank_bot::*,
    execution_recording::*,
    market_event::*,
    order_domain::*,
    order_manager::place_wanna_orders_concurrent,
    paper_execution::*,
    replay::*,
    trading_venue::{AccountSnapshot, TradingVenue},
};
use std::sync::{Arc, Mutex, atomic::AtomicBool};
use support::TestDiagnostics;

fn engine(
    clock: PaperClock,
    delta: i64,
    recorder: Option<ExecutionRecorder>,
) -> PaperExecutionEngine {
    let initial_local_ms = match &clock {
        PaperClock::Replay { now_ms } => *now_ms,
        PaperClock::Realtime => bitbankutil_rs::execution_recording::timestamp_ms() as i64,
    };
    let exchange_ms = initial_local_ms.checked_add(delta).unwrap();
    let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
    config.time_model = PaperTimeModel::FixedOffset {
        exchange_minus_local_ms: delta,
    };
    config.fee_schedule = PaperFeeSchedule::new(literal("0"), literal("0"));
    PaperExecutionEngine::new(
        config,
        clock,
        [("btc", 3), ("jpy", 0)]
            .into_iter()
            .map(|(asset, n)| BalanceSnapshot {
                asset: asset.into(),
                free_amount: n.into(),
                locked_amount: 0.into(),
                onhand_amount: n.into(),
            })
            .collect(),
        recorder,
    )
    .map(|engine| seeded_engine(engine, exchange_ms))
    .unwrap()
}
fn sell(price: i32) -> DesiredLimitOrder {
    DesiredLimitOrder::limit("btc_jpy".into(), OrderSide::Sell, 1.into(), price.into())
}
fn latency() -> PaperLatencyConfig {
    PaperLatencyConfig {
        placement: PaperOperationLatency {
            entry_ms: 50,
            response_ms: 50,
        },
        cancellation: PaperOperationLatency {
            entry_ms: 150,
            response_ms: 50,
        },
        notification_ms: 20,
        ..Default::default()
    }
}
fn trades() -> MarketEvent {
    MarketEvent::Transactions {
        pair: "btc_jpy".into(),
        transactions: vec![
            MarketTrade {
                transaction_id: 1,
                executed_at: 1060,
                side: OrderSide::Buy,
                price: 101.into(),
                amount: literal("1.5"),
            },
            MarketTrade {
                transaction_id: 2,
                executed_at: 1160,
                side: OrderSide::Buy,
                price: 101.into(),
                amount: literal("0.25"),
            },
        ],
    }
}
fn balance(account: &AccountSnapshot, asset: &str) -> BalanceSnapshot {
    account
        .balances
        .iter()
        .find(|b| b.asset == asset)
        .unwrap()
        .clone()
}
struct Strategy(Arc<Mutex<Vec<i64>>>);
impl BotStrategy<PaperTradingVenue> for Strategy {
    type CustomEvent = ();
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<()>,
        ctx: &BotContext<(), PaperTradingVenue>,
    ) -> Result<(), String> {
        if let BotEvent::Custom(()) = event {
            let account = ctx.venue().account_snapshot("btc_jpy").await.unwrap();
            place_wanna_orders_concurrent(
                vec![sell(101)],
                account.open_orders.clone(),
                balance(&account, "btc").free_amount,
                balance(&account, "jpy").free_amount,
                "btc_jpy".into(),
                ctx.venue().clone(),
            )
            .await
            .map_err(|e| e.to_string())?;
            self.0.lock().unwrap().push(ctx.now_ms());
        }
        Ok(())
    }
}
async fn overlap(delta: i64, recording: bool) -> (AccountSnapshot, Vec<PaperEvent>) {
    let recorder = recording.then(|| {
        ExecutionRecorder::new("latency", serde_json::json!({}), JsonlSink(std::io::sink()))
    });
    let mut e = engine(
        PaperClock::Replay {
            now_ms: 900 - delta,
        },
        delta,
        recorder,
    );
    e.place_order(sell(100)).unwrap();
    let venue = PaperTradingVenue::with_latency(e, latency()).unwrap();
    let observed = Arc::new(Mutex::new(vec![]));
    let mut runner = BotRunner::new(Strategy(observed.clone()), venue.clone());
    runner
        .submit_replay(1000 - delta, None, BotEvent::Custom(()), false)
        .await
        .unwrap();
    assert!(runner.handler_pending());
    assert_eq!(venue.engine().lock().unwrap().open_orders().len(), 1);
    while runner
        .next_internal_time()
        .is_some_and(|t| t <= 1050 - delta)
    {
        runner.process_next_internal(false).await.unwrap();
    } // 1050: B有効、応答前。
    assert_eq!(venue.engine().lock().unwrap().open_orders().len(), 2);
    assert_eq!(venue.known_account_snapshot().open_orders.len(), 1);
    while runner
        .next_internal_time()
        .is_some_and(|t| t <= 1200 - delta)
    {
        runner.process_next_internal(false).await.unwrap();
    }
    assert!(!runner.handler_pending());
    assert_eq!(*observed.lock().unwrap(), [1200 - delta]);
    let before = venue.known_account_snapshot();
    assert_eq!(before.open_orders.len(), 1);
    assert_eq!(before.open_orders[0].order_id, OrderId(2));
    assert_eq!(balance(&before, "btc").locked_amount, literal("2"));
    assert_eq!(
        venue
            .paper_settlement_status()
            .unwrap()
            .pending_cancellations,
        1
    );
    runner
        .submit_replay(1250 - delta, None, BotEvent::Market(trades()), false)
        .await
        .unwrap();
    venue.advance_trade_watermark(1200).unwrap();
    // 確定は1250。通知は1270以降で、過去の1200には遡らない。
    assert!(venue.has_pending_operations());
    assert!(venue.paper_settlement_status().unwrap().pending_operations > 0);
    assert_eq!(
        balance(&venue.known_account_snapshot(), "jpy").onhand_amount,
        literal("0")
    );
    runner.process_next_internal(false).await.unwrap();
    assert_eq!(runner.now_ms(), Some(1270 - delta));
    assert!(!venue.has_pending_operations());
    assert_eq!(
        venue.paper_settlement_status().unwrap().pending_operations,
        0
    );
    let after = venue.known_account_snapshot();
    assert_eq!(after.open_orders.len(), 1);
    assert_eq!(after.open_orders[0].remaining_amount, literal("0.25"));
    assert_eq!(balance(&after, "btc").free_amount, literal("1"));
    assert_eq!(balance(&after, "btc").locked_amount, literal("0.25"));
    assert_eq!(balance(&after, "jpy").onhand_amount, literal("175.75"));
    assert_eq!(
        venue
            .paper_settlement_status()
            .unwrap()
            .pending_cancellations,
        0
    );
    let events = venue.engine().lock().unwrap().drain_events();
    (after, events)
}

#[tokio::test]
async fn concurrent_replace_preserves_overlap_late_fills_and_notification_time() {
    let mut expected = None;
    for _ in 0..3 {
        for delta in [-100, 0, 100] {
            for recording in [false, true] {
                let result = overlap(delta, recording).await;
                if let Some(expected) = &expected {
                    assert_eq!(&result, expected);
                } else {
                    expected = Some(result);
                }
            }
        }
    }
    let (_, events) = expected.unwrap();
    let fills = events
        .iter()
        .filter_map(|e| {
            if let PaperEvent::OrderFilled {
                order_id,
                amount,
                trade,
                ..
            } = e
            {
                Some((*order_id, *amount, trade.executed_at))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        fills,
        [
            (OrderId(1), literal("1"), 1060),
            (OrderId(2), literal("0.5"), 1060),
            (OrderId(2), literal("0.25"), 1160)
        ]
    );
}

fn record(seq: u64, time: i64, payload: CapturePayload) -> (usize, CaptureRecord) {
    (
        0,
        CaptureRecord {
            format_version: 1,
            exchange: "bybit".into(),
            stream: "linear".into(),
            session_id: "s".into(),
            connection_id: "c".into(),
            sequence: seq,
            received_at_micros: (time * 1000) as u64,
            payload,
        },
    )
}
#[tokio::test]
async fn end_boundary_retains_sent_operation_without_activating_or_unlocking_it() {
    let mut e = engine(PaperClock::Replay { now_ms: 900 }, 0, None);
    e.place_order(sell(100)).unwrap();
    let venue = PaperTradingVenue::with_latency(e, latency()).unwrap();
    let mut runner = BotRunner::new(Strategy(Default::default()), venue.clone());
    let input = vec![
        Ok(record(
            1,
            999,
            CapturePayload::Connected {
                endpoint: "fixture".into(),
                subscriptions: vec![],
            },
        )),
        Ok(record(
            2,
            1000,
            CapturePayload::Message {
                message: serde_json::json!(1),
            },
        )),
    ];
    support::configure(runner.venue(), 0);
    let report = run_replay_with_options(
        &mut runner,
        input,
        ReplayOptions {
            end_time_ms: Some(1030),
            recovery: false,
        },
        &AtomicBool::new(false),
        |_| Ok(Some(())),
    )
    .await;
    assert!(report.error.is_none(), "{:?}", report.error);
    assert!(report.pending_handler && report.incomplete);
    assert_eq!(report.final_time_ms, Some(1000));
    assert_eq!(venue.pending_operations().len(), 2);
    assert_eq!(venue.engine().lock().unwrap().open_orders().len(), 1);
    assert_eq!(
        venue
            .engine()
            .lock()
            .unwrap()
            .balances()
            .iter()
            .find(|b| b.asset == "btc")
            .unwrap()
            .locked_amount,
        literal("1")
    );
}

#[tokio::test]
async fn realtime_operations_progress_without_market_input_and_share_handles() {
    let venue = PaperTradingVenue::with_latency(
        engine(PaperClock::Realtime, 0, None),
        PaperLatencyConfig {
            placement: PaperOperationLatency {
                entry_ms: 5,
                response_ms: 5,
            },
            cancellation: PaperOperationLatency {
                entry_ms: 5,
                response_ms: 5,
            },
            notification_ms: 0,
            ..Default::default()
        },
    )
    .unwrap();
    let shared = venue.clone();
    let id = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        venue.place_order(sell(100).into()),
    )
    .await
    .unwrap()
    .unwrap()
    .order_id
    .unwrap();
    assert_eq!(shared.known_account_snapshot().open_orders.len(), 1);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        shared.cancel_orders("btc_jpy", vec![id, id]),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(venue.known_account_snapshot().open_orders.is_empty());
    assert_eq!(
        venue
            .paper_settlement_status()
            .unwrap()
            .pending_cancellations,
        1
    );
    assert_eq!(
        balance(&venue.known_account_snapshot(), "btc").locked_amount,
        literal("1")
    );
    let through = bitbankutil_rs::execution_recording::timestamp_ms() as i64;
    shared.advance_trade_watermark(through).unwrap();
    assert_eq!(
        balance(&venue.known_account_snapshot(), "btc").locked_amount,
        literal("0")
    );
}

#[test]
fn invalid_latency_is_rejected() {
    assert!(
        PaperTradingVenue::with_latency(
            engine(PaperClock::Replay { now_ms: 0 }, 0, None),
            PaperLatencyConfig {
                notification_ms: -1,
                ..Default::default()
            }
        )
        .is_err()
    );
}

struct Competing(Arc<Mutex<Vec<bool>>>);
impl BotStrategy<PaperTradingVenue> for Competing {
    type CustomEvent = ();
    type Error = String;
    async fn handle_event(
        &mut self,
        _: BotEvent<()>,
        ctx: &BotContext<(), PaperTradingVenue>,
    ) -> Result<(), String> {
        let mut order = sell(100);
        order.amount = 2.into();
        let (a, b) = tokio::join!(
            ctx.venue().place_order(order.clone().into()),
            ctx.venue().place_order(order.into())
        );
        *self.0.lock().unwrap() = vec![a.is_ok(), b.is_ok()];
        Ok(())
    }
}
#[tokio::test]
async fn concurrent_requests_check_exchange_funds_at_entry_and_report_failure_on_response() {
    let venue = PaperTradingVenue::with_latency(
        engine(PaperClock::Replay { now_ms: 1000 }, 0, None),
        latency(),
    )
    .unwrap();
    let results = Arc::new(Mutex::new(vec![]));
    let mut runner = BotRunner::new(Competing(results.clone()), venue.clone());
    runner
        .submit_replay(1000, None, BotEvent::Custom(()), false)
        .await
        .unwrap();
    runner.process_next_internal(false).await.unwrap();
    let actual = venue.engine().lock().unwrap().balances();
    let base = actual.iter().find(|b| b.asset == "btc").unwrap();
    assert_eq!(
        (base.free_amount, base.locked_amount),
        (literal("1"), literal("2"))
    );
    assert_eq!(
        balance(&venue.known_account_snapshot(), "btc").free_amount,
        literal("3")
    );
    runner.process_next_internal(false).await.unwrap();
    assert_eq!(*results.lock().unwrap(), [true, false]);
    assert_eq!(venue.pending_operations().len(), 0);
    assert_eq!(venue.known_account_snapshot().open_orders.len(), 1);
}

#[tokio::test]
async fn full_fill_before_cancel_and_duplicate_cancel_never_unlock_twice() {
    let venue = PaperTradingVenue::with_latency(
        engine(PaperClock::Replay { now_ms: 1000 }, 0, None),
        PaperLatencyConfig {
            placement: PaperOperationLatency {
                entry_ms: 0,
                response_ms: 0,
            },
            cancellation: PaperOperationLatency {
                entry_ms: 100,
                response_ms: 20,
            },
            notification_ms: 0,
            ..Default::default()
        },
    )
    .unwrap();
    let id = venue
        .place_order(sell(100).into())
        .await
        .unwrap()
        .order_id
        .unwrap();
    let v = venue.clone();
    let mut cancel = Box::pin(v.cancel_orders("btc_jpy", vec![id, id]));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(cancel.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    venue.advance_replay_time(1050).unwrap();
    venue
        .on_market_event(&MarketEvent::Transactions {
            pair: "btc_jpy".into(),
            transactions: vec![MarketTrade {
                transaction_id: 1,
                executed_at: 1040,
                side: OrderSide::Buy,
                price: 101.into(),
                amount: literal("1"),
            }],
        })
        .unwrap();
    venue.advance_trade_watermark(1050).unwrap();
    venue.process_internal_events(1100).unwrap();
    venue.process_internal_events(1120).unwrap();
    cancel.await.unwrap();
    venue.advance_trade_watermark(1120).unwrap();
    let state = venue.known_account_snapshot();
    assert!(state.open_orders.is_empty());
    assert_eq!(
        (
            balance(&state, "btc").free_amount,
            balance(&state, "btc").locked_amount
        ),
        (literal("2"), literal("0"))
    );
    assert_eq!(balance(&state, "jpy").free_amount, literal("100"));
    assert_eq!(
        venue
            .paper_settlement_status()
            .unwrap()
            .pending_cancellations,
        0
    );
}

#[tokio::test]
async fn overflow_rejection_does_not_enqueue_or_lock_funds() {
    let venue = PaperTradingVenue::with_latency(
        engine(PaperClock::Replay { now_ms: 1000 }, 0, None),
        PaperLatencyConfig {
            placement: PaperOperationLatency {
                entry_ms: i64::MAX,
                response_ms: 0,
            },
            ..Default::default()
        },
    )
    .unwrap();
    assert!(venue.place_order(sell(100).into()).await.is_err());
    assert!(venue.pending_operations().is_empty());
    assert_eq!(venue.next_internal_time(), None);
    assert_eq!(
        balance(&venue.known_account_snapshot(), "btc").locked_amount,
        literal("0")
    );
}

#[derive(Clone, Default)]
struct Records(Arc<Mutex<Vec<ExecutionRecord>>>);
impl EventSink for Records {
    fn append(&mut self, record: &ExecutionRecord) -> std::io::Result<()> {
        self.0.lock().unwrap().push(record.clone());
        Ok(())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn activation_boundary_and_pre_ack_fill_preserve_recording_context() {
    let records = Records::default();
    let recorder = ExecutionRecorder::new("boundary", serde_json::json!({}), records.clone());
    let venue = PaperTradingVenue::with_latency(
        engine(PaperClock::Replay { now_ms: 1000 }, 0, Some(recorder)),
        latency(),
    )
    .unwrap();
    let v = venue.clone();
    let context = RecordingContext {
        input_id: Some(InputId {
            session_id: "origin".into(),
            sequence: 1,
        }),
        event_time_ms: Some(1000),
        ..Default::default()
    };
    let mut place = Box::pin(scope(context, async move {
        v.place_order(sell(100).into()).await
    }));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(place.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    venue.process_internal_events(1050).unwrap();
    venue.advance_replay_time(1052).unwrap();
    venue
        .on_market_event(&MarketEvent::Transactions {
            pair: "btc_jpy".into(),
            transactions: [1049, 1050, 1051]
                .into_iter()
                .map(|time| MarketTrade {
                    transaction_id: time,
                    executed_at: time,
                    side: OrderSide::Buy,
                    price: 101.into(),
                    amount: literal("1"),
                })
                .collect(),
        })
        .unwrap();
    venue.advance_trade_watermark(1051).unwrap();
    assert!(venue.engine().lock().unwrap().open_orders().is_empty());
    assert_eq!(
        balance(&venue.known_account_snapshot(), "jpy").free_amount,
        literal("0")
    );
    venue.process_internal_events(1100).unwrap();
    place.await.unwrap();
    let state = venue.known_account_snapshot();
    assert!(state.open_orders.is_empty());
    assert_eq!(balance(&state, "jpy").free_amount, literal("100"));
    let records = records.0.lock().unwrap();
    let effective = records
        .iter()
        .find(|r| r.kind == "paper_operation_effective")
        .unwrap();
    assert_eq!(effective.event_time_ms, Some(1050));
    assert_eq!(effective.input_id.as_ref().unwrap().session_id, "origin");
    assert!(effective.operation_id.is_some());
    assert_eq!(effective.data["effective_exchange_ms"], 1050);
    assert_eq!(
        records
            .iter()
            .filter(|r| r.kind == "paper_event" && r.data["event"].get("OrderFilled").is_some())
            .count(),
        1
    );
    assert!(
        records
            .iter()
            .filter(|r| r.kind == "paper_local_state")
            .all(|r| r.event_time_ms.unwrap() >= 1100)
    );
}

struct Reverse(Arc<Mutex<Vec<(bool, i64)>>>);
impl BotStrategy<PaperTradingVenue> for Reverse {
    type CustomEvent = ();
    type Error = String;
    async fn handle_event(
        &mut self,
        _: BotEvent<()>,
        ctx: &BotContext<(), PaperTradingVenue>,
    ) -> Result<(), String> {
        let seen = self.0.clone();
        let place = async {
            ctx.venue().place_order(sell(101).into()).await.unwrap();
            seen.lock().unwrap().push((true, ctx.now_ms()));
        };
        let cancel = async {
            ctx.venue()
                .cancel_orders("btc_jpy", vec![OrderId(1)])
                .await
                .unwrap();
            seen.lock().unwrap().push((false, ctx.now_ms()));
        };
        tokio::join!(place, cancel);
        Ok(())
    }
}
#[tokio::test]
async fn response_order_does_not_determine_exchange_activation_order() {
    let mut e = engine(PaperClock::Replay { now_ms: 900 }, 0, None);
    e.place_order(sell(100)).unwrap();
    let venue = PaperTradingVenue::with_latency(
        e,
        PaperLatencyConfig {
            placement: PaperOperationLatency {
                entry_ms: 10,
                response_ms: 300,
            },
            cancellation: PaperOperationLatency {
                entry_ms: 150,
                response_ms: 20,
            },
            notification_ms: 0,
            ..Default::default()
        },
    )
    .unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let mut runner = BotRunner::new(Reverse(seen.clone()), venue.clone());
    runner
        .submit_replay(1000, None, BotEvent::Custom(()), false)
        .await
        .unwrap();
    runner.process_next_internal(false).await.unwrap();
    assert_eq!(runner.now_ms(), Some(1010));
    assert_eq!(venue.engine().lock().unwrap().open_orders().len(), 2);
    while runner.next_internal_time().is_some() {
        runner.process_next_internal(false).await.unwrap();
    }
    assert_eq!(*seen.lock().unwrap(), [(false, 1170), (true, 1310)]);
    assert_eq!(
        venue.known_account_snapshot().open_orders[0].order_id,
        OrderId(2)
    );
}

#[tokio::test]
async fn direct_runner_releases_driver_for_later_direct_venue_operations() {
    let venue = PaperTradingVenue::new(engine(PaperClock::Replay { now_ms: 1000 }, 0, None));
    let mut runner = BotRunner::new(Strategy(Default::default()), venue.clone());
    runner
        .process(1000, None, BotEvent::Custom(()))
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        venue.place_order(sell(102).into()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(venue.known_account_snapshot().open_orders.len(), 2);
}

#[tokio::test]
async fn delayed_order_manager_completion_records_current_time_inside_nested_scopes() {
    let records = Records::default();
    let recorder = ExecutionRecorder::new("completion", serde_json::json!({}), records.clone());
    let mut e = engine(PaperClock::Replay { now_ms: 900 }, 0, Some(recorder));
    e.place_order(sell(100)).unwrap();
    let venue = PaperTradingVenue::with_latency(e, latency()).unwrap();
    let mut runner = BotRunner::new(Strategy(Default::default()), venue);
    runner
        .process(
            1000,
            Some(InputId {
                session_id: "request".into(),
                sequence: 7,
            }),
            BotEvent::Custom(()),
        )
        .await
        .unwrap();
    let records = records.0.lock().unwrap();
    let finished = records
        .iter()
        .filter(|r| {
            r.kind == "operation_finished"
                && ["place_order", "cancel_orders"]
                    .contains(&r.data["operation"].as_str().unwrap_or(""))
        })
        .collect::<Vec<_>>();
    assert_eq!(finished.len(), 2);
    for end in finished {
        let start = records
            .iter()
            .find(|r| r.kind == "operation_started" && r.operation_id == end.operation_id)
            .unwrap();
        let response = records
            .iter()
            .find(|r| r.kind == "paper_operation_response" && r.operation_id == end.operation_id)
            .unwrap();
        assert_eq!(start.event_time_ms, Some(1000));
        assert_eq!(end.event_time_ms, response.event_time_ms);
        assert_eq!(end.input_id.as_ref().unwrap().sequence, 7);
        assert_eq!(start.input_id, end.input_id);
        assert!(end.event_time_ms.unwrap() > 1000);
    }
}

struct EmitInvalid {
    delayed: bool,
    scheduled: bool,
    delivered: Arc<Mutex<usize>>,
}
impl BotStrategy<PaperTradingVenue> for EmitInvalid {
    type CustomEvent = u8;
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<u8>,
        ctx: &BotContext<u8, PaperTradingVenue>,
    ) -> Result<(), String> {
        if let BotEvent::Custom(n) = event {
            if self.scheduled && n == 0 {
                ctx.schedule_after(10, 1).unwrap();
                return Ok(());
            }
            if self.delayed {
                ctx.venue().place_order(sell(100).into()).await.unwrap();
            }
            ctx.emit(BotEvent::Market(MarketEvent::Transactions {
                pair: "btc_jpy".into(),
                transactions: vec![MarketTrade {
                    transaction_id: 1,
                    executed_at: 1001,
                    side: OrderSide::Buy,
                    price: 101.into(),
                    amount: literal("0"),
                }],
            }))
            .await
            .unwrap();
        } else {
            *self.delivered.lock().unwrap() += 1;
        }
        Ok(())
    }
}
#[tokio::test]
async fn derived_market_validation_inherits_strict_or_recovery_mode_after_waits_and_schedules() {
    for recovery in [false, true] {
        for (delayed, scheduled) in [(false, false), (true, false), (false, true)] {
            let venue = PaperTradingVenue::with_latency(
                engine(PaperClock::Replay { now_ms: 1000 }, 0, None),
                PaperLatencyConfig {
                    placement: PaperOperationLatency {
                        entry_ms: 10,
                        response_ms: 10,
                    },
                    ..Default::default()
                },
            )
            .unwrap();
            let delivered = Arc::new(Mutex::new(0));
            let mut runner = BotRunner::new(
                EmitInvalid {
                    delayed,
                    scheduled,
                    delivered: delivered.clone(),
                },
                venue.clone(),
            );
            let mut result = if recovery {
                runner
                    .process_recovering(1000, None, BotEvent::Custom(0))
                    .await
            } else {
                runner.process(1000, None, BotEvent::Custom(0)).await
            };
            if scheduled {
                result.unwrap();
                result = runner.process_next_scheduled(recovery).await;
            }
            assert_eq!(
                result.is_ok(),
                recovery,
                "delayed={delayed}, scheduled={scheduled}"
            );
            assert_eq!(*delivered.lock().unwrap(), usize::from(recovery));
            assert!(
                venue
                    .paper_settlement_status()
                    .unwrap()
                    .issues
                    .iter()
                    .any(|s| s.contains("invalid trade 1"))
            );
        }
    }
}

struct QueueLimit;
impl BotStrategy<PaperTradingVenue> for QueueLimit {
    type CustomEvent = u8;
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<u8>,
        ctx: &BotContext<u8, PaperTradingVenue>,
    ) -> Result<(), String> {
        match event {
            BotEvent::Custom(0) => {
                for (time, n) in [(1010, 1), (1020, 2), (1030, 3)] {
                    ctx.schedule_at(time, n).unwrap();
                }
            }
            BotEvent::Custom(1) => {
                ctx.venue().place_order(sell(100).into()).await.unwrap();
            }
            _ => {}
        }
        Ok(())
    }
}
#[tokio::test]
async fn scheduled_deliveries_respect_capacity_and_preserve_unconsumed_reservation() {
    let venue = PaperTradingVenue::with_latency(
        engine(PaperClock::Replay { now_ms: 1000 }, 0, None),
        PaperLatencyConfig {
            placement: PaperOperationLatency {
                entry_ms: 0,
                response_ms: 100,
            },
            ..Default::default()
        },
    )
    .unwrap();
    let mut runner = BotRunner::new(QueueLimit, venue);
    runner.delivery_limit = 1;
    runner
        .process(1000, None, BotEvent::Custom(0))
        .await
        .unwrap();
    runner.process_next_internal(false).await.unwrap(); // 1010のハンドラーが発注応答待ち。
    runner.process_next_internal(false).await.unwrap(); // 同msの注文有効化。
    runner.process_next_internal(false).await.unwrap(); // 1020を保留、キュー満杯。
    assert_eq!(runner.pending_delivery_count(), 1);
    assert!(matches!(
        runner.process_next_internal(false).await,
        Err(BotRunError::DeliveryLimit)
    ));
    assert_eq!(runner.pending_delivery_count(), 1);
    assert_eq!(runner.pending_scheduled().len(), 1);
    assert_eq!(runner.pending_scheduled()[0].execute_at_ms, 1030);
}

#[tokio::test]
async fn delayed_realtime_timer_preserves_response_delay_after_actual_activation() {
    let records = Records::default();
    let recorder = ExecutionRecorder::new("late_timer", serde_json::json!({}), records.clone());
    let venue = PaperTradingVenue::with_latency(
        engine(PaperClock::Realtime, 0, Some(recorder)),
        PaperLatencyConfig {
            placement: PaperOperationLatency {
                entry_ms: 2,
                response_ms: 30,
            },
            cancellation: PaperOperationLatency {
                entry_ms: 2,
                response_ms: 30,
            },
            ..Default::default()
        },
    )
    .unwrap();
    let mut place = Box::pin(venue.place_order(sell(100).into()));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(place.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    // 単一スレッドruntimeでtimer実行を遅らせ、予定応答時刻も過ぎてから再開する。
    std::thread::sleep(std::time::Duration::from_millis(40));
    tokio::time::timeout(std::time::Duration::from_secs(2), place)
        .await
        .unwrap()
        .unwrap();
    let mut cancel = Box::pin(venue.cancel_orders("btc_jpy", vec![OrderId(1)]));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(cancel.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    std::thread::sleep(std::time::Duration::from_millis(40));
    tokio::time::timeout(std::time::Duration::from_secs(2), cancel)
        .await
        .unwrap()
        .unwrap();
    let records = records.0.lock().unwrap();
    let entries = records
        .iter()
        .filter(|r| r.kind == "paper_operation_effective")
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 2);
    for entry in entries {
        let response = records
            .iter()
            .find(|r| r.kind == "paper_operation_response" && r.data["id"] == entry.data["id"])
            .unwrap()
            .data["local_ms"]
            .as_i64()
            .unwrap();
        let entry = entry.data["local_ms"].as_i64().unwrap();
        assert!(response - entry >= 30, "entry={entry}, response={response}");
    }
}

struct EmitTwo;
impl BotStrategy<PaperTradingVenue> for EmitTwo {
    type CustomEvent = u8;
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<u8>,
        ctx: &BotContext<u8, PaperTradingVenue>,
    ) -> Result<(), String> {
        if matches!(event, BotEvent::Custom(0) | BotEvent::Custom(3)) {
            ctx.venue().place_order(sell(100).into()).await.unwrap();
            if matches!(event, BotEvent::Custom(0)) {
                ctx.emit(BotEvent::Custom(3)).await.unwrap();
                ctx.emit(BotEvent::Custom(4)).await.unwrap();
            }
        }
        Ok(())
    }
}
#[tokio::test]
async fn emitted_delivery_capacity_includes_queued_external_inputs_without_dropping_them() {
    let venue = PaperTradingVenue::with_latency(
        engine(PaperClock::Replay { now_ms: 1000 }, 0, None),
        PaperLatencyConfig {
            placement: PaperOperationLatency {
                entry_ms: 10,
                response_ms: 100,
            },
            ..Default::default()
        },
    )
    .unwrap();
    let mut runner = BotRunner::new(EmitTwo, venue);
    runner.delivery_limit = 2;
    runner
        .submit_replay(1000, None, BotEvent::Custom(0), false)
        .await
        .unwrap();
    runner
        .submit_replay(1001, None, BotEvent::Custom(1), false)
        .await
        .unwrap();
    runner
        .submit_replay(1002, None, BotEvent::Custom(2), false)
        .await
        .unwrap();
    runner.process_next_internal(false).await.unwrap(); // 1010有効化。
    assert_eq!(runner.pending_delivery_count(), 2);
    assert!(matches!(
        runner.process_next_internal(false).await,
        Err(BotRunError::DeliveryLimit)
    ));
    assert_eq!(runner.pending_delivery_count(), 2); // 起点の外部保留は失わない。
}

#[tokio::test]
async fn failed_derived_market_validation_restores_queued_external_input() {
    let venue = PaperTradingVenue::with_latency(
        engine(PaperClock::Replay { now_ms: 1000 }, 0, None),
        PaperLatencyConfig {
            placement: PaperOperationLatency {
                entry_ms: 10,
                response_ms: 10,
            },
            ..Default::default()
        },
    )
    .unwrap();
    let mut runner = BotRunner::new(
        EmitInvalid {
            delayed: true,
            scheduled: false,
            delivered: Default::default(),
        },
        venue,
    );
    runner
        .submit_replay(1000, None, BotEvent::Custom(0), false)
        .await
        .unwrap();
    runner
        .submit_replay(1001, None, BotEvent::Custom(9), false)
        .await
        .unwrap();
    runner.process_next_internal(false).await.unwrap();
    assert!(matches!(
        runner.process_next_internal(false).await,
        Err(BotRunError::Venue(_))
    ));
    assert_eq!(runner.pending_delivery_count(), 1);
}

// 時刻・資金の回帰fixture。注文価格をspread内に置き、先行数量を0にする。
fn seeded_engine(mut engine: PaperExecutionEngine, exchange_ms: i64) -> PaperExecutionEngine {
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
