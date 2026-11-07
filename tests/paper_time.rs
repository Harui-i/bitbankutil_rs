use bitbankutil_rs::book_units::literal;
mod support;
use bitbankutil_rs::{
    bitbank_bot::{BotContext, BotEvent, BotRunner, BotStrategy},
    execution_recording::{ExecutionRecorder, JsonlSink},
    market_event::{MarketDepthSnapshot, MarketEvent, MarketTrade},
    order_domain::{BalanceSnapshot, DesiredLimitOrder, OrderId, OrderSide},
    paper_execution::{
        PaperClock, PaperEvent, PaperExecutionConfig, PaperExecutionEngine, PaperExecutionError,
        PaperTimeModel, PaperTradingVenue,
    },
    replay::{CaptureRecord, run_replay},
    trading_venue::{AccountSnapshot, TradingVenue},
};
use serde_json::json;
use std::{collections::BTreeMap, sync::atomic::AtomicBool};
use support::TestDiagnostics;

fn engine(clock: PaperClock, recording: bool) -> PaperExecutionEngine {
    offset_engine(clock, recording, 0)
}
fn model(delta: i64) -> PaperTimeModel {
    PaperTimeModel::FixedOffset {
        exchange_minus_local_ms: delta,
    }
}
fn offset_engine(clock: PaperClock, recording: bool, delta: i64) -> PaperExecutionEngine {
    let initial_local_ms = match &clock {
        PaperClock::Replay { now_ms } => *now_ms,
        PaperClock::Realtime => bitbankutil_rs::execution_recording::timestamp_ms() as i64,
    };
    let exchange_ms = initial_local_ms.checked_add(delta).unwrap();
    let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
    config.time_model = model(delta);
    PaperExecutionEngine::new(
        config,
        clock,
        [("btc", 10), ("jpy", 1000)]
            .into_iter()
            .map(|(asset, n)| BalanceSnapshot {
                asset: asset.into(),
                free_amount: n.into(),
                locked_amount: 0.into(),
                onhand_amount: n.into(),
            })
            .collect(),
        recording.then(|| ExecutionRecorder::new("time", json!({}), JsonlSink(std::io::sink()))),
    )
    .map(|engine| seeded_engine(engine, exchange_ms))
    .unwrap()
}
fn order() -> DesiredLimitOrder {
    DesiredLimitOrder::limit("btc_jpy".into(), OrderSide::Sell, 1.into(), 100.into())
}
fn trades(times: &[i64]) -> MarketEvent {
    MarketEvent::Transactions {
        pair: "btc_jpy".into(),
        transactions: times
            .iter()
            .map(|t| MarketTrade {
                amount: literal("0.5"),
                price: 100.into(),
                side: OrderSide::Buy,
                executed_at: *t,
                transaction_id: *t,
            })
            .collect(),
    }
}
fn book(time: i64) -> MarketEvent {
    MarketEvent::DepthUpdated {
        pair: "btc_jpy".into(),
        depth: MarketDepthSnapshot::new(
            BTreeMap::from([(1_000_000.into(), literal("1.0"))]),
            BTreeMap::from([(1.into(), literal("1.0"))]),
            time,
        ),
    }
}
fn filled_ids(events: Vec<PaperEvent>) -> Vec<OrderId> {
    events
        .into_iter()
        .filter_map(|e| match e {
            PaperEvent::OrderFilled { order_id, .. } => Some(order_id),
            _ => None,
        })
        .collect()
}
#[test]
fn mixed_trades_partial_fills_cancel_and_replace_preserve_activation() {
    for recording in [false, true] {
        let mut e = engine(PaperClock::Replay { now_ms: 900 }, recording);
        e.place_order(order()).unwrap();
        e.apply_market_event(&book(1100));
        e.advance_replay_time(1110).unwrap();
        e.place_order(order()).unwrap();
        e.apply_market_event(&trades(&[1000, 900, 1110]));
        e.apply_market_event(&trades(&[1100, 1110, 1111]));
        assert_eq!(
            filled_ids(e.advance_trade_watermark(1111).unwrap()),
            [OrderId(1), OrderId(1), OrderId(2)]
        );
        e.cancel_orders("wrong_pair", vec![OrderId(2)]);
        assert_eq!(e.open_orders().len(), 1);
        e.cancel_orders("btc_jpy", vec![OrderId(2)]);
        e.advance_replay_time(1200).unwrap();
        e.place_order(order()).unwrap();
        e.apply_market_event(&trades(&[1111, 1200]));
        e.apply_market_event(&trades(&[1201, 1202]));
        assert_eq!(
            filled_ids(e.advance_trade_watermark(1202).unwrap()),
            [OrderId(3), OrderId(3)]
        );
        assert!(e.open_orders().is_empty());
        assert!(e.advance_replay_time(1199).is_err());
    }
}
#[test]
fn observed_exchange_book_time_is_a_floor_even_when_local_clock_lags() {
    let mut e = engine(PaperClock::Replay { now_ms: 100 }, false);
    e.apply_market_event(&book(1100));
    e.apply_market_event(&book(1000));
    e.place_order(order()).unwrap();
    assert!(e.apply_market_event(&trades(&[1099, 1100])).is_empty());
    e.apply_market_event(&trades(&[1101]));
    assert_eq!(e.advance_trade_watermark(1101).unwrap().len(), 1);
}
#[tokio::test]
async fn realtime_venue_uses_acceptance_clock_without_recording_context() {
    for delta in [0, 250, -250] {
        for recording in [false, true] {
            let v = PaperTradingVenue::new(offset_engine(PaperClock::Realtime, recording, delta));
            let before = bitbankutil_rs::execution_recording::timestamp_ms() as i64;
            v.place_order(order().into()).await.unwrap();
            let shared = v.engine();
            let after = shared
                .lock()
                .unwrap()
                .drain_events()
                .into_iter()
                .find_map(|e| match e {
                    PaperEvent::OrderAccepted {
                        effective_after_ms, ..
                    } => Some(effective_after_ms),
                    _ => None,
                })
                .unwrap();
            let local_after = bitbankutil_rs::execution_recording::timestamp_ms() as i64;
            assert!((before + delta..=local_after + delta).contains(&after));
            v.on_market_event(&trades(&[after - 110, after])).unwrap();
            assert_eq!(
                v.account_snapshot("btc_jpy").await.unwrap().open_orders[0].remaining_amount,
                1.into()
            );
            v.on_market_event(&trades(&[after + 1, after + 2])).unwrap();
            v.advance_trade_watermark(after + 2).unwrap();
            assert!(
                v.account_snapshot("btc_jpy")
                    .await
                    .unwrap()
                    .open_orders
                    .is_empty()
            );
            assert!(v.advance_replay_time(after).is_err());
        }
    }
}
struct Strategy(bool, i64);
impl BotStrategy<PaperTradingVenue> for Strategy {
    type CustomEvent = ();
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<()>,
        ctx: &BotContext<(), PaperTradingVenue>,
    ) -> Result<(), String> {
        if matches!(event, BotEvent::Market(MarketEvent::DepthUpdated { .. })) && !self.0 {
            assert_eq!(ctx.now_ms(), 1110 - self.1);
            ctx.venue()
                .place_order(order().into())
                .await
                .map_err(|e| format!("{e:?}"))?;
            self.0 = true;
        }
        Ok(())
    }
}
async fn replay(recording: bool, slow: bool, delta: i64) -> (AccountSnapshot, Vec<PaperEvent>) {
    let v = PaperTradingVenue::new(offset_engine(
        PaperClock::Replay { now_ms: -delta },
        recording,
        delta,
    ));
    let mut runner = BotRunner::new(Strategy(false, delta), v.clone());
    let messages = [
        (
            900,
            json!({"kind":"connected", "endpoint":"test", "subscriptions":["depth_whole_btc_jpy", "transactions_btc_jpy"]}),
        ),
        (
            1110,
            json!({"kind":"message", "message":["message", {"room_name":"depth_whole_btc_jpy", "message":{"data":{
                "asks":[["1000000","1"]],"bids":[["1","1"]],"asks_over":"0","bids_under":"0","asks_under":"0","bids_over":"0","ask_market":"0","bid_market":"0","timestamp":1100,"sequenceId":"1"
            }}}]}),
        ),
        (
            1200,
            json!({"kind":"message", "message":["message", {"room_name":"transactions_btc_jpy", "message":{"data":{"transactions":[
                {"transaction_id":1,"side":"buy","price":"100","amount":"1","executed_at":1000}
            ]}}}]}),
        ),
        (
            1300,
            json!({"kind":"message", "message":["message", {"room_name":"transactions_btc_jpy", "message":{"data":{"transactions":[
                {"transaction_id":2,"side":"buy","price":"100","amount":"0.5","executed_at":1111}
            ]}}}]}),
        ),
        (1600, json!({"kind":"message", "message":["heartbeat"]})),
    ];
    let input = messages.into_iter().enumerate().map(|(i, (time, payload))| {
        if slow { std::thread::sleep(std::time::Duration::from_millis(3)); }
        // 同ms内の端数は切り捨て、入力・戦略の時計にはδを適用しない。
        let mut r = json!({"format_version":1,"exchange":"bitbank","stream":"public","session_id":"test","connection_id":"one","sequence":i+1,"received_at_micros":(time-delta)*1000+999});
        r.as_object_mut().unwrap().extend(payload.as_object().unwrap().clone());
        Ok((0, serde_json::from_value::<CaptureRecord>(r).unwrap()))
    });
    support::configure(runner.venue(), 300);
    let report = run_replay(&mut runner, input, false, &AtomicBool::new(false), |_| {
        Ok(None)
    })
    .await;
    assert!(report.error.is_none(), "{:?}", report.error);
    assert_eq!(report.records, 5);
    assert_eq!(report.settlement.as_ref().unwrap().time_model, model(delta));
    assert!(!report.incomplete); // 待機時間を引いた境界まで評価済み。
    v.advance_trade_watermark(1300).unwrap(); // この合成入力の既知の完了境界。
    let mut account = v.account_snapshot("btc_jpy").await.unwrap();
    // 比較対象は取引結果。ローカル採取時刻のδ依存は照会の専用テストで検証する。
    account.query = None;
    assert_eq!(account.open_orders[0].remaining_amount, literal("0.5"));
    let events = v.engine().lock().unwrap().drain_events();
    (account, events)
}
#[tokio::test]
async fn replay_is_independent_of_wall_time_speed_and_recorder() {
    let expected = replay(false, false, 0).await;
    for delta in [0, 250, -250] {
        assert_eq!(expected, replay(false, true, delta).await);
        assert_eq!(expected, replay(true, true, delta).await);
    }
}

