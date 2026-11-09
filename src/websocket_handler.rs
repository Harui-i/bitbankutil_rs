use crate::bitbank_bot::BitbankInboundMessage;
use crypto_botters::{
    Client,
    bitbank::BitbankOption,
    websocket::{TungsteniteError, WebSocketConfig},
};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{Instrument, instrument::WithSubscriber};

pub(crate) struct ObservedMessage {
    pub received_at_ms: i64,
    pub connection_epoch: u64,
    pub message: BitbankInboundMessage,
    pub span: tracing::Span,
}
#[derive(Clone)]
enum CallbackSender {
    Plain(mpsc::Sender<BitbankInboundMessage>),
    Observed(mpsc::Sender<ObservedMessage>),
}
impl CallbackSender {
    fn is_closed(&self) -> bool {
        match self {
            Self::Plain(tx) => tx.is_closed(),
            Self::Observed(tx) => tx.is_closed(),
        }
    }
    async fn send(&self, message: ObservedMessage) -> bool {
        match self {
            Self::Plain(tx) => tx.send(message.message).await.is_ok(),
            Self::Observed(tx) => tx.send(message).await.is_ok(),
        }
    }
}
#[derive(Default)]
struct CallbackState {
    failures: crate::telemetry::Failures,
    last_valid: Option<std::time::Instant>,
    stale: bool,
    pending: u64,
    connection_epoch: u64,
}

