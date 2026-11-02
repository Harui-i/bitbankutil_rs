use bitbankutil_rs::{
    bitbank_bot::*, execution_recording::*, market_event::*, order_domain::*, paper_execution::*,
    replay::*, trading_venue::*,
};
use rust_decimal::Decimal;
use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Default)]
struct Records(Arc<Mutex<Vec<ExecutionRecord>>>);
impl EventSink for Records {
    fn append(&mut self, r: &ExecutionRecord) -> io::Result<()> {
        self.0.lock().unwrap().push(r.clone());
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn order(price: i64) -> DesiredLimitOrder {
    DesiredLimitOrder::limit("btc_jpy".into(), OrderSide::Sell, 1.into(), price.into())
}
fn venue(clock: PaperClock, delta: i64, recorder: Option<ExecutionRecorder>) -> PaperTradingVenue {
    PaperTradingVenue::with_latency(engine(clock, delta, recorder), query_latency()).unwrap()
}
fn query_latency() -> PaperLatencyConfig {
    PaperLatencyConfig {
        orders_query: PaperOperationLatency {
            entry_ms: 20,
            response_ms: 40,
        },
        balances_query: PaperOperationLatency {
            entry_ms: 40,
            response_ms: 40,
        },
        ..Default::default()
    }
}
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
    config.fee_schedule = PaperFeeSchedule::new(Decimal::ZERO, Decimal::ZERO);
    let mut engine = PaperExecutionEngine::new(
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
    .unwrap();
    engine.place_order(order(100)).unwrap();
    engine
}
fn trade(executed_at: i64) -> MarketEvent {
    MarketEvent::Transactions {
        pair: "btc_jpy".into(),
        transactions: vec![MarketTrade {
            transaction_id: 1,
            executed_at,
            side: OrderSide::Buy,
            price: 101.into(),
            amount: 1.into(),
        }],
    }
}
fn balance(s: &AccountSnapshot, asset: &str) -> Decimal {
    s.balances
        .iter()
        .find(|b| b.asset == asset)
        .unwrap()
        .onhand_amount
}
async fn poll_once<T>(
    mut future: std::pin::Pin<&mut impl std::future::Future<Output = T>>,
) -> std::task::Poll<T> {
    std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx))).await
}

#[tokio::test]
async fn individual_samples_are_frozen_and_old_query_cannot_overwrite_new_notifications() {
    for delta in [-100, 0, 100] {
        let v = venue(
            PaperClock::Replay {
                now_ms: 900 - delta,
            },
            delta,
            None,
        );
        v.advance_replay_time(1000 - delta).unwrap();
        let mut query = Box::pin(v.account_snapshot("btc_jpy"));
        assert!(poll_once(query.as_mut()).await.is_pending());
        assert_eq!(v.pending_operations().len(), 2);
        v.process_internal_events(1020 - delta).unwrap();
        v.advance_replay_time(1030 - delta).unwrap();
        v.on_market_event(&trade(1025)).unwrap();
        v.advance_trade_watermark(1030).unwrap();
        v.process_internal_events(1040 - delta).unwrap();
        // 採取後の新規注文を凍結済み応答へ混ぜない。
        v.advance_replay_time(1050 - delta).unwrap();
        v.place_order(order(110).into()).await.unwrap();
        let newer = v.known_account_snapshot();
        assert_eq!(newer.open_orders[0].order_id, OrderId(2));
        v.process_internal_events(1060 - delta).unwrap();
        assert!(poll_once(query.as_mut()).await.is_pending());
        v.process_internal_events(1080 - delta).unwrap();
        let result = query.await.unwrap();
        assert_eq!(result.open_orders[0].order_id, OrderId(1));
        assert_eq!(balance(&result, "btc"), Decimal::from(2));
        assert_eq!(balance(&result, "jpy"), Decimal::from(100));
        assert_eq!(v.known_account_snapshot(), newer);
        let m = result.query.unwrap();
        assert_eq!(
            (m.orders.sampled_exchange_ms, m.balances.sampled_exchange_ms),
            (1020, 1040)
        );
        assert_eq!(
            (m.orders.sampled_local_ms, m.balances.sampled_local_ms),
            (1020 - delta, 1040 - delta)
        );
        assert_eq!(
            (m.orders.received_local_ms, m.balances.received_local_ms),
            (Some(1060 - delta), Some(1080 - delta))
        );
        assert_eq!(m.completed_local_ms, 1080 - delta);
        assert_eq!(m.orders.watermark_ms, None);
        assert_eq!(m.balances.watermark_ms, Some(1030));
        assert!(m.orders.unsettled && m.balances.unsettled);
    }
}

