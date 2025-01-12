use std::env;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bitbankutil_rs::bitbank_bot::{BotTrait, SimplifiedOrder};
use bitbankutil_rs::bitbank_private::BitbankPrivateApiClient;
use bitbankutil_rs::bitbank_structs::BitbankDepth;
use bitbankutil_rs::depth::Depth;
use crypto_botters::generic_api_client::websocket::WebSocketConfig;
use log::LevelFilter;
use rust_decimal::prelude::*;

struct MyBot {
    pair: String,
    tick_size: Decimal,
    refresh_cycle: u128,
    lot: Decimal,
    max_lot: Decimal,
    bb_api_client: BitbankPrivateApiClient,
    last_updated: u128,
    depth: BitbankDepth,
    last_bestbid: Decimal,
    last_bestask: Decimal,
}

impl MyBot {
    fn new(
        bitbank_key: String,
        bitbank_secret: String,
        pair: String,
        tick_size: Decimal,
        refresh_cycle: u128,
        lot: Decimal,
        max_lot: Decimal,
    ) -> MyBot {
        MyBot {
            pair: pair,
            tick_size,
            refresh_cycle,
            lot,
            max_lot,
            bb_api_client: BitbankPrivateApiClient::new(bitbank_key, bitbank_secret, None),

            last_updated: 0,
            depth: BitbankDepth::new(),
            last_bestask: Decimal::zero(),
            last_bestbid: Decimal::zero(),
        }
    }

    async fn update_orders(&mut self) {
        let now_inst = std::time::Instant::now();
        let now: u128 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();

        assert!(self.last_updated <= now);

        if now - self.last_updated >= self.refresh_cycle {
            log::info!(
                "{} milliseconds have passed since the last order update",
                now - self.last_updated
            );

            if !self.depth.is_complete() {
                log::info!("depth is not complete");
                return;
            }

            // update `self.last_updated` here, in order to prevent call api too frequently
            self.last_updated = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis();

            let bb_client2 = self.bb_api_client.clone();
            let pair2 = self.pair.clone();
            let order_info_task = tokio::spawn(async move {
                bb_client2
                    .get_active_orders(Some(&pair2), None, None, None, None, None)
                    .await
            });

            let bb_client3 = self.bb_api_client.clone();
            let current_asset_task = tokio::spawn(async move { bb_client3.get_assets().await });

            // wait until two task has ended
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

            let asset_name = self.pair.split("_").next().unwrap();

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
            // calculate locked jpy for this pair
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
            let btc_amount_remainder = btc_amount - (btc_amount / self.lot).floor() * self.lot;
            let jpy_free_amount = jpy_asset.free_amount.clone().parse::<Decimal>().unwrap();
            let jpy_amount = jpy_free_amount + btc_locked_jpy_amount;

            log::debug!("btc_free_amount: {:?}, btc_locked_amount: {:?}, jpy_free_amount{:?}, btc_locked_jpy_amount: {:?}", btc_free_amount, btc_locked_amount, jpy_free_amount, btc_locked_jpy_amount);
            log::info!(
                "{}_amount: {}, jpy_amount: {}",
                self.pair.clone(),
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
                if has_bestask_order || best_ask_price - self.tick_size == best_bid_price {
                    best_ask_price
                } else {
                    best_ask_price - self.tick_size
                }
            };

            let buy_price = {
                if has_bestbid_order || best_bid_price + self.tick_size == best_ask_price {
                    best_bid_price
                } else {
                    best_bid_price + self.tick_size
                }
            };

            log::info!("target spread: {}", sell_price - buy_price);

            let can_buy =
                jpy_amount >= buy_price * self.lot && btc_amount + self.lot <= self.max_lot;
            let can_sell = btc_amount >= self.lot;

            let mut wanna_place_orders = Vec::new();

            if can_buy {
                wanna_place_orders.push(SimplifiedOrder {
                    pair: self.pair.clone(),
                    side: "buy".to_owned(),
                    amount: self.lot,
                    price: buy_price,
                });
            }

            if can_sell {
                wanna_place_orders.push(SimplifiedOrder {
                    pair: self.pair.clone(),
                    side: "sell".to_owned(),
                    amount: self.lot + btc_amount_remainder,
                    price: sell_price,
                });
            }

            log::debug!("wanna_place_orders: {:?}", wanna_place_orders);
            log::info!("evaluated asset: {}", btc_amount * sell_price + jpy_amount);
            {
                let bb_client = self.bb_api_client.clone();
                bitbankutil_rs::bitbank_bot::place_wanna_orders_concurrent(
                    wanna_place_orders,
                    active_orders_info.orders,
                    btc_free_amount,
                    jpy_free_amount,
                    self.pair.clone(),
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

impl BotTrait for MyBot {
    async fn on_transactions(
        &mut self,
        transactions: &Vec<bitbankutil_rs::bitbank_structs::BitbankTransactionDatum>,
    ) {
        log::debug!("transaction updated: {:?}", transactions);

        self.update_orders().await;
    }

    async fn on_depth_update(&mut self, depth: &bitbankutil_rs::bitbank_structs::BitbankDepth) {
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

            self.update_orders().await;
        }

        self.depth = depth.clone();
    }
}

#[tokio::main]
async fn main() {
    //     Pipe(Box<dyn io::Write + Send + 'static>),
    env_logger::builder()
        .filter_level(LevelFilter::Debug)
        .format_timestamp_millis()
        .init();

    let args: Vec<String> = env::args().collect();

    if args.len() != 6_usize {
        log::error!("there should be five arguments: pair(like `btc_jpy`), tick size(like: `1`), refresh_cycle(ms)(like `5000`), lot(like `0.0001`), max_lot(like `0.0005`).");
        log::error!("example: cargo run --example best_mm xrp_jpy 0.001 300 1 5");
        std::process::exit(-1);
    }

    let bitbank_key: String = env::var("BITBANK_API_KEY")
        .expect("there should be BITBANK_API_KEY in enviroment variables");
    let bitbank_secret: String = env::var("BITBANK_API_SECRET")
        .expect("there should be BITBANK_API_SECRET in environment variables");

    let mut wsc = WebSocketConfig::default();
    wsc.refresh_after = Duration::from_secs(3600);
    wsc.ignore_duplicate_during_reconnection = true;
    let wsc = wsc; // make it immutable

    let pair = args[1].clone();
    let tick_size: Decimal = args[2].parse().unwrap();
    let refresh_cycle: u128 = args[3].parse().unwrap();
    let lot: Decimal = args[4].parse().unwrap();
    let max_lot: Decimal = args[5].parse().unwrap();

    assert!(lot <= max_lot);

    let mut bot = MyBot::new(
        bitbank_key,
        bitbank_secret,
        pair.clone(),
        tick_size,
        refresh_cycle,
        lot,
        max_lot,
    );

    let _bot_task = tokio::spawn(async move {
        bot.run(pair.clone(), vec![], wsc).await;
    })
    .await
    .unwrap();

    println!("end");
}