/// 公開WSを起動する。接続エラーは安全な理由を記録して終了する。
pub async fn run_websocket(
    pair: String,
    client_options: Vec<BitbankOption>,
    wsc: WebSocketConfig,
    tx: mpsc::Sender<BitbankInboundMessage>,
    observation: Option<crate::execution_recording::ExecutionRecorder>,
) {
    run_websocket_inner(
        pair,
        client_options,
        wsc,
        CallbackSender::Plain(tx),
        observation,
        std::time::Duration::from_secs(30),
    )
    .await;
}
pub(crate) async fn run_observed_websocket(
    pair: String,
    client_options: Vec<BitbankOption>,
    wsc: WebSocketConfig,
    tx: mpsc::Sender<ObservedMessage>,
    observation: Option<crate::execution_recording::ExecutionRecorder>,
    stale_after: std::time::Duration,
) {
    run_websocket_inner(
        pair,
        client_options,
        wsc,
        CallbackSender::Observed(tx),
        observation,
        stale_after,
    )
    .await;
}
#[tracing::instrument(skip_all, fields(pair = %crate::telemetry::safe(&pair)))]
async fn run_websocket_inner(
    pair: String,
    client_options: Vec<BitbankOption>,
    wsc: WebSocketConfig,
    tx: CallbackSender,
    observation: Option<crate::execution_recording::ExecutionRecorder>,
    stale_after: std::time::Duration,
) {
    let _lifetime = crate::telemetry::TaskLifetime("websocket");
    let mut client = Client::new();
    for option in client_options {
        client.update_default_option(option);
    }
    let channels = [
        "ticker",
        "transactions",
        "depth_diff",
        "depth_whole",
        "circuit_break_info",
    ]
    .into_iter()
    .map(|channel| format!("{channel}_{pair}"))
    .collect();
    let state = Arc::new(Mutex::new(CallbackState {
        failures: crate::telemetry::Failures::new("websocket", &pair),
        ..Default::default()
    }));
    let callback_state = state.clone();
    let callback_tx = tx.clone();
    let observer = observation.map(crate::clock_observation::WsObserver::new);
    let connection = client
        .websocket(
            "",
            move |value| {
                dispatch_observed_callback(value, observer.as_ref(), &callback_tx, &callback_state)
            },
            [
                BitbankOption::WebSocketChannels(channels),
                BitbankOption::WebSocketConfig(wsc),
            ],
        )
        .await;
    let connection = match connection {
        Ok(connection) => connection,
        Err(error) => {
            log_start_failure(&error);
            return;
        }
    };
    let reconnect = connection.reconnect_state();
    let mut reconnecting = false;
    let started = std::time::Instant::now();
    tracing::info!(
        event = "market_feed_state",
        state = "waiting_snapshot",
        reason_code = "connected"
    );
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if tx.is_closed() {
            tracing::info!(
                event = "feed_stopped",
                reason_code = "downstream_closed",
                action = "close_connection"
            );
            return;
        }
        let current = reconnect.is_reconnecting();
        if current != reconnecting {
            if current {
                state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .connection_epoch += 1;
            }
            tracing::warn!(
                event = "market_feed_state",
                state = if current {
                    "reconnecting"
                } else {
                    "reconnected_waiting_snapshot"
                },
                reason_code = "connection_state_changed"
            );
            reconnecting = current;
        }
        check_stale(
            &mut state.lock().unwrap_or_else(|e| e.into_inner()),
            started,
            stale_after,
        );
    }
}
fn check_stale(
    state: &mut CallbackState,
    started: std::time::Instant,
    stale_after: std::time::Duration,
) {
    let age = state.last_valid.unwrap_or(started).elapsed();
    if age >= stale_after && !state.stale {
        state.stale = true;
        tracing::warn!(
            event = "market_feed_state",
            state = "stale",
            reason_code = "no_valid_input",
            last_valid_age_ms = age.as_millis() as u64,
            stale_after_ms = stale_after.as_millis() as u64,
            pending_deliveries = state.pending,
            action = "observe_only"
        );
    }
}
fn note_valid_input(state: &mut CallbackState) {
    state.last_valid = Some(std::time::Instant::now());
    if let Some(failure_count) = state.failures.recover("decode") {
        tracing::info!(
            event = "ws_input_recovered",
            failure_count,
            reason_code = "valid_input_resumed"
        );
    }
    if state.stale {
        state.stale = false;
        tracing::info!(
            event = "market_feed_state",
            state = "receiving",
            reason_code = "valid_input_resumed"
        );
    }
}
fn log_start_failure(error: &TungsteniteError) {
    let reason_code = match error {
        TungsteniteError::Io(_) => "io",
        TungsteniteError::Tls(_) => "tls",
        TungsteniteError::Http(_) => "http_handshake",
        TungsteniteError::Protocol(_) => "protocol",
        TungsteniteError::Url(_) => "url",
        _ => "websocket",
    };
    tracing::error!(
        event = "market_feed_start_failed",
        reason_code,
        action = "stop_feed"
    );
}
fn log_decode_failure(
    error: &std::io::Error,
    value: &serde_json::Value,
    failures: &mut crate::telemetry::Failures,
) {
    let diagnostic = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<crate::telemetry::DecodeDiagnostic>());
    let reason_code = diagnostic.map_or("invalid_callback", |e| e.reason_code);
    if failures.record_for("decode", reason_code) {
        tracing::warn!(
            event = "ws_decode_failed",
            reason_code,
            field = diagnostic.map(|e| e.field.as_str()),
            channel = value
                .pointer("/1/room_name")
                .and_then(serde_json::Value::as_str)
                .map(crate::telemetry::safe),
            action = "drop_message",
            failure_count = failures.total,
            suppressed_count = failures.suppressed
        );
    }
}
fn source_span(value: &serde_json::Value, received_at_ms: i64) -> tracing::Span {
    tracing::info_span!(
        "market_input",
        input_sequence = crate::telemetry::next_id(),
        received_at_ms,
        channel = value
            .pointer("/1/room_name")
            .and_then(serde_json::Value::as_str)
            .map(crate::telemetry::safe)
    )
}
#[cfg(test)]
fn dispatch_callback(
    value: serde_json::Value,
    observer: Option<&crate::clock_observation::WsObserver>,
    tx: &mpsc::Sender<BitbankInboundMessage>,
) {
    dispatch_observed_callback(
        value,
        observer,
        &CallbackSender::Plain(tx.clone()),
        &Arc::new(Mutex::new(CallbackState::default())),
    );
}
fn dispatch_observed_callback(
    value: serde_json::Value,
    observer: Option<&crate::clock_observation::WsObserver>,
    tx: &CallbackSender,
    state: &Arc<Mutex<CallbackState>>,
) {
    let received = crate::clock_observation::ClockObservation::now();
    let span = source_span(&value, (received.unix_us / 1000) as i64);
    let _entered = span.enter();
    let decoded = crate::replay::decode_bitbank(&value);
    if let Some(observer) = observer {
        observer.observe(&value, received, matches!(&decoded, Ok(Some(_))));
    }
    match decoded {
        Ok(Some((_, message))) => {
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            note_valid_input(&mut guard);
            let connection_epoch = guard.connection_epoch;
            guard.pending += 1;
            drop(guard);
            let tx = tx.clone();
            let state = state.clone();
            let observed = ObservedMessage {
                connection_epoch,
                received_at_ms: (received.unix_us / 1000) as i64,
                message,
                span: span.clone(),
            };
            tokio::spawn(
                async move {
                    let started = std::time::Instant::now();
                    let delivered = tx.send(observed).await;
                    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                    guard.pending -= 1;
                    if !delivered && guard.failures.record("receiver_closed") {
                        tracing::debug!(
                            event = "ws_delivery_closed",
                            reason = "receiver_closed",
                            action = "drop_message",
                            failure_count = guard.failures.total
                        );
                    } else if delivered
                        && started.elapsed() >= std::time::Duration::from_secs(1)
                        && guard.failures.record("delivery_delayed")
                    {
                        tracing::warn!(
                            event = "ws_delivery_delayed",
                            reason_code = "downstream_backpressure",
                            delivery_wait_ms = started.elapsed().as_millis() as u64,
                            pending_deliveries = guard.pending,
                            action = "delivered_after_wait"
                        );
                    }
                }
                .in_current_span()
                .with_current_subscriber(),
            );
        }
        Ok(None) => {
            if value.get(0).and_then(serde_json::Value::as_str) == Some("message") {
                let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                if guard.failures.record("unknown_channel") {
                    tracing::warn!(
                        event = "ws_message_ignored",
                        reason_code = "unknown_channel",
                        action = "ignore_message"
                    );
                }
            }
        }
        Err(error) => log_decode_failure(
            &error,
            &value,
            &mut state.lock().unwrap_or_else(|e| e.into_inner()).failures,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        clock_observation::{ClockObservation, WsObserver},
        execution_recording::{ExecutionRecorder, JsonlSink, read_records, tests::Memory},
    };
    use serde_json::json;
    #[tokio::test]
    async fn closed_receiver_is_observable_without_logging_payload() {
        let capture = crate::telemetry::tests::Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let span = tracing::info_span!("websocket", connection_id = 42);
        span.in_scope(|| dispatch_callback(
            json!(["message",{"room_name":"depth_diff_btc_jpy","message":{"data":{"a":[],"b":[],"t":123,"s":"42"},"secret":"secret-sentinel"}}]),
            None,
            &tx,
        ));
        tokio::task::yield_now().await;
        assert!(
            capture
                .events()
                .iter()
                .any(|e| e["fields"]["event"] == "ws_delivery_closed")
        );
        let events = capture.events();
        let closed = events
            .iter()
            .find(|e| e["fields"]["event"] == "ws_delivery_closed")
            .unwrap();
        assert_eq!(
            crate::telemetry::tests::span_field(closed, "connection_id"),
            Some(json!(42))
        );
        assert!(!capture.text().contains("secret-sentinel"));
    }
    #[tokio::test]
    async fn blocked_downstream_keeps_callback_timestamp() {
        let m = Memory::default();
        let recorder = ExecutionRecorder::new("ws", json!({}), JsonlSink(m.clone()));
        let observer = WsObserver::new(recorder.clone());
        let (tx, mut rx) = mpsc::channel(1);
        let raw = json!(["message",{"room_name":"depth_diff_btc_jpy","message":{"data":{"a":[],"b":[],"t":123,"s":"42"}}}]);
        dispatch_callback(raw.clone(), Some(&observer), &tx);
        dispatch_callback(raw, Some(&observer), &tx);
        // 容量1の受信先を消費しなくても、両方の時計観測は既に保存済み。
        let records = read_records(std::io::Cursor::new(m.0.lock().unwrap().clone())).unwrap();
        let events: Vec<_> = records
            .iter()
            .filter(|r| r.kind == "ws_clock_observed")
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].data["receive_sequence"], 2);
        assert_eq!(events[0].data["exchange_times"][0]["field"], "t");
        assert_eq!(events[0].data["exchange_times"][0]["sequence_id"], "42");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let after = ClockObservation::now();
        assert!(
            events[1].data["received_clock"]["monotonic_us"]
                .as_i64()
                .unwrap()
                < after.monotonic_us
        );
        rx.recv().await.unwrap();
        rx.recv().await.unwrap();
    }
    #[tokio::test]
    async fn observed_input_keeps_callback_context_through_the_queue() {
        let capture = crate::telemetry::tests::Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        let (tx, mut rx) = mpsc::channel(1);
        let state = Arc::new(Mutex::new(CallbackState::default()));
        let ws = tracing::info_span!("websocket", connection_id = 7, generation = 2);
        ws.in_scope(|| dispatch_observed_callback(json!(["message",{"room_name":"depth_diff_trx_jpy","message":{"data":{"a":[["100","-1"]],"b":[],"t":123,"s":"42"}}}]), None, &CallbackSender::Observed(tx), &state));
        let observed = rx.recv().await.unwrap();
        let received = observed.received_at_ms;
        observed.span.in_scope(|| {
            let mut converter =
                crate::bitbank_bot::BitbankMarketEventConverter::new("trx_jpy".into());
            assert!(converter.convert(observed.message).is_err());
        });
        let events = capture.events();
        let failed = events
            .iter()
            .find(|e| e["fields"]["event"] == "market_conversion_failed")
            .unwrap();
        assert_eq!(
            crate::telemetry::tests::span_field(failed, "connection_id"),
            Some(json!(7))
        );
        assert_eq!(
            crate::telemetry::tests::span_field(failed, "generation"),
            Some(json!(2))
        );
        assert_eq!(
            crate::telemetry::tests::span_field(failed, "received_at_ms"),
            Some(json!(received))
        );
        assert!(crate::telemetry::tests::span_field(failed, "input_sequence").is_some());
    }
    #[test]
    fn decoder_reports_missing_fields_and_stale_recovery_once_per_transition() {
        let capture = crate::telemetry::tests::Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        let value = json!(["message",{"room_name":"depth_diff_trx_jpy","message":{"data":{"a":[["secret-sentinel","1"]],"b":[],"t":123}}}]);
        let error = crate::replay::decode_bitbank(&value).unwrap_err();
        let mut state = CallbackState::default();
        log_decode_failure(&error, &value, &mut state.failures);
        let started = std::time::Instant::now() - std::time::Duration::from_secs(31);
        check_stale(&mut state, started, std::time::Duration::from_secs(30));
        check_stale(&mut state, started, std::time::Duration::from_secs(30));
        note_valid_input(&mut state);
        let events = capture.events();
        let failed = events
            .iter()
            .find(|e| e["fields"]["event"] == "ws_decode_failed")
            .unwrap();
        assert_eq!(failed["fields"]["reason_code"], "missing_field");
        assert_eq!(failed["fields"]["field"], "s");
        assert_eq!(failed["fields"]["channel"], "depth_diff_trx_jpy");
        assert_eq!(
            events
                .iter()
                .filter(|e| e["fields"]["state"] == "stale")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e["fields"]["state"] == "receiving")
                .count(),
            1
        );
        assert!(!capture.text().contains("secret-sentinel"));
    }
}

