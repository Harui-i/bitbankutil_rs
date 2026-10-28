use bitbankutil_rs::{
    bitbank_bot::*, depth::Depth, market_event::MarketEvent, replay::*,
    trading_venue::NoTradingVenue,
};
use std::sync::{Arc, Mutex, atomic::AtomicBool};
fn manifest() -> String {
    format!(
        "{}/fixtures/market-data-v1/manifest.json",
        env!("CARGO_MANIFEST_DIR")
    )
}
#[test]
fn fixture_merge_and_books() {
    let input = ReplayInput::open(manifest()).unwrap();
    let expected: serde_json::Value =
        serde_json::from_reader(std::fs::File::open(manifest()).unwrap()).unwrap();
    let mut adapter = ReplayAdapter::default();
    let mut ids = Vec::new();
    let mut checkpoints = Vec::new();
    let mut custom = 0;
    for item in input {
        let (source, r) = item.unwrap();
        ids.push(serde_json::json!([r.session_id, r.sequence]));
        let event = adapter
            .adapt(source, &r, &mut |_| {
                custom += 1;
                Ok(Some(()))
            })
            .unwrap();
        if let Some(BotEvent::Market(MarketEvent::DepthUpdated { depth, .. })) = event {
            checkpoints.push((
                r.sequence,
                depth.best_bid().unwrap().0.to_string(),
                *depth.best_bid().unwrap().1,
                depth.best_ask().unwrap().0.to_string(),
                *depth.best_ask().unwrap().1,
            ));
        }
    }
    assert_eq!(
        serde_json::json!(ids),
        expected["expected"]["merged_record_ids"]
    );
    assert_eq!(
        checkpoints,
        vec![
            (3, "100".into(), 0.4, "101".into(), 1.5),
            (4, "99.5".into(), 2.0, "101".into(), 1.5),
            (9, "98".into(), 3.0, "103".into(), 2.0)
        ]
    );
    assert_eq!(custom, 3);
}
struct Probe(Arc<Mutex<Vec<(i64, u8)>>>);
impl BotStrategy for Probe {
    type CustomEvent = u8;
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<u8>,
        ctx: &BotContext<u8>,
    ) -> Result<(), String> {
        if let BotEvent::Custom(n) = event {
            self.0.lock().unwrap().push((ctx.now_ms(), n));
            if n == 0 {
                ctx.emit(BotEvent::Custom(1)).await.unwrap();
                ctx.emit(BotEvent::Custom(2)).await.unwrap();
            }
            if n == 1 {
                ctx.emit(BotEvent::Custom(3)).await.unwrap();
            }
        }
        Ok(())
    }
}
#[tokio::test]
async fn derived_fifo_and_time_reversal() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut runner = BotRunner::new(Probe(seen.clone()), NoTradingVenue);
    runner
        .process(100, None, BotEvent::Custom(0))
        .await
        .unwrap();
    runner
        .process(100, None, BotEvent::Custom(4))
        .await
        .unwrap();
    assert!(matches!(
        runner.process(99, None, BotEvent::Custom(5)).await,
        Err(BotRunError::TimeReversed)
    ));
    assert_eq!(
        *seen.lock().unwrap(),
        [(100, 0), (100, 1), (100, 2), (100, 3), (100, 4)]
    );
}
#[tokio::test]
async fn disconnect_recovery_and_cancel() {
    let mut runner = BotRunner::new(Probe(Arc::default()), NoTradingVenue);
    let report = run_replay(
        &mut runner,
        ReplayInput::open(manifest()).unwrap(),
        false,
        &AtomicBool::new(false),
        |_| Ok(None),
    )
    .await;
    assert!(report.incomplete);
    assert_eq!(report.last_input.unwrap().sequence, 6);
    assert!(report.error.is_none());
    let mut runner = BotRunner::new(Probe(Arc::default()), NoTradingVenue);
    let report = run_replay(
        &mut runner,
        ReplayInput::open(manifest()).unwrap(),
        true,
        &AtomicBool::new(false),
        |_| Ok(None),
    )
    .await;
    assert!(report.incomplete);
    assert_eq!(report.records, 14);
    assert_eq!(report.gaps.len(), 2);
    let report = run_replay(
        &mut runner,
        ReplayInput::open(manifest()).unwrap(),
        true,
        &AtomicBool::new(true),
        |_| Ok(None),
    )
    .await;
    assert!(report.interrupted);
    assert_eq!(report.records, 0);
}
#[test]
fn malformed_depth_is_error() {
    let mut converter = BitbankMarketEventConverter::new("btc_jpy".into());
    let raw = serde_json::json!(["message",{"room_name":"depth_diff_btc_jpy","message":{"data":{"a":[["bad"]],"b":[],"t":0,"s":"9"}}}]);
    let (_, message) = decode_bitbank(&raw).unwrap().unwrap();
    assert!(converter.convert(message).is_err());
}