#[test]
fn offset_preserves_exchange_causality_and_late_cancellation() {
    let mut expected = None;
    for delta in [0, 250, -250] {
        for recording in [false, true] {
            let mut e = offset_engine(
                PaperClock::Replay {
                    now_ms: 1000 - delta,
                },
                recording,
                delta,
            );
            e.apply_market_event(&book(1005));
            e.place_order(order()).unwrap();
            e.advance_replay_time(1100 - delta).unwrap();
            e.cancel_orders("btc_jpy", vec![OrderId(1)]);
            e.advance_replay_time(1200 - delta).unwrap();
            e.place_order(order()).unwrap();
            e.apply_market_event(&trades(&[1004, 1005, 1006, 1100, 1199, 1200, 1201]));
            let events = e.advance_trade_watermark(1300).unwrap();
            assert_eq!(filled_ids(events), [OrderId(1), OrderId(2)]);
            assert_eq!(e.open_orders().len(), 1);
            assert_eq!(e.open_orders()[0].remaining_amount, literal("0.5"));
            assert!(!e.settlement_status().unsettled);
            let result = (e.balances(), e.open_orders(), e.drain_events());
            if let Some(expected) = &expected {
                assert_eq!(&result, expected);
            } else {
                expected = Some(result);
            }
        }
    }
}

