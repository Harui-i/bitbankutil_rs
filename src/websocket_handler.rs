use crate::bitbank_bot::BitbankInboundMessage;
use crate::bitbank_structs::websocket_struct::BitbankWebSocketMessage;
use crate::bitbank_structs::{
    BitbankCircuitBreakInfo, BitbankDepthDiff, BitbankDepthWhole, BitbankTickerResponse,
    BitbankTransactionsData,
};
use crypto_botters::{
    bitbank::BitbankOption, generic_api_client::websocket::WebSocketConfig, Client,
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

    let ws_client = ws_client; // 不変にします

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
            move |val: serde_json::Value| {
                let ws_msg: BitbankWebSocketMessage =
                    serde_json::from_value(val[1].clone()).unwrap();
                let room_name = ws_msg.room_name;

                // room_nameに応じてディスパッチします
                if room_name.starts_with("ticker") {
                    let ticker: BitbankTickerResponse =
                        serde_json::from_value(ws_msg.message.data).unwrap();
                    let tx2 = tx.clone();
                    tokio::spawn(async move {
                        if tx2
                            .send(BitbankInboundMessage::Ticker(ticker))
                            .await
                            .is_err()
                        {
                            log::debug!("dropping ticker message; receiver hung up");
                        }
                    });
                } else if room_name.starts_with("transactions") {
                    let transaction_message: BitbankTransactionsData =
                        serde_json::from_value(ws_msg.message.data).unwrap();
                    let transactions = transaction_message.transactions;

                    let tx2 = tx.clone();
                    tokio::spawn(async move {
                        if tx2
                            .send(BitbankInboundMessage::Transactions(transactions))
                            .await
                            .is_err()
                        {
                            log::debug!("dropping transactions message; receiver hung up");
                        }
                    });
                } else if room_name.starts_with("depth_diff") {
                    let depth_diff_message: BitbankDepthDiff =
                        serde_json::from_value(ws_msg.message.data).unwrap();

                    let tx2 = tx.clone();

                    // `move`なしではtx2は借用されますが、`move`を追加すると、tx2はこのクロージャに移動されます。
                    tokio::spawn(async move {
                        if tx2
                            .send(BitbankInboundMessage::DepthDiff(depth_diff_message))
                            .await
                            .is_err()
                        {
                            log::debug!("dropping depth diff message; receiver hung up");
                        }
                    });
                } else if room_name.starts_with("depth_whole") {
                    let depth_whole_message: BitbankDepthWhole =
                        serde_json::from_value(ws_msg.message.data).unwrap();
                    let tx2 = tx.clone();

                    // `move`なしではtx2は借用されますが、`move`を追加すると、tx2はこのクロージャに移動されます。
                    tokio::spawn(async move {
                        if tx2
                            .send(BitbankInboundMessage::DepthWhole(depth_whole_message))
                            .await
                            .is_err()
                        {
                            log::debug!("dropping depth snapshot; receiver hung up");
                        }
                    });
                } else if room_name.starts_with("circuit_break_info") {
                    let circuit_break_info: BitbankCircuitBreakInfo =
                        serde_json::from_value(ws_msg.message.data).unwrap();
                    let tx2 = tx.clone();
                    tokio::spawn(async move {
                        if tx2
                            .send(BitbankInboundMessage::CircuitBreakInfo(circuit_break_info))
                            .await
                            .is_err()
                        {
                            log::debug!("dropping circuit break info; receiver hung up");
                        }
                    });
                } else {
                    panic!("unknown room name: {}", room_name);
                }
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
