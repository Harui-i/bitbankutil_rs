use crate::bitbank_structs::{
    BitbankCircuitBreakInfo, BitbankDepth, BitbankDepthDiff, BitbankDepthWhole,
    BitbankTickerResponse, BitbankTransactionDatum,
};
use crate::websocket_handler::run_websocket;
use crypto_botters::bitbank::BitbankOption;
use crypto_botters::generic_api_client::websocket::WebSocketConfig;
use log::{error, trace, warn};
use std::marker::PhantomData;
use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinError, JoinHandle};

/// Shared context that allows strategies to emit follow-up events back into the
/// runtime. The context clones the underlying sender so strategies can freely
/// store it if they need to schedule work later.
#[derive(Clone)]
pub struct BotContext<E> {
    event_tx: mpsc::Sender<E>,
}

impl<E: Send + 'static> BotContext<E> {
    pub fn new(event_tx: mpsc::Sender<E>) -> Self {
        Self { event_tx }
    }

    /// Obtain a clone of the internal event sender. This is useful if the
    /// strategy wants to pipe custom events or replay data (e.g. from logs)
    /// into the runtime without going through Bitbank's websocket.
    pub fn event_sender(&self) -> mpsc::Sender<E> {
        self.event_tx.clone()
    }

    /// Push an event back into the runtime. This helper awaits the channel
    /// send operation and propagates the result to the caller.
    pub async fn emit(&self, event: E) -> Result<(), mpsc::error::SendError<E>> {
        self.event_tx.send(event).await
    }
}

/// Strategies implement this trait to express how incoming events should be
/// handled. The runtime owns the strategy instance and guarantees exclusive
/// mutable access per event, so users can keep their state directly on `self`
/// without external synchronisation primitives such as `Mutex`.
pub trait BotStrategy: Send + 'static {
    type Event: Send + 'static;

    fn handle_event(
        &mut self,
        event: Self::Event,
        ctx: &BotContext<Self::Event>,
    ) -> impl std::future::Future<Output = ()> + Send;
}

/// Handle that keeps the bot actor alive and offers basic lifecycle control.
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

/// Raw messages forwarded from Bitbank's websocket connection.
#[derive(Debug, Clone)]
pub enum BitbankInboundMessage {
    Ticker(BitbankTickerResponse),
    Transactions(Vec<BitbankTransactionDatum>),
    DepthDiff(BitbankDepthDiff),
    DepthWhole(BitbankDepthWhole),
    CircuitBreakInfo(BitbankCircuitBreakInfo),
}

/// High-level events exposed to bot strategies. `DepthUpdated` only fires when
/// a complete order book snapshot is available for the corresponding pair.
#[derive(Debug, Clone)]
pub enum BitbankEvent {
    Ticker {
        pair: String,
        ticker: BitbankTickerResponse,
    },
    Transactions {
        pair: String,
        transactions: Vec<BitbankTransactionDatum>,
    },
    DepthUpdated {
        pair: String,
        depth: BitbankDepth,
    },
    CircuitBreakInfo {
        pair: String,
        info: BitbankCircuitBreakInfo,
    },
}

async fn run_bitbank_pair_feed<E>(
    pair: String,
    client_options: Vec<BitbankOption>,
    websocket_config: WebSocketConfig,
    event_tx: mpsc::Sender<E>,
) where
    E: From<BitbankEvent> + Send + 'static,
{
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<BitbankInboundMessage>(128);
    let ws_task = tokio::spawn(run_websocket(
        pair.clone(),
        client_options,
        websocket_config,
        inbound_tx,
    ));

    let mut depth = BitbankDepth::new();
    while let Some(message) = inbound_rx.recv().await {
        let event = match message {
            BitbankInboundMessage::Ticker(ticker) => Some(BitbankEvent::Ticker {
                pair: pair.clone(),
                ticker,
            }),
            BitbankInboundMessage::Transactions(transactions) => Some(BitbankEvent::Transactions {
                pair: pair.clone(),
                transactions,
            }),
            BitbankInboundMessage::DepthDiff(depth_diff) => {
                depth.insert_diff(depth_diff);
                if depth.is_complete() {
                    Some(BitbankEvent::DepthUpdated {
                        pair: pair.clone(),
                        depth: depth.clone(),
                    })
                } else {
                    None
                }
            }
            BitbankInboundMessage::DepthWhole(depth_whole) => {
                depth.update_whole(depth_whole);
                if depth.is_complete() {
                    Some(BitbankEvent::DepthUpdated {
                        pair: pair.clone(),
                        depth: depth.clone(),
                    })
                } else {
                    None
                }
            }
            BitbankInboundMessage::CircuitBreakInfo(info) => Some(BitbankEvent::CircuitBreakInfo {
                pair: pair.clone(),
                info,
            }),
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
            BitbankOption::HttpUrl(url) => BitbankOption::HttpUrl(url.clone()),
            BitbankOption::HttpAuth(auth) => BitbankOption::HttpAuth(auth.clone()),
            BitbankOption::RequestConfig(cfg) => BitbankOption::RequestConfig(cfg.clone()),
            BitbankOption::WebSocketUrl(url) => BitbankOption::WebSocketUrl(url.clone()),
            BitbankOption::WebSocketChannels(channels) => {
                BitbankOption::WebSocketChannels(channels.clone())
            }
            BitbankOption::WebSocketConfig(config) => {
                BitbankOption::WebSocketConfig(config.clone())
            }
        })
        .collect()
}

/// Builder that wires Bitbank's websocket feeds into a [`BotStrategy`].
pub struct BitbankBotBuilder<S, E>
where
    S: BotStrategy<Event = E>,
    E: From<BitbankEvent> + Send + 'static,
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
    E: From<BitbankEvent> + Send + 'static,
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

/// Runtime that keeps the actor and feed tasks alive. Dropping the runtime will
/// abort the feed tasks; call [`Self::shutdown`] if you need a graceful stop.
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

/// Helper for advanced scenarios where the caller supplies their own stream of
/// [`BitbankInboundMessage`] values (for instance, log replay). The function
/// consumes messages from `inbound_rx` and forwards ready events to `event_tx`.
pub async fn forward_bitbank_messages<E>(
    pair: String,
    inbound_rx: &mut mpsc::Receiver<BitbankInboundMessage>,
    event_tx: &mpsc::Sender<E>,
) where
    E: From<BitbankEvent> + Send + 'static,
{
    let mut depth = BitbankDepth::new();
    while let Some(message) = inbound_rx.recv().await {
        let event = match message {
            BitbankInboundMessage::Ticker(ticker) => Some(BitbankEvent::Ticker {
                pair: pair.clone(),
                ticker,
            }),
            BitbankInboundMessage::Transactions(transactions) => Some(BitbankEvent::Transactions {
                pair: pair.clone(),
                transactions,
            }),
            BitbankInboundMessage::DepthDiff(diff) => {
                depth.insert_diff(diff);
                depth.is_complete().then(|| BitbankEvent::DepthUpdated {
                    pair: pair.clone(),
                    depth: depth.clone(),
                })
            }
            BitbankInboundMessage::DepthWhole(whole) => {
                depth.update_whole(whole);
                depth.is_complete().then(|| BitbankEvent::DepthUpdated {
                    pair: pair.clone(),
                    depth: depth.clone(),
                })
            }
            BitbankInboundMessage::CircuitBreakInfo(info) => Some(BitbankEvent::CircuitBreakInfo {
                pair: pair.clone(),
                info,
            }),
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