#[tokio::test]
async fn late_trade_does_not_rewrite_sample_and_uncertainty_survives_final_watermark() {
    let records = Records::default();
    let v = venue(
        PaperClock::Replay { now_ms: 900 },
        0,
        Some(ExecutionRecorder::new(
            "uncertain",
            serde_json::json!({}),
            records.clone(),
        )),
    );
    v.advance_replay_time(1000).unwrap();
    let mut query = Box::pin(v.account_snapshot("btc_jpy"));
    assert!(poll_once(query.as_mut()).await.is_pending());
    v.process_internal_events(1080).unwrap();
    let sampled = query.await.unwrap();
    assert_eq!(sampled.open_orders.len(), 1);
    assert_eq!(balance(&sampled, "jpy"), Decimal::ZERO);
    assert!(sampled.query.as_ref().unwrap().orders.unsettled);
    v.advance_replay_time(1090).unwrap();
    v.on_market_event(&trade(1010)).unwrap();
    v.advance_trade_watermark(1200).unwrap();
    assert_eq!(balance(&sampled, "jpy"), Decimal::ZERO);
    assert_eq!(
        balance(&v.known_account_snapshot(), "jpy"),
        Decimal::from(100)
    );
    let status = v.paper_settlement_status().unwrap();
    assert!(status.unsettled);
    assert_eq!(status.uncertain_query_observations, 2);
    assert_eq!(status.pending_operations, 0);
    let engine_status = v.engine().lock().unwrap().settlement_status();
    assert_eq!(engine_status.uncertain_query_observations, 2);
    assert!(engine_status.unsettled);
    // 履歴は維持するが、後続の確定可能な採取まで未確定扱いにはしない。
    let mut later = Box::pin(v.account_snapshot("btc_jpy"));
    assert!(poll_once(later.as_mut()).await.is_pending());
    v.process_internal_events(1170).unwrap();
    let meta = later.await.unwrap().query.unwrap();
    assert!(!meta.orders.unsettled && !meta.balances.unsettled);
    assert_eq!(
        v.paper_settlement_status()
            .unwrap()
            .uncertain_query_observations,
        2
    );
    let records = records.0.lock().unwrap();
    let last = records
        .iter()
        .rfind(|r| r.kind == "paper_settlement")
        .unwrap();
    assert_eq!(last.data["uncertain_query_observations"], 2);
    assert_eq!(last.data["unsettled"], true);
}

