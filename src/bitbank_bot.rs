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
/// 市場データはまずvenueに渡し（復旧経路では `on_market_event_recovering`）、その後で戦略に渡す。
/// paper tradingでは、この順序により戦略がwatermarkまで確定した注文・残高を参照できる。
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
    received_at_ms: i64,
    time: Option<std::sync::Arc<std::sync::atomic::AtomicI64>>,
    pending: std::sync::Arc<std::sync::Mutex<Pending<C>>>,
}

// CにCloneを要求せず、同じ派生キューと注文・口座操作を共有する。
impl<C, V: Clone> Clone for BotContext<C, V> {
    fn clone(&self) -> Self {
        Self {
            venue: self.venue.clone(),
            received_at_ms: self.received_at_ms,
            time: self.time.clone(),
            pending: self.pending.clone(),
        }
    }
}

impl<C: Send + 'static, V: TradingVenue> BotContext<C, V> {
    /// このイベントが登録された時刻。API待機・配送滞留後も変わらない。
    pub fn received_at_ms(&self) -> i64 {
        self.received_at_ms
    }
    pub fn now_ms(&self) -> i64 {
        self.time
            .as_ref()
            .map(|t| t.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or_else(|| crate::execution_recording::timestamp_ms() as i64)
    }

    pub fn venue(&self) -> &V {
        &self.venue
    }

    /// ローカル仮想時刻にカスタムイベントを予約する。live経路・過去・閉じたcontextは拒否する。
    /// 同時刻は予約採番順。実行はreplayランナーが行う。模擬API待機中は時計が進む。
    pub fn schedule_at(&self, local_ms: i64, event: C) -> Result<(), ScheduleError> {
        self.time.as_ref().ok_or(ScheduleError::NotReplay)?;
        let now = self.now_ms();
        if local_ms < now {
            return Err(ScheduleError::Past);
        }
        let mut p = self.pending.lock().unwrap();
        if !p.open {
            return Err(ScheduleError::Closed);
        }
        if p.scheduled.len() >= p.schedule_limit {
            p.schedule_overflow = true;
            return Err(ScheduleError::Limit);
        }
        p.scheduled.push((local_ms, event));
        Ok(())
    }

    /// ローカル仮想時刻からの非負の遅延で予約する。加算の範囲外は拒否する。
    pub fn schedule_after(&self, delay_ms: i64, event: C) -> Result<(), ScheduleError> {
        self.time.as_ref().ok_or(ScheduleError::NotReplay)?;
        let now = self.now_ms();
        if delay_ms < 0 {
            return Err(ScheduleError::Past);
        }
        let at = now.checked_add(delay_ms).ok_or(ScheduleError::Overflow)?;
        self.schedule_at(at, event)
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
/// paper tradingの約定登録と、設定したwatermarkまでの残高更新は `handle_event()` の前に行う。
/// 戦略内でvenueへ市場データを再適用する必要はない。
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
        let wake = runner.wake.clone();
        loop {
            if let Err(err) = runner.poll_live() {
                runner
                    .venue()
                    .mark_market_incomplete("bot processing failed");
                runner.record_settlement();
                return Err(format!("bot processing failed: {err:?}"));
            }
            select! {
                biased;
                _ = &mut shutdown_rx, if !stopping => { stopping = true; event_rx.close(); }
                maybe_event = event_rx.recv(), if !runner.handler_pending() || runner.venue().paper_settlement_status().is_some_and(|s|s.pending_operations>0) => {
                    let Some(event) = maybe_event else {
                        // 将来の市場入力がなければwatermark待ちを完了できない。
                        // 送信済み操作はvenueに残し、shutdownを無期限に待たせない。
                        if runner.handler_pending() {
                            if runner.venue().paper_settlement_status().is_some_and(|s|s.pending_operations>0) {
                                runner.venue().mark_market_incomplete("shutdown with pending strategy operation");
                                runner.stop_pending_handler();
                            } else if let Err(err)=runner.drive_direct(false).await {
                                runner.venue().mark_market_incomplete("bot processing failed");
                                runner.record_settlement();
                                return Err(format!("bot processing failed: {err:?}"));
                            }
                        }
                        break;
                    };
                    if let Err(err) = runner.ingest(None, event, true, None, false) {
                        runner.venue().mark_market_incomplete("bot input failed");
                        runner.record_settlement();
                        return Err(format!("bot input failed: {err:?}"));
                    }
                }
                _ = wake.notified() => {}
            }
        }
        runner.record_settlement();
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
    // 合成の時刻/資金fixture用。注文をspread内に置き、先行数量を0にする。
    fn seeded_engine(
        mut engine: crate::paper_execution::PaperExecutionEngine,
        exchange_ms: i64,
    ) -> crate::paper_execution::PaperExecutionEngine {
        engine.apply_market_event(&crate::market_event::MarketEvent::DepthUpdated {
            pair: "btc_jpy".into(),
            depth: crate::market_event::MarketDepthSnapshot::new(
                std::collections::BTreeMap::from([(1_000_000.into(), 1.0)]),
                std::collections::BTreeMap::from([(1.into(), 1.0)]),
                exchange_ms,
            ),
        });
        engine
    }

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
                    query: None,
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
            event: BotEvent,
            ctx: &BotContext<Infallible, crate::paper_execution::PaperTradingVenue>,
        ) -> Result<(), Self::Error> {
            if let BotEvent::Market(MarketEvent::Transactions { transactions, .. }) = event {
                let through = transactions.iter().map(|t| t.executed_at).max().unwrap();
                // この合成入力では取引の完了境界が既知。
                ctx.venue().advance_trade_watermark(through).unwrap();
            }
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
            crate::paper_execution::PaperClock::Replay { now_ms: 0 },
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
        .map(|engine| seeded_engine(engine, 0))
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

/// 記録セッション内の入力を識別するセッションIDとsequence。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InputId {
    pub session_id: String,
    pub sequence: u64,
}

/// 予約APIの入力・ライフサイクルエラー。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    NotReplay,
    Past,
    Closed,
    Overflow,
    Limit,
}

