//! Application identity and log-writer health. The library never installs a subscriber.
//! See `docs/logging.md` for configuration, monitoring and safe diagnostic fields.
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Bound untrusted diagnostic fields and escape controls even for text subscribers.
pub(crate) fn safe(value: &str) -> String {
    let mut output = String::new();
    for ch in value.chars() {
        let escaped = ch.escape_default().to_string();
        if output.len() + escaped.len() > 128 {
            break;
        }
        output.push_str(&escaped);
    }
    output
}

pub(crate) fn category(error: &crate::trading_venue::TradingVenueError) -> &'static str {
    use crate::trading_venue::TradingVenueError::*;
    match error {
        Numeric(_) => "numeric",
        Bitbank(_) => "bitbank",
        Parse(_) => "parse",
        Paper(_) => "paper",
        StatePoisoned => "state_poisoned",
        Unavailable => "unavailable",
    }
}

pub(crate) fn api_outcome(
    error: &Option<crypto_botters::bitbank::BitbankHandleError>,
) -> &'static str {
    use crypto_botters::bitbank::BitbankHandleError as B;
    match error {
        None
        | Some(
            B::ParseError
            | B::ApiError(_)
            | B::SystemError(_)
            | B::TimeoutWaitingForResponse(_)
            | B::RequestLimitExceeded(_),
        ) => "unknown",
        _ => "rejected",
    }
}

pub(crate) fn outcome(error: &crate::trading_venue::TradingVenueError) -> &'static str {
    use crate::trading_venue::TradingVenueError::*;
    match error {
        Bitbank(error) => api_outcome(error),
        Parse(_) => "unknown",
        _ => "rejected",
    }
}

pub(crate) fn api_failure(error: &Option<crypto_botters::bitbank::BitbankHandleError>) {
    let outcome = api_outcome(error);
    let (reason_code, exchange_code, security_relevant) = crate::error_diagnostics::api(error);
    if outcome == "unknown" {
        tracing::error!(
            event = "order_api_finished",
            outcome,
            reason_code,
            exchange_code,
            security_relevant
        );
    } else {
        tracing::warn!(
            event = "order_api_finished",
            outcome,
            reason_code,
            exchange_code,
            security_relevant
        );
    }
}

