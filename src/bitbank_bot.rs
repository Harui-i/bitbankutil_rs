use crate::bitbank_structs::{
    BitbankCircuitBreakInfo, BitbankDepth, BitbankDepthDiff, BitbankDepthWhole,
    BitbankTickerResponse, BitbankTransactionDatum,
};
use crate::websocket_handler::run_websocket;
use crypto_botters::bitbank::BitbankOption;
use crypto_botters::generic_api_client::websocket::WebSocketConfig;

use tokio::sync::mpsc;

pub trait BotTrait<T: Send> {
    // async fn run(...);
    fn run(
        &self,
        pair: String,
        client_options: Vec<BitbankOption>,
        wsc: WebSocketConfig,
        initial_state: T,
    ) -> impl std::future::Future<Output = ()> + Send
    where
        Self: Sync + Send,
    {
        async {
            let mut state = initial_state;
            let (tx, mut rx) = mpsc::channel(100);

            let mut depth = BitbankDepth::new();

            let ws_task = tokio::spawn(run_websocket(pair, client_options, wsc, tx));

            // receive messages
            while let Some(msg) = rx.recv().await {
                match msg {
                    BotMessage::Transactions(transactions) => {
                        state = self.on_transactions(&transactions, state).await;
                    }
                    BotMessage::DepthDiff(depth_diff) => {
                        depth.insert_diff(depth_diff);

                        if depth.is_complete() {
                            state = self.on_depth_update(&depth, state).await;
                        }
                    }
                    BotMessage::DepthWhole(depth_whole) => {
                        depth.update_whole(depth_whole);

                        if depth.is_complete() {
                            state = self.on_depth_update(&depth, state).await;
                        }
                    }
                    BotMessage::Ticker(_bitbank_ticker_response) => {
                        state = self.on_ticker(&_bitbank_ticker_response, state).await;
                    }
                    BotMessage::CircuitBreakInfo(circuit_break_info) => {
                        state = self.on_circuit_break_info(&circuit_break_info, state).await;
                    }
                }
            }

            let _ = ws_task.await; // Wait for the termination of ws_task
        }
    }

    fn on_ticker(
        &self,
        _ticker: &BitbankTickerResponse,
        state: T,
    ) -> impl std::future::Future<Output = T> + Send {
        async { state }
    }

    fn on_transactions(
        &self,
        _transactions: &Vec<BitbankTransactionDatum>,
        state: T,
    ) -> impl std::future::Future<Output = T> + Send {
        async { state }
    }

    fn on_depth_update(
        &self,
        _depth: &BitbankDepth,
        state: T,
    ) -> impl std::future::Future<Output = T> + Send {
        async { state }
    }

    fn on_circuit_break_info(
        &self,
        _circuit_break_info: &BitbankCircuitBreakInfo,
        state: T,
    ) -> impl std::future::Future<Output = T> + Send {
        async { state }
    }
}

pub enum BotMessage {
    Ticker(BitbankTickerResponse),
    Transactions(Vec<BitbankTransactionDatum>),
    DepthDiff(BitbankDepthDiff),
    DepthWhole(BitbankDepthWhole),
    CircuitBreakInfo(BitbankCircuitBreakInfo),
}
