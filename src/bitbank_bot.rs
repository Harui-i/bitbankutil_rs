use crate::bitbank_structs::{
    BitbankCircuitBreakInfo, BitbankDepth, BitbankDepthDiff, BitbankDepthWhole,
    BitbankTickerResponse, BitbankTransactionDatum,
};
use crate::market_event::{
    MarketDepthSnapshot, MarketEvent, MarketEventConversionError, MarketTrade,
};
use crate::trading_venue::{NoTradingVenue, TradingVenue};
use crate::websocket_handler::run_websocket;
use crypto_botters::bitbank::BitbankOption;
use crypto_botters::generic_api_client::websocket::WebSocketConfig;
use log::{error, warn};
use std::convert::Infallible;
use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// ボットが受け取る市場データ、またはユーザー定義のイベント。
/// 市場データはまず `TradingVenue::on_market_event()` に渡し、その後で戦略に渡す。
/// paperでは、この順序により戦略が仮想約定後の注文・残高を参照できる。
#[derive(Debug, Clone, PartialEq)]
pub enum BotEvent<C = Infallible> {
    Market(MarketEvent),
    Custom(C),
}

/// 戦略が残高取得・発注・取消を行い、ボットへイベントを送るための窓口。
/// `venue()` でlive tradingまたはpaper tradingの注文・口座操作を利用できる。
/// クローンしても別口座は作らず、同じ口座と注文を操作する。
pub struct BotContext<C = Infallible, V = NoTradingVenue> {
    venue: V,
    time: Option<i64>,
    pending: std::sync::Arc<std::sync::Mutex<Pending<C>>>,
}

// CにCloneを要求せず、同じ派生キューと注文・口座操作を共有する。
impl<C, V: Clone> Clone for BotContext<C, V> {
    fn clone(&self) -> Self {
        Self {
            venue: self.venue.clone(),
            time: self.time,
            pending: self.pending.clone(),
        }
    }
}

impl<C: Send + 'static, V: TradingVenue> BotContext<C, V> {
    pub fn now_ms(&self) -> i64 {
        self.time
            .unwrap_or_else(|| crate::execution_recording::timestamp_ms() as i64)
    }

    pub fn venue(&self) -> &V {
        &self.venue
    }

    pub async fn emit(
        &self,
        event: BotEvent<C>,
    ) -> Result<(), mpsc::error::SendError<BotEvent<C>>> {
        {
            let pending = &self.pending;
            let mut guard = pending.lock().unwrap();
            if !guard.open {
                return Err(mpsc::error::SendError(event));
            }
            if guard.emitted >= guard.limit {
                guard.overflow = true;
                return Err(mpsc::error::SendError(event));
            }
            guard.emitted += 1;
            guard.queue.push_back(event);
            return Ok(());
        }
    }
}

/// 市場データなどを受け取り、発注や取消を判断する処理。
/// paperの仮想約定・残高更新は、ボットが `handle_event()` を呼ぶ前に行う。
/// 戦略内で `TradingVenue::on_market_event()` を呼ぶ必要はない。
/// ユーザー定義のイベントを使わない場合はCustomEventにInfallibleを指定する。
pub trait BotStrategy<V: TradingVenue = NoTradingVenue>: Send + 'static {
    type CustomEvent: Send + 'static;
    type Error: std::fmt::Debug + Send + 'static;

    fn handle_event(
        &mut self,
        event: BotEvent<Self::CustomEvent>,
        ctx: &BotContext<Self::CustomEvent, V>,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send;
}

/// ボットアクターを生かし続け、基本的なライフサイクル制御を提供するハンドル。
pub struct BotHandle<E> {
    event_tx: mpsc::Sender<BotEvent<E>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<JoinHandle<Result<(), String>>>,
}

