use crate::bitbank_bot::BitbankInboundMessage;
use crypto_botters::{
    Client, bitbank::BitbankOption, generic_api_client::websocket::WebSocketConfig,
};
use tokio::sync::mpsc;

pub async fn run_websocket(
    pair: String,
    client_options: Vec<BitbankOption>,
    wsc: WebSocketConfig,
    tx: mpsc::Sender<BitbankInboundMessage>,
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

    let _transactions_connection = ws_client
        .websocket(
            "",
            move |val: serde_json::Value| match crate::replay::decode_bitbank(&val) {
                Ok(Some((_, message))) => {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(message).await;
                    });
                }
                Ok(None) => {}
                Err(error) => log::warn!("invalid Bitbank callback: {error}"),
            },
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
