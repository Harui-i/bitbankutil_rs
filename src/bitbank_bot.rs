use crate::bitbank_structs::{
    BitbankDepth, BitbankDepthDiff, BitbankDepthWhole, BitbankTransactionDatum,
};
use crate::websocket_handler::run_websocket;
use crypto_botters::{bitbank::BitbankOption, generic_api_client::websocket::WebSocketConfig};

use tokio::sync::mpsc;

pub trait BotTrait<T> {
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
        T: Sync + Send,
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
                }
            }

            let _ = ws_task.await; // Wait for the termination of ws_task
        }
    }
    fn on_transactions(
        &self,
        transactions: &Vec<BitbankTransactionDatum>,
        state: T,
    ) -> impl std::future::Future<Output = T> + Send;
    fn on_depth_update(
        &self,
        depth: &BitbankDepth,
        state: T,
    ) -> impl std::future::Future<Output = T> + Send;
}

pub enum BotMessage {
    Transactions(Vec<BitbankTransactionDatum>),
    DepthDiff(BitbankDepthDiff),
    DepthWhole(BitbankDepthWhole),
}