#[tokio::test]
async fn handler_completion_freezes_clock_and_application() {
    struct Gated {
        entered: Option<tokio::sync::oneshot::Sender<()>>,
        release: Option<tokio::sync::oneshot::Receiver<()>>,
        seen: Arc<Mutex<Vec<i64>>>,
    }
    impl BotStrategy for Gated {
        type CustomEvent = ();
        type Error = String;
        async fn handle_event(
            &mut self,
            _: BotEvent<()>,
            ctx: &BotContext<()>,
        ) -> Result<(), String> {
            let time = ctx.now_ms();
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.release.take().unwrap().await.unwrap();
            }
            assert_eq!(time, ctx.now_ms());
            self.seen.lock().unwrap().push(time);
            Ok(())
        }
    }
    let (entered, rx) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let copy = seen.clone();
    let task = tokio::spawn(async move {
        let mut runner = BotRunner::new(
            Gated {
                entered: Some(entered),
                release: Some(wait),
                seen: copy,
            },
            NoTradingVenue,
        );
        runner.process(1, None, BotEvent::Custom(())).await.unwrap();
        runner.process(2, None, BotEvent::Custom(())).await.unwrap();
    });
    rx.await.unwrap();
    assert!(seen.lock().unwrap().is_empty());
    release.send(()).unwrap();
    task.await.unwrap();
    assert_eq!(*seen.lock().unwrap(), [1, 2]);
}
#[tokio::test]
async fn ignored_emit_errors_still_fail_on_limit_and_stale_context_closes() {
    struct Loop(Arc<Mutex<Option<BotContext<()>>>>);
    impl BotStrategy for Loop {
        type CustomEvent = ();
        type Error = String;
        async fn handle_event(
            &mut self,
            _: BotEvent<()>,
            ctx: &BotContext<()>,
        ) -> Result<(), String> {
            *self.0.lock().unwrap() = Some(ctx.clone());
            let _ = ctx.emit(BotEvent::Custom(())).await;
            Ok(())
        }
    }
    let saved = Arc::new(Mutex::new(None));
    let mut runner = BotRunner::new(Loop(saved.clone()), NoTradingVenue);
    runner.derived_limit = 2;
    assert!(matches!(
        runner.process(0, None, BotEvent::Custom(())).await,
        Err(BotRunError::DerivedLimit)
    ));
    let ctx = saved.lock().unwrap().take().unwrap();
    assert!(ctx.emit(BotEvent::Custom(())).await.is_err());
}
#[tokio::test]
async fn strategy_error_is_observable_on_shutdown() {
    struct Fail;
    impl BotStrategy for Fail {
        type CustomEvent = ();
        type Error = String;
        async fn handle_event(
            &mut self,
            _: BotEvent<()>,
            _: &BotContext<()>,
        ) -> Result<(), String> {
            Err("strategy failed".into())
        }
    }
    let runtime = BitbankBotBuilder::market_data(Fail).spawn();
    runtime
        .event_sender()
        .send(BotEvent::Custom(()))
        .await
        .unwrap();
    assert!(
        runtime
            .shutdown()
            .await
            .unwrap_err()
            .contains("strategy failed")
    );
}
#[test]
fn input_rejects_missing_duplicate_reversed_and_truncated_records() {
    let original = std::fs::read_to_string(format!(
        "{}/fixtures/market-data-v1/bitbank-20261027.jsonl",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let lines: Vec<_> = original.lines().collect();
    let cases = [
        format!("{}\n{}\n", lines[0], lines[2]),
        format!("{}\n{}\n", lines[0], lines[0]),
        format!(
            "{}\n{}\n",
            lines[0],
            lines[1].replace("1793113199910000", "1")
        ),
        format!("{}\n{{bad\n", lines[0]),
        original.trim_end().to_owned(),
    ];
    for (n, data) in cases.into_iter().enumerate() {
        let dir = std::env::temp_dir().join(format!("replay-invalid-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("data.jsonl"), data).unwrap();
        std::fs::write(dir.join("manifest.json"),r#"{"format_version":1,"sources":[{"session_id":"sample-bb","exchange":"bitbank","stream":"public","files":["data.jsonl"]}]}"#).unwrap();
        assert!(
            ReplayInput::open(dir.join("manifest.json"))
                .unwrap()
                .any(|r| r.is_err())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn numeric_sequence_equal_diff_and_late_whole() {
    let raw: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(format!(
            "{}/fixtures/market-data-v1/bitbank-20261027.jsonl",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
        .lines()
        .nth(2)
        .unwrap(),
    )
    .unwrap();
    let mut converter = BitbankMarketEventConverter::new("btc_jpy".into());
    for (seq, amount) in [(9, "1"), (10, "2"), (11, "3")] {
        let diff = serde_json::json!(["message",{"room_name":"depth_diff_btc_jpy","message":{"data":{"a":[],"b":[["100",amount]],"t":seq,"s":seq.to_string()}}}]);
        assert!(
            converter
                .convert(decode_bitbank(&diff).unwrap().unwrap().1)
                .unwrap()
                .is_none()
        );
    }
    let mut whole = raw["message"].clone();
    whole[1]["message"]["data"]["sequenceId"] = serde_json::json!("9");
    let event = converter
        .convert(decode_bitbank(&whole).unwrap().unwrap().1)
        .unwrap()
        .unwrap();
    let MarketEvent::DepthUpdated { depth, .. } = event else {
        panic!()
    };
    assert_eq!(*depth.best_bid().unwrap().1, 3.0);
    whole[1]["message"]["data"]["sequenceId"] = serde_json::json!("11");
    let event = converter
        .convert(decode_bitbank(&whole).unwrap().unwrap().1)
        .unwrap()
        .unwrap();
    let MarketEvent::DepthUpdated { depth, .. } = event else {
        panic!()
    };
    assert_eq!(*depth.best_bid().unwrap().1, 1.0);
}
#[test]
fn duplicate_trades_do_not_fill_twice_and_conflicts_fail() {
    let record: CaptureRecord = serde_json::from_str(
        std::fs::read_to_string(format!(
            "{}/fixtures/market-data-v1/bitbank-20261028.jsonl",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
        .lines()
        .nth(1)
        .unwrap(),
    )
    .unwrap();
    let mut adapter = ReplayAdapter::default();
    assert!(
        adapter
            .adapt::<()>(0, &record, &mut |_| Ok(None))
            .unwrap()
            .is_some()
    );
    assert!(
        adapter
            .adapt::<()>(0, &record, &mut |_| Ok(None))
            .unwrap()
            .is_none()
    );
    let mut bad = record;
    if let CapturePayload::Message { message } = &mut bad.payload {
        message[1]["message"]["data"]["transactions"][0]["amount"] = serde_json::json!("9");
    }
    assert!(adapter.adapt::<()>(0, &bad, &mut |_| Ok(None)).is_err());
}

#[tokio::test]
async fn empty_input_and_application_failure() {
    use bitbankutil_rs::{order_domain::OrderId, trading_venue::*};
    #[derive(Clone)]
    struct FailVenue;
    impl TradingVenue for FailVenue {
        fn account_snapshot<'a>(
            &'a self,
            pair: &'a str,
        ) -> TradingVenueFuture<'a, AccountSnapshot> {
            NoTradingVenue.account_snapshot(pair)
        }
        fn place_order(&self, request: PlacementRequest) -> TradingVenueFuture<'_, PlacedOrder> {
            NoTradingVenue.place_order(request)
        }
        fn cancel_orders<'a>(
            &'a self,
            pair: &'a str,
            ids: Vec<OrderId>,
        ) -> TradingVenueFuture<'a, ()> {
            NoTradingVenue.cancel_orders(pair, ids)
        }
        fn on_market_event(&self, _: &MarketEvent) -> Result<(), TradingVenueError> {
            Err(TradingVenueError::StatePoisoned)
        }
    }
    struct Unreachable;
    impl BotStrategy<FailVenue> for Unreachable {
        type CustomEvent = ();
        type Error = String;
        async fn handle_event(
            &mut self,
            _: BotEvent<()>,
            _: &BotContext<(), FailVenue>,
        ) -> Result<(), String> {
            panic!("strategy must not run on apply failure")
        }
    }
    let mut runner = BotRunner::new(Unreachable, FailVenue);
    let report = run_replay(
        &mut runner,
        std::iter::empty(),
        false,
        &AtomicBool::new(false),
        |_| Ok(None),
    )
    .await;
    assert_eq!(report.records, 0);
    assert!(!report.incomplete && !report.interrupted && report.error.is_none());
    let result = runner
        .process(
            0,
            None,
            BotEvent::Market(MarketEvent::Transactions {
                pair: "btc_jpy".into(),
                transactions: vec![],
            }),
        )
        .await;
    assert!(matches!(result, Err(BotRunError::Venue(_))));
}
