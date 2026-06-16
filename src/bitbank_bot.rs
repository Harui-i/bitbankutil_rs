use crate::bitbank_structs::{
    BitbankCircuitBreakInfo, BitbankDepth, BitbankDepthDiff, BitbankDepthWhole,
    BitbankTickerResponse, BitbankTransactionDatum,
};
use crate::market_event::{
    MarketCircuitBreakInfo, MarketDepthSnapshot, MarketEvent, MarketEventConversionError,
    MarketTicker, MarketTrade,
};
use crate::websocket_handler::run_websocket;
use crypto_botters::bitbank::BitbankOption;
use crypto_botters::generic_api_client::websocket::WebSocketConfig;
use log::{error, trace, warn};
use std::marker::PhantomData;
use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinError, JoinHandle};

/// 戦略がフォローアップイベントをランタイムに送り返すことを可能にする共有コンテキスト。
/// コンテキストは基になる送信者をクローンするため、戦略は後で作業をスケジュールする必要がある場合に自由に保存できる。
#[derive(Clone)]
pub struct BotContext<E> {
    event_tx: mpsc::Sender<E>,
}

impl<E: Send + 'static> BotContext<E> {
    pub fn new(event_tx: mpsc::Sender<E>) -> Self {
        Self { event_tx }
    }

    /// 内部イベント送信者のクローンを取得する。これは、戦略がBitbankのWebsocketを経由せずに
    /// カスタムイベントをパイプしたり、データ（ログなどから）を再生したりする場合に便利である。
    pub fn event_sender(&self) -> mpsc::Sender<E> {
        self.event_tx.clone()
    }

    /// イベントをランタイムにプッシュバックする。このヘルパーはチャネルの送信操作を待機し、
    /// 結果を呼び出し元に伝播する。
    pub async fn emit(&self, event: E) -> Result<(), mpsc::error::SendError<E>> {
        self.event_tx.send(event).await
    }
}

/// 戦略は、受信イベントの処理方法を表現するためにこのトレイトを実装する。
/// ランタイムは戦略インスタンスを所有し、イベントごとに排他的な可変アクセスを保証するため、
/// ユーザーは`Mutex`などの外部同期プリミティブなしで`self`に直接状態を保持できる。
pub trait BotStrategy: Send + 'static {
    type Event: Send + 'static;

    fn handle_event(
        &mut self,
        event: Self::Event,
        ctx: &BotContext<Self::Event>,
    ) -> impl std::future::Future<Output = ()> + Send;
}

/// ボットアクターを生かし続け、基本的なライフサイクル制御を提供するハンドル。
pub struct BotHandle<E> {
    event_tx: mpsc::Sender<E>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<JoinHandle<()>>,
}

impl<E: Send + 'static> BotHandle<E> {
    pub fn event_sender(&self) -> mpsc::Sender<E> {
        self.event_tx.clone()
    }

    pub async fn shutdown(mut self) -> Result<(), JoinError> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join_handle.take() {
            handle.await
        } else {
            Ok(())
        }
    }
}

impl<E> Drop for BotHandle<E> {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join_handle.take() {
            handle.abort();
        }
    }
}

fn spawn_bot_actor<S>(mut strategy: S, buffer: usize) -> BotHandle<S::Event>
where
    S: BotStrategy,
{
    let (event_tx, mut event_rx) = mpsc::channel(buffer);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let context = BotContext::new(event_tx.clone());

    let join_handle = tokio::spawn(async move {
        loop {
            select! {
                biased;
                _ = &mut shutdown_rx => {
                    trace!("bot actor received shutdown");
                    break;
                }
                maybe_event = event_rx.recv() => {
                    let Some(event) = maybe_event else { break; };
                    strategy.handle_event(event, &context).await;
                }
            }
        }
    });

    BotHandle {
        event_tx,
        shutdown_tx: Some(shutdown_tx),
        join_handle: Some(join_handle),
    }
}

