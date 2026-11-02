use bitbankutil_rs::{
    bitbank_bot::{BotContext, BotEvent, BotRunner, BotStrategy},
    execution_recording::{ExecutionRecorder, JsonlSink},
    market_event::{MarketDepthSnapshot, MarketEvent, MarketTrade},
    order_domain::{BalanceSnapshot, DesiredLimitOrder, OrderId, OrderSide},
    paper_execution::{
        PaperClock, PaperEvent, PaperExecutionConfig, PaperExecutionEngine, PaperTradingVenue,
    },
    replay::{CaptureRecord, run_replay},
    trading_venue::{AccountSnapshot, TradingVenue},
};
use serde_json::json;
use std::{collections::BTreeMap, sync::atomic::AtomicBool};

fn engine(clock: PaperClock, recording: bool) -> PaperExecutionEngine {
    PaperExecutionEngine::new(
        PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap(),
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
            .enumerate()
            .map(|(i, t)| MarketTrade {
                amount: rust_decimal::Decimal::new(5, 1),
                price: 100.into(),
                side: OrderSide::Buy,
                executed_at: *t,
                transaction_id: i as i64,
            })
            .collect(),
    }
}
fn book(time: i64) -> MarketEvent {
    MarketEvent::DepthUpdated {
        pair: "btc_jpy".into(),
        depth: MarketDepthSnapshot::new(BTreeMap::new(), BTreeMap::new(), time),
    }
}
fn filled_ids(events: Vec<PaperEvent>) -> Vec<OrderId> {
    events
        .into_iter()
        .map(|e| match e {
            PaperEvent::OrderFilled { order_id, .. } => order_id,
            _ => panic!(),
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
        // 遅延した過去の約定は古い注文だけを部分約定させる。
        assert_eq!(
            filled_ids(e.apply_market_event(&trades(&[1000, 900, 1110]))),
            [OrderId(1), OrderId(1)]
        );
        assert_eq!(e.open_orders()[0].order_id, OrderId(2));
        // 同一メッセージ内の有効開始前・同一ms・開始後を個別に判定。
        assert_eq!(
            filled_ids(e.apply_market_event(&trades(&[1100, 1110, 1111]))),
            [OrderId(2)]
        );
        e.cancel_orders("wrong_pair", vec![OrderId(2)]);
        assert_eq!(e.open_orders().len(), 1);
        e.cancel_orders("btc_jpy", vec![OrderId(2)]);
        e.advance_replay_time(1200).unwrap();
        e.place_order(order()).unwrap();
        assert!(e.apply_market_event(&trades(&[1111, 1200])).is_empty());
        assert_eq!(
            filled_ids(e.apply_market_event(&trades(&[1201, 1202]))),
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
    assert_eq!(e.apply_market_event(&trades(&[1101])).len(), 1);
}
#[tokio::test]
async fn realtime_venue_uses_acceptance_clock_without_recording_context() {
    for recording in [false, true] {
        let v = PaperTradingVenue::new(engine(PaperClock::Realtime, recording));
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
        v.on_market_event(&trades(&[after - 110, after])).unwrap();
        assert_eq!(
            v.account_snapshot("btc_jpy").await.unwrap().open_orders[0].remaining_amount,
            1.into()
        );
        v.on_market_event(&trades(&[after + 1, after + 2])).unwrap();
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
struct Strategy(bool);
impl BotStrategy<PaperTradingVenue> for Strategy {
    type CustomEvent = ();
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<()>,
        ctx: &BotContext<(), PaperTradingVenue>,
    ) -> Result<(), String> {
        if matches!(event, BotEvent::Market(MarketEvent::DepthUpdated { .. })) && !self.0 {
            assert_eq!(ctx.now_ms(), 1110);
            ctx.venue()
                .place_order(order().into())
                .await
                .map_err(|e| format!("{e:?}"))?;
            self.0 = true;
        }
        Ok(())
    }
}
async fn replay(recording: bool, slow: bool) -> (AccountSnapshot, Vec<PaperEvent>) {
    let v = PaperTradingVenue::new(engine(PaperClock::Replay { now_ms: 0 }, recording));
    let mut runner = BotRunner::new(Strategy(false), v.clone());
    let messages = [
        (
            900,
            json!({"kind":"connected", "endpoint":"test", "subscriptions":["depth_whole_btc_jpy", "transactions_btc_jpy"]}),
        ),
        (
            1110,
            json!({"kind":"message", "message":["message", {"room_name":"depth_whole_btc_jpy", "message":{"data":{
                "asks":[],"bids":[],"asks_over":"0","bids_under":"0","asks_under":"0","bids_over":"0","ask_market":"0","bid_market":"0","timestamp":1100,"sequenceId":"1"
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
    ];
    let input = messages.into_iter().enumerate().map(|(i, (time, payload))| {
        if slow { std::thread::sleep(std::time::Duration::from_millis(3)); }
        let mut r = json!({"format_version":1,"exchange":"bitbank","stream":"public","session_id":"test","connection_id":"one","sequence":i+1,"received_at_micros":time*1000});
        r.as_object_mut().unwrap().extend(payload.as_object().unwrap().clone());
        Ok((0, serde_json::from_value::<CaptureRecord>(r).unwrap()))
    });
    let report = run_replay(&mut runner, input, false, &AtomicBool::new(false), |_| {
        Ok(None)
    })
    .await;
    assert!(report.error.is_none(), "{:?}", report.error);
    assert_eq!(report.records, 4);
    let account = v.account_snapshot("btc_jpy").await.unwrap();
    assert_eq!(
        account.open_orders[0].remaining_amount,
        rust_decimal::Decimal::new(5, 1)
    );
    let events = v.engine().lock().unwrap().drain_events();
    (account, events)
}
#[tokio::test]
async fn replay_is_independent_of_wall_time_speed_and_recorder() {
    let expected = replay(false, false).await;
    assert_eq!(expected, replay(false, true).await);
    assert_eq!(expected, replay(true, true).await);
}
