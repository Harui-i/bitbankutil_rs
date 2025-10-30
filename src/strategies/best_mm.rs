use crate::bitbank_bot::{BitbankEvent, BotContext, BotStrategy};
use crate::bitbank_private::BitbankPrivateApiClient;
use crate::bitbank_structs::BitbankDepth;
use crate::depth::Depth;
use crate::trading_api::BitbankTradingApi;
use rust_decimal::prelude::*;

pub struct MyBot<T: BitbankTradingApi> {
    pub(crate) bot_config: MyBotConfig<T>,
    depth: BitbankDepth,
    last_updated: u128,
    last_bestbid: Decimal,
    last_bestask: Decimal,
}

pub struct MyBotConfig<T: BitbankTradingApi> {
    pub pair: String,
    pub tick_size: Decimal,
    pub refresh_cycle: u128,
    pub lot: Decimal,
    pub max_lot: Decimal,
    pub bb_api_client: T,
}

impl<T: BitbankTradingApi> MyBot<T> {
    pub fn with_api(
        api_client: T,
        pair: String,
        tick_size: Decimal,
        refresh_cycle: u128,
        lot: Decimal,
        max_lot: Decimal,
    ) -> MyBot<T> {
        MyBot {
            bot_config: MyBotConfig {
                pair,
                tick_size,
                refresh_cycle,
                lot,
                max_lot,
                bb_api_client: api_client,
            },
            depth: BitbankDepth::new(),
            last_updated: 0,
            last_bestbid: Decimal::zero(),
            last_bestask: Decimal::zero(),
        }
    }
}

impl MyBot<BitbankPrivateApiClient> {
    pub fn new(
        bitbank_key: String,
        bitbank_secret: String,
        pair: String,
        tick_size: Decimal,
        refresh_cycle: u128,
        lot: Decimal,
        max_lot: Decimal,
    ) -> MyBot<BitbankPrivateApiClient> {
        MyBot::with_api(
            BitbankPrivateApiClient::new(bitbank_key, bitbank_secret, None),
            pair,
            tick_size,
            refresh_cycle,
            lot,
            max_lot,
        )
    }
}