/// 予約の識別子はレコーダーとは独立したランナー内の単調採番。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduledEventInfo {
    pub id: u64,
    pub scheduled_at_ms: i64,
    pub execute_at_ms: i64,
    pub input_id: Option<InputId>,
}
struct ScheduledEvent<C> {
    info: ScheduledEventInfo,
    event: C,
}

#[derive(Debug)]
pub enum BotRunError<E> {
    TimeReversed,
    Venue(crate::trading_venue::TradingVenueError),
    Strategy(E),
    DerivedLimit,
    ScheduledLimit,
    ScheduleIdOverflow,
    DeliveryLimit,
    Stopped,
}

/// 市場受信、venue内部期限、逐次の戦略配送を管理するランナー。
/// 模擬操作を待つ間も市場はvenueへ登録するが、戦略ハンドラーは同時実行しない。
/// 詳しい実行手順は `docs/offline-replay.md` を参照。
struct QueuedEvent<C> {
    received_at_ms: i64,
    event: BotEvent<C>,
    input_id: Option<InputId>,
    market_id: Option<u64>,
    derived: bool,
    recovery: bool,
}

struct ActiveHandler<S: BotStrategy<V>, V: TradingVenue> {
    future: std::pin::Pin<Box<dyn std::future::Future<Output = (S, Result<(), S::Error>)> + Send>>,
    pending: std::sync::Arc<std::sync::Mutex<Pending<S::CustomEvent>>>,
    input_id: Option<InputId>,
    market_id: Option<u64>,
    recovery: bool,
}

struct RunnerWake(std::sync::Arc<tokio::sync::Notify>);
impl std::task::Wake for RunnerWake {
    fn wake(self: std::sync::Arc<Self>) {
        self.0.notify_one();
    }
    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        self.0.notify_one();
    }
}

