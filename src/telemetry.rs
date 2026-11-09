//! Internal correlation and safe diagnostic fields. No subscriber is installed here.
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Bound untrusted diagnostic fields and escape controls even for text subscribers.
pub(crate) fn safe(value: &str) -> String {
    value
        .chars()
        .take(128)
        .flat_map(char::escape_default)
        .collect()
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
    if outcome == "unknown" {
        tracing::error!(event = "order_api_finished", outcome);
    } else {
        tracing::warn!(event = "order_api_finished", outcome);
    }
}

/// Reports exit even when a task is aborted or unwinds. The outcome is not inferred.
pub(crate) struct TaskLifetime(pub &'static str);
impl Drop for TaskLifetime {
    fn drop(&mut self) {
        tracing::debug!(event = "task_ended", task = self.0);
    }
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