#[test]
fn automatic_watermark_and_delay_limit_use_exchange_clock() {
    for delta in [0, 250, -250] {
        let mut e = offset_engine(
            PaperClock::Replay {
                now_ms: 1000 - delta,
            },
            false,
            delta,
        )
        .with_max_trade_delay_ms(100)
        .unwrap();
        e.place_order(order()).unwrap();
        e.advance_replay_time(1102 - delta).unwrap();
        e.apply_market_event(&trades(&[1002])); // ちょうど上限: まだ未確定。
        assert_eq!(e.settlement_status().watermark_ms, Some(1001));
        assert_eq!(e.settlement_status().buffered_trades, 1);
        assert!(e.settlement_status().issues.is_empty());
        e.advance_replay_time(1103 - delta).unwrap();
        assert_eq!(filled_ids(e.apply_market_event(&book(1000))), [OrderId(1)]);
        assert_eq!(e.settlement_status().watermark_ms, Some(1002));
        e.apply_market_event(&trades(&[1001])); // 封鎖済み境界への遅着。
        assert!(!e.settlement_status().issues.is_empty());

        let mut e = offset_engine(
            PaperClock::Replay {
                now_ms: 1102 - delta,
            },
            false,
            delta,
        )
        .with_max_trade_delay_ms(100)
        .unwrap();
        e.apply_market_event(&trades(&[1001])); // watermarkの有無に関係なく上限超過。
        assert!(e.settlement_status().issues[0].contains("exceeded configured delay"));
        assert_eq!(e.settlement_status().watermark_ms, None);
    }
}