type Seen = Arc<Mutex<Vec<(u8, i64, i64)>>>;
struct Strategy {
    seen: Seen,
    snapshots: Arc<Mutex<Vec<AccountSnapshot>>>,
}
impl BotStrategy<PaperTradingVenue> for Strategy {
    type CustomEvent = u8;
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<u8>,
        ctx: &BotContext<u8, PaperTradingVenue>,
    ) -> Result<(), String> {
        let BotEvent::Custom(n) = event else {
            return Ok(());
        };
        if n == 1 {
            let price = 110; // 待機前に決めた値。
            let account = ctx
                .venue()
                .account_snapshot("btc_jpy")
                .await
                .map_err(|e| format!("{e:?}"))?;
            self.snapshots.lock().unwrap().push(account);
            ctx.venue().place_order(order(price).into()).await.unwrap();
            ctx.schedule_after(0, 3).unwrap();
        }
        self.seen
            .lock()
            .unwrap()
            .push((n, ctx.received_at_ms(), ctx.now_ms()));
        Ok(())
    }
}
fn capture(seq: u64, time: i64, payload: CapturePayload) -> io::Result<(usize, CaptureRecord)> {
    Ok((
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
    ))
}
fn inputs(delta: i64) -> Vec<io::Result<(usize, CaptureRecord)>> {
    vec![
        capture(
            1,
            900 - delta,
            CapturePayload::Connected {
                endpoint: "fixture".into(),
                subscriptions: vec![],
            },
        ),
        capture(
            2,
            1000 - delta,
            CapturePayload::Message {
                message: serde_json::json!(1),
            },
        ),
        capture(
            3,
            1030 - delta,
            CapturePayload::Message {
                message: serde_json::json!(2),
            },
        ),
    ]
}
fn custom(r: &CaptureRecord) -> io::Result<Option<u8>> {
    Ok(match &r.payload {
        CapturePayload::Message { message } => Some(message.as_u64().unwrap() as u8),
        _ => None,
    })
}
#[tokio::test]
async fn query_wait_preserves_receive_time_fifo_price_and_context_without_wall_sleep() {
    for delta in [-100, 0, 100] {
        for recording in [false, true] {
            let records = Records::default();
            let recorder = recording
                .then(|| ExecutionRecorder::new("query", serde_json::json!({}), records.clone()));
            let v = venue(
                PaperClock::Replay {
                    now_ms: 900 - delta,
                },
                delta,
                recorder,
            );
            let seen = Seen::default();
            let snapshots = Arc::new(Mutex::new(vec![]));
            let mut runner = BotRunner::new(
                Strategy {
                    seen: seen.clone(),
                    snapshots: snapshots.clone(),
                },
                v.clone(),
            );
            let report = run_replay_with_options(
                &mut runner,
                inputs(delta),
                ReplayOptions {
                    recovery: false,
                    end_time_ms: Some(1100 - delta),
                },
                &AtomicBool::new(false),
                custom,
            )
            .await;
            assert!(report.error.is_none(), "{:?}", report.error);
            assert_eq!(
                *seen.lock().unwrap(),
                [
                    (1, 1000 - delta, 1080 - delta),
                    (2, 1030 - delta, 1080 - delta),
                    (3, 1080 - delta, 1080 - delta)
                ]
            );
            assert_eq!(
                v.known_account_snapshot().open_orders[1].price,
                Some(110.into())
            );
            assert_eq!(snapshots.lock().unwrap().len(), 1);
            assert_eq!(report.settlement.unwrap().uncertain_query_observations, 2);
            if recording {
                let records = records.0.lock().unwrap();
                for kind in [
                    "paper_query_requested",
                    "paper_query_sampled",
                    "paper_query_response",
                ] {
                    let events = records
                        .iter()
                        .filter(|r| r.kind == kind)
                        .collect::<Vec<_>>();
                    assert_eq!(events.len(), 2);
                    assert!(
                        events
                            .iter()
                            .all(|r| r.input_id.as_ref().unwrap().sequence == 2)
                    );
                }
                let delivery = records
                    .iter()
                    .find(|r| {
                        r.kind == "custom_event"
                            && r.input_id.as_ref().is_some_and(|id| id.sequence == 3)
                    })
                    .unwrap();
                assert_eq!(delivery.data["received_at_ms"], 1030 - delta);
                assert_eq!(delivery.data["processed_at_ms"], 1080 - delta);
            }
        }
    }
}