pub struct BotRunner<S: BotStrategy<V>, V: TradingVenue> {
    strategy: Option<S>,
    active: Option<ActiveHandler<S, V>>,
    delivery: std::collections::VecDeque<QueuedEvent<S::CustomEvent>>,
    venue: V,
    last_time: Option<i64>,
    replay_clock: std::sync::Arc<std::sync::atomic::AtomicI64>,
    wake: std::sync::Arc<tokio::sync::Notify>,
    derived_count: usize,
    pub derived_limit: usize,
    /// 戦略待機中に配送を保留する入力の最大件数。
    pub delivery_limit: usize,
    /// 未実行予約数と、同一時刻での内部実行数の上限。
    pub scheduled_limit: usize,
    scheduled: std::collections::BTreeMap<(i64, u64), ScheduledEvent<S::CustomEvent>>,
    next_scheduled_id: u64,
    scheduled_tick: Option<(i64, usize)>,
}
impl<S: BotStrategy<V>, V: TradingVenue> Drop for BotRunner<S, V> {
    fn drop(&mut self) {
        self.venue.set_replay_waker(None);
        self.venue.set_replay_driver(false);
    }
}
impl<S: BotStrategy<V>, V: TradingVenue> BotRunner<S, V> {
    pub fn new(strategy: S, venue: V) -> Self {
        Self {
            strategy: Some(strategy),
            active: None,
            delivery: Default::default(),
            venue,
            last_time: None,
            replay_clock: std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0)),
            wake: std::sync::Arc::new(tokio::sync::Notify::new()),
            derived_count: 0,
            derived_limit: 10_000,
            delivery_limit: 10_000,
            scheduled_limit: 10_000,
            scheduled: Default::default(),
            next_scheduled_id: 0,
            scheduled_tick: None,
        }
    }
    pub fn venue(&self) -> &V {
        &self.venue
    }
    pub fn pending_scheduled(&self) -> Vec<ScheduledEventInfo> {
        self.scheduled.values().map(|e| e.info.clone()).collect()
    }
    pub fn pending_delivery_count(&self) -> usize {
        self.delivery.len()
    }
    /// 全追加経路で、退避・復元分を含む最終保留数を事前検証する。
    fn ensure_delivery_capacity(&self, additional: usize) -> Result<(), BotRunError<S::Error>> {
        if self
            .delivery
            .len()
            .checked_add(additional)
            .is_none_or(|len| len > self.delivery_limit)
        {
            return Err(BotRunError::DeliveryLimit);
        }
        Ok(())
    }
    pub fn handler_pending(&self) -> bool {
        self.active.is_some()
    }
    pub fn now_ms(&self) -> Option<i64> {
        self.last_time
    }
    pub fn next_scheduled_time(&self) -> Option<i64> {
        self.scheduled.first_key_value().map(|(key, _)| key.0)
    }
    pub fn next_internal_time(&self) -> Option<i64> {
        self.next_scheduled_time()
            .into_iter()
            .chain(self.venue.next_internal_time())
            .min()
            .map(|t| self.last_time.map_or(t, |now| now.max(t)))
    }
    /// 現時刻で戦略をpollし、模擬注文待ちの場合だけ呼出元へ制御を戻す。
    /// 通常のasync処理は従来どおり時刻を固定して完了を待つ。
    pub async fn poll_replay(&mut self) -> Result<(), BotRunError<S::Error>> {
        self.venue.set_replay_driver(true);
        loop {
            let waker = std::task::Waker::from(std::sync::Arc::new(RunnerWake(self.wake.clone())));
            self.venue.set_replay_waker(Some(waker.clone()));
            self.start_handler(true)?;
            if self.active.is_none() {
                return Ok(());
            }
            let polled = self.poll_active(&mut std::task::Context::from_waker(&waker));
            if let std::task::Poll::Ready((strategy, result)) = polled {
                self.strategy = Some(strategy);
                self.finish_handler(result)?;
                continue;
            }
            // JoinSet内で要求を登録するタスクにも一度実行機会を与える。
            tokio::task::yield_now().await;
            if let std::task::Poll::Ready((strategy, result)) =
                self.poll_active(&mut std::task::Context::from_waker(&waker))
            {
                self.strategy = Some(strategy);
                self.finish_handler(result)?;
                continue;
            }
            if self.venue.next_internal_time().is_some()
                || self
                    .venue
                    .paper_settlement_status()
                    .is_some_and(|s| s.pending_operations > 0)
            {
                return Ok(());
            }
            self.wake.notified().await;
        }
    }
    pub async fn process_next_internal(
        &mut self,
        recovery: bool,
    ) -> Result<(), BotRunError<S::Error>> {
        let Some(time) = self.next_internal_time() else {
            return Ok(());
        };
        self.advance_time(time)?;
        // 同時刻はvenue内部処理、戦略予約の順。市場入力は呼出元で先に処理する。
        self.venue
            .process_internal_events(time)
            .map_err(BotRunError::Venue)?;
        if self.next_scheduled_time().is_some_and(|t| t <= time) {
            self.enqueue_next_scheduled(recovery)?;
        }
        self.poll_replay().await
    }
    fn enqueue_next_scheduled(&mut self, recovery: bool) -> Result<(), BotRunError<S::Error>> {
        let Some((&(time, _), _)) = self.scheduled.first_key_value() else {
            return Ok(());
        };
        // 容量不足なら予約を消費せず、終了結果で未実行として追跡できるようにする。
        self.ensure_delivery_capacity(1)?;
        let time = self.last_time.map_or(time, |now| now.max(time));
        let count = match self.scheduled_tick {
            Some((tick, count)) if tick == time => count,
            _ => 0,
        };
        if count >= self.scheduled_limit {
            return Err(BotRunError::ScheduledLimit);
        }
        self.scheduled_tick = Some((time, count + 1));
        let (_, entry) = self.scheduled.pop_first().unwrap();
        if let Some(r) = self.venue.recorder() {
            r.record(
                "scheduled_event_executed",
                serde_json::json!({"event":entry.info,"executed_at_ms":time}),
            );
        }
        self.delivery.push_back(QueuedEvent {
            received_at_ms: entry.info.execute_at_ms,
            event: BotEvent::Custom(entry.event),
            input_id: entry.info.input_id,
            market_id: None,
            derived: false,
            recovery,
        });
        Ok(())
    }
    pub async fn process_next_scheduled(
        &mut self,
        recovery: bool,
    ) -> Result<(), BotRunError<S::Error>> {
        let Some(time) = self.next_scheduled_time() else {
            return Ok(());
        };
        self.advance_time(self.last_time.map_or(time, |now| now.max(time)))?;
        self.enqueue_next_scheduled(recovery)?;
        self.drive_direct(true).await
    }
    fn record_settlement(&self) {
        if let Some(status) = self.venue.paper_settlement_status() {
            log::info!("paper settlement at shutdown: {status:?}");
            if let Some(recorder) = self.venue.recorder() {
                recorder.record("paper_settlement", serde_json::json!(status));
            }
        }
    }
    pub fn advance_time(&mut self, time_ms: i64) -> Result<(), BotRunError<S::Error>> {
        if self.last_time.is_some_and(|t| time_ms < t) {
            return Err(BotRunError::TimeReversed);
        }
        self.venue
            .advance_replay_time(time_ms)
            .map_err(BotRunError::Venue)?;
        self.last_time = Some(time_ms);
        self.replay_clock
            .store(time_ms, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn ingest(
        &mut self,
        input_id: Option<InputId>,
        event: BotEvent<S::CustomEvent>,
        recovery: bool,
        inherited_market_id: Option<u64>,
        derived: bool,
    ) -> Result<(), BotRunError<S::Error>> {
        self.ensure_delivery_capacity(1)?;
        let recorder = self.venue.recorder();
        let market_id = if matches!(event, BotEvent::Market(_)) {
            recorder.as_ref().map(|r| r.next_id())
        } else {
            inherited_market_id
        };
        let received_at_ms = self
            .last_time
            .unwrap_or_else(|| crate::execution_recording::timestamp_ms() as i64);
        let recording_context = crate::execution_recording::RecordingContext {
            event_time_ms: Some(received_at_ms),
            market_event_id: market_id,
            input_id: input_id.clone(),
            ..Default::default()
        };
        crate::execution_recording::sync_scope(recording_context, || {
            if let BotEvent::Market(market) = &event {
                if recovery {
                    self.venue.on_market_event_recovering(market)
                } else {
                    self.venue.on_market_event(market)
                }
                .map_err(BotRunError::Venue)?;
            }
            if let Some(r) = recorder {
                r.record("input_registered", serde_json::json!({"received_at_ms":received_at_ms,"input_id":input_id,"market_event_id":market_id}));
            }
            Ok::<(), BotRunError<S::Error>>(())
        })?;
        self.delivery.push_back(QueuedEvent {
            received_at_ms,
            event,
            input_id,
            market_id,
            derived,
            recovery,
        });
        Ok(())
    }
    /// 市場を現在の受信時刻で登録し、戦略が模擬操作待ちなら配送を保留する。
    /// このメソッド自身は将来の期限へ時計を進めない。
    pub async fn submit_replay(
        &mut self,
        time_ms: i64,
        input_id: Option<InputId>,
        event: BotEvent<S::CustomEvent>,
        recovery: bool,
    ) -> Result<(), BotRunError<S::Error>> {
        self.advance_time(time_ms)?;
        self.ingest(input_id, event, recovery, None, false)?;
        self.poll_replay().await
    }
    /// 直接入力を処理する。待機する模擬操作の期限も駆動するため、途中の入力が
    /// あり得る収録データにはsubmit_replayを使うrun_replayを選ぶ。
    pub async fn process(
        &mut self,
        time_ms: i64,
        input_id: Option<InputId>,
        event: BotEvent<S::CustomEvent>,
    ) -> Result<(), BotRunError<S::Error>> {
        self.submit_replay(time_ms, input_id, event, false).await?;
        self.drive_direct(true).await
    }
    pub async fn process_recovering(
        &mut self,
        time_ms: i64,
        input_id: Option<InputId>,
        event: BotEvent<S::CustomEvent>,
    ) -> Result<(), BotRunError<S::Error>> {
        self.submit_replay(time_ms, input_id, event, true).await?;
        self.drive_direct(true).await
    }
    fn poll_live(&mut self) -> Result<(), BotRunError<S::Error>> {
        self.venue.set_replay_driver(false);
        let waker = std::task::Waker::from(std::sync::Arc::new(RunnerWake(self.wake.clone())));
        self.venue.set_replay_waker(Some(waker.clone()));
        loop {
            self.start_handler(false)?;
            if self.active.is_none() {
                return Ok(());
            }
            match self.poll_active(&mut std::task::Context::from_waker(&waker)) {
                std::task::Poll::Ready((strategy, result)) => {
                    self.strategy = Some(strategy);
                    self.finish_handler(result)?;
                }
                std::task::Poll::Pending => return Ok(()),
            }
        }
    }
    async fn drive_direct(&mut self, replay: bool) -> Result<(), BotRunError<S::Error>> {
        struct ReleaseDriver<V: TradingVenue>(V);
        impl<V: TradingVenue> Drop for ReleaseDriver<V> {
            fn drop(&mut self) {
                self.0.set_replay_driver(false);
                self.0.set_replay_waker(None);
            }
        }
        let _release = ReleaseDriver(self.venue.clone());
        if replay {
            self.poll_replay().await?;
            while self.active.is_some() {
                let Some(t) = self.venue.next_internal_time() else {
                    break;
                };
                self.advance_time(t)?;
                self.venue
                    .process_internal_events(t)
                    .map_err(BotRunError::Venue)?;
                self.poll_replay().await?;
            }
            return Ok(());
        }
        loop {
            self.start_handler(false)?;
            if self.active.is_none() {
                return Ok(());
            }
            let (strategy, result) = std::future::poll_fn(|cx| self.poll_active(cx)).await;
            self.strategy = Some(strategy);
            self.finish_handler(result)?;
        }
    }
    fn poll_active(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<(S, Result<(), S::Error>)> {
        let active = self.active.as_mut().expect("active handler");
        let context = crate::execution_recording::RecordingContext {
            event_time_ms: Some(
                self.last_time
                    .unwrap_or_else(|| crate::execution_recording::timestamp_ms() as i64),
            ),
            market_event_id: active.market_id,
            input_id: active.input_id.clone(),
            replay_clock: self.last_time.map(|_| self.replay_clock.clone()),
            ..Default::default()
        };
        crate::execution_recording::sync_scope(context, || active.future.as_mut().poll(cx))
    }
    fn start_handler(&mut self, replay: bool) -> Result<(), BotRunError<S::Error>> {
        if self.active.is_some() {
            return Ok(());
        }
        let Some(entry) = self.delivery.front() else {
            return Ok(());
        };
        let next_count = if entry.derived {
            self.derived_count
                .checked_add(1)
                .ok_or(BotRunError::DerivedLimit)?
        } else {
            1
        };
        if next_count > self.derived_limit.saturating_add(1) {
            return Err(BotRunError::DerivedLimit);
        }
        if self.strategy.is_none() {
            return Err(BotRunError::Stopped);
        }
        let entry = self.delivery.pop_front().unwrap();
        self.derived_count = next_count;
        let pending = std::sync::Arc::new(std::sync::Mutex::new(Pending {
            queue: Default::default(),
            scheduled: Vec::new(),
            schedule_limit: self.scheduled_limit.saturating_sub(self.scheduled.len()),
            schedule_overflow: false,
            open: true,
            emitted: 0,
            limit: self.derived_limit.saturating_sub(
                self.derived_count.saturating_sub(1)
                    + self.delivery.iter().filter(|e| e.derived).count(),
            ),
            overflow: false,
        }));
        let ctx = BotContext {
            venue: self.venue.clone(),
            received_at_ms: entry.received_at_ms,
            time: replay.then(|| self.replay_clock.clone()),
            pending: pending.clone(),
        };
        let mut strategy = self.strategy.take().ok_or(BotRunError::Stopped)?;
        let recorder = self.venue.recorder();
        let input_id = entry.input_id.clone();
        let market_id = entry.market_id;
        let close = ClosePending(pending.clone());
        let future = Box::pin(async move {
            let _close = close;
            if let Some(r) = recorder {
                let kind = if matches!(entry.event, BotEvent::Market(_)) {
                    "market_event"
                } else {
                    "custom_event"
                };
                let event_type = match &entry.event {
                    BotEvent::Market(MarketEvent::Ticker { .. }) => "ticker",
                    BotEvent::Market(MarketEvent::Transactions { .. }) => "transactions",
                    BotEvent::Market(MarketEvent::DepthUpdated { .. }) => "depth",
                    BotEvent::Market(MarketEvent::CircuitBreakInfo { .. }) => "circuit_break_info",
                    BotEvent::Custom(_) => "custom",
                };
                r.record(
                    kind,
                    serde_json::json!({"id":market_id,"input_id":input_id,"type":event_type,
                        "received_at_ms":ctx.received_at_ms(),"processed_at_ms":ctx.now_ms()}),
                );
            }
            let result = strategy.handle_event(entry.event, &ctx).await;
            (strategy, result)
        });
        self.active = Some(ActiveHandler {
            future,
            pending,
            input_id: entry.input_id.clone(),
            market_id,
            recovery: entry.recovery,
        });
        Ok(())
    }
    fn finish_handler(
        &mut self,
        result: Result<(), S::Error>,
    ) -> Result<(), BotRunError<S::Error>> {
        let active = self.active.take().unwrap();
        let (emitted, scheduled) = {
            let mut p = active.pending.lock().unwrap();
            p.open = false;
            if p.schedule_overflow {
                return Err(BotRunError::ScheduledLimit);
            }
            if p.overflow {
                return Err(BotRunError::DerivedLimit);
            }
            (
                p.queue.drain(..).collect::<Vec<_>>(),
                p.scheduled.drain(..).collect::<Vec<_>>(),
            )
        };
        result.map_err(BotRunError::Strategy)?;
        // FIFO優先のため外部入力を退避する前に、復元後の全件数を検証する。
        self.ensure_delivery_capacity(emitted.len())?;
        // 派生イベントは待機中に受信した市場入力より先にFIFOで配送する。
        let external_start = self
            .delivery
            .iter()
            .position(|e| !e.derived)
            .unwrap_or(self.delivery.len());
        let queued = self.delivery.split_off(external_start);
        let ingestion = emitted.into_iter().try_for_each(|event| {
            self.ingest(
                active.input_id.clone(),
                event,
                active.recovery,
                active.market_id,
                true,
            )
        });
        self.delivery.extend(queued);
        ingestion?;
        for (execute_at_ms, event) in scheduled {
            if self.scheduled.len() >= self.scheduled_limit {
                return Err(BotRunError::ScheduledLimit);
            }
            self.next_scheduled_id = self
                .next_scheduled_id
                .checked_add(1)
                .ok_or(BotRunError::ScheduleIdOverflow)?;
            let info = ScheduledEventInfo {
                id: self.next_scheduled_id,
                scheduled_at_ms: self.last_time.expect("replay schedule"),
                execute_at_ms,
                input_id: active.input_id.clone(),
            };
            if let Some(r) = self.venue.recorder() {
                r.record("scheduled_event_reserved", serde_json::json!(&info));
            }
            self.scheduled
                .insert((execute_at_ms, info.id), ScheduledEvent { info, event });
        }
        Ok(())
    }
    /// 評価終了時に戦略Futureを破棄する。送信済みvenue操作は取り消さない。
    pub fn stop_pending_handler(&mut self) {
        self.active.take();
        self.venue.set_replay_waker(None);
        self.venue.set_replay_driver(false);
    }
}

struct Pending<C> {
    scheduled: Vec<(i64, C)>,
    schedule_limit: usize,
    schedule_overflow: bool,
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
