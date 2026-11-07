use crate::bitbank_bot::BitbankInboundMessage;
use crypto_botters::{
    Client, bitbank::BitbankOption, generic_api_client::websocket::WebSocketConfig,
};
use tokio::sync::mpsc;

/// 公開WSを起動する。observationを指定するとJSONコールバック冒頭の時計を記録する。
/// 詳細は[`crate::clock_observation`]を参照。
pub async fn run_websocket(
    pair: String,
    client_options: Vec<BitbankOption>,
    wsc: WebSocketConfig,
    tx: mpsc::Sender<BitbankInboundMessage>,
    observation: Option<crate::execution_recording::ExecutionRecorder>,
) {
    let mut ws_client = Client::new();

    for option in client_options {
        ws_client.update_default_option(option);
    }

    let ws_client = ws_client; // 不変にする

    let channels = vec![
        format!("ticker_{}", pair).to_owned(),
        format!("transactions_{}", pair).to_owned(),
        format!("depth_diff_{}", pair).to_owned(),
        format!("depth_whole_{}", pair).to_owned(),
        format!("circuit_break_info_{}", pair).to_owned(),
    ];

    let observer = observation.map(crate::clock_observation::WsObserver::new);
    let _transactions_connection = ws_client
        .websocket(
            "",
            move |val: serde_json::Value| dispatch_callback(val, observer.as_ref(), &tx),
            [
                BitbankOption::WebSocketChannels(channels),
                BitbankOption::WebSocketConfig(wsc),
            ],
        )
        .await
        .unwrap();

    // スリープ
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    }
}

fn dispatch_callback(
    val: serde_json::Value,
    observer: Option<&crate::clock_observation::WsObserver>,
    tx: &mpsc::Sender<BitbankInboundMessage>,
) {
    let received = crate::clock_observation::ClockObservation::now();
    let decoded = crate::replay::decode_bitbank(&val);
    if let Some(observer) = observer {
        observer.observe(&val, received, matches!(&decoded, Ok(Some(_))));
    }
    match decoded {
        Ok(Some((_, message))) => {
            let tx = tx.clone();
            tokio::spawn(async move {
                let _ = tx.send(message).await;
            });
        }
        Ok(None) => {}
        Err(error) => log::warn!("invalid Bitbank callback: {error}"),
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
}

/// 遅延Paper用。コールバック内で時刻を採取し、その場で順序を保って登録する。
/// 満杯を子タスクへ逃がすと受信順が崩れるため、overflowは評価異常にする。
pub(crate) async fn run_simulation_websocket(
    pair: String,
    client_options: Vec<BitbankOption>,
    wsc: WebSocketConfig,
    tx: mpsc::Sender<crate::bitbank_bot::TimedBitbankMessage>,
    failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    observation: Option<crate::execution_recording::ExecutionRecorder>,
) {
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
    let connection = client
        .websocket(
            "",
            move |value| {
                let received = crate::clock_observation::ClockObservation::now();
                let decoded = crate::replay::decode_bitbank(&value);
                if let Some(observer) = &observer {
                    observer.observe(&value, received, matches!(&decoded, Ok(Some(_))));
                }
                match decoded {
                    Ok(Some((pair, message))) => {
                        if tx
                            .try_send((received.unix_us / 1000, pair, message))
                            .is_err()
                        {
                            callback_failed.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    Ok(None) => {}
                    Err(_) => callback_failed.store(true, std::sync::atomic::Ordering::Relaxed),
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
            loop {
                let reconnecting = reconnect.is_reconnecting();
                if reconnecting && !observed_reconnect {
                    failed.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                observed_reconnect = reconnecting;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        Err(_) => {
            failed.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