#[tokio::test]
async fn query_end_disconnect_and_interruption_keep_pending_requests() {
    for mode in 0..3 {
        let v = venue(PaperClock::Replay { now_ms: 900 }, 0, None);
        let mut runner = BotRunner::new(
            Strategy {
                seen: Seen::default(),
                snapshots: Default::default(),
            },
            v.clone(),
        );
        let mut input = inputs(0);
        input.truncate(2);
        let cancel = AtomicBool::new(false);
        if mode == 1 {
            input.push(capture(
                3,
                1010,
                CapturePayload::Disconnected {
                    reason: "capture_gap".into(),
                },
            ));
        }
        // 入力取得時に中断する。要求開始後、採取前。
        let iter = input.into_iter().enumerate().map(|(i, r)| {
            if mode == 2 && i == 1 {
                cancel.store(true, Ordering::Relaxed);
            }
            r
        });
        let report = run_replay_with_options(
            &mut runner,
            iter,
            ReplayOptions {
                recovery: false,
                end_time_ms: Some(1010),
            },
            &cancel,
            custom,
        )
        .await;
        if mode != 2 {
            assert!(report.pending_handler);
            assert_eq!(v.pending_operations().len(), 2);
        }
        assert!(report.incomplete);
        assert!(report.settlement.unwrap().unsettled);
        assert_eq!(
            report.end_reason,
            match mode {
                0 => ReplayEndReason::EndTime,
                1 => ReplayEndReason::Disconnected,
                _ => ReplayEndReason::Interrupted,
            }
        );
    }
}

