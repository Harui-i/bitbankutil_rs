これは、別のプロジェクトで、bybitとBitbankの情報を同時に扱うために作った実装です。ActorとHandleの分離などを意識しました。
実装の際には、Alicr RyhlのActorモデルに関するブログを参考にしました。

```rust
use std::collections::BTreeMap;

use bitbankutil_rs::{
    bitbank_structs::{
        websocket_struct::BitbankWebSocketMessage, BitbankDepth, BitbankDepthDiff,
        BitbankDepthWhole, BitbankTransactionDatum, BitbankTransactionsData,
    },
    bybit::{self, BybitDepth, BybitTransactionDatum},
};
use crypto_botters::{bitbank::BitbankOption, bybit::BybitOption};

pub trait BybankStrategy {
    fn on_bb_transactions(
        &mut self,
        transactions: Vec<BitbankTransactionDatum>,
        symbol: String,
    ) -> impl std::future::Future<Output = ()> + Send;

    fn on_bb_depth_update(
        &mut self,
        depth: &BitbankDepth,
        symbol: String,
    ) -> impl std::future::Future<Output = ()> + Send;

    fn on_byb_transactions(
        &mut self,
        trades: Vec<BybitTransactionDatum>,
    ) -> impl std::future::Future<Output = ()> + Send;

    fn on_byb_depth_update(
        &mut self,
        depth: &BybitDepth,
        symbol: String,
    ) -> impl std::future::Future<Output = ()> + Send;
}

pub enum BotMessage {
    BbTransactions((Vec<BitbankTransactionDatum>, String)),
    BbDepthDiff((BitbankDepthDiff, String)),
    BbDepthWhole((BitbankDepthWhole, String)),
    ByOrderBookMessage(bybit::BybitOrderbookWebSocketMessage),
    ByTradeMessage(bybit::BybitTradeWebSocketMessage),
}

// 各取引所ごとに分けたメッセージ列挙型
pub enum BitbankMessage {
    Transactions((Vec<BitbankTransactionDatum>, String)),
    DepthDiff((BitbankDepthDiff, String)),
    DepthWhole((BitbankDepthWhole, String)),
}

pub enum BybitMessage {
    OrderBook(bybit::BybitOrderbookWebSocketMessage),
    Trade(bybit::BybitTradeWebSocketMessage),
}

// アクター: 状態を所有しメッセージを処理してユーザーのストラテジを呼び出す。
pub struct BybankActor<S: BybankStrategy + Send + 'static> {
    strategy: S,
    receiver: tokio::sync::mpsc::Receiver<BotMessage>,
    bb_depthes: BTreeMap<String, BitbankDepth>,
    byb_depthes: BTreeMap<String, BybitDepth>,
}

impl<S: BybankStrategy + Send + 'static> BybankActor<S> {
    pub fn new(
        strategy: S,
        receiver: tokio::sync::mpsc::Receiver<BotMessage>,
        pairs: Vec<String>,
    ) -> Self {
        let mut bb_depthes = BTreeMap::new();
        let mut byb_depthes = BTreeMap::new();

        for pair in &pairs {
            bb_depthes.insert(pair.clone(), BitbankDepth::new());
            let byb_pair = pair.replace("_jpy", "usdt").to_uppercase();
            byb_depthes.insert(byb_pair, BybitDepth::new());
        }

        Self {
            strategy,
            receiver,
            bb_depthes,
            byb_depthes,
        }
    }

    pub async fn run(&mut self) {
        while let Some(msg) = self.receiver.recv().await {
            match msg {
                BotMessage::BbTransactions((transactions, symbol)) => {
                    self.strategy.on_bb_transactions(transactions, symbol).await;
                }
                BotMessage::BbDepthDiff((depth_diff, pair)) => {
                    if let Some(depth) = self.bb_depthes.get_mut(&pair) {
                        depth.insert_diff(depth_diff);
                        if depth.is_complete() {
                            self.strategy.on_bb_depth_update(depth, pair).await;
                        }
                    }
                }
                BotMessage::BbDepthWhole((depth_whole, pair)) => {
                    if let Some(depth) = self.bb_depthes.get_mut(&pair) {
                        depth.update_whole(depth_whole);
                        if depth.is_complete() {
                            self.strategy.on_bb_depth_update(depth, pair).await;
                        }
                    }
                }
                BotMessage::ByOrderBookMessage(bybit_orderbook_web_socket_message) => {
                    let msg_type = bybit_orderbook_web_socket_message.r#type;
                    let orderbook_data: bybit::BybitOrderbookData =
                        bybit_orderbook_web_socket_message.data;
                    let symbol = orderbook_data.s.clone();

                    if let Some(depth) = self.byb_depthes.get_mut(&symbol) {
                        if msg_type == "snapshot" {
                            *depth = BybitDepth::new();
                            depth.update(orderbook_data);
                        } else if msg_type == "delta" {
                            depth.update(orderbook_data);
                        } else {
                            log::error!("unknown type: {}", msg_type);
                        }

                        self.strategy.on_byb_depth_update(depth, symbol).await;
                    }
                }
                BotMessage::ByTradeMessage(bybit_trade_web_socket_message) => {
                    self.strategy
                        .on_byb_transactions(bybit_trade_web_socket_message.data)
                        .await;
                }
            }
        }
    }
}

// ハンドル: websocket とアクターのタスクを生成し、クローン可能な送信子を公開する。
#[derive(Clone)]
pub struct BybankHandle {
    sender: tokio::sync::mpsc::Sender<BotMessage>,
    // Keep join handles to prevent tasks from being dropped prematurely.
    _tasks: std::sync::Arc<Vec<tokio::task::JoinHandle<()>>>,
}

impl BybankHandle {
    pub fn spawn<S: BybankStrategy + Send + 'static>(
        strategy: S,
        pairs: Vec<String>,
        wsc: crypto_botters::generic_api_client::websocket::WebSocketConfig,
    ) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(512);
        let tx_bb = tx.clone();
        let tx_byb = tx.clone();

        // Spawn the actor task
        let mut actor = BybankActor::new(strategy, rx, pairs.clone());
        let actor_task = tokio::spawn(async move { actor.run().await });

        // Build websocket topics/channels
        let mut bb_channels = Vec::new();
        for pair in &pairs {
            bb_channels.push(format!("transactions_{}", pair));
            bb_channels.push(format!("depth_diff_{}", pair));
            bb_channels.push(format!("depth_whole_{}", pair));
        }

        let byb_pairs: Vec<String> = pairs
            .iter()
            .map(|pair| pair.replace("_jpy", "usdt").to_uppercase())
            .collect();
        let mut byb_topics = Vec::new();
        for byb_pair in byb_pairs {
            byb_topics.push(format!("publicTrade.{}", byb_pair));
            byb_topics.push(format!("orderbook.50.{}", byb_pair));
        }

        // Spawn websocket tasks
        let byb_wsc = wsc.clone();
        let by_ws_client = crypto_botters::Client::new();
        let byb_task = tokio::spawn(async move {
            let bybit_closure = move |val: serde_json::Value| {
                let topic = val["topic"].as_str().unwrap();

                let tx2 = tx_byb.clone();
                if tx2.capacity() * 100 < tx2.max_capacity() * 30 {
                    log::warn!("Sender's capacity is less than 30%(bybit).");
                }
                if tx2.capacity() < 10 {
                    log::warn!("Since sender's capacity is less than 10, we will skip sending this(bybit) message.");
                    return;
                }

                if topic.starts_with("orderbook") {
                    let orderbook_message: bitbankutil_rs::bybit::BybitOrderbookWebSocketMessage =
                        serde_json::from_value(val).unwrap();
                    tokio::spawn(async move {
                        let _ = tx2
                            .send(BotMessage::ByOrderBookMessage(orderbook_message))
                            .await;
                    });
                } else if topic.starts_with("publicTrade") {
                    let trade_message: bitbankutil_rs::bybit::BybitTradeWebSocketMessage =
                        serde_json::from_value(val).unwrap();
                    tokio::spawn(async move {
                        let _ = tx2.send(BotMessage::ByTradeMessage(trade_message)).await;
                    });
                } else {
                    log::error!("unknown topic: {}", topic);
                }
            };

            let _bybit_connection = by_ws_client
                .websocket(
                    "/v5/public/linear",
                    bybit_closure,
                    [
                        BybitOption::WebSocketTopics(byb_topics),
                        BybitOption::WebSocketConfig(byb_wsc),
                    ],
                )
                .await
                .expect("failed to connect bybit websocket");

            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            }
        });

        let bb_ws_client = crypto_botters::Client::new();
        let bb_task = tokio::spawn(async move {
            let bitbank_closure = move |val: serde_json::Value| {
                let msg: BitbankWebSocketMessage = serde_json::from_value(val[1].clone()).unwrap();

                let room_name = msg.room_name;
                let room_name_splitted: Vec<&str> = room_name.split("_").collect();
                let pair = room_name_splitted[room_name_splitted.len() - 2].to_owned()
                    + "_"
                    + room_name_splitted[room_name_splitted.len() - 1];

                let tx2 = tx_bb.clone();
                log::debug!("Sender's capacity: {}", tx2.capacity());
                if tx2.capacity() * 100 < tx2.max_capacity() * 30 {
                    log::warn!("Sender's capacity is less than 30%(bitbank).");
                }

                if room_name.starts_with("transactions") {
                    let transaction_message: BitbankTransactionsData =
                        serde_json::from_value(msg.message.data).unwrap();
                    let transactions = transaction_message.transactions;

                    tokio::spawn(async move {
                        let _ = tx2
                            .send(BotMessage::BbTransactions((transactions, pair)))
                            .await;
                    });
                } else if room_name.starts_with("depth_diff") {
                    let depth_diff: BitbankDepthDiff =
                        serde_json::from_value(msg.message.data).unwrap();
                    tokio::spawn(async move {
                        let _ = tx2.send(BotMessage::BbDepthDiff((depth_diff, pair))).await;
                    });
                } else if room_name.starts_with("depth_whole") {
                    let depth_whole: BitbankDepthWhole =
                        serde_json::from_value(msg.message.data).unwrap();
                    tokio::spawn(async move {
                        let _ = tx2
                            .send(BotMessage::BbDepthWhole((depth_whole, pair)))
                            .await;
                    });
                } else {
                    log::error!("unknown room name: {}", room_name);
                }
            };

            let _transactions_connection = bb_ws_client
                .websocket(
                    "",
                    bitbank_closure,
                    [
                        BitbankOption::WebSocketChannels(bb_channels),
                        BitbankOption::WebSocketConfig(wsc),
                    ],
                )
                .await
                .expect("failed to connect bitbank websocket");

            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            }
        });

        Self {
            sender: tx,
            _tasks: std::sync::Arc::new(vec![actor_task, byb_task, bb_task]),
        }
    }

    pub fn sender(&self) -> tokio::sync::mpsc::Sender<BotMessage> {
        self.sender.clone()
    }
}

// チャネルへメッセージを転送する分離された websocket ハンドル
pub struct BitbankWsHandle {
    _task: tokio::task::JoinHandle<()>,
}

impl BitbankWsHandle {
    pub fn spawn(
        pairs: Vec<String>,
        wsc: crypto_botters::generic_api_client::websocket::WebSocketConfig,
        out: tokio::sync::mpsc::Sender<BitbankMessage>,
    ) -> Self {
        let bb_ws_client = crypto_botters::Client::new();
        let mut bb_channels = Vec::new();
        for pair in pairs {
            bb_channels.push(format!("transactions_{}", pair));
            bb_channels.push(format!("depth_diff_{}", pair));
            bb_channels.push(format!("depth_whole_{}", pair));
        }
        let task = tokio::spawn(async move {
            let bitbank_closure = move |val: serde_json::Value| {
                let msg: BitbankWebSocketMessage = serde_json::from_value(val[1].clone()).unwrap();
                let room_name = msg.room_name;
                let room_name_splitted: Vec<&str> = room_name.split("_").collect();
                let pair = room_name_splitted[room_name_splitted.len() - 2].to_owned()
                    + "_"
                    + room_name_splitted[room_name_splitted.len() - 1];

                let out2 = out.clone();
                if room_name.starts_with("transactions") {
                    let transaction_message: BitbankTransactionsData =
                        serde_json::from_value(msg.message.data).unwrap();
                    let transactions = transaction_message.transactions;
                    tokio::spawn(async move {
                        let _ = out2
                            .send(BitbankMessage::Transactions((transactions, pair)))
                            .await;
                    });
                } else if room_name.starts_with("depth_diff") {
                    let depth_diff: BitbankDepthDiff =
                        serde_json::from_value(msg.message.data).unwrap();
                    tokio::spawn(async move {
                        let _ = out2
                            .send(BitbankMessage::DepthDiff((depth_diff, pair)))
                            .await;
                    });
                } else if room_name.starts_with("depth_whole") {
                    let depth_whole: BitbankDepthWhole =
                        serde_json::from_value(msg.message.data).unwrap();
                    tokio::spawn(async move {
                        let _ = out2
                            .send(BitbankMessage::DepthWhole((depth_whole, pair)))
                            .await;
                    });
                } else {
                    log::error!("unknown room name: {}", room_name);
                }
            };

            let _transactions_connection = bb_ws_client
                .websocket(
                    "",
                    bitbank_closure,
                    [
                        BitbankOption::WebSocketChannels(bb_channels),
                        BitbankOption::WebSocketConfig(wsc),
                    ],
                )
                .await
                .expect("failed to connect bitbank websocket");

            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            }
        });

        Self { _task: task }
    }
}

pub struct BybitWsHandle {
    _task: tokio::task::JoinHandle<()>,
}

impl BybitWsHandle {
    pub fn spawn(
        pairs: Vec<String>,
        wsc: crypto_botters::generic_api_client::websocket::WebSocketConfig,
        out: tokio::sync::mpsc::Sender<BybitMessage>,
    ) -> Self {
        let by_ws_client = crypto_botters::Client::new();
        let byb_pairs: Vec<String> = pairs
            .iter()
            .map(|pair| pair.replace("_jpy", "usdt").to_uppercase())
            .collect();
        let mut byb_topics = Vec::new();
        for byb_pair in byb_pairs {
            byb_topics.push(format!("publicTrade.{}", byb_pair));
            byb_topics.push(format!("orderbook.50.{}", byb_pair));
        }

        let task = tokio::spawn(async move {
            let bybit_closure = move |val: serde_json::Value| {
                let topic = val["topic"].as_str().unwrap();

                let out2 = out.clone();
                if topic.starts_with("orderbook") {
                    let orderbook_message: bitbankutil_rs::bybit::BybitOrderbookWebSocketMessage =
                        serde_json::from_value(val).unwrap();
                    tokio::spawn(async move {
                        let _ = out2.send(BybitMessage::OrderBook(orderbook_message)).await;
                    });
                } else if topic.starts_with("publicTrade") {
                    let trade_message: bitbankutil_rs::bybit::BybitTradeWebSocketMessage =
                        serde_json::from_value(val).unwrap();
                    tokio::spawn(async move {
                        let _ = out2.send(BybitMessage::Trade(trade_message)).await;
                    });
                } else {
                    log::error!("unknown topic: {}", topic);
                }
            };

            let _bybit_connection = by_ws_client
                .websocket(
                    "/v5/public/linear",
                    bybit_closure,
                    [
                        BybitOption::WebSocketTopics(byb_topics),
                        BybitOption::WebSocketConfig(wsc),
                    ],
                )
                .await
                .expect("failed to connect bybit websocket");

            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            }
        });

        Self { _task: task }
    }
}

// 2つのチャネル経由で分離されたメッセージを受け取り集約するアクター
pub struct AggregatorActor<S: BybankStrategy + Send + 'static> {
    strategy: S,
    bb_rx: tokio::sync::mpsc::Receiver<BitbankMessage>,
    byb_rx: tokio::sync::mpsc::Receiver<BybitMessage>,
    bb_depthes: BTreeMap<String, BitbankDepth>,
    byb_depthes: BTreeMap<String, BybitDepth>,
}

impl<S: BybankStrategy + Send + 'static> AggregatorActor<S> {
    pub fn new(
        strategy: S,
        bb_rx: tokio::sync::mpsc::Receiver<BitbankMessage>,
        byb_rx: tokio::sync::mpsc::Receiver<BybitMessage>,
        pairs: Vec<String>,
    ) -> Self {
        let mut bb_depthes = BTreeMap::new();
        let mut byb_depthes = BTreeMap::new();
        for pair in &pairs {
            bb_depthes.insert(pair.clone(), BitbankDepth::new());
            byb_depthes.insert(
                pair.replace("_jpy", "usdt").to_uppercase(),
                BybitDepth::new(),
            );
        }
        Self {
            strategy,
            bb_rx,
            byb_rx,
            bb_depthes,
            byb_depthes,
        }
    }

    pub async fn run(&mut self) {
        loop {
            tokio::select! {
                maybe_bb = self.bb_rx.recv() => {
                    let Some(msg) = maybe_bb else { break; };
                    match msg {
                        BitbankMessage::Transactions((transactions, symbol)) => {
                            self.strategy.on_bb_transactions(transactions, symbol).await;
                        }
                        BitbankMessage::DepthDiff((depth_diff, pair)) => {
                            if let Some(depth) = self.bb_depthes.get_mut(&pair) {
                                depth.insert_diff(depth_diff);
                                if depth.is_complete() {
                                    self.strategy.on_bb_depth_update(depth, pair).await;
                                }
                            }
                        }
                        BitbankMessage::DepthWhole((depth_whole, pair)) => {
                            if let Some(depth) = self.bb_depthes.get_mut(&pair) {
                                depth.update_whole(depth_whole);
                                if depth.is_complete() {
                                    self.strategy.on_bb_depth_update(depth, pair).await;
                                }
                            }
                        }
                    }
                },
                maybe_byb = self.byb_rx.recv() => {
                    let Some(msg) = maybe_byb else { break; };
                    match msg {
                        BybitMessage::OrderBook(bybit_orderbook_web_socket_message) => {
                            let msg_type = bybit_orderbook_web_socket_message.r#type;
                            let orderbook_data: bybit::BybitOrderbookData = bybit_orderbook_web_socket_message.data;
                            let symbol = orderbook_data.s.clone();
                            if let Some(depth) = self.byb_depthes.get_mut(&symbol) {
                                if msg_type == "snapshot" {
                                    *depth = BybitDepth::new();
                                    depth.update(orderbook_data);
                                } else if msg_type == "delta" {
                                    depth.update(orderbook_data);
                                } else {
                                    log::error!("unknown type: {}", msg_type);
                                }
                                self.strategy.on_byb_depth_update(depth, symbol).await;
                            }
                        }
                        BybitMessage::Trade(bybit_trade_web_socket_message) => {
                            self.strategy.on_byb_transactions(bybit_trade_web_socket_message.data).await;
                        }
                    }
                }
            }
        }
    }
}

pub struct AggregatorHandle {
    _task: tokio::task::JoinHandle<()>,
}

impl AggregatorHandle {
    pub fn spawn_with_receivers<S: BybankStrategy + Send + 'static>(
        strategy: S,
        bb_rx: tokio::sync::mpsc::Receiver<BitbankMessage>,
        byb_rx: tokio::sync::mpsc::Receiver<BybitMessage>,
        pairs: Vec<String>,
    ) -> Self {
        let mut actor = AggregatorActor::new(strategy, bb_rx, byb_rx, pairs);
        let task = tokio::spawn(async move { actor.run().await });
        Self { _task: task }
    }
}
```