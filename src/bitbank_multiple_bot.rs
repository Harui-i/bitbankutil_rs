use crate::bitbank_bot::BotTrait;
use crate::bitbank_structs::{
    BitbankCircuitBreakInfo, BitbankDepth, BitbankTickerResponse, BitbankTransactionDatum,
};
use crypto_botters::{bitbank::BitbankOption, generic_api_client::websocket::WebSocketConfig};
use tokio::sync::mpsc;

// A private helper struct that implements the single-pair BotTrait
// This bot's only job is to forward messages from the websocket to the MultiBot.
struct InternalBot {
    tx: mpsc::Sender<MultiBotMessage>,
}

impl BotTrait<String> for InternalBot {
    async fn on_depth_update(&self, depth: &BitbankDepth, pair: String) -> String {
        // To send the `depth` object, it must be cloneable.
        // We assume `BitbankDepth` derives `Clone`.
        self.tx
            .send(MultiBotMessage::DepthUpdate(pair.clone(), depth.clone()))
            .await
            .unwrap();
        pair
    }

    async fn on_ticker(&self, ticker: &BitbankTickerResponse, pair: String) -> String {
        self.tx
            .send(MultiBotMessage::Ticker(pair.clone(), ticker.clone()))
            .await
            .unwrap();
        pair
    }

    async fn on_transactions(
        &self,
        transactions: &Vec<BitbankTransactionDatum>,
        pair: String,
    ) -> String {
        self.tx
            .send(MultiBotMessage::Transactions(
                pair.clone(),
                transactions.clone(),
            ))
            .await
            .unwrap();
        pair
    }

    async fn on_circuit_break_info(&self, info: &BitbankCircuitBreakInfo, pair: String) -> String {
        self.tx
            .send(MultiBotMessage::CircuitBreakInfo(
                pair.clone(),
                info.clone(),
            ))
            .await
            .unwrap();
        pair
    }
}

/// Messages sent from each `InternalBot` to the main `MultiBotTrait` runner.
/// It contains the pair name and the data payload.
#[derive(Clone)]
pub enum MultiBotMessage {
    Ticker(String, BitbankTickerResponse),
    Transactions(String, Vec<BitbankTransactionDatum>),
    DepthUpdate(String, BitbankDepth),
    CircuitBreakInfo(String, BitbankCircuitBreakInfo),
}

fn clone_bitbank_option(option: &BitbankOption) -> BitbankOption {
    match option {
        BitbankOption::Default => BitbankOption::Default,
        BitbankOption::Key(key) => BitbankOption::Key(key.clone()),
        BitbankOption::Secret(secret) => BitbankOption::Secret(secret.clone()),
        BitbankOption::HttpUrl(bitbank_http_url) => {
            BitbankOption::HttpUrl(bitbank_http_url.clone())
        }
        BitbankOption::HttpAuth(http_auth) => BitbankOption::HttpAuth(http_auth.clone()),
        BitbankOption::RequestConfig(request_config) => {
            BitbankOption::RequestConfig(request_config.clone())
        }
        BitbankOption::WebSocketUrl(bitbank_web_socket_url) => {
            BitbankOption::WebSocketUrl(bitbank_web_socket_url.clone())
        }
        BitbankOption::WebSocketChannels(items) => BitbankOption::WebSocketChannels(items.clone()),
        BitbankOption::WebSocketConfig(web_socket_config) => {
            BitbankOption::WebSocketConfig(web_socket_config.clone())
        }
    }
}

/// A trait for handling multiple pairs in a single bot.
pub trait MultiBotTrait<T: Send> {
    /// Runs the bot for multiple pairs.
    fn run(
        &self,
        pairs: Vec<String>,
        client_options: Vec<BitbankOption>,
        wsc: WebSocketConfig,
        initial_state: T,
    ) -> impl std::future::Future<Output = ()> + Send
    where
        Self: Sync + Send,
    {
        async move {
            let mut state = initial_state;
            let (tx, mut rx) = mpsc::channel(100); // Channel for MultiBotMessage

            let mut bot_tasks = Vec::new();

            for pair in pairs {
                let wsc = wsc.clone();
                //let client_options = client_options.clone();
                let client_options: Vec<BitbankOption> = client_options
                    .iter()
                    .map(|opt| clone_bitbank_option(opt))
                    .collect();
                let internal_bot = InternalBot { tx: tx.clone() };

                let bot_task = tokio::spawn(async move {
                    internal_bot
                        .run(pair.clone(), client_options, wsc, pair)
                        .await;
                });
                bot_tasks.push(bot_task);
            }

            // Main loop to receive from all internal bots and dispatch to the user's trait implementation
            while let Some(msg) = rx.recv().await {
                match msg {
                    MultiBotMessage::Ticker(pair, data) => {
                        state = self.on_ticker(&pair, &data, state).await;
                    }
                    MultiBotMessage::Transactions(pair, data) => {
                        state = self.on_transactions(&pair, &data, state).await;
                    }
                    MultiBotMessage::DepthUpdate(pair, data) => {
                        state = self.on_depth_update(&pair, &data, state).await;
                    }
                    MultiBotMessage::CircuitBreakInfo(pair, data) => {
                        state = self.on_circuit_break_info(&pair, &data, state).await;
                    }
                }
            }

            for task in bot_tasks {
                task.await.unwrap();
            }
        }
    }

    // Default implementations for the callbacks.
    // Users of this trait will override the ones they need.

    fn on_ticker(
        &self,
        _pair: &str,
        _ticker: &BitbankTickerResponse,
        state: T,
    ) -> impl std::future::Future<Output = T> + Send {
        async { state }
    }

    fn on_transactions(
        &self,
        _pair: &str,
        _transactions: &Vec<BitbankTransactionDatum>,
        state: T,
    ) -> impl std::future::Future<Output = T> + Send {
        async { state }
    }

    fn on_depth_update(
        &self,
        _pair: &str,
        _depth: &BitbankDepth,
        state: T,
    ) -> impl std::future::Future<Output = T> + Send {
        async { state }
    }

    fn on_circuit_break_info(
        &self,
        _pair: &str,
        _info: &BitbankCircuitBreakInfo,
        state: T,
    ) -> impl std::future::Future<Output = T> + Send {
        async { state }
    }
}