#[tokio::test]
async fn query_failure_and_invalid_latency_do_not_modify_balances() {
    let v = venue(PaperClock::Replay { now_ms: 900 }, 0, None);
    let before = v.known_account_snapshot();
    let mut query = Box::pin(v.account_snapshot("wrong_pair"));
    assert!(poll_once(query.as_mut()).await.is_pending());
    v.process_internal_events(1000).unwrap();
    assert!(matches!(
        query.await,
        Err(TradingVenueError::Paper(
            PaperExecutionError::PairMismatch { .. }
        ))
    ));
    assert_eq!(before, v.known_account_snapshot());
    assert!(v.pending_operations().is_empty());
    assert!(v.next_internal_time().is_none());
    let v = PaperTradingVenue::with_latency(
        engine(PaperClock::Replay { now_ms: 900 }, 0, None),
        PaperLatencyConfig {
            orders_query: PaperOperationLatency {
                entry_ms: i64::MAX,
                response_ms: 0,
            },
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        v.account_snapshot("btc_jpy").await,
        Err(TradingVenueError::Paper(PaperExecutionError::InvalidClock))
    ));
    assert!(v.pending_operations().is_empty());
    assert!(
        PaperTradingVenue::with_latency(
            engine(PaperClock::Replay { now_ms: 900 }, 0, None),
            PaperLatencyConfig {
                balances_query: PaperOperationLatency {
                    entry_ms: -1,
                    response_ms: 0
                },
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[tokio::test]
async fn real_market_applies_once_during_query_and_same_ms_sampling_is_after_market() {
    // 合成fixtureでだけ「通知遅延は0」を仮定。1025成立・1030受信でなく、
    // 1025成立・1025受信とし、次の1030市場入力で1029まで確定する。
    let records = Records::default();
    let e = engine(
        PaperClock::Replay { now_ms: 900 },
        0,
        Some(ExecutionRecorder::new(
            "market",
            serde_json::json!({}),
            records.clone(),
        )),
    )
    .with_max_trade_delay_ms(0)
    .unwrap();
    let mut latency = query_latency();
    latency.balances_query.entry_ms = 1030 - 1000;
    let v = PaperTradingVenue::with_latency(e, latency).unwrap();
    let snapshots = Arc::new(Mutex::new(vec![]));
    let seen = Seen::default();
    let mut runner = BotRunner::new(
        Strategy {
            seen: seen.clone(),
            snapshots: snapshots.clone(),
        },
        v.clone(),
    );
    let market_record = |seq, time, message: serde_json::Value| {
        let mut r = capture(seq, time, CapturePayload::Message { message }).unwrap();
        r.0 = 1;
        r.1.exchange = "bitbank".into();
        r.1.stream = "public".into();
        r.1.session_id = "bb".into();
        Ok(r)
    };
    let mut connected = capture(
        1,
        900,
        CapturePayload::Connected {
            endpoint: "fixture".into(),
            subscriptions: vec!["transactions_btc_jpy".into()],
        },
    )
    .unwrap();
    connected.0 = 1;
    connected.1.exchange = "bitbank".into();
    connected.1.stream = "public".into();
    connected.1.session_id = "bb".into();
    let mut input = inputs(0);
    input.truncate(2);
    input.insert(1, Ok(connected));
    input.push(market_record(2,1025,serde_json::json!(["message",{"room_name":"transactions_btc_jpy","message":{"data":{"transactions":[{"transaction_id":1,"side":"buy","price":"101","amount":"1","executed_at":1025}]}}}])));
    input.push(market_record(3,1030,serde_json::json!(["message",{"room_name":"transactions_btc_jpy","message":{"data":{"transactions":[{"transaction_id":2,"side":"sell","price":"100","amount":"1","executed_at":1030}]}}}])));
    let report = run_replay_with_options(
        &mut runner,
        input,
        ReplayOptions {
            end_time_ms: Some(1100),
            recovery: false,
        },
        &AtomicBool::new(false),
        custom,
    )
    .await;
    assert!(report.error.is_none(), "{:?}", report.error);
    let samples = snapshots.lock().unwrap();
    let s = &samples[0];
    assert_eq!(s.open_orders[0].order_id, OrderId(1)); // 1020に採取。
    assert_eq!(balance(s, "jpy"), Decimal::from(100)); // 1030の確定後に採取。
    assert_eq!(s.query.as_ref().unwrap().balances.watermark_ms, Some(1029));
    let events = v.engine().lock().unwrap().drain_events();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, PaperEvent::OrderFilled { .. }))
            .count(),
        1
    );
    let records = records.0.lock().unwrap();
    assert_eq!(
        records
            .iter()
            .filter(|r| r.kind == "input_registered"
                && r.input_id
                    .as_ref()
                    .is_some_and(|id| id.session_id == "bb" && id.sequence > 1))
            .count(),
        2
    );
    assert_eq!(
        records.iter().filter(|r| r.kind == "market_event").count(),
        2
    );
}

#[tokio::test]
async fn realtime_queries_finish_without_market_input() {
    let v = venue(PaperClock::Realtime, 0, None);
    let snapshot = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        v.account_snapshot("btc_jpy"),
    )
    .await
    .unwrap()
    .unwrap();
    let meta = snapshot.query.unwrap();
    assert!(meta.orders.sampled_local_ms >= meta.orders.requested_local_ms + 20);
    assert!(meta.orders.received_local_ms.unwrap() >= meta.orders.sampled_local_ms + 40);
    assert!(meta.balances.sampled_local_ms >= meta.balances.requested_local_ms + 40);
    assert!(meta.balances.received_local_ms.unwrap() >= meta.balances.sampled_local_ms + 40);
    assert!(v.pending_operations().is_empty());
}

#[tokio::test]
async fn paper_runtime_awaits_queries_and_preserves_sequential_delivery() {
    struct RuntimeStrategy(tokio::sync::mpsc::UnboundedSender<(u8, i64, i64)>);
    impl BotStrategy<PaperTradingVenue> for RuntimeStrategy {
        type CustomEvent = u8;
        type Error = String;
        async fn handle_event(
            &mut self,
            event: BotEvent<u8>,
            ctx: &BotContext<u8, PaperTradingVenue>,
        ) -> Result<(), String> {
            let BotEvent::Custom(n) = event else {
                return Ok(());
            };
            if n == 1 {
                ctx.venue().account_snapshot("btc_jpy").await.unwrap();
            }
            self.0
                .send((n, ctx.received_at_ms(), ctx.now_ms()))
                .unwrap();
            Ok(())
        }
    }
    let v = venue(PaperClock::Realtime, 0, None);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let runtime = BitbankBotBuilder::new(RuntimeStrategy(tx), v).spawn();
    let sender = runtime.event_sender();
    sender.send(BotEvent::Custom(1)).await.unwrap();
    sender.send(BotEvent::Custom(2)).await.unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first.0, second.0), (1, 2));
    assert!(second.1 <= first.2 && first.2 <= second.2);
    runtime.shutdown().await.unwrap();
}