/// BitbankのWebSocket接続から転送された生メッセージ。
#[derive(Debug, Clone)]
pub enum BitbankInboundMessage {
    Ticker(BitbankTickerResponse),
    Transactions(Vec<BitbankTransactionDatum>),
    DepthDiff(BitbankDepthDiff),
    DepthWhole(BitbankDepthWhole),
    CircuitBreakInfo(BitbankCircuitBreakInfo),
}

/// ボット戦略に公開される高レベルのイベント。`DepthUpdated`は、
/// 対応するペアの完全なオーダーブックスナップショットが利用可能な場合にのみ発生する。
#[derive(Debug, Clone)]
pub enum BitbankEvent {
    Ticker {
        pair: String,
        ticker: MarketTicker,
    },
    Transactions {
        pair: String,
        transactions: Vec<MarketTrade>,
    },
    DepthUpdated {
        pair: String,
        depth: MarketDepthSnapshot,
    },
    CircuitBreakInfo {
        pair: String,
        info: MarketCircuitBreakInfo,
    },
}

impl From<MarketEvent> for BitbankEvent {
    fn from(event: MarketEvent) -> Self {
        match event {
            MarketEvent::Ticker { pair, ticker } => Self::Ticker { pair, ticker },
            MarketEvent::Transactions { pair, transactions } => {
                Self::Transactions { pair, transactions }
            }
            MarketEvent::DepthUpdated { pair, depth } => Self::DepthUpdated { pair, depth },
            MarketEvent::CircuitBreakInfo { pair, info } => Self::CircuitBreakInfo { pair, info },
        }
    }
}

#[derive(Debug)]
struct BitbankMarketEventConverter {
    pair: String,
    depth: BitbankDepth,
}

impl BitbankMarketEventConverter {
    fn new(pair: String) -> Self {
        Self {
            pair,
            depth: BitbankDepth::new(),
        }
    }

    fn convert(
        &mut self,
        message: BitbankInboundMessage,
    ) -> Result<Option<MarketEvent>, MarketEventConversionError> {
        let event = match message {
            BitbankInboundMessage::Ticker(ticker) => Some(MarketEvent::Ticker {
                pair: self.pair.clone(),
                ticker: ticker.into(),
            }),
            BitbankInboundMessage::Transactions(transactions) => {
                let mut transactions = transactions
                    .into_iter()
                    .map(MarketTrade::try_from)
                    .collect::<Result<Vec<_>, _>>()?;
                transactions.sort_by_key(|trade| (trade.executed_at, trade.transaction_id));
                Some(MarketEvent::Transactions {
                    pair: self.pair.clone(),
                    transactions,
                })
            }
            BitbankInboundMessage::DepthDiff(depth_diff) => {
                self.depth.insert_diff(depth_diff);
                if self.depth.is_complete() {
                    Some(MarketEvent::DepthUpdated {
                        pair: self.pair.clone(),
                        depth: MarketDepthSnapshot::from(&self.depth),
                    })
                } else {
                    None
                }
            }
            BitbankInboundMessage::DepthWhole(depth_whole) => {
                self.depth.update_whole(depth_whole);
                if self.depth.is_complete() {
                    Some(MarketEvent::DepthUpdated {
                        pair: self.pair.clone(),
                        depth: MarketDepthSnapshot::from(&self.depth),
                    })
                } else {
                    None
                }
            }
            BitbankInboundMessage::CircuitBreakInfo(info) => Some(MarketEvent::CircuitBreakInfo {
                pair: self.pair.clone(),
                info: info.into(),
            }),
        };

        Ok(event)
    }
}

