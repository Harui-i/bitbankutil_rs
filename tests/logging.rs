//! Exercise the public runtime with a process-wide subscriber, as in production.
use bitbankutil_rs::{
    bitbank_bot::{BitbankBotBuilder, BotContext, BotEvent, BotStrategy},
    telemetry::RunMetadata,
};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};
use tracing::Instrument;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn span_field(event: &Value, field: &str) -> Option<Value> {
    event["spans"]
        .as_array()?
        .iter()
        .rev()
        .find_map(|s| s.get(field).cloned())
}
struct Fails;
impl BotStrategy for Fails {
    type CustomEvent = ();
    type Error = &'static str;
    fn error_code(_: &Self::Error) -> &'static str {
        "account_snapshot_missing"
    }
    async fn handle_event(
        &mut self,
        _: BotEvent<()>,
        _: &BotContext<()>,
    ) -> Result<(), Self::Error> {
        Err("secret-sentinel")
    }
}
#[tokio::test]
async fn queued_strategy_failure_keeps_ws_generation_input_and_safe_reason() {
    // This integration executable owns its subscriber; no tests replace it.
    let capture = Capture::default();
    let writer = capture.clone();
    tracing_subscriber::fmt()
        .json()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .init();
    let runtime = BitbankBotBuilder::market_data(Fails)
        .run_metadata(RunMetadata {
            application: "logging-test".into(),
            build: "revision1".into(),
            ..Default::default()
        })
        .spawn();
    let ws = tracing::info_span!("websocket", connection_id = 42, generation = 3);
    let input =
        tracing::info_span!(parent: &ws, "market_input", input_sequence = 81, received_at_ms = 123);
    runtime
        .event_sender()
        .send_at(123, BotEvent::Custom(()))
        .instrument(input)
        .await
        .unwrap();
    assert!(runtime.shutdown().await.is_err());
    let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    let events: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let failed = events
        .iter()
        .find(|e| e["fields"]["event"] == "strategy_failed")
        .unwrap();
    assert_eq!(failed["fields"]["reason_code"], "account_snapshot_missing");
    assert_eq!(failed["fields"]["action"], "stop_bot");
    for (field, value) in [
        ("connection_id", 42),
        ("generation", 3),
        ("input_sequence", 81),
    ] {
        assert_eq!(span_field(failed, field), Some(json!(value)), "{failed}");
    }
    assert_eq!(span_field(failed, "event_type"), Some(json!("custom")));
    assert!(span_field(failed, "run_id").is_some());
    let configured = events
        .iter()
        .find(|e| e["fields"]["event"] == "bot_configured")
        .unwrap();
    assert_eq!(
        span_field(configured, "application"),
        Some(json!("logging-test"))
    );
    assert_eq!(span_field(configured, "mode"), Some(json!("market_data")));
    assert!(!text.contains("secret-sentinel"));
}