#[test]
fn extreme_offsets_and_arithmetic_do_not_wrap_or_saturate() {
    let initial = engine(PaperClock::Replay { now_ms: 0 }, false);
    let mut config = initial.config().clone();
    config.time_model = model(i64::MAX);
    assert!(matches!(
        PaperExecutionEngine::new(
            config,
            PaperClock::Replay { now_ms: 1 },
            initial.balances(),
            None
        ),
        Err(PaperExecutionError::InvalidClock)
    ));
    assert_eq!(model(i64::MAX).local_to_exchange_ms(0), Ok(i64::MAX));
    assert_eq!(model(i64::MIN).local_to_exchange_ms(0), Ok(i64::MIN));
    assert_eq!(
        model(i64::MAX).local_to_exchange_ms(1),
        Err(PaperExecutionError::InvalidClock)
    );
    assert_eq!(
        model(i64::MIN).local_to_exchange_ms(-1),
        Err(PaperExecutionError::InvalidClock)
    );
    let mut e = offset_engine(PaperClock::Replay { now_ms: 0 }, false, i64::MAX);
    assert_eq!(
        e.advance_replay_time(1),
        Err(PaperExecutionError::InvalidClock)
    );
    e.place_order(order()).unwrap();
    assert!(matches!(
        e.drain_events()[0],
        PaperEvent::OrderAccepted {
            effective_after_ms: i64::MAX,
            ..
        }
    ));
    assert_eq!(
        e.advance_replay_time(-1),
        Err(PaperExecutionError::InvalidClock)
    );
    e.advance_trade_watermark(i64::MAX).unwrap();
    e.cancel_orders("btc_jpy", vec![OrderId(1)]);
    assert!(e.settlement_status().issues[0].contains("terminal watermark"));

    for delay in [0, 1] {
        let mut e = offset_engine(PaperClock::Replay { now_ms: 0 }, false, i64::MIN)
            .with_max_trade_delay_ms(delay)
            .unwrap();
        e.apply_market_event(&book(i64::MIN));
        assert!(e.settlement_status().issues[0].contains("overflow"));
        assert_eq!(e.settlement_status().watermark_ms, None);
    }
    assert!(
        engine(PaperClock::Replay { now_ms: 0 }, false)
            .with_max_trade_delay_ms(-1)
            .is_err()
    );
}

#[tokio::test]
async fn recording_preserves_local_context_and_exchange_times_with_model() {
    use bitbankutil_rs::execution_recording::{
        EventSink, ExecutionRecord, RecordingContext, scope,
    };
    use std::sync::{Arc, Mutex};
    #[derive(Clone)]
    struct Sink(Arc<Mutex<Vec<ExecutionRecord>>>);
    impl EventSink for Sink {
        fn append(&mut self, record: &ExecutionRecord) -> std::io::Result<()> {
            self.0.lock().unwrap().push(record.clone());
            Ok(())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for delta in [0, 250, -250] {
        let records = Sink(Arc::new(Mutex::new(Vec::new())));
        let recorder = ExecutionRecorder::new("offset", json!({}), records.clone());
        let initial = offset_engine(
            PaperClock::Replay {
                now_ms: 1000 - delta,
            },
            false,
            delta,
        );
        let config = initial.config().clone();
        let serialized = serde_json::to_value(&config).unwrap();
        assert_eq!(
            serde_json::from_value::<PaperExecutionConfig>(serialized).unwrap(),
            config
        );
        let mut e = PaperExecutionEngine::new(
            config,
            PaperClock::Replay {
                now_ms: 1000 - delta,
            },
            initial.balances(),
            Some(recorder.clone()),
        )
        .map(|engine| seeded_engine(engine, 1000))
        .unwrap();
        scope(
            RecordingContext {
                event_time_ms: Some(1000 - delta),
                ..Default::default()
            },
            async {
                e.place_order(order()).unwrap();
                e.apply_market_event(&trades(&[1001]));
                e.advance_trade_watermark(1001).unwrap();
            },
        )
        .await;
        recorder.finish().unwrap();
        let records = records.0.lock().unwrap();
        let initial = records
            .iter()
            .find(|r| r.kind == "paper_initial_state")
            .unwrap();
        assert_eq!(
            initial.data["config"]["time_model"],
            json!({"model":"fixed_offset", "exchange_minus_local_ms":delta})
        );
        assert_eq!(initial.data["clock"]["Replay"]["now_ms"], 1000 - delta);
        let events: Vec<_> = records.iter().filter(|r| r.kind == "paper_event").collect();
        assert!(events.iter().all(|r| r.event_time_ms == Some(1000 - delta)));
        assert_eq!(
            events[0].data["event"]["OrderAccepted"]["effective_after_ms"],
            1000
        );
        assert_eq!(
            events[1].data["event"]["OrderFilled"]["trade"]["executed_at"],
            1001
        );
        let status = records
            .iter()
            .rfind(|r| r.kind == "paper_settlement")
            .unwrap();
        assert_eq!(
            status.data["time_model"],
            initial.data["config"]["time_model"]
        );
        assert_eq!(status.data["watermark_ms"], 1001);
    }
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