impl<T: BitbankTradingApi> MyBot<T> {
    pub async fn update_orders(&mut self) {
        let now_inst = std::time::Instant::now();
        let now: u128 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();

        assert!(self.last_updated <= now);

        if now - self.last_updated >= self.bot_config.refresh_cycle {
            log::info!(
                "{} milliseconds have passed since the last order update",
                now - self.last_updated
            );

            if !self.depth.is_complete() {
                log::info!("depth is not complete");
                return;
            }

            self.last_updated = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis();

            let bb_client2 = self.bot_config.bb_api_client.clone();
            let pair2 = self.bot_config.pair.clone();
            let order_info_task = tokio::spawn(async move {
                bb_client2
                    .get_active_orders(Some(&pair2), None, None, None, None, None)
                    .await
            });

            let bb_client3 = self.bot_config.bb_api_client.clone();
            let current_asset_task = tokio::spawn(async move { bb_client3.get_assets().await });

            let (order_info, current_asset) = tokio::join!(order_info_task, current_asset_task);

            let active_orders_info_res = order_info.unwrap();
            let current_asset_res = current_asset.unwrap();

            if let Err(err) = active_orders_info_res {
                log::error!("order info cannot get properly due to an error : {:?}", err);
                return;
            }
            let active_orders_info = active_orders_info_res.unwrap();

            if let Err(err) = current_asset_res {
                log::error!(
                    "current asset cannot get properly due to an error: {:?}",
                    err
                );
                return;
            }
            let current_asset = current_asset_res.unwrap();

            log::debug!("active orders: {:?}", active_orders_info);

            let asset_name = self.bot_config.pair.split('_').next().unwrap();

            let btc_asset = current_asset
                .assets
                .iter()
                .find(|asset| asset.asset == asset_name)
                .unwrap();
            let jpy_asset = current_asset
                .assets
                .iter()
                .find(|asset| asset.asset == "jpy".to_owned())
                .unwrap();

            let mut btc_locked_jpy_amount: Decimal = Decimal::zero();
            for current_order in active_orders_info.clone().orders {
                if current_order.r#type == "limit" && current_order.side == "buy" {
                    btc_locked_jpy_amount +=
                        current_order.price.unwrap().parse::<Decimal>().unwrap()
                            * current_order
                                .remaining_amount
                                .unwrap()
                                .parse::<Decimal>()
                                .unwrap();
                }
            }

            let btc_free_amount = btc_asset.free_amount.clone().parse::<Decimal>().unwrap();
            let btc_locked_amount = btc_asset.locked_amount.clone().parse::<Decimal>().unwrap();
            let btc_amount = btc_free_amount + btc_locked_amount;
            let btc_amount_remainder =
                btc_amount - (btc_amount / self.bot_config.lot).floor() * self.bot_config.lot;
            let jpy_free_amount = jpy_asset.free_amount.clone().parse::<Decimal>().unwrap();
            let jpy_amount = jpy_free_amount + btc_locked_jpy_amount;

            log::debug!(
                "btc_free_amount: {:?}, btc_locked_amount: {:?}, jpy_free_amount{:?}, btc_locked_jpy_amount: {:?}",
                btc_free_amount,
                btc_locked_amount,
                jpy_free_amount,
                btc_locked_jpy_amount
            );
            log::info!(
                "{}_amount: {}, jpy_amount: {}",
                self.bot_config.pair.clone(),
                btc_amount,
                jpy_amount
            );

            let best_ask_price = self.depth.best_ask().unwrap().0.clone();
            let best_bid_price = self.depth.best_bid().unwrap().0.clone();

            let has_bestask_order = active_orders_info.clone().orders.iter().any(|ord| {
                ord.side == "sell"
                    && ord.r#type == "limit"
                    && ord.price.clone().unwrap() == best_ask_price.to_string()
            });

            let has_bestbid_order = active_orders_info.clone().orders.iter().any(|ord| {
                ord.side == "buy"
                    && ord.r#type == "limit"
                    && ord.price.clone().unwrap() == best_bid_price.to_string()
            });

            let sell_price = {
                if has_bestask_order || best_ask_price - self.bot_config.tick_size == best_bid_price
                {
                    best_ask_price
                } else {
                    best_ask_price - self.bot_config.tick_size
                }
            };

            let buy_price = {
                if has_bestbid_order || best_bid_price + self.bot_config.tick_size == best_ask_price
                {
                    best_bid_price
                } else {
                    best_bid_price + self.bot_config.tick_size
                }
            };

            log::info!("target spread: {}", sell_price - buy_price);

            let can_buy = jpy_amount >= buy_price * self.bot_config.lot
                && btc_amount + self.bot_config.lot <= self.bot_config.max_lot;
            let can_sell = btc_amount >= self.bot_config.lot;

            let mut wanna_place_orders = Vec::new();

            if can_buy {
                wanna_place_orders.push(crate::order_manager::SimplifiedOrder {
                    pair: self.bot_config.pair.clone(),
                    side: "buy".to_owned(),
                    amount: self.bot_config.lot,
                    price: buy_price,
                });
            }

            if can_sell {
                wanna_place_orders.push(crate::order_manager::SimplifiedOrder {
                    pair: self.bot_config.pair.clone(),
                    side: "sell".to_owned(),
                    amount: self.bot_config.lot + btc_amount_remainder,
                    price: sell_price,
                });
            }

            log::debug!("wanna_place_orders: {:?}", wanna_place_orders);
            log::info!("evaluated asset: {}", btc_amount * sell_price + jpy_amount);
            {
                let bb_client = self.bot_config.bb_api_client.clone();
                crate::order_manager::place_wanna_orders_concurrent(
                    wanna_place_orders,
                    active_orders_info.orders,
                    btc_free_amount,
                    jpy_free_amount,
                    self.bot_config.pair.clone(),
                    bb_client,
                )
                .await;
            }
        }
        log::info!(
            "update_orders has finished within {} ms",
            now_inst.elapsed().as_millis()
        );
    }
}

impl<T: BitbankTradingApi> BotStrategy for MyBot<T> {
    type Event = BitbankEvent;
    async fn handle_event(&mut self, event: Self::Event, _ctx: &BotContext<Self::Event>) {
        match event {
            BitbankEvent::Transactions { transactions, .. } => {
                log::debug!("transaction updated: {:?}", transactions);
                self.update_orders().await;
            }
            BitbankEvent::DepthUpdated { depth, .. } => {
                log::debug!("depth updated");

                if depth.is_complete() {
                    let bestask = depth.best_ask().unwrap().0.clone();
                    let bestbid = depth.best_bid().unwrap().0.clone();

                    if bestask != self.last_bestask || bestbid != self.last_bestbid {
                        log::debug!(
                            "best ask diff: {}, best bid diff: {}",
                            bestask - self.last_bestask,
                            bestbid - self.last_bestbid
                        );
                        self.last_bestask = bestask;
                        self.last_bestbid = bestbid;
                    }
                }

                self.depth = depth;
                if self.depth.is_complete() {
                    self.update_orders().await;
                }
            }
            BitbankEvent::CircuitBreakInfo { info, .. } => {
                log::debug!("circuit break info updated: {:?}", info);
            }
            BitbankEvent::Ticker { .. } => {}
        }
    }
}
