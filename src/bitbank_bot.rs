use std::collections::BTreeSet;

use crate::{bitbank_private::BitbankPrivateApiClient, bitbank_structs::BitbankCreateOrderResponse};
use crate::bitbank_structs::{
    BitbankDepth, BitbankDepthDiff, BitbankDepthDiffMessage, BitbankDepthWhole,
    BitbankDepthWholeMessage, BitbankGetOrderResponse, BitbankTransactionDatum,
    BitbankTransactionMessage,
};
use crypto_botters::{
    bitbank::BitbankOption, generic_api_client::websocket::WebSocketConfig, Client,
};

use rust_decimal::prelude::*;
use tokio::{sync::mpsc, task::JoinSet, time::Instant};

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct SimplifiedOrder {
    pub pair: String,
    pub side: String,
    pub amount: Decimal,
    pub price: Decimal,
}

pub trait BotTrait {
    // async fn run(...);
    fn run(
        &mut self,
        pair: String,
        client_options: Vec<BitbankOption>,
        wsc: WebSocketConfig,
    ) -> impl std::future::Future<Output = ()> + Send
    where
        Self: Sync + Send,
    {
        async {
            let mut ws_client = Client::new();

            for option in client_options {
                ws_client.update_default_option(option);
            }

            let ws_client = ws_client; // immutalize

            let (tx, mut rx) = mpsc::channel(100);

            let mut depth = BitbankDepth::new();

            let ws_task = tokio::spawn(async move {
                let channels = vec![
                    format!("transactions_{}", pair).to_owned(),
                    format!("depth_diff_{}", pair).to_owned(),
                    format!("depth_whole_{}", pair).to_owned(),
                ];

                let _transactions_connection = ws_client
                    .websocket(
                        "",
                        move |val: serde_json::Value| {
                            let room_name = val[1]["room_name"].as_str().unwrap();
                            let msg: serde_json::Value =
                                serde_json::from_value(val[1].clone()).unwrap();

                            if room_name.starts_with("transactions") {
                                let transaction_message: BitbankTransactionMessage =
                                    serde_json::from_value(msg["message"].clone()).unwrap();
                                let transactions = transaction_message.data.transactions;

                                let tx2 = tx.clone();
                                tokio::spawn(async move {
                                    tx2.send(BotMessage::Transactions(transactions))
                                        .await
                                        .unwrap();
                                });
                            } else if room_name.starts_with("depth_diff") {
                                let depth_diff_message: BitbankDepthDiffMessage =
                                    serde_json::from_value(msg["message"].clone()).unwrap();

                                let tx2 = tx.clone();

                                // without `move`, tx2 is borrowed. but adding `move`, tx2 is moved to this closure.
                                tokio::spawn(async move {
                                    tx2.send(BotMessage::DepthDiff(depth_diff_message.data))
                                        .await
                                        .unwrap();
                                });
                            } else if room_name.starts_with("depth_whole") {
                                let depth_whole_message: BitbankDepthWholeMessage =
                                    serde_json::from_value(msg["message"].clone()).unwrap();
                                let tx2 = tx.clone();

                                // without `move`, tx2 is borrowed. but adding `move`, tx2 is moved to this closure.
                                tokio::spawn(async move {
                                    tx2.send(BotMessage::DepthWhole(depth_whole_message.data))
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
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                }
            });

            // receive messages
            while let Some(msg) = rx.recv().await {
                match msg {
                    BotMessage::Transactions(transactions) => {
                        self.on_transactions(&transactions).await;
                    }

                    BotMessage::DepthDiff(depth_diff) => {
                        depth.insert_diff(depth_diff);

                        if depth.is_complete() {
                            self.on_depth_update(&depth).await;
                        }
                    }

                    BotMessage::DepthWhole(depth_whole) => {
                        depth.update_whole(depth_whole);

                        if depth.is_complete() {
                            self.on_depth_update(&depth).await;
                        }
                    }
                }
            }

            let _ = ws_task.await; // ws_taskの終了を待つ
        }
    }
    fn on_transactions(
        &mut self,
        transactions: &Vec<BitbankTransactionDatum>,
    ) -> impl std::future::Future<Output = ()> + Send;
    fn on_depth_update(
        &mut self,
        depth: &BitbankDepth,
    ) -> impl std::future::Future<Output = ()> + Send;

    // Replace active orders
    // `current_orders` : Vec of BitbankGetOrderResponse, represents current orders in the pair
    // `pair` : &str represents the pair you want to replace orders.
    fn place_wanna_orders(
        mut wanna_place_orders: BTreeSet<SimplifiedOrder>,
        current_orders: Vec<BitbankGetOrderResponse>,
        pair: String,
        api_client: BitbankPrivateApiClient,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            let start = Instant::now();
            let mut should_cancelled_orderids = vec![];
            let mut js = JoinSet::new();

            for cur_order in current_orders {
                let current_sord = SimplifiedOrder {
                    pair: cur_order.pair.clone(),
                    side: cur_order.side.to_string(),
                    amount: cur_order
                        .remaining_amount
                        .clone()
                        .unwrap()
                        .parse::<Decimal>()
                        .unwrap(),
                    price: cur_order.price.clone().unwrap().parse::<Decimal>().unwrap(),
                };

                // this order shoulb be canceled
                if !wanna_place_orders.contains(&current_sord) && current_sord.pair == pair {
                    log::debug!("this order is cancelled. {:?}", current_sord);
                    should_cancelled_orderids.push(cur_order.order_id.as_u64().unwrap());
                }
                // this current order is in wanna_place_orders. (i.e. already placed order)
                else {
                    wanna_place_orders.remove(&current_sord);
                }
            }

            if !should_cancelled_orderids.is_empty() {
                let cancel_order_response_result = api_client
                    .post_cancel_orders(&pair.clone(), should_cancelled_orderids)
                    .await;

                if let Err(err) = cancel_order_response_result {
                    log::error!(
                        "in place_wanna_orders, post_cancel_orders has returned error: {:?}",
                        err
                    );
                    return;
                }
                let cancel_order_response = cancel_order_response_result.unwrap();

                log::info!(
                    "cancel current orders. response: {:?}",
                    cancel_order_response
                );
            }

            // side, lot, price
            // place orders
            for sord in wanna_place_orders {
                let bbc2 = api_client.clone();
                let pair2 = pair.clone();
                js.spawn(async move {
                    bbc2.post_order(
                        &pair2,
                        &sord.amount.to_string(),
                        Some(&sord.price.to_string()),
                        &sord.side,
                        "limit",
                        Some(true),
                        None,
                    )
                    .await
                });
            }

            while let Some(js_res) = js.join_next().await {
                let bcor: Result<
                    BitbankCreateOrderResponse,
                    Option<crypto_botters::bitbank::BitbankHandleError>,
                > = js_res.unwrap();

                log::info!("order result: {:?}", bcor);
            }

            log::debug!("Replaced orders within {} ms.", start.elapsed().as_millis());
        }
    }
}

pub enum BotMessage {
    Transactions(Vec<BitbankTransactionDatum>),
    DepthDiff(BitbankDepthDiff),
    DepthWhole(BitbankDepthWhole),
}

