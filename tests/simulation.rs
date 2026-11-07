use bitbankutil_rs::{
    bitbank_bot::*, market_event::MarketEvent, order_domain::*, paper_execution::*, replay::*,
    simulation::*, trading_venue::TradingVenue,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, atomic::AtomicBool};

fn venue(delta: i64, latency: PaperLatencyConfig) -> PaperTradingVenue {
    let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
    config.time_model = PaperTimeModel::FixedOffset {
        exchange_minus_local_ms: delta,
    };
    config.fee_schedule = PaperFeeSchedule::new(Decimal::ZERO, Decimal::ZERO);
    let engine = PaperExecutionEngine::new(
        config,
        PaperClock::Replay { now_ms: 0 },
        [("btc", 10), ("jpy", 10_000)]
            .into_iter()
            .map(|(asset, amount)| BalanceSnapshot {
                asset: asset.into(),
                free_amount: amount.into(),
                locked_amount: 0.into(),
                onhand_amount: amount.into(),
            })
            .collect(),
        None,
    )
    .unwrap();
    PaperTradingVenue::with_latency(engine, latency)
        .unwrap()
        .with_simulation_config(SimulationConfig { lookahead_ms: 100 })
        .unwrap()
}
fn raw(channel: &str, data: Value) -> BitbankInboundMessage {
    decode_bitbank(
        &json!(["message", {"room_name":format!("{channel}_btc_jpy"),"message":{"data":data}}]),
    )
    .unwrap()
    .unwrap()
    .1
}
fn whole(time: i64, sequence: u64, ask: i64, bid: i64) -> BitbankInboundMessage {
    raw(
        "depth_whole",
        json!({"asks":[[ask.to_string(),"10"]],"bids":[[bid.to_string(),"10"]],
        "asks_over":"0","bids_under":"0","asks_under":"0","bids_over":"0","ask_market":"0","bid_market":"0",
        "timestamp":time,"sequenceId":sequence.to_string()}),
    )
}
fn diff(time: i64, sequence: u64, asks: Value) -> BitbankInboundMessage {
    raw(
        "depth_diff",
        json!({"a":asks,"b":[],"t":time,"s":sequence.to_string()}),
    )
}
fn trade(time: i64, id: i64) -> BitbankInboundMessage {
    raw(
        "transactions",
        json!({"transactions":[{"executed_at":time,"transaction_id":id,"side":"sell","amount":"1","price":"99"}]}),
    )
}
#[derive(Clone, Debug, PartialEq)]
struct Observation {
    receipt: i64,
    now: i64,
    ask: Option<Decimal>,
    accepted: Option<bool>,
}
struct Strategy {
    seen: Arc<Mutex<Vec<Observation>>>,
    first: bool,
    place: bool,
}
impl BotStrategy<PaperTradingVenue> for Strategy {
    type CustomEvent = ();
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<()>,
        ctx: &BotContext<(), PaperTradingVenue>,
    ) -> Result<(), String> {
        let ask = match &event {
            BotEvent::Market(MarketEvent::DepthUpdated { depth, .. }) => {
                use bitbankutil_rs::depth::Depth;
                depth.best_ask().map(|(p, _)| *p)
            }
            _ => None,
        };
        let receipt = ctx.received_at_ms();
        let mut accepted = None;
        if self.first && ask.is_some() {
            self.first = false;
            if self.place {
                let mut order = DesiredLimitOrder::limit(
                    "btc_jpy".into(),
                    OrderSide::Buy,
                    1.into(),
                    100.into(),
                );
                order.post_only = Some(true);
                accepted = Some(ctx.venue().place_order(order.into()).await.is_ok());
            }
        }
        self.seen.lock().unwrap().push(Observation {
            receipt,
            now: ctx.now_ms(),
            ask,
            accepted,
        });
        Ok(())
    }
}
fn runner(
    venue: PaperTradingVenue,
    place: bool,
) -> (
    BotRunner<Strategy, PaperTradingVenue>,
    Arc<Mutex<Vec<Observation>>>,
) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    (
        BotRunner::new(
            Strategy {
                seen: seen.clone(),
                first: true,
                place,
            },
            venue,
        ),
        seen,
    )
}
fn admit(driver: &mut SimulationDriver<()>, receipt: i64, message: BitbankInboundMessage) {
    driver
        .admit_bitbank(
            receipt,
            None,
            "connection".into(),
            "btc_jpy".into(),
            message,
        )
        .unwrap();
}

