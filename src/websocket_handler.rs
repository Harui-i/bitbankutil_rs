use crate::bitbank_bot::BotMessage;
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
    tx: mpsc::Sender<BotMessage>, // TODO: 野望として、ここでmpsc::Senderを渡すのではなくて、それをラップしたHandlerを渡すようにするといいかも
) {
    let mut ws_client = Client::new();

    for option in client_options {
        ws_client.update_default_option(option);
    }

    let ws_client = ws_client; // immutalize

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

                // dispatch according to room_name
                // TODO: ここ、 fn dispatch_message(ws_msg: BitbankWebSocketMessage) -> BotMessage　なる関数を作って、ディスパッチ処理をそこに移してもいいかも
                if room_name.starts_with("ticker") {
                    let ticker: BitbankTickerResponse =
                        serde_json::from_value(ws_msg.message.data).unwrap();
                    let tx2 = tx.clone();
                    tokio::spawn(async move {
                        tx2.send(BotMessage::Ticker(ticker)).await.unwrap();
                    });
                } else if room_name.starts_with("transactions") {
                    let transaction_message: BitbankTransactionsData =
                        serde_json::from_value(ws_msg.message.data).unwrap();
                    let transactions = transaction_message.transactions;

                    let tx2 = tx.clone();
                    tokio::spawn(async move {
                        tx2.send(BotMessage::Transactions(transactions))
                            .await
                            .unwrap();
                    });
                } else if room_name.starts_with("depth_diff") {
                    let depth_diff_message: BitbankDepthDiff =
                        serde_json::from_value(ws_msg.message.data).unwrap();

                    let tx2 = tx.clone();

                    // without `move`, tx2 is borrowed. but adding `move`, tx2 is moved to this closure.
                    tokio::spawn(async move {
                        tx2.send(BotMessage::DepthDiff(depth_diff_message))
                            .await
                            .unwrap();
                    });
                } else if room_name.starts_with("depth_whole") {
                    let depth_whole_message: BitbankDepthWhole =
                        serde_json::from_value(ws_msg.message.data).unwrap();
                    let tx2 = tx.clone();

                    // without `move`, tx2 is borrowed. but adding `move`, tx2 is moved to this closure.
                    tokio::spawn(async move {
                        tx2.send(BotMessage::DepthWhole(depth_whole_message))
                            .await
                            .unwrap();
                    });
                } else if room_name.starts_with("circuit_break_info") {
                    let circuit_break_info: BitbankCircuitBreakInfo =
                        serde_json::from_value(ws_msg.message.data).unwrap();
                    let tx2 = tx.clone();
                    tokio::spawn(async move {
                        tx2.send(BotMessage::CircuitBreakInfo(circuit_break_info))
                            .await
                            .unwrap();
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

    // sleep
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    }
}