pub(crate) const INPUT_OVERFLOW: u8 = 1;
pub(crate) const INPUT_MALFORMED: u8 = 2;
pub(crate) const INPUT_RECONNECT: u8 = 4;
pub(crate) const INPUT_TERMINATED: u8 = 8;
/// 遅延Paper用。コールバック内で時刻を採取し、その場で順序を保って登録する。
/// 満杯を子タスクへ逃がすと受信順が崩れるため、overflowは評価異常にする。
#[tracing::instrument(skip_all, fields(pair = %crate::telemetry::safe(&pair)))]
pub(crate) async fn run_simulation_websocket(
    pair: String,
    client_options: Vec<BitbankOption>,
    wsc: WebSocketConfig,
    tx: mpsc::Sender<crate::bitbank_bot::TimedBitbankMessage>,
    failed: std::sync::Arc<std::sync::atomic::AtomicU8>,
    observation: Option<crate::execution_recording::ExecutionRecorder>,
    stale_after: std::time::Duration,
) {
    let _lifetime = crate::telemetry::TaskLifetime("simulation_websocket");
    let mut client = Client::new();
    for option in client_options {
        client.update_default_option(option);
    }
    let channels = [
        "ticker",
        "transactions",
        "depth_diff",
        "depth_whole",
        "circuit_break_info",
    ]
    .into_iter()
    .map(|channel| format!("{channel}_{pair}"))
    .collect();
    let observer = observation.map(crate::clock_observation::WsObserver::new);
    let callback_failed = failed.clone();
    let callback_state = Arc::new(Mutex::new(CallbackState {
        failures: crate::telemetry::Failures::new("websocket", &pair),
        ..Default::default()
    }));
    let monitor_state = callback_state.clone();
    let connection = client
        .websocket(
            "",
            move |value| {
                let received = crate::clock_observation::ClockObservation::now();
                let span = source_span(&value, (received.unix_us / 1000) as i64);
                let _entered = span.enter();
                let decoded = crate::replay::decode_bitbank(&value);
                if let Some(observer) = &observer {
                    observer.observe(&value, received, matches!(&decoded, Ok(Some(_))));
                }
                match decoded {
                    Ok(Some((pair, message))) => {
                        note_valid_input(
                            &mut callback_state.lock().unwrap_or_else(|e| e.into_inner()),
                        );
                        if let Err(error) =
                            tx.try_send((received.unix_us / 1000, pair, message, span.clone()))
                        {
                            match error {
                                mpsc::error::TrySendError::Full(_) => {
                                    callback_failed.fetch_or(
                                        INPUT_OVERFLOW,
                                        std::sync::atomic::Ordering::Relaxed,
                                    );
                                    let mut state =
                                        callback_state.lock().unwrap_or_else(|e| e.into_inner());
                                    if state.failures.record("buffer_full") {
                                        tracing::warn!(
                                            event = "ws_delivery_failed",
                                            reason = "buffer_full",
                                            action = "invalidate_evaluation",
                                            failure_count = state.failures.total,
                                            suppressed_count = state.failures.suppressed
                                        );
                                    }
                                }
                                mpsc::error::TrySendError::Closed(_) => {
                                    callback_failed.fetch_or(
                                        INPUT_TERMINATED,
                                        std::sync::atomic::Ordering::Relaxed,
                                    );
                                    let mut state =
                                        callback_state.lock().unwrap_or_else(|e| e.into_inner());
                                    if state.failures.record("receiver_closed") {
                                        tracing::debug!(
                                            event = "ws_delivery_closed",
                                            reason = "receiver_closed",
                                            action = "drop_message",
                                            failure_count = state.failures.total
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        log_decode_failure(
                            &error,
                            &value,
                            &mut callback_state
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .failures,
                        );
                        callback_failed
                            .fetch_or(INPUT_MALFORMED, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            },
            [
                BitbankOption::WebSocketChannels(channels),
                BitbankOption::WebSocketConfig(wsc),
            ],
        )
        .await;
    match connection {
        Ok(connection) => {
            let reconnect = connection.reconnect_state();
            let mut observed_reconnect = false;
            let started = std::time::Instant::now();
            loop {
                let reconnecting = reconnect.is_reconnecting();
                if reconnecting && !observed_reconnect {
                    failed.fetch_or(INPUT_RECONNECT, std::sync::atomic::Ordering::Relaxed);
                    tracing::warn!(
                        event = "market_feed_state",
                        state = "reconnecting",
                        reason_code = "connection_state_changed",
                        action = "invalidate_evaluation"
                    );
                }
                if observed_reconnect && !reconnecting {
                    tracing::info!(
                        event = "market_feed_state",
                        state = "receiving",
                        reason_code = "reconnected",
                        evaluation_valid = false
                    );
                }
                observed_reconnect = reconnecting;
                check_stale(
                    &mut monitor_state.lock().unwrap_or_else(|e| e.into_inner()),
                    started,
                    stale_after,
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        Err(error) => {
            log_start_failure(&error);
            failed.fetch_or(INPUT_TERMINATED, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