async fn run_bitbank_pair_feed<E>(
    pair: String,
    client_options: Vec<BitbankOption>,
    websocket_config: WebSocketConfig,
    event_tx: mpsc::Sender<E>,
) where
    E: From<MarketEvent> + Send + 'static,
{
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<BitbankInboundMessage>(128);
    let ws_task = tokio::spawn(run_websocket(
        pair.clone(),
        client_options,
        websocket_config,
        inbound_tx,
    ));

    let mut converter = BitbankMarketEventConverter::new(pair.clone());
    while let Some(message) = inbound_rx.recv().await {
        let event = match converter.convert(message) {
            Ok(event) => event,
            Err(err) => {
                warn!(
                    "bitbank feed for pair {} dropped an invalid market event: {:?}",
                    pair, err
                );
                continue;
            }
        };

        if let Some(event) = event {
            if event_tx.send(event.into()).await.is_err() {
                warn!(
                    "bitbank feed for pair {} stopped because downstream receiver closed",
                    pair
                );
                break;
            }
        }
    }

    ws_task.abort();
}

fn duplicate_bitbank_options(options: &[BitbankOption]) -> Vec<BitbankOption> {
    options
        .iter()
        .map(|option| match option {
            BitbankOption::Default => BitbankOption::Default,
            BitbankOption::Key(key) => BitbankOption::Key(key.clone()),
            BitbankOption::Secret(secret) => BitbankOption::Secret(secret.clone()),
            BitbankOption::HttpUrl(url) => BitbankOption::HttpUrl(*url),
            BitbankOption::HttpAuth(auth) => BitbankOption::HttpAuth(*auth),
            BitbankOption::RequestConfig(cfg) => BitbankOption::RequestConfig(cfg.clone()),
            BitbankOption::WebSocketUrl(url) => BitbankOption::WebSocketUrl(*url),
            BitbankOption::WebSocketChannels(channels) => {
                BitbankOption::WebSocketChannels(channels.clone())
            }
            BitbankOption::WebSocketConfig(config) => {
                BitbankOption::WebSocketConfig(config.clone())
            }
        })
        .collect()
}

/// BitbankのWebSocketフィードを[`BotStrategy`]に接続するビルダー。
pub struct BitbankBotBuilder<S, E>
where
    S: BotStrategy<Event = E>,
    E: From<MarketEvent> + Send + 'static,
{
    strategy: S,
    pairs: Vec<String>,
    default_options: Vec<BitbankOption>,
    websocket_config: WebSocketConfig,
    buffer_size: usize,
    _marker: PhantomData<E>,
}

impl<S, E> BitbankBotBuilder<S, E>
where
    S: BotStrategy<Event = E>,
    E: From<MarketEvent> + Send + 'static,
{
    pub fn new(strategy: S) -> Self {
        Self {
            strategy,
            pairs: Vec::new(),
            default_options: Vec::new(),
            websocket_config: WebSocketConfig::default(),
            buffer_size: 128,
            _marker: PhantomData,
        }
    }

    pub fn add_pair(mut self, pair: impl Into<String>) -> Self {
        self.pairs.push(pair.into());
        self
    }

    pub fn with_pairs(mut self, pairs: Vec<String>) -> Self {
        self.pairs = pairs;
        self
    }

    pub fn websocket_config(mut self, websocket_config: WebSocketConfig) -> Self {
        self.websocket_config = websocket_config;
        self
    }

    pub fn default_options(mut self, options: Vec<BitbankOption>) -> Self {
        self.default_options = options;
        self
    }

    pub fn buffer_size(mut self, size: usize) -> Self {
        self.buffer_size = size;
        self
    }

    pub fn spawn(self) -> BitbankBotRuntime<E> {
        if self.pairs.is_empty() {
            warn!("spawning a Bitbank bot without any subscribed pair");
        }

        let actor = spawn_bot_actor(self.strategy, self.buffer_size);
        let event_tx = actor.event_sender();
        let mut feed_handles = Vec::new();

        for pair in self.pairs {
            let pair_options = duplicate_bitbank_options(&self.default_options);
            let tx = event_tx.clone();
            let config = self.websocket_config.clone();
            let handle = tokio::spawn(async move {
                run_bitbank_pair_feed(pair, pair_options, config, tx).await;
            });
            feed_handles.push(handle);
        }

        BitbankBotRuntime {
            bot_handle: Some(actor),
            feed_handles,
        }
    }
}