impl<E: Send + 'static> BotHandle<E> {
    pub fn event_sender(&self) -> mpsc::Sender<BotEvent<E>> {
        self.event_tx.clone()
    }

    pub async fn shutdown(mut self) -> Result<(), String> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        match self.join_handle.take() {
            Some(handle) => handle.await.map_err(|e| e.to_string())?,
            _ => Ok(()),
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

fn spawn_bot_actor<S, V>(strategy: S, venue: V, buffer: usize) -> BotHandle<S::CustomEvent>
where
    V: TradingVenue,
    S: BotStrategy<V>,
{
    let (event_tx, mut event_rx) = mpsc::channel(buffer);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let join_handle = tokio::spawn(async move {
        let mut runner = BotRunner::new(strategy, venue);
        let mut stopping = false;
        loop {
            select! {
                biased;
                _ = &mut shutdown_rx, if !stopping => { stopping = true; event_rx.close(); }
                maybe_event = event_rx.recv() => {
                    let Some(event) = maybe_event else { break; };
                    if let Err(err) = runner.process_live(event).await { return Err(format!("bot processing failed: {err:?}")); }
                }
            }
        }
        Ok(())
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

#[derive(Debug)]
pub struct BitbankMarketEventConverter {
    pair: String,
    depth: BitbankDepth,
}

impl BitbankMarketEventConverter {
    pub fn new(pair: String) -> Self {
        Self {
            pair,
            depth: BitbankDepth::new(),
        }
    }

    pub fn convert(
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
                self.depth
                    .insert_diff(depth_diff)
                    .map_err(MarketEventConversionError::InvalidDepth)?;
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
                self.depth
                    .update_whole(depth_whole)
                    .map_err(MarketEventConversionError::InvalidDepth)?;
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
    event_tx: mpsc::Sender<BotEvent<E>>,
) where
    E: Send + 'static,
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
            if event_tx.send(BotEvent::Market(event)).await.is_err() {
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
pub struct BitbankBotBuilder<S, V = NoTradingVenue>
where
    V: TradingVenue,
    S: BotStrategy<V>,
{
    strategy: S,
    pairs: Vec<String>,
    default_options: Vec<BitbankOption>,
    websocket_config: WebSocketConfig,
    buffer_size: usize,
    venue: V,
}

impl<S, V> BitbankBotBuilder<S, V>
where
    V: TradingVenue,
    S: BotStrategy<V>,
{
    pub fn new(strategy: S, venue: V) -> Self {
        Self {
            strategy,
            pairs: Vec::new(),
            default_options: Vec::new(),
            websocket_config: WebSocketConfig::default(),
            buffer_size: 128,
            venue,
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

    pub fn spawn(self) -> BitbankBotRuntime<S::CustomEvent> {
        if self.pairs.is_empty() {
            warn!("spawning a Bitbank bot without any subscribed pair");
        }

        let actor = spawn_bot_actor(self.strategy, self.venue, self.buffer_size);
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

impl<S: BotStrategy> BitbankBotBuilder<S, NoTradingVenue> {
    /// 注文・口座取得を使わない市場データ閲覧用の起動設定。
    pub fn market_data(strategy: S) -> Self {
        Self::new(strategy, NoTradingVenue)
    }
}

/// アクターとフィードタスクを生かし続けるランタイム。ランタイムをドロップすると
/// フィードタスクが中止される。正常に停止する必要がある場合は[`Self::shutdown`]を呼び出す。
pub struct BitbankBotRuntime<E> {
    bot_handle: Option<BotHandle<E>>,
    feed_handles: Vec<JoinHandle<()>>,
}

impl<E: Send + 'static> BitbankBotRuntime<E> {
    pub fn event_sender(&self) -> mpsc::Sender<BotEvent<E>> {
        self.bot_handle
            .as_ref()
            .expect("bitbank bot runtime missing actor handle")
            .event_sender()
    }

    pub async fn shutdown(mut self) -> Result<(), String> {
        for handle in &self.feed_handles {
            handle.abort();
        }
        match self.bot_handle.take() {
            Some(bot_handle) => bot_handle.shutdown().await,
            _ => Ok(()),
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
    event_tx: &mpsc::Sender<BotEvent<E>>,
) where
    E: Send + 'static,
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
            if event_tx.send(BotEvent::Market(event)).await.is_err() {
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

    use crate::order_domain::OrderId;
    use crate::trading_venue::{
        AccountSnapshot, PlacedOrder, PlacementRequest, TradingVenueError, TradingVenueFuture,
    };
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct ProbeVenue {
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    impl TradingVenue for ProbeVenue {
        fn account_snapshot<'a>(
            &'a self,
            _pair: &'a str,
        ) -> TradingVenueFuture<'a, AccountSnapshot> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("snapshot");
                Ok(AccountSnapshot {
                    open_orders: vec![],
                    balances: vec![],
                })
            })
        }
        fn place_order(&self, request: PlacementRequest) -> TradingVenueFuture<'_, PlacedOrder> {
            NoTradingVenue.place_order(request)
        }
        fn cancel_orders<'a>(
            &'a self,
            pair: &'a str,
            ids: Vec<OrderId>,
        ) -> TradingVenueFuture<'a, ()> {
            NoTradingVenue.cancel_orders(pair, ids)
        }
        fn on_market_event(&self, event: &MarketEvent) -> Result<(), TradingVenueError> {
            self.calls.lock().unwrap().push("apply");
            if matches!(event, MarketEvent::Transactions { pair, .. } if pair == "fail") {
                return Err(TradingVenueError::StatePoisoned);
            }
            Ok(())
        }
    }

    struct ProbeStrategy {
        handled: mpsc::UnboundedSender<()>,
        gate: Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>,
    }

    impl BotStrategy<ProbeVenue> for ProbeStrategy {
        type CustomEvent = ();
        type Error = String;
        async fn handle_event(
            &mut self,
            event: BotEvent<()>,
            ctx: &BotContext<(), ProbeVenue>,
        ) -> Result<(), Self::Error> {
            if let Some((entered, release)) = self.gate.take() {
                entered.send(()).unwrap();
                release.await.unwrap();
            }
            ctx.venue().calls.lock().unwrap().push(match event {
                BotEvent::Market(_) => "market",
                BotEvent::Custom(()) => "custom",
            });
            ctx.venue().account_snapshot("btc_jpy").await.unwrap();
            self.handled.send(()).unwrap();
            Ok(())
        }
    }

    fn transaction_event(pair: &str) -> BotEvent<()> {
        BotEvent::Market(MarketEvent::Transactions {
            pair: pair.to_owned(),
            transactions: vec![],
        })
    }

    async fn wait_handled(receiver: &mut mpsc::UnboundedReceiver<()>) {
        tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn actor_applies_market_once_before_strategy() {
        let venue = ProbeVenue::default();
        let (handled, mut receiver) = mpsc::unbounded_channel();
        let runtime = BitbankBotBuilder::new(
            ProbeStrategy {
                handled,
                gate: None,
            },
            venue.clone(),
        )
        .spawn();
        let sender = runtime.event_sender();
        sender.send(transaction_event("btc_jpy")).await.unwrap();
        sender.send(BotEvent::Custom(())).await.unwrap();
        wait_handled(&mut receiver).await;
        wait_handled(&mut receiver).await;
        runtime.shutdown().await.unwrap();
        assert_eq!(
            *venue.calls.lock().unwrap(),
            ["apply", "market", "snapshot", "custom", "snapshot"]
        );
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn next_market_event_cannot_apply_while_strategy_is_running() {
        let venue = ProbeVenue::default();
        let (handled, mut receiver) = mpsc::unbounded_channel();
        let (entered, entered_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let runtime = BitbankBotBuilder::new(
            ProbeStrategy {
                handled,
                gate: Some((entered, release_rx)),
            },
            venue.clone(),
        )
        .spawn();
        let sender = runtime.event_sender();
        sender.send(BotEvent::Custom(())).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), entered_rx)
            .await
            .unwrap()
            .unwrap();
        sender.send(transaction_event("btc_jpy")).await.unwrap();
        assert!(venue.calls.lock().unwrap().is_empty());
        release.send(()).unwrap();
        wait_handled(&mut receiver).await;
        wait_handled(&mut receiver).await;
        runtime.shutdown().await.unwrap();
        assert_eq!(
            *venue.calls.lock().unwrap(),
            ["custom", "snapshot", "apply", "market", "snapshot"]
        );
    }

    #[tokio::test]
    async fn market_data_runtime_delivers_events_without_credentials() {
        struct Strategy(Option<oneshot::Sender<()>>);
        impl BotStrategy for Strategy {
            type CustomEvent = Infallible;
            type Error = String;
            async fn handle_event(
                &mut self,
                event: BotEvent,
                ctx: &BotContext,
            ) -> Result<(), Self::Error> {
                assert!(matches!(event, BotEvent::Market(_)));
                assert!(matches!(
                    ctx.venue().account_snapshot("btc_jpy").await,
                    Err(TradingVenueError::Unavailable)
                ));
                self.0.take().unwrap().send(()).unwrap();
                Ok(())
            }
        }
        let (done, done_rx) = oneshot::channel();
        let runtime = BitbankBotBuilder::market_data(Strategy(Some(done))).spawn();
        runtime
            .event_sender()
            .send(BotEvent::Market(MarketEvent::Transactions {
                pair: "btc_jpy".into(),
                transactions: vec![],
            }))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), done_rx)
            .await
            .unwrap()
            .unwrap();
        runtime.shutdown().await.unwrap();
    }

    struct PaperProbe {
        snapshots: mpsc::UnboundedSender<AccountSnapshot>,
    }

    impl BotStrategy<crate::paper_execution::PaperTradingVenue> for PaperProbe {
        type CustomEvent = Infallible;
        type Error = String;
        async fn handle_event(
            &mut self,
            _event: BotEvent,
            ctx: &BotContext<Infallible, crate::paper_execution::PaperTradingVenue>,
        ) -> Result<(), Self::Error> {
            self.snapshots
                .send(ctx.venue().account_snapshot("btc_jpy").await.unwrap())
                .unwrap();
            Ok(())
        }
    }

    #[tokio::test]
    async fn manual_and_replayed_trades_update_paper_before_strategy_reads_account() {
        use crate::order_domain::{BalanceSnapshot, DesiredLimitOrder};
        use crate::paper_execution::{
            PaperExecutionConfig, PaperExecutionEngine, PaperTradingVenue,
        };
        let engine = PaperExecutionEngine::new(
            PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap(),
            vec![
                BalanceSnapshot {
                    asset: "btc".into(),
                    free_amount: Decimal::ZERO,
                    locked_amount: Decimal::ZERO,
                    onhand_amount: Decimal::ZERO,
                },
                BalanceSnapshot {
                    asset: "jpy".into(),
                    free_amount: Decimal::new(1000, 0),
                    locked_amount: Decimal::ZERO,
                    onhand_amount: Decimal::new(1000, 0),
                },
            ],
            None,
        )
        .unwrap();
        let venue = PaperTradingVenue::new(engine);
        venue
            .place_order(
                DesiredLimitOrder::limit(
                    "btc_jpy".into(),
                    OrderSide::Buy,
                    Decimal::ONE,
                    Decimal::new(100, 0),
                )
                .into(),
            )
            .await
            .unwrap();
        let (snapshots, mut receiver) = mpsc::unbounded_channel();
        let runtime = BitbankBotBuilder::new(PaperProbe { snapshots }, venue).spawn();
        let sender = runtime.event_sender();
        sender
            .send(BotEvent::Market(MarketEvent::Transactions {
                pair: "btc_jpy".into(),
                transactions: vec![MarketTrade {
                    amount: Decimal::new(25, 2),
                    executed_at: 1,
                    price: Decimal::new(100, 0),
                    side: OrderSide::Sell,
                    transaction_id: 1,
                }],
            }))
            .await
            .unwrap();
        let (inbound, mut inbound_rx) = mpsc::channel(1);
        inbound
            .send(BitbankInboundMessage::Transactions(vec![
                BitbankTransactionDatum {
                    amount: Decimal::new(25, 2),
                    executed_at: 2,
                    price: Decimal::new(100, 0),
                    side: "sell".into(),
                    transaction_id: 2,
                },
            ]))
            .await
            .unwrap();
        drop(inbound);
        forward_bitbank_messages("btc_jpy".into(), &mut inbound_rx, &sender).await;
        for remaining in [Decimal::new(75, 2), Decimal::new(5, 1)] {
            let snapshot = tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(snapshot.open_orders[0].remaining_amount, remaining);
            assert_eq!(
                snapshot
                    .balances
                    .iter()
                    .find(|b| b.asset == "btc")
                    .unwrap()
                    .free_amount,
                Decimal::ONE - remaining
            );
        }
        runtime.shutdown().await.unwrap();
    }

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
        let (event_tx, mut event_rx) = mpsc::channel::<BotEvent>(4);

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
        let BotEvent::Market(MarketEvent::Ticker { pair, ticker }) = event else {
            panic!("expected ticker");
        };
        assert_eq!(pair, "btc_jpy");
        assert_eq!(ticker.last, "100");
    }
}

/// Input identity from a capture session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InputId {
    pub session_id: String,
    pub sequence: u64,
}

#[derive(Debug)]
pub enum BotRunError<E> {
    TimeReversed,
    Venue(crate::trading_venue::TradingVenueError),
    Strategy(E),
    DerivedLimit,
}

/// Sequential execution shared by network and offline inputs.
pub struct BotRunner<S: BotStrategy<V>, V: TradingVenue> {
    strategy: S,
    venue: V,
    last_time: Option<i64>,
    pub derived_limit: usize,
}
impl<S: BotStrategy<V>, V: TradingVenue> BotRunner<S, V> {
    pub fn new(strategy: S, venue: V) -> Self {
        Self {
            strategy,
            venue,
            last_time: None,
            derived_limit: 10_000,
        }
    }
    pub fn venue(&self) -> &V {
        &self.venue
    }
    pub fn advance_time(&mut self, time_ms: i64) -> Result<(), BotRunError<S::Error>> {
        if self.last_time.is_some_and(|t| time_ms < t) {
            return Err(BotRunError::TimeReversed);
        }
        self.last_time = Some(time_ms);
        Ok(())
    }
    pub async fn process(
        &mut self,
        time_ms: i64,
        input_id: Option<InputId>,
        event: BotEvent<S::CustomEvent>,
    ) -> Result<(), BotRunError<S::Error>> {
        self.advance_time(time_ms)?;
        self.process_inner(Some(time_ms), input_id, event).await
    }
    async fn process_live(
        &mut self,
        event: BotEvent<S::CustomEvent>,
    ) -> Result<(), BotRunError<S::Error>> {
        self.process_inner(None, None, event).await
    }
    async fn process_inner(
        &mut self,
        time: Option<i64>,
        input_id: Option<InputId>,
        event: BotEvent<S::CustomEvent>,
    ) -> Result<(), BotRunError<S::Error>> {
        let mut queue = std::collections::VecDeque::from([(event, None)]);
        let mut count = 0;
        while let Some((event, inherited_market_id)) = queue.pop_front() {
            if count > self.derived_limit {
                return Err(BotRunError::DerivedLimit);
            }
            count += 1;
            let pending = std::sync::Arc::new(std::sync::Mutex::new(Pending {
                queue: std::collections::VecDeque::new(),
                open: true,
                emitted: 0,
                limit: self.derived_limit.saturating_sub(count - 1 + queue.len()),
                overflow: false,
            }));
            let _close = ClosePending(pending.clone());
            let ctx = BotContext {
                venue: self.venue.clone(),
                time,
                pending: pending.clone(),
            };
            let recorder = self.venue.recorder();
            let is_market = matches!(event, BotEvent::Market(_));
            let recording_context = crate::execution_recording::RecordingContext {
                event_time_ms: Some(ctx.now_ms()),
                market_event_id: if is_market {
                    recorder.as_ref().map(|r| r.next_id())
                } else {
                    inherited_market_id
                },
                input_id: input_id.clone(),
                ..Default::default()
            };
            let market_id = recording_context.market_event_id;
            let result = crate::execution_recording::scope(recording_context, async {
                if let Some(r) = &recorder {
                    r.record(
                        if is_market {
                            "market_event"
                        } else {
                            "custom_event"
                        },
                        serde_json::json!({"id":market_id,"input_id":input_id,"type":match &event {
                            BotEvent::Market(MarketEvent::Ticker{..})=>"ticker",
                            BotEvent::Market(MarketEvent::Transactions{..})=>"transactions",
                            BotEvent::Market(MarketEvent::DepthUpdated{..})=>"depth",
                            BotEvent::Market(MarketEvent::CircuitBreakInfo{..})=>"circuit_break_info",
                            BotEvent::Custom(_)=>"custom",
                        }}),
                    );
                }
                if let BotEvent::Market(market) = &event {
                    self.venue
                        .on_market_event(market)
                        .map_err(BotRunError::Venue)?;
                }
                self.strategy
                    .handle_event(event, &ctx)
                    .await
                    .map_err(BotRunError::Strategy)
            })
            .await;
            {
                let mut p = pending.lock().unwrap();
                p.open = false;
                if p.overflow {
                    return Err(BotRunError::DerivedLimit);
                }
                queue.extend(p.queue.drain(..).map(|event| (event, market_id)));
            }
            result?;
        }
        Ok(())
    }
}

struct Pending<C> {
    queue: std::collections::VecDeque<BotEvent<C>>,
    open: bool,
    emitted: usize,
    limit: usize,
    overflow: bool,
}
struct ClosePending<C>(std::sync::Arc<std::sync::Mutex<Pending<C>>>);
impl<C> Drop for ClosePending<C> {
    fn drop(&mut self) {
        if let Ok(mut p) = self.0.lock() {
            p.open = false;
        }
    }
}

impl<E: std::fmt::Debug> std::fmt::Display for BotRunError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl<E: std::fmt::Debug> std::error::Error for BotRunError<E> {}