#[tokio::test]
async fn strategy_sees_old_book_but_post_only_uses_arrival_book() {
    let v = venue(
        0,
        PaperLatencyConfig {
            placement: PaperOperationLatency {
                entry_ms: 30,
                response_ms: 10,
            },
            ..Default::default()
        },
    );
    v.begin_simulation().unwrap();
    let (mut runner, seen) = runner(v.clone(), true);
    let mut driver =
        SimulationDriver::new(v.simulation_config().unwrap(), v.simulation_time_model()).unwrap();
    admit(&mut driver, 100, whole(90, 1, 110, 90));
    admit(
        &mut driver,
        160,
        diff(110, 2, json!([["110", "0"], ["99", "10"]])),
    );
    driver
        .advance_to(&mut runner, 200, None, &AtomicBool::new(false))
        .await
        .unwrap();
    assert!(seen.lock().unwrap().is_empty()); // ハンドラーは発注応答待ち。
    driver
        .advance_to(&mut runner, 300, None, &AtomicBool::new(false))
        .await
        .unwrap();
    let observations = seen.lock().unwrap();
    assert_eq!(
        observations[0],
        Observation {
            receipt: 100,
            now: 140,
            ask: Some(110.into()),
            accepted: Some(false)
        }
    );
    assert_eq!(observations[1].receipt, 160);
    assert_eq!(observations[1].ask, Some(99.into()));
    assert!(runner.paper_diagnostics().unwrap().open_orders().is_empty());
    assert!(
        runner
            .paper_diagnostics()
            .unwrap()
            .events()
            .iter()
            .any(|e| matches!(
                e,
                PaperEvent::OrderRejected {
                    reason: PaperRejectReason::PostOnlyWouldTakeLiquidity,
                    ..
                }
            ))
    );
}

#[tokio::test]
async fn raw_books_are_reconstructed_independently_and_sequence_gaps_are_allowed() {
    let v = venue(0, Default::default());
    v.begin_simulation().unwrap();
    let (mut runner, seen) = runner(v.clone(), false);
    let mut driver =
        SimulationDriver::new(v.simulation_config().unwrap(), Default::default()).unwrap();
    admit(&mut driver, 100, whole(90, 1, 110, 90));
    admit(
        &mut driver,
        160,
        diff(130, 8, json!([["110", "0"], ["115", "10"]])),
    );
    admit(
        &mut driver,
        170,
        diff(120, 4, json!([["110", "0"], ["112", "10"]])),
    );
    driver
        .advance_to(&mut runner, 230, None, &AtomicBool::new(false))
        .await
        .unwrap();
    assert_eq!(
        runner
            .paper_diagnostics()
            .unwrap()
            .settlement_status()
            .book
            .unwrap()
            .best_ask,
        Decimal::from(112)
    );
    // 内部: t120で112を追加し、t130で115を追加。戦略はt160まで何も知らない。
    assert_eq!(seen.lock().unwrap().len(), 1);
    driver
        .advance_to(&mut runner, 280, None, &AtomicBool::new(false))
        .await
        .unwrap();
    assert_eq!(seen.lock().unwrap()[1].ask, Some(115.into()));
    assert_eq!(seen.lock().unwrap()[2].ask, Some(115.into())); // 受信側は古いsequenceを適用しない。
}

#[tokio::test]
async fn fills_happen_at_occurrence_and_notification_precedes_public_receipt() {
    for delta in [-20, 0, 20] {
        let v = venue(
            delta,
            PaperLatencyConfig {
                notification_ms: 5,
                ..Default::default()
            },
        );
        v.begin_simulation().unwrap();
        let (mut runner, _) = runner(v.clone(), true);
        let mut driver =
            SimulationDriver::new(v.simulation_config().unwrap(), v.simulation_time_model())
                .unwrap();
        admit(&mut driver, 100, whole(90 + delta, 1, 110, 90));
        admit(&mut driver, 160, trade(120 + delta, 1));
        driver
            .advance_to(&mut runner, 220, None, &AtomicBool::new(false))
            .await
            .unwrap();
        assert!(runner.paper_diagnostics().unwrap().open_orders().is_empty());
        assert_eq!(v.known_account_snapshot().open_orders.len(), 1);
        driver
            .advance_to(&mut runner, 225, None, &AtomicBool::new(false))
            .await
            .unwrap();
        assert!(v.known_account_snapshot().open_orders.is_empty());
        assert_eq!(
            v.known_account_snapshot()
                .balances
                .iter()
                .find(|b| b.asset == "btc")
                .unwrap()
                .free_amount,
            Decimal::from(11)
        );
    }
}