/// アクターとフィードタスクを生かし続けるランタイム。ランタイムをドロップすると
/// フィードタスクが中止される。正常に停止する必要がある場合は[`Self::shutdown`]を呼び出す。
pub struct BitbankBotRuntime<E> {
    bot_handle: Option<BotHandle<E>>,
    feed_handles: Vec<JoinHandle<()>>,
}

impl<E: Send + 'static> BitbankBotRuntime<E> {
    pub fn event_sender(&self) -> mpsc::Sender<E> {
        self.bot_handle
            .as_ref()
            .expect("bitbank bot runtime missing actor handle")
            .event_sender()
    }

    pub async fn shutdown(mut self) -> Result<(), JoinError> {
        for handle in &self.feed_handles {
            handle.abort();
        }
        if let Some(bot_handle) = self.bot_handle.take() {
            bot_handle.shutdown().await
        } else {
            Ok(())
        }
    }
}

impl<E> Drop for BitbankBotRuntime<E> {
    fn drop(&mut self) {
        for handle in &self.feed_handles {
            handle.abort();
        }
        if let Some(bot_handle) = self.bot_handle.take() {
            drop(bot_handle);
        }
    }
}

/// 呼び出し元が独自の[`BitbankInboundMessage`]値のストリーム（たとえば、ログ再生）を提供する
/// 高度なシナリオ向けのヘルパー。この関数は`inbound_rx`からのメッセージを消費し、
/// 準備完了イベントを`event_tx`に転送する。
pub async fn forward_bitbank_messages<E>(
    pair: String,
    inbound_rx: &mut mpsc::Receiver<BitbankInboundMessage>,
    event_tx: &mpsc::Sender<E>,
) where
    E: From<MarketEvent> + Send + 'static,
{
    let mut converter = BitbankMarketEventConverter::new(pair.clone());
    while let Some(message) = inbound_rx.recv().await {
        let event = match converter.convert(message) {
            Ok(event) => event,
            Err(err) => {
                error!(
                    "forwarder dropping invalid market event while replaying {}: {:?}",
                    pair, err
                );
                continue;
            }
        };

        if let Some(event) = event {
            if event_tx.send(event.into()).await.is_err() {
                error!(
                    "forwarder stopping: downstream closed while replaying {}",
                    pair
                );
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::depth::Depth;
    use crate::order_domain::OrderSide;
    use rust_decimal::Decimal;
    use serde_json::Number;

    fn depth_whole() -> BitbankDepthWhole {
        serde_json::from_value(serde_json::json!({
            "asks": [["101", "1.5"]],
            "bids": [["100", "2.0"]],
            "asks_over": "0",
            "bids_under": "0",
            "asks_under": "0",
            "bids_over": "0",
            "ask_market": "0",
            "bid_market": "0",
            "timestamp": 1234,
            "sequenceId": "10"
        }))
        .unwrap()
    }

    #[test]
    fn bitbank_converter_waits_for_complete_depth() {
        let mut converter = BitbankMarketEventConverter::new("btc_jpy".to_owned());
        let diff = BitbankDepthDiff {
            a: vec![vec!["101".to_owned(), "0.5".to_owned()]],
            b: vec![vec!["100".to_owned(), "1.0".to_owned()]],
            ao: None,
            bu: None,
            au: None,
            bo: None,
            am: None,
            bm: None,
            t: 1200,
            s: "9".to_owned(),
        };

        let event = converter
            .convert(BitbankInboundMessage::DepthDiff(diff))
            .unwrap();
        assert!(event.is_none());

        let event = converter
            .convert(BitbankInboundMessage::DepthWhole(depth_whole()))
            .unwrap();

        let Some(MarketEvent::DepthUpdated { pair, depth }) = event else {
            panic!("expected depth update");
        };
        assert_eq!(pair, "btc_jpy");
        assert!(depth.is_complete());
        assert_eq!(depth.best_ask().unwrap().0, &Decimal::new(101, 0));
        assert_eq!(depth.best_bid().unwrap().0, &Decimal::new(100, 0));
        assert_eq!(depth.last_timestamp(), 1234);
    }

    #[test]
    fn bitbank_converter_maps_transactions_to_domain_side() {
        let mut converter = BitbankMarketEventConverter::new("btc_jpy".to_owned());
        let trade = BitbankTransactionDatum {
            amount: Decimal::new(25, 1),
            executed_at: 1000,
            price: Decimal::new(100, 0),
            side: "buy".to_owned(),
            transaction_id: 42,
        };

        let event = converter
            .convert(BitbankInboundMessage::Transactions(vec![trade]))
            .unwrap();

        let Some(MarketEvent::Transactions { pair, transactions }) = event else {
            panic!("expected transactions");
        };
        assert_eq!(pair, "btc_jpy");
        assert_eq!(transactions.len(), 1);
        assert_eq!(transactions[0].side, OrderSide::Buy);
    }

    #[test]
    fn bitbank_converter_orders_transactions_chronologically() {
        let mut converter = BitbankMarketEventConverter::new("btc_jpy".to_owned());
        let newest = BitbankTransactionDatum {
            amount: Decimal::new(1, 0),
            executed_at: 3000,
            price: Decimal::new(90, 0),
            side: "sell".to_owned(),
            transaction_id: 3,
        };
        let middle = BitbankTransactionDatum {
            amount: Decimal::new(1, 0),
            executed_at: 2000,
            price: Decimal::new(100, 0),
            side: "sell".to_owned(),
            transaction_id: 2,
        };
        let oldest = BitbankTransactionDatum {
            amount: Decimal::new(1, 0),
            executed_at: 1000,
            price: Decimal::new(110, 0),
            side: "sell".to_owned(),
            transaction_id: 1,
        };

        let event = converter
            .convert(BitbankInboundMessage::Transactions(vec![
                newest, middle, oldest,
            ]))
            .unwrap();

        let Some(MarketEvent::Transactions { transactions, .. }) = event else {
            panic!("expected transactions");
        };
        let ids = transactions
            .iter()
            .map(|trade| trade.transaction_id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn bitbank_converter_rejects_unknown_transaction_side() {
        let mut converter = BitbankMarketEventConverter::new("btc_jpy".to_owned());
        let trade = BitbankTransactionDatum {
            amount: Decimal::new(25, 1),
            executed_at: 1000,
            price: Decimal::new(100, 0),
            side: "unknown".to_owned(),
            transaction_id: 42,
        };

        let err = converter
            .convert(BitbankInboundMessage::Transactions(vec![trade]))
            .unwrap_err();

        assert!(matches!(
            err,
            MarketEventConversionError::InvalidTradeSide(_)
        ));
    }

    #[tokio::test]
    async fn forward_bitbank_messages_emits_market_events() {
        let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
        let (event_tx, mut event_rx) = mpsc::channel::<MarketEvent>(4);

        inbound_tx
            .send(BitbankInboundMessage::Ticker(BitbankTickerResponse {
                sell: Some("101".to_owned()),
                buy: Some("100".to_owned()),
                high: "110".to_owned(),
                low: "90".to_owned(),
                open: "95".to_owned(),
                last: "100".to_owned(),
                vol: "12.5".to_owned(),
                timestamp: Number::from(1234),
            }))
            .await
            .unwrap();
        drop(inbound_tx);

        forward_bitbank_messages("btc_jpy".to_owned(), &mut inbound_rx, &event_tx).await;

        let event = event_rx.recv().await.unwrap();
        let MarketEvent::Ticker { pair, ticker } = event else {
            panic!("expected ticker");
        };
        assert_eq!(pair, "btc_jpy");
        assert_eq!(ticker.last, "100");
    }
}