// 時刻・資金の回帰fixture。注文価格をspread内に置き、先行数量を0にする。
fn seeded_engine(mut engine: PaperExecutionEngine, exchange_ms: i64) -> PaperExecutionEngine {
    engine.apply_market_event(&bitbankutil_rs::market_event::MarketEvent::DepthUpdated {
        pair: "btc_jpy".into(),
        depth: bitbankutil_rs::market_event::MarketDepthSnapshot::new(
            std::collections::BTreeMap::from([(1_000_000.into(), 1.0)]),
            std::collections::BTreeMap::from([(1.into(), 1.0)]),
            exchange_ms,
        ),
    });
    engine
}

#[tokio::test]
async fn lightweight_sampling_preserves_issues_and_recording_parity() {
    let mut expected = None;
    for profile in [
        None,
        Some(RecordingProfile::Disabled),
        Some(RecordingProfile::Financial),
        Some(RecordingProfile::Diagnostic {
            queue: QueueRecording::Full,
        }),
        Some(RecordingProfile::Diagnostic {
            queue: QueueRecording::Periodic {
                every: 3.try_into().unwrap(),
            },
        }),
        Some(RecordingProfile::Diagnostic {
            queue: QueueRecording::Delta {
                snapshot_every: 3.try_into().unwrap(),
            },
        }),
        Some(RecordingProfile::Diagnostic {
            queue: QueueRecording::Counters,
        }),
    ] {
        let records = Records::default();
        let recorder = profile.map(|profile| {
            ExecutionRecorder::with_profile(
                "parity",
                serde_json::json!({}),
                profile,
                records.clone(),
            )
        });
        let v = venue(PaperClock::Replay { now_ms: 900 }, 0, recorder);
        v.advance_replay_time(1000).unwrap();
        v.on_market_event(&trade(950)).unwrap();
        v.advance_trade_watermark(1000).unwrap();
        v.mark_market_incomplete("settlement issue first");
        v.engine()
            .lock()
            .unwrap()
            .apply_market_event(&MarketEvent::DepthUpdated {
                pair: "btc_jpy".into(),
                depth: MarketDepthSnapshot::new(Default::default(), Default::default(), 1000),
            });
        let status = v.paper_settlement_status().unwrap();
        assert!(!status.issues.is_empty() && !status.model_issues.is_empty());
        let issues: Vec<_> = status
            .issues
            .into_iter()
            .chain(status.model_issues)
            .collect();
        let mut query = Box::pin(v.account_snapshot("btc_jpy"));
        assert!(poll_once(query.as_mut()).await.is_pending());
        assert!(v.has_pending_operations());
        assert!(v.paper_settlement_status().unwrap().pending_operations > 0);
        v.process_internal_events(1080).unwrap();
        let snapshot = query.await.unwrap();
        for observation in [
            &snapshot.query.as_ref().unwrap().orders,
            &snapshot.query.as_ref().unwrap().balances,
        ] {
            assert_eq!(observation.issues, issues);
            assert_eq!(observation.watermark_ms, status.watermark_ms);
            assert!(observation.unsettled);
        }
        assert!(!v.has_pending_operations());
        let events = v.engine().lock().unwrap().drain_events();
        let result = (snapshot, events, v.paper_settlement_status().unwrap());
        assert_eq!(expected.get_or_insert_with(|| result.clone()), &result);
    }
}