/// Reports exit even when a task is aborted or unwinds. The outcome is not inferred.
pub(crate) struct TaskLifetime(pub &'static str);
impl Drop for TaskLifetime {
    fn drop(&mut self) {
        tracing::info!(
            event = "task_ended",
            task = self.0,
            panicking = std::thread::panicking(),
            outcome = "unobserved"
        );
    }
}

/// Stable categorization; the serde message itself can contain arbitrary response data.
pub(crate) fn decode_reason(error: &serde_json::Error) -> &'static str {
    let message = error.to_string();
    if message.starts_with("missing field") {
        "missing_field"
    } else if message.starts_with("unknown field") {
        "unknown_field"
    } else if message.starts_with("unknown variant") {
        "unknown_variant"
    } else if message.starts_with("invalid type") {
        "invalid_type"
    } else if message.starts_with("invalid value") {
        "invalid_value"
    } else if message.starts_with("invalid length") {
        "invalid_length"
    } else {
        match error.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "invalid_json",
            serde_json::error::Category::Data => "invalid_data",
            serde_json::error::Category::Eof => "unexpected_eof",
        }
    }
}
pub(crate) fn decode_path(error: &serde_json::Error, path: &str) -> String {
    let message = error.to_string();
    if let Some(field) = message
        .strip_prefix("missing field `")
        .and_then(|v| v.split('`').next())
    {
        safe(&if path == "." || path.is_empty() {
            field.to_owned()
        } else {
            format!("{path}.{field}")
        })
    } else {
        safe(path)
    }
}
#[derive(Debug)]
pub(crate) struct DecodeDiagnostic {
    pub reason_code: &'static str,
    pub field: String,
}
impl std::fmt::Display for DecodeDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}", self.reason_code, self.field)
    }
}
impl std::error::Error for DecodeDiagnostic {}
pub(crate) fn decode_value<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> std::io::Result<T> {
    use serde::de::IntoDeserializer;
    serde_path_to_error::deserialize(value.into_deserializer()).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            DecodeDiagnostic {
                reason_code: decode_reason(error.inner()),
                field: decode_path(error.inner(), &error.path().to_string()),
            },
        )
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        execution_recording::{
            EventSink, ExecutionRecord, ExecutionRecorder, JsonlSink, operation, read_records,
        },
        trading_venue::{PlacedOrder, TradingVenueError},
    };
    use serde_json::{Value, json};
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Default)]
    pub(crate) struct Capture(pub Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Capture {
        pub(crate) fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync + use<> {
            let writer = self.clone();
            tracing_subscriber::fmt()
                .json()
                .with_ansi(false)
                .without_time()
                .with_max_level(tracing::Level::TRACE)
                .with_writer(move || writer.clone())
                .finish()
        }
        pub(crate) fn events(&self) -> Vec<Value> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
        pub(crate) fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }
    pub(crate) fn span_field(event: &Value, field: &str) -> Option<Value> {
        event["spans"]
            .as_array()?
            .iter()
            .rev()
            .find_map(|s| s.get(field).cloned())
    }

    #[tokio::test]
    async fn parallel_operations_keep_ids_and_recorder_correlation() {
        for recording in [false, true] {
            let capture = Capture::default();
            let _guard = tracing::subscriber::set_default(capture.subscriber());
            let records = Capture::default();
            let recorder = recording
                .then(|| ExecutionRecorder::new("test", json!({}), JsonlSink(records.clone())));
            let (a, b) = tokio::join!(
                operation(
                    recorder.as_ref(),
                    "place_order",
                    json!({"order":{"pair":"btc_jpy","side":"buy","price":"0.00000001","amount":"1"}}),
                    async {
                        tokio::task::yield_now().await;
                        tracing::debug!(event = "inside_a");
                        Ok(PlacedOrder {
                            order_id: Some(crate::order_domain::OrderId(71)),
                        })
                    }
                ),
                operation(
                    recorder.as_ref(),
                    "cancel_orders",
                    json!({"pair":"btc_jpy","order_ids":[72]}),
                    async {
                        tracing::debug!(event = "inside_b");
                        Err::<(), _>(TradingVenueError::Bitbank(None))
                    }
                )
            );
            assert!(a.is_ok());
            assert!(b.is_err());
            let events = capture.events();
            let by_name = |name| {
                events
                    .iter()
                    .find(|e| e["fields"]["event"] == name)
                    .unwrap()
            };
            let a_id = span_field(by_name("inside_a"), "operation_id").unwrap();
            let b_id = span_field(by_name("inside_b"), "operation_id").unwrap();
            assert_ne!(a_id, b_id);
            for id in [&a_id, &b_id] {
                for kind in ["operation_started", "operation_finished"] {
                    assert_eq!(
                        events
                            .iter()
                            .filter(|e| e["fields"]["event"] == kind
                                && span_field(e, "operation_id").as_ref() == Some(id))
                            .count(),
                        1
                    );
                }
            }
            let unknown = events
                .iter()
                .find(|e| e["fields"]["outcome"] == "unknown")
                .unwrap();
            assert_eq!(unknown["level"], "ERROR");
            assert!(events.iter().any(|e| e["fields"]["price"] == "0.00000001"));
            if recording {
                let saved = read_records(&records.0.lock().unwrap()[..]).unwrap();
                for id in [a_id.as_u64().unwrap(), b_id.as_u64().unwrap()] {
                    assert!(
                        saved
                            .iter()
                            .any(|r| r.operation_id == Some(id) && r.kind == "operation_finished")
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn rejection_and_external_errors_do_not_dump_payloads() {
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        let error = crypto_botters::bitbank::BitbankHandleError::AuthenticationFailed(
            json!({"secret":"secret-sentinel\nforged"}),
        );
        let result = operation(None, "place_order", json!({"pair":"btc\nforged"}), async {
            Err::<(), _>(TradingVenueError::Bitbank(Some(error)))
        })
        .await;
        assert!(result.is_err());
        assert!(!capture.text().contains("secret-sentinel"));
        let events = capture.events();
        assert!(
            events
                .iter()
                .any(|e| e["fields"]["outcome"] == "rejected" && e["level"] == "WARN")
        );
        assert!(events.iter().any(|e| e["fields"]["pair"] == "btc\\nforged"));
        assert!(safe(&"x".repeat(1000)).len() <= 128);
    }

    struct FailedSink {
        flush_only: bool,
    }
    impl EventSink for FailedSink {
        fn append(&mut self, _: &ExecutionRecord) -> io::Result<()> {
            if self.flush_only {
                Ok(())
            } else {
                Err(io::Error::other("secret-sentinel"))
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("secret-sentinel"))
        }
    }
    #[test]
    fn recorder_failure_is_reported_once_without_error_text() {
        for flush_only in [false, true] {
            let capture = Capture::default();
            let _guard = tracing::subscriber::set_default(capture.subscriber());
            let recorder = ExecutionRecorder::new("test", json!({}), FailedSink { flush_only });
            recorder.record("test", json!({}));
            assert!(recorder.flush().is_err());
            recorder.record("test", json!({}));
            assert!(recorder.finish().is_err());
            assert_eq!(
                capture
                    .events()
                    .iter()
                    .filter(|e| e["fields"]["event"] == "recording_failed")
                    .count(),
                1
            );
            let events = capture.events();
            let failure = events
                .iter()
                .find(|e| e["fields"]["event"] == "recording_failed")
                .unwrap();
            assert_eq!(
                failure["fields"]["stage"],
                if flush_only { "flush" } else { "append" }
            );
            assert!(failure["fields"]["recorder_id"].as_u64().is_some());
            assert!(
                failure["fields"]["first_failure_sequence"]
                    .as_u64()
                    .is_some()
            );
            assert!(
                events
                    .iter()
                    .any(|e| e["fields"]["event"] == "recording_status"
                        && e["fields"]["dropped_records"].as_u64().unwrap() > 0)
            );
            assert!(!capture.text().contains("secret-sentinel"));
            assert!(recorder.status().persistence_uncertain);
        }
    }
    #[tokio::test]
    async fn strategy_failure_is_logged_without_debugging_error() {
        use crate::bitbank_bot::{BotContext, BotEvent, BotRunner, BotStrategy};
        use crate::trading_venue::NoTradingVenue;
        struct Fails;
        impl BotStrategy<NoTradingVenue> for Fails {
            type CustomEvent = ();
            type Error = &'static str;
            async fn handle_event(
                &mut self,
                _: BotEvent<()>,
                _: &BotContext<(), NoTradingVenue>,
            ) -> Result<(), Self::Error> {
                Err("secret-sentinel")
            }
        }
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        let mut runner = BotRunner::new(Fails, NoTradingVenue);
        assert!(
            runner
                .process(100, None, BotEvent::Custom(()))
                .await
                .is_err()
        );
        let events = capture.events();
        let failure = events
            .iter()
            .find(|e| e["fields"]["event"] == "strategy_failed")
            .unwrap();
        assert!(span_field(failure, "decision_id").is_some());
        assert!(!capture.text().contains("secret-sentinel"));
    }
}

/// Application-owned identity. Never put keys, tokens or raw account numbers here.
#[derive(Debug, Clone, Default)]
pub struct RunMetadata {
    pub application: String,
    pub application_version: String,
    pub build: String,
    pub instance: String,
    pub strategy: String,
    pub account_alias: String,
}
impl RunMetadata {
    /// Unique within a process lifetime and distinguishable across restarts.
    pub fn span(&self) -> tracing::Span {
        static PROCESS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let process = PROCESS.get_or_init(|| {
            format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            )
        });
        tracing::info_span!("application", process_instance = %process, execution_id = next_id(),
            application = %safe(&self.application), application_version = %safe(&self.application_version),
            build = %safe(&self.build), instance = %safe(&self.instance), strategy = %safe(&self.strategy),
            account_alias = %safe(&self.account_alias))
    }
}

/// Emit the first occurrence and exponentially spaced repeats, retaining exact counts.
/// Keys must be internal codes, never external strings (bounded cardinality).
#[derive(Debug, Default)]
pub(crate) struct Failures {
    stage: &'static str,
    pair: Option<String>,
    counts: std::collections::BTreeMap<(&'static str, &'static str), u64>,
    pub(crate) total: u64,
    pub(crate) suppressed: u64,
}
impl Failures {
    pub(crate) fn new(stage: &'static str, pair: &str) -> Self {
        Self {
            stage,
            pair: Some(safe(pair)),
            counts: Default::default(),
            total: 0,
            suppressed: 0,
        }
    }
    pub(crate) fn record(&mut self, key: &'static str) -> bool {
        self.record_for(key, key)
    }
    pub(crate) fn record_for(&mut self, kind: &'static str, reason: &'static str) -> bool {
        self.total += 1;
        let count = self.counts.entry((kind, reason)).or_default();
        *count += 1;
        let emit = count.is_power_of_two();
        if !emit {
            self.suppressed += 1;
        }
        emit
    }
    pub(crate) fn recover(&mut self, key: &'static str) -> Option<u64> {
        let count = self
            .counts
            .iter()
            .filter(|((kind, _), _)| *kind == key)
            .map(|(_, count)| *count)
            .sum::<u64>();
        self.counts.retain(|(kind, _), _| *kind != key);
        (count > 0).then_some(count)
    }
}
impl Drop for Failures {
    fn drop(&mut self) {
        if self.total > 0 {
            tracing::info!(
                event = "input_failure_summary",
                stage = self.stage,
                pair = self.pair.as_deref(),
                failure_count = self.total,
                suppressed_count = self.suppressed,
                unresolved = !self.counts.is_empty()
            );
        }
    }
}

/// Shared health counters for the application's operational log writer.
/// The writer never logs recursively. Poll these counters from a separate monitor.
#[derive(Clone, Default)]
pub struct LoggingHealth(std::sync::Arc<WriterHealth>);
#[derive(Default)]
struct WriterHealth {
    write_failures: AtomicU64,
    flush_failures: AtomicU64,
    last_error: std::sync::Mutex<Option<std::io::ErrorKind>>,
    bytes_written: AtomicU64,
    last_successful_write_ms: AtomicU64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoggingHealthSnapshot {
    pub write_failures: u64,
    pub flush_failures: u64,
    pub last_error: Option<std::io::ErrorKind>,
    pub bytes_written: u64,
    pub last_successful_write_ms: u64,
}
impl LoggingHealth {
    pub fn wrap<W: std::io::Write>(&self, writer: W) -> MonitoredWriter<W> {
        MonitoredWriter {
            writer,
            health: self.clone(),
        }
    }
    pub fn snapshot(&self) -> LoggingHealthSnapshot {
        LoggingHealthSnapshot {
            write_failures: self.0.write_failures.load(Ordering::Relaxed),
            flush_failures: self.0.flush_failures.load(Ordering::Relaxed),
            last_error: *self.0.last_error.lock().unwrap_or_else(|e| e.into_inner()),
            bytes_written: self.0.bytes_written.load(Ordering::Relaxed),
            last_successful_write_ms: self.0.last_successful_write_ms.load(Ordering::Relaxed),
        }
    }
    fn failed(&self, error: &std::io::Error, flushing: bool) {
        let counter = if flushing {
            &self.0.flush_failures
        } else {
            &self.0.write_failures
        };
        counter.fetch_add(1, Ordering::Relaxed);
        *self.0.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.kind());
    }
}
pub struct MonitoredWriter<W> {
    writer: W,
    health: LoggingHealth,
}
impl<W: std::io::Write> std::io::Write for MonitoredWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let result = self.writer.write(bytes);
        match &result {
            Err(e) => self.health.failed(e, false),
            Ok(0) if !bytes.is_empty() => self
                .health
                .failed(&std::io::Error::from(std::io::ErrorKind::WriteZero), false),
            Ok(count) if *count > 0 => {
                self.health
                    .0
                    .bytes_written
                    .fetch_add(*count as u64, Ordering::Relaxed);
                self.health.0.last_successful_write_ms.store(
                    crate::execution_recording::timestamp_ms(),
                    Ordering::Relaxed,
                );
            }
            _ => {}
        }
        result
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let result = self.writer.flush();
        if let Err(e) = &result {
            self.health.failed(e, true);
        }
        result
    }
}

/// Match only known internal failures; arbitrary strategy error text is never emitted.
pub(crate) fn simulation_reason(reason: &str) -> &'static str {
    match reason {
        "input_buffer_full" => "input_buffer_full",
        "malformed_input" => "malformed_input",
        "feed_reconnecting" => "feed_reconnecting",
        "feed_terminated" => "feed_terminated",
        "book sequence contradicts exchange timestamp order" => "book_sequence_conflict",
        "conflicting book sequence" => "book_sequence_conflict",
        "conflicting transaction ID" => "transaction_conflict",
        "received time reversed" => "received_time_reversed",
        "simulation limit reversed" => "simulation_time_reversed",
        "market occurrence is after receipt; check clock offset" => "clock_offset",
        "market exceeded simulation lookahead" => "lookahead_exceeded",
        "unknown market event arrived behind simulation boundary" => "late_market_event",
        "simulation interrupted" => "interrupted",
        "market sequence overflow" => "sequence_overflow",
        "bot processing failed" => "bot_processing_failed",
        "bot input failed" => "bot_input_failed",
        "shutdown with pending strategy operation" => "shutdown_pending_operation",
        _ => "market_incomplete",
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::tests::{Capture, span_field};
    use super::*;
    use crate::{
        bitbank_bot::{BitbankInboundMessage, BitbankMarketEventConverter},
        bitbank_structs::BitbankDepthWhole,
    };
    use serde_json::json;
    fn whole(amount: &str) -> BitbankInboundMessage {
        BitbankInboundMessage::DepthWhole(
            serde_json::from_value::<BitbankDepthWhole>(json!({
                "asks":[["100", amount]],"bids":[["99","1"]],"timestamp":123,"sequenceId":"42", "asks_over":"0", "bids_under":"0", "asks_under":"0", "bids_over":"0", "ask_market":"0", "bid_market":"0"
            }))
            .unwrap(),
        )
    }
    #[test]
    fn conversion_failures_explain_field_location_recovery_and_exact_loss_count() {
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        let connection = tracing::info_span!("websocket", connection_id = 73);
        connection.in_scope(|| {
            let mut converter = BitbankMarketEventConverter::new("trx_jpy".into());
            for _ in 0..17 {
                assert!(converter.convert(whole("0")).is_err());
            }
            // A different reason must be visible even during a burst of invalid snapshots.
            assert!(converter.convert(whole("0.000000001")).is_err());
            assert!(converter.convert(whole("1")).unwrap().is_some());
            assert!(converter.convert(whole("secret-sentinel\nforged")).is_err());
        });
        let events = capture.events();
        let failures: Vec<_> = events
            .iter()
            .filter(|e| e["fields"]["event"] == "market_conversion_failed")
            .collect();
        assert_eq!(failures.len(), 7); // 1,2,4,8,16; changed reason; first failure after recovery
        for event in &failures {
            assert_eq!(event["fields"]["field"], "amount");
            assert_eq!(event["fields"]["side"], "sell");
            assert_eq!(event["fields"]["level_index"], 0);
            assert_eq!(event["fields"]["message_type"], "depth_whole");
            assert_eq!(event["fields"]["sequence_id"], "42");
            assert_eq!(event["fields"]["event_time_ms"], 123);
            assert_eq!(event["fields"]["action"], "drop_message");
            assert_eq!(span_field(event, "connection_id"), Some(json!(73)));
        }
        assert_eq!(failures[0]["fields"]["reason_code"], "non_positive_amount");
        assert_eq!(failures[5]["fields"]["reason_code"], "numeric_precision");
        assert!(
            events
                .iter()
                .any(|e| e["fields"]["event"] == "market_input_recovered"
                    && e["fields"]["failure_count"] == 18)
        );
        assert!(events.iter().any(|e| e["fields"]["state"] == "ready"));
        let summary = events
            .iter()
            .find(|e| e["fields"]["event"] == "input_failure_summary")
            .unwrap();
        assert_eq!(summary["fields"]["failure_count"], 19);
        assert_eq!(summary["fields"]["suppressed_count"], 12);
        assert!(!capture.text().contains("secret-sentinel"));
    }
    #[test]
    fn safe_fields_have_a_bound_after_escaping_and_do_not_split_an_escape() {
        for value in ["\n".repeat(1000), "🦀".repeat(1000), "a".repeat(1000)] {
            let bounded = safe(&value);
            assert!(bounded.len() <= 128);
            assert!(!bounded.contains(['\n', '\r']));
        }
        assert_eq!(safe("\r\n"), "\\r\\n");
    }
    #[test]
    fn safe_api_reason_preserves_code_without_formatting_secret_payloads() {
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        let error = Some(
            crypto_botters::bitbank::BitbankHandleError::InvalidAccessSignature(json!({
                "data":{"code":20005,"secret":"secret-sentinel"},"token":"token-sentinel"
            })),
        );
        api_failure(&error);
        let events = capture.events();
        assert_eq!(
            events[0]["fields"]["reason_code"],
            "invalid_access_signature"
        );
        assert_eq!(events[0]["fields"]["exchange_code"], 20005);
        assert_eq!(events[0]["fields"]["security_relevant"], true);
        assert!(!capture.text().contains("sentinel"));
    }
    #[test]
    fn restart_identity_and_application_metadata_are_propagated_safely() {
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        let metadata = RunMetadata {
            application: "bot\nforged".into(),
            build: "revision1".into(),
            ..Default::default()
        };
        metadata.span().in_scope(|| tracing::info!(event = "first"));
        metadata
            .span()
            .in_scope(|| tracing::info!(event = "second"));
        let events = capture.events();
        assert_ne!(
            span_field(&events[0], "execution_id"),
            span_field(&events[1], "execution_id")
        );
        assert!(
            span_field(&events[0], "process_instance")
                .unwrap()
                .as_str()
                .is_some_and(|s| !s.is_empty())
        );
        assert_eq!(
            span_field(&events[0], "application"),
            Some(json!("bot\\nforged"))
        );
        assert_eq!(span_field(&events[0], "build"), Some(json!("revision1")));
    }
    #[test]
    fn writer_failures_are_counted_without_recursion_or_error_text() {
        use std::io::{self, Write};
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    "secret-sentinel",
                ))
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "secret-sentinel",
                ))
            }
        }
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        let health = LoggingHealth::default();
        let mut writer = health.wrap(Broken);
        assert!(writer.write_all(b"event").is_err());
        assert!(writer.flush().is_err());
        let status = health.snapshot();
        assert_eq!(status.write_failures, 1);
        assert_eq!(status.flush_failures, 1);
        assert_eq!(status.last_error, Some(io::ErrorKind::PermissionDenied));
        assert_eq!(status.bytes_written, 0);
        assert!(capture.events().is_empty());
        let mut working = health.wrap(Vec::new());
        working.write_all(b"abc").unwrap();
        assert_eq!(health.snapshot().bytes_written, 3);
        assert!(health.snapshot().last_successful_write_ms > 0);
    }
}
