use bitbankutil_rs::{
    bitbank_bot::*,
    execution_recording::*,
    order_domain::{BalanceSnapshot, DesiredLimitOrder, OrderSide},
    paper_execution::*,
    replay::*,
    trading_venue::TradingVenue,
};
use rust_decimal::Decimal;
use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Debug)]
enum Event {
    Input(u8),
    Timer(u8),
}
type Seen = Arc<Mutex<Vec<(i64, u8, u8)>>>;
struct Strategy {
    seen: Seen,
    latest: u8,
    looping: bool,
    fail: bool,
    cancel: Option<Arc<AtomicBool>>,
}
impl BotStrategy<PaperTradingVenue> for Strategy {
    type CustomEvent = Event;
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<Event>,
        ctx: &BotContext<Event, PaperTradingVenue>,
    ) -> Result<(), String> {
        let BotEvent::Custom(event) = event else {
            panic!("unexpected market")
        };
        let n = match event {
            Event::Input(n) => {
                self.latest = n;
                if n == 1 {
                    for (at, timer) in [(1100, 10), (1200, 11), (1300, 12), (1400, 14), (1500, 15)]
                    {
                        ctx.schedule_at(at, Event::Timer(timer)).unwrap();
                    }
                    assert_eq!(
                        ctx.schedule_at(999, Event::Timer(0)),
                        Err(ScheduleError::Past)
                    );
                    assert_eq!(
                        ctx.schedule_after(-1, Event::Timer(0)),
                        Err(ScheduleError::Past)
                    );
                    assert_eq!(
                        ctx.schedule_after(i64::MAX, Event::Timer(0)),
                        Err(ScheduleError::Overflow)
                    );
                }
                n
            }
            Event::Timer(n) => {
                // 実際の注文操作も仮想時計で行われる。未来の入力はまだ配送されない。
                ctx.venue().account_snapshot("btc_jpy").await.unwrap();
                if n == 10 {
                    ctx.venue()
                        .place_order(
                            DesiredLimitOrder::limit(
                                "btc_jpy".into(),
                                OrderSide::Buy,
                                Decimal::ONE,
                                Decimal::from(100),
                            )
                            .into(),
                        )
                        .await
                        .unwrap();
                    ctx.schedule_after(0, Event::Timer(20)).unwrap();
                    if let Some(cancel) = &self.cancel {
                        cancel.store(true, Ordering::Relaxed);
                    }
                    if self.fail {
                        return Err("internal failure".into());
                    }
                }
                if n == 11 {
                    let account = ctx.venue().account_snapshot("btc_jpy").await.unwrap();
                    ctx.venue()
                        .cancel_orders(
                            "btc_jpy",
                            account.open_orders.iter().map(|o| o.order_id).collect(),
                        )
                        .await
                        .unwrap();
                }
                if self.looping {
                    ctx.schedule_after(0, Event::Timer(n)).unwrap();
                }
                n
            }
        };
        self.seen
            .lock()
            .unwrap()
            .push((ctx.now_ms(), n, self.latest));
        Ok(())
    }
}
fn record(seq: u64, micros: u64, payload: CapturePayload) -> CaptureRecord {
    CaptureRecord {
        format_version: 1,
        exchange: "bybit".into(),
        stream: "linear".into(),
        session_id: "s".into(),
        connection_id: "c".into(),
        sequence: seq,
        received_at_micros: micros,
        payload,
    }
}
fn inputs() -> Vec<io::Result<(usize, CaptureRecord)>> {
    vec![
        Ok((
            0,
            record(
                1,
                1_000_000,
                CapturePayload::Connected {
                    endpoint: "fixture".into(),
                    subscriptions: vec!["test".into()],
                },
            ),
        )),
        Ok((
            0,
            record(
                2,
                1_000_001,
                CapturePayload::Message {
                    message: serde_json::json!(1),
                },
            ),
        )),
        Ok((
            0,
            record(
                3,
                1_300_000,
                CapturePayload::Message {
                    message: serde_json::json!(2),
                },
            ),
        )),
        Ok((
            0,
            record(
                4,
                1_300_001,
                CapturePayload::Message {
                    message: serde_json::json!(3),
                },
            ),
        )),
    ]
}
fn custom(record: &CaptureRecord) -> io::Result<Option<Event>> {
    if let CapturePayload::Message { message } = &record.payload {
        Ok(Some(Event::Input(message.as_u64().unwrap() as u8)))
    } else {
        Ok(None)
    }
}
fn venue(delta: i64, recorder: Option<ExecutionRecorder>) -> PaperTradingVenue {
    let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
    config.time_model = PaperTimeModel::FixedOffset {
        exchange_minus_local_ms: delta,
    };
    PaperTradingVenue::new(
        PaperExecutionEngine::new(
            config,
            PaperClock::Replay { now_ms: 0 },
            vec![
                BalanceSnapshot {
                    asset: "jpy".into(),
                    free_amount: Decimal::from(1000),
                    locked_amount: Decimal::ZERO,
                    onhand_amount: Decimal::from(1000),
                },
                BalanceSnapshot {
                    asset: "btc".into(),
                    free_amount: Decimal::ZERO,
                    locked_amount: Decimal::ZERO,
                    onhand_amount: Decimal::ZERO,
                },
            ],
            recorder,
        )
        .unwrap(),
    )
}
fn runner(
    delta: i64,
    recorder: Option<ExecutionRecorder>,
) -> (BotRunner<Strategy, PaperTradingVenue>, Seen) {
    let seen = Seen::default();
    (
        BotRunner::new(
            Strategy {
                seen: seen.clone(),
                latest: 0,
                looping: false,
                fail: false,
                cancel: None,
            },
            venue(delta, recorder),
        ),
        seen,
    )
}
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<ExecutionRecord>>>);
impl EventSink for Sink {
    fn append(&mut self, record: &ExecutionRecord) -> io::Result<()> {
        self.0.lock().unwrap().push(record.clone());
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn interleaves_timers_without_future_information_and_preserves_ties() {
    let expected = vec![
        (1000, 1, 1),
        (1100, 10, 1),
        (1100, 20, 1),
        (1200, 11, 1),
        (1300, 2, 2),
        (1300, 3, 3),
        (1300, 12, 3),
        (1400, 14, 3),
    ];
    for delta in [-200, 0, 200] {
        for recording in [false, true] {
            let sink = Sink::default();
            let recorder = recording
                .then(|| ExecutionRecorder::new("test", serde_json::json!({}), sink.clone()));
            let (mut runner, seen) = runner(delta, recorder);
            let report = run_replay_with_options(
                &mut runner,
                inputs(),
                ReplayOptions {
                    recovery: false,
                    end_time_ms: Some(1400),
                },
                &AtomicBool::new(false),
                custom,
            )
            .await;
            assert!(report.error.is_none(), "{:?}", report.error);
            assert_eq!(report.end_reason, ReplayEndReason::EndTime);
            assert_eq!(*seen.lock().unwrap(), expected);
            let account = runner.venue().account_snapshot("btc_jpy").await.unwrap();
            let jpy = account.balances.iter().find(|b| b.asset == "jpy").unwrap();
            // 時計を境界まで進めても、watermarkなしでは取消資金を解放しない。
            assert_eq!(jpy.locked_amount, Decimal::from(100));
            let accepted: Vec<_> = runner
                .venue()
                .engine()
                .lock()
                .unwrap()
                .drain_events()
                .into_iter()
                .filter_map(|e| match e {
                    PaperEvent::OrderAccepted {
                        effective_after_ms, ..
                    } => Some(effective_after_ms),
                    _ => None,
                })
                .collect();
            assert_eq!(accepted, [1100 + delta]);
            assert_eq!(report.records, 4);
            assert_eq!(report.final_time_ms, Some(1400));
            assert_eq!(report.pending_scheduled.len(), 1);
            assert_eq!(report.pending_scheduled[0].execute_at_ms, 1500);
            assert_eq!(
                report.pending_scheduled[0]
                    .input_id
                    .as_ref()
                    .unwrap()
                    .sequence,
                2
            );
            assert_eq!(
                report.settlement.unwrap().time_model,
                PaperTimeModel::FixedOffset {
                    exchange_minus_local_ms: delta
                }
            );
            let records = sink.0.lock().unwrap();
            let executed: Vec<_> = records
                .iter()
                .filter(|r| r.kind == "scheduled_event_executed")
                .collect();
            if recording {
                assert_eq!(executed.len(), 5);
                assert_eq!(
                    executed
                        .iter()
                        .map(|r| r.data["event"]["id"].as_u64().unwrap())
                        .collect::<Vec<_>>(),
                    [1, 6, 2, 3, 4]
                );
                assert!(
                    executed
                        .iter()
                        .all(|r| r.data["event"]["input_id"]["sequence"] == 2)
                );
            }
        }
    }
}
#[tokio::test]
async fn eof_does_not_extend_period_and_explicit_boundary_can_precede_next_input() {
    for end in [None, Some(1250), Some(1600)] {
        let (mut runner, seen) = runner(0, None);
        let report = run_replay_with_options(
            &mut runner,
            inputs(),
            ReplayOptions {
                recovery: false,
                end_time_ms: end,
            },
            &AtomicBool::new(false),
            custom,
        )
        .await;
        assert!(report.error.is_none());
        match end {
            None => {
                assert_eq!(report.end_reason, ReplayEndReason::Eof);
                assert_eq!(report.pending_scheduled.len(), 2);
                assert_eq!(report.final_time_ms, Some(1300));
            }
            Some(1250) => {
                assert_eq!(report.records, 2);
                assert_eq!(seen.lock().unwrap().last().unwrap(), &(1200, 11, 1));
                assert_eq!(report.pending_scheduled.len(), 3);
            }
            _ => {
                assert!(report.pending_scheduled.is_empty());
                assert_eq!(report.final_time_ms, Some(1600));
            }
        }
    }
}
#[tokio::test]
async fn failure_cancel_disconnect_and_bad_input_stop_reservations() {
    for mode in 0..5 {
        let seen = Seen::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let strategy = Strategy {
            seen,
            latest: 0,
            looping: mode == 2,
            fail: mode == 0,
            cancel: (mode == 1).then(|| cancel.clone()),
        };
        let mut runner = BotRunner::new(strategy, venue(0, None));
        runner.scheduled_limit = 8;
        let mut input = inputs();
        if mode == 3 {
            input[2] = Ok((
                0,
                record(
                    3,
                    1_050_000,
                    CapturePayload::Disconnected {
                        reason: "capture_gap".into(),
                    },
                ),
            ));
            input.truncate(3);
        }
        if mode == 4 {
            input[2] = Err(io::Error::other("invalid input"));
        }
        let report = run_replay_with_options(
            &mut runner,
            input,
            ReplayOptions {
                recovery: false,
                end_time_ms: Some(2000),
            },
            &cancel,
            custom,
        )
        .await;
        assert!(report.incomplete);
        assert!(!report.pending_scheduled.is_empty());
        assert!(report.settlement.unwrap().unsettled);
        assert_eq!(
            report.end_reason,
            match mode {
                1 => ReplayEndReason::Interrupted,
                3 => ReplayEndReason::Disconnected,
                _ => ReplayEndReason::Error,
            }
        );
        if mode == 2 {
            assert!(report.error.unwrap().contains("ScheduledLimit"));
        }
    }
}
#[tokio::test]
async fn cancellation_in_last_reservation_stops_before_input_or_end_time() {
    struct CancelLast {
        cancel: Arc<AtomicBool>,
        seen: Seen,
    }
    impl BotStrategy<PaperTradingVenue> for CancelLast {
        type CustomEvent = Event;
        type Error = String;
        async fn handle_event(
            &mut self,
            event: BotEvent<Event>,
            ctx: &BotContext<Event, PaperTradingVenue>,
        ) -> Result<(), String> {
            match event {
                BotEvent::Custom(Event::Input(1)) => {
                    ctx.schedule_at(1100, Event::Timer(10)).unwrap();
                }
                BotEvent::Custom(Event::Timer(10)) => {
                    self.cancel.store(true, Ordering::Relaxed);
                    self.seen.lock().unwrap().push((ctx.now_ms(), 10, 1));
                }
                _ => panic!("input after cancellation must not be delivered"),
            }
            Ok(())
        }
    }
    for at_eof in [false, true] {
        let cancel = Arc::new(AtomicBool::new(false));
        let seen = Seen::default();
        let mut runner = BotRunner::new(
            CancelLast {
                cancel: cancel.clone(),
                seen: seen.clone(),
            },
            venue(0, None),
        );
        let mut input = inputs();
        if at_eof {
            input.truncate(2);
        }
        let report = run_replay_with_options(
            &mut runner,
            input,
            ReplayOptions {
                recovery: false,
                end_time_ms: Some(2000),
            },
            &cancel,
            custom,
        )
        .await;
        assert_eq!(report.end_reason, ReplayEndReason::Interrupted);
        assert!(report.interrupted);
        assert!(report.incomplete);
        assert!(report.error.is_none());
        assert_eq!(report.final_time_ms, Some(1100));
        assert_eq!(report.records, 2);
        assert!(report.pending_scheduled.is_empty());
        assert_eq!(*seen.lock().unwrap(), [(1100, 10, 1)]);
        assert!(report.settlement.unwrap().unsettled);
    }
}

#[tokio::test]
async fn empty_input_boundary_and_invalid_clock_conversion() {
    let (mut runner, _) = runner(0, None);
    let report = run_replay_with_options(
        &mut runner,
        [],
        ReplayOptions {
            recovery: false,
            end_time_ms: Some(100),
        },
        &AtomicBool::new(false),
        custom,
    )
    .await;
    assert_eq!(report.final_time_ms, Some(100));
    let report = run_replay_with_options(
        &mut runner,
        [],
        ReplayOptions {
            recovery: false,
            end_time_ms: Some(99),
        },
        &AtomicBool::new(false),
        custom,
    )
    .await;
    assert_eq!(report.end_reason, ReplayEndReason::Error);
    let (mut runner, _) = self::runner(i64::MAX, None);
    let report = run_replay(
        &mut runner,
        inputs(),
        false,
        &AtomicBool::new(false),
        custom,
    )
    .await;
    assert_eq!(report.end_reason, ReplayEndReason::Error);
}

#[tokio::test]
async fn recovery_continues_timers_without_restoring_execution() {
    struct Recovery(Arc<Mutex<Vec<i64>>>);
    impl BotStrategy<PaperTradingVenue> for Recovery {
        type CustomEvent = Event;
        type Error = String;
        async fn handle_event(
            &mut self,
            event: BotEvent<Event>,
            ctx: &BotContext<Event, PaperTradingVenue>,
        ) -> Result<(), String> {
            if let BotEvent::Custom(Event::Input(_)) = event {
                ctx.schedule_at(1100, Event::Timer(1)).unwrap();
            } else {
                self.0.lock().unwrap().push(ctx.now_ms());
                assert!(ctx.venue().paper_settlement_status().unwrap().unsettled);
            }
            Ok(())
        }
    }
    let mut input = inputs();
    input.truncate(2);
    input.push(Ok((
        0,
        record(
            3,
            1_050_000,
            CapturePayload::Disconnected {
                reason: "capture_gap".into(),
            },
        ),
    )));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut runner = BotRunner::new(Recovery(seen.clone()), venue(0, None));
    let report = run_replay_with_options(
        &mut runner,
        input,
        ReplayOptions {
            recovery: true,
            end_time_ms: Some(1200),
        },
        &AtomicBool::new(false),
        custom,
    )
    .await;
    assert_eq!(*seen.lock().unwrap(), [1100]);
    assert!(report.error.is_none());
    assert!(report.incomplete);
    assert!(report.input_exhausted);
}

#[tokio::test]
async fn schedule_limits_and_closed_contexts_are_enforced() {
    use bitbankutil_rs::trading_venue::NoTradingVenue;
    struct Save(Arc<Mutex<Option<BotContext<()>>>>);
    impl BotStrategy for Save {
        type CustomEvent = ();
        type Error = String;
        async fn handle_event(
            &mut self,
            _: BotEvent<()>,
            ctx: &BotContext<()>,
        ) -> Result<(), String> {
            *self.0.lock().unwrap() = Some(ctx.clone());
            // 呼び出し側がエラーを無視しても実行失敗にする。
            for _ in 0..3 {
                let _ = ctx.schedule_after(0, ());
            }
            Ok(())
        }
    }
    let saved = Arc::new(Mutex::new(None));
    let mut runner = BotRunner::new(Save(saved.clone()), NoTradingVenue);
    runner.scheduled_limit = 2;
    assert!(matches!(
        runner.process(100, None, BotEvent::Custom(())).await,
        Err(BotRunError::ScheduledLimit)
    ));
    let ctx = saved.lock().unwrap().take().unwrap();
    assert_eq!(ctx.schedule_after(0, ()), Err(ScheduleError::Closed));
    let runtime = BitbankBotBuilder::market_data(Save(saved.clone())).spawn();
    runtime
        .event_sender()
        .send(BotEvent::Custom(()))
        .await
        .unwrap();
    runtime.shutdown().await.unwrap();
    let ctx = saved.lock().unwrap().take().unwrap();
    assert_eq!(ctx.schedule_after(0, ()), Err(ScheduleError::NotReplay));
}