#[test]
fn late_future_duplicate_conflict_and_invalid_config_are_explicit() {
    assert!(
        SimulationDriver::<()>::new(SimulationConfig { lookahead_ms: -1 }, Default::default())
            .is_err()
    );
    assert!(
        SimulationConfig { lookahead_ms: 1 }
            .progress_limit(i64::MIN)
            .is_err()
    );
    let mut driver =
        SimulationDriver::<()>::new(SimulationConfig { lookahead_ms: 100 }, Default::default())
            .unwrap();
    admit(&mut driver, 100, whole(90, 1, 110, 90));
    admit(&mut driver, 110, whole(90, 1, 110, 90));
    assert!(
        driver
            .admit_bitbank(
                110,
                None,
                "connection".into(),
                "btc_jpy".into(),
                whole(90, 1, 111, 90)
            )
            .unwrap_err()
            .contains("conflicting")
    );
    let mut driver =
        SimulationDriver::<()>::new(SimulationConfig { lookahead_ms: 100 }, Default::default())
            .unwrap();
    assert!(
        driver
            .admit_bitbank(
                100,
                None,
                "connection".into(),
                "btc_jpy".into(),
                whole(101, 1, 110, 90)
            )
            .unwrap_err()
            .contains("after receipt")
    );
    assert!(
        driver
            .admit_bitbank(
                200,
                None,
                "connection".into(),
                "btc_jpy".into(),
                trade(99, 1)
            )
            .unwrap_err()
            .contains("lookahead")
    );
}

#[tokio::test]
async fn offline_end_uses_shortest_source_tail_and_preserves_original_receipt() {
    let v = venue(0, Default::default());
    let (mut runner, seen) = runner(v, false);
    let records = vec![
        (
            0,
            CaptureRecord {
                format_version: 1,
                exchange: "bitbank".into(),
                stream: "public".into(),
                session_id: "bb".into(),
                connection_id: "a".into(),
                sequence: 1,
                received_at_micros: 80_000,
                payload: CapturePayload::Connected {
                    endpoint: "fixture".into(),
                    subscriptions: vec![],
                },
            },
        ),
        (
            1,
            CaptureRecord {
                format_version: 1,
                exchange: "bybit".into(),
                stream: "linear".into(),
                session_id: "by".into(),
                connection_id: "b".into(),
                sequence: 1,
                received_at_micros: 80_000,
                payload: CapturePayload::Connected {
                    endpoint: "fixture".into(),
                    subscriptions: vec![],
                },
            },
        ),
        (
            1,
            CaptureRecord {
                format_version: 1,
                exchange: "bybit".into(),
                stream: "linear".into(),
                session_id: "by".into(),
                connection_id: "b".into(),
                sequence: 2,
                received_at_micros: 100_000,
                payload: CapturePayload::Message { message: json!({}) },
            },
        ),
        (
            0,
            CaptureRecord {
                format_version: 1,
                exchange: "bitbank".into(),
                stream: "public".into(),
                session_id: "bb".into(),
                connection_id: "a".into(),
                sequence: 2,
                received_at_micros: 250_000,
                payload: CapturePayload::Message {
                    message: json!(["heartbeat"]),
                },
            },
        ),
        (
            1,
            CaptureRecord {
                format_version: 1,
                exchange: "bybit".into(),
                stream: "linear".into(),
                session_id: "by".into(),
                connection_id: "b".into(),
                sequence: 3,
                received_at_micros: 300_000,
                payload: CapturePayload::Message { message: json!({}) },
            },
        ),
    ];
    let report = run_replay(
        &mut runner,
        records.into_iter().map(Ok),
        false,
        &AtomicBool::new(false),
        |_| Ok(Some(())),
    )
    .await;
    assert!(report.error.is_none(), "{:?}", report.error);
    assert_eq!(report.final_time_ms, Some(150));
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(seen.lock().unwrap()[0].receipt, 100);
    assert_eq!(report.simulation.unwrap().config.lookahead_ms, 100);
}

#[tokio::test]
async fn cancellation_arriving_with_trade_excludes_that_millisecond_and_releases_funds() {
    struct Cancel;
    impl BotStrategy<PaperTradingVenue> for Cancel {
        type CustomEvent = ();
        type Error = String;
        async fn handle_event(
            &mut self,
            event: BotEvent<()>,
            ctx: &BotContext<(), PaperTradingVenue>,
        ) -> Result<(), String> {
            if matches!(event, BotEvent::Market(MarketEvent::DepthUpdated { .. })) {
                let placed = ctx
                    .venue()
                    .place_order(
                        DesiredLimitOrder::limit(
                            "btc_jpy".into(),
                            OrderSide::Buy,
                            1.into(),
                            100.into(),
                        )
                        .into(),
                    )
                    .await
                    .unwrap();
                ctx.venue()
                    .cancel_orders("btc_jpy", vec![placed.order_id.unwrap()])
                    .await
                    .unwrap();
            }
            Ok(())
        }
    }
    for entry_ms in [0, 20] {
        let v = venue(
            0,
            PaperLatencyConfig {
                cancellation: PaperOperationLatency {
                    entry_ms,
                    response_ms: 10,
                },
                ..Default::default()
            },
        );
        v.begin_simulation().unwrap();
        let mut runner = BotRunner::new(Cancel, v.clone());
        let mut driver =
            SimulationDriver::new(v.simulation_config().unwrap(), Default::default()).unwrap();
        admit(&mut driver, 100, whole(90, 1, 110, 90));
        admit(&mut driver, 160, trade(120, 1));
        driver
            .advance_to(&mut runner, 300, None, &AtomicBool::new(false))
            .await
            .unwrap();
        let snapshot = runner.paper_diagnostics().unwrap();
        assert!(snapshot.open_orders().is_empty());
        assert!(
            !snapshot
                .events()
                .iter()
                .any(|e| matches!(e, PaperEvent::OrderFilled { .. }))
        );
        assert_eq!(snapshot.settlement_status().pending_cancellations, 0);
        assert_eq!(
            snapshot
                .balances()
                .iter()
                .find(|b| b.asset == "jpy")
                .unwrap()
                .free_amount,
            Decimal::from(10_000)
        );
        assert!(!v.paper_settlement_status().unwrap().unsettled);
    }
}

#[tokio::test]
async fn strategy_handle_cannot_read_or_drive_internal_market_state() {
    struct Restricted;
    impl BotStrategy<PaperTradingVenue> for Restricted {
        type CustomEvent = ();
        type Error = String;
        async fn handle_event(
            &mut self,
            event: BotEvent<()>,
            ctx: &BotContext<(), PaperTradingVenue>,
        ) -> Result<(), String> {
            if matches!(event, BotEvent::Custom(_)) {
                ctx.emit(BotEvent::Market(MarketEvent::DepthUpdated {
                    pair: "btc_jpy".into(),
                    depth: bitbankutil_rs::market_event::MarketDepthSnapshot::new(
                        [(7.into(), 1.0)].into(),
                        [(6.into(), 1.0)].into(),
                        100,
                    ),
                }))
                .await
                .unwrap();
            }
            let v = ctx.venue();
            assert_eq!(format!("{v:?}"), "PaperTradingVenue { .. }");
            assert!(v.paper_settlement_status().is_none());
            assert!(v.begin_simulation().is_err());
            assert!(v.advance_replay_time(1_000).is_err());
            assert!(v.process_internal_events(1_000).is_err());
            assert!(v.advance_trade_watermark(1_000).is_err());
            assert!(
                v.clone()
                    .with_simulation_config(SimulationConfig { lookahead_ms: 0 })
                    .is_err()
            );
            assert!(
                BotRunner::new(Restricted, v.clone())
                    .paper_diagnostics()
                    .is_none()
            );
            Ok(())
        }
    }
    let v = venue(0, Default::default());
    v.begin_simulation().unwrap();
    let mut runner = BotRunner::new(Restricted, v.clone());
    let mut driver =
        SimulationDriver::new(v.simulation_config().unwrap(), Default::default()).unwrap();
    driver.admit_event(100, None, BotEvent::Custom(())).unwrap();
    driver
        .advance_to(&mut runner, 200, None, &AtomicBool::new(false))
        .await
        .unwrap();
    assert!(
        runner
            .paper_diagnostics()
            .unwrap()
            .settlement_status()
            .book
            .is_none()
    );
}

#[tokio::test]
async fn late_input_is_sticky_and_identical_old_retransmission_is_ignored() {
    let v = venue(0, Default::default());
    v.begin_simulation().unwrap();
    let (mut runner, _) = runner(v.clone(), false);
    let mut driver =
        SimulationDriver::new(v.simulation_config().unwrap(), Default::default()).unwrap();
    admit(&mut driver, 100, whole(90, 1, 110, 90));
    driver
        .advance_to(&mut runner, 300, None, &AtomicBool::new(false))
        .await
        .unwrap();
    admit(&mut driver, 310, whole(90, 1, 110, 90));
    assert!(
        driver
            .admit_bitbank(
                310,
                None,
                "connection".into(),
                "btc_jpy".into(),
                trade(190, 1)
            )
            .is_err()
    );
    driver
        .advance_to(&mut runner, 310, None, &AtomicBool::new(false))
        .await
        .unwrap();
    assert!(!v.paper_settlement_status().unwrap().issues.is_empty());
    let mut order = Box::pin(v.place_order(
        DesiredLimitOrder::limit("btc_jpy".into(), OrderSide::Buy, 1.into(), 100.into()).into(),
    ));
    // request Futureをpollしてから内部期限を進める。
    std::future::poll_fn(|cx| {
        let _ = order.as_mut().poll(cx);
        std::task::Poll::Ready(())
    })
    .await;
    v.process_internal_events(210).unwrap();
    assert!(order.await.is_err());
}

#[tokio::test]
async fn fake_realtime_paper_and_offline_replay_have_identical_observations_and_finances() {
    let v = venue(
        0,
        PaperLatencyConfig {
            notification_ms: 5,
            ..Default::default()
        },
    );
    v.begin_simulation().unwrap();
    let (mut paper, paper_seen) = runner(v.clone(), true);
    let mut driver =
        SimulationDriver::new(v.simulation_config().unwrap(), Default::default()).unwrap();
    for wall in 100..=300 {
        if wall == 100 {
            admit(&mut driver, wall, whole(90, 1, 110, 90));
        }
        if wall == 160 {
            admit(&mut driver, wall, trade(120, 1));
        }
        driver
            .advance_to(&mut paper, wall, None, &AtomicBool::new(false))
            .await
            .unwrap();
    }
    let v = venue(
        0,
        PaperLatencyConfig {
            notification_ms: 5,
            ..Default::default()
        },
    );
    let (mut offline, offline_seen) = runner(v, true);
    let record = |sequence: u64, received: u64, payload| {
        Ok((
            0,
            CaptureRecord {
                format_version: 1,
                exchange: "bitbank".into(),
                stream: "public".into(),
                session_id: "fixture".into(),
                connection_id: "connection".into(),
                sequence,
                received_at_micros: received * 1000,
                payload,
            },
        ))
    };
    let input = vec![
        record(
            1,
            80,
            CapturePayload::Connected {
                endpoint: "fixture".into(),
                subscriptions: vec![],
            },
        ),
        record(
            2,
            100,
            CapturePayload::Message {
                message: json!(["message", {"room_name":"depth_whole_btc_jpy","message":{"data":{
                    "asks":[["110","10"]],"bids":[["90","10"]],"asks_over":"0","bids_under":"0","asks_under":"0","bids_over":"0","ask_market":"0","bid_market":"0","timestamp":90,"sequenceId":"1"
                }}}]),
            },
        ),
        record(
            3,
            160,
            CapturePayload::Message {
                message: json!(["message", {"room_name":"transactions_btc_jpy","message":{"data":{"transactions":[{"executed_at":120,"transaction_id":1,"side":"sell","amount":"1","price":"99"}]}}}]),
            },
        ),
        record(
            4,
            300,
            CapturePayload::Message {
                message: json!(["heartbeat"]),
            },
        ),
    ];
    let report = run_replay(&mut offline, input, false, &AtomicBool::new(false), |_| {
        Ok(None)
    })
    .await;
    assert!(report.error.is_none(), "{report:?}");
    assert_eq!(*paper_seen.lock().unwrap(), *offline_seen.lock().unwrap());
    let a = paper.paper_diagnostics().unwrap();
    let b = offline.paper_diagnostics().unwrap();
    assert_eq!(a.balances(), b.balances());
    assert_eq!(a.open_orders(), b.open_orders());
    assert_eq!(a.events(), b.events());
    assert_eq!(a.settlement_status(), b.settlement_status());
}
