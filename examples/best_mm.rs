use std::env;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bitbankutil_rs::bitbank_bot::{BitbankBotBuilder, BitbankBotRuntime, BotContext, BotStrategy};
use bitbankutil_rs::bitbank_private::BitbankPrivateApiClient;
use bitbankutil_rs::depth::Depth;
use bitbankutil_rs::market_event::{MarketDepthSnapshot, MarketEvent};
use bitbankutil_rs::order_domain::{BalanceSnapshot, DesiredLimitOrder, OrderSide, OrderType};
use bitbankutil_rs::paper_execution::{
    PaperExecutionConfig, PaperExecutionEngine, PaperTradingVenue,
};
use bitbankutil_rs::trading_venue::{BitbankTradingVenue, TradingVenue};
use crypto_botters::generic_api_client::websocket::WebSocketConfig;
use log::LevelFilter;
use rust_decimal::prelude::*;

struct MyBot<V: TradingVenue> {
    bot_config: MyBotConfig<V>,
    depth: MarketDepthSnapshot,
    last_updated: u128,
    last_bestbid: Decimal,
    last_bestask: Decimal,
}

struct MyBotConfig<V: TradingVenue> {
    pair: String,
    tick_size: Decimal,
    refresh_cycle: u128,
    lot: Decimal,
    max_lot: Decimal,
    venue: V,
}

impl<V: TradingVenue> MyBot<V> {
    fn new(
        venue: V,
        pair: String,
        tick_size: Decimal,
        refresh_cycle: u128,
        lot: Decimal,
        max_lot: Decimal,
    ) -> MyBot<V> {
        MyBot {
            bot_config: MyBotConfig {
                pair,
                tick_size,
                refresh_cycle,
                lot,
                max_lot,
                venue,
            },
            depth: MarketDepthSnapshot::empty(),
            last_updated: 0,
            last_bestbid: Decimal::zero(),
            last_bestask: Decimal::zero(),
        }
    }

    async fn update_orders(&mut self) {
        let now_inst = std::time::Instant::now();
        let now: u128 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();

        assert!(self.last_updated <= now);

        if now - self.last_updated >= self.bot_config.refresh_cycle {
            log::debug!(
                "{} milliseconds have passed since the last order update",
                now - self.last_updated
            );

            if !self.depth.is_complete() {
                log::info!("depth is not complete");
                return;
            }

            // APIの呼び出し頻度が高くなりすぎないように、ここで `self.last_updated` を更新する
            // ここでは可変参照が必要である。
            self.last_updated = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis();

            let account = match self
                .bot_config
                .venue
                .account_snapshot(&self.bot_config.pair)
                .await
            {
                Ok(account) => account,
                Err(err) => {
                    log::error!("account snapshot failed: {:?}", err);
                    return;
                }
            };
            let current_orders = account.open_orders;
            let asset_name = self.bot_config.pair.split('_').next().unwrap();
             let Some(btc_balance_snapshot) = account
                 .balances
                 .iter()
                 .find(|asset| asset.asset == asset_name)
             else {
                 log::error!("account snapshot is missing {}", asset_name);
                 return;
            };
            let Some(jpy_balance_snapshot) =
                account.balances.iter().find(|asset| asset.asset == "jpy")
            else {
                 log::error!("account snapshot is missing jpy");
                 return;
             };
             log::debug!("active orders: {:?}", current_orders);

            let mut btc_locked_jpy_amount: Decimal = Decimal::zero();
            // このペアのロックされたjpyを計算する
            for current_order in &current_orders {
                if current_order.order_type == OrderType::Limit
                    && current_order.side == OrderSide::Buy
                {
                    btc_locked_jpy_amount +=
                        current_order.price.expect("limit order must have a price")
                            * current_order.remaining_amount;
                }
            }

            let btc_free_amount = btc_balance_snapshot.free_amount;
            let btc_locked_amount = btc_balance_snapshot.locked_amount;
            let btc_amount = btc_free_amount + btc_locked_amount;
            let btc_amount_remainder =
                btc_amount - (btc_amount / self.bot_config.lot).floor() * self.bot_config.lot;
            let jpy_free_amount = jpy_balance_snapshot.free_amount;
            let jpy_amount = jpy_free_amount + btc_locked_jpy_amount;

            log::debug!("btc_free_amount: {:?}, btc_locked_amount: {:?}, jpy_free_amount{:?}, btc_locked_jpy_amount: {:?}", btc_free_amount, btc_locked_amount, jpy_free_amount, btc_locked_jpy_amount);
            log::info!(
                "{}_amount: {}, jpy_amount: {}",
                self.bot_config.pair.clone(),
                btc_amount,
                jpy_amount
            );

            let best_ask_price = self.depth.best_ask().unwrap().0.clone();
            let best_bid_price = self.depth.best_bid().unwrap().0.clone();

            let has_bestask_order = current_orders.iter().any(|ord| {
                ord.side == OrderSide::Sell
                    && ord.order_type == OrderType::Limit
                    && ord.price == Some(best_ask_price)
            });

            let has_bestbid_order = current_orders.iter().any(|ord| {
                ord.side == OrderSide::Buy
                    && ord.order_type == OrderType::Limit
                    && ord.price == Some(best_bid_price)
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

            log::debug!("target spread: {}", sell_price - buy_price);

            let can_buy = jpy_amount >= buy_price * self.bot_config.lot
                && btc_amount + self.bot_config.lot <= self.bot_config.max_lot;
            let can_sell = btc_amount >= self.bot_config.lot;

            let mut wanna_place_orders = Vec::new();

            if can_buy {
                wanna_place_orders.push(DesiredLimitOrder::limit(
                    self.bot_config.pair.clone(),
                    OrderSide::Buy,
                    self.bot_config.lot,
                    buy_price,
                ));
            }

            if can_sell {
                wanna_place_orders.push(DesiredLimitOrder::limit(
                    self.bot_config.pair.clone(),
                    OrderSide::Sell,
                    self.bot_config.lot + btc_amount_remainder,
                    sell_price,
                ));
            }

            log::debug!("wanna_place_orders: {:?}", wanna_place_orders);
            log::info!("evaluated asset: {}", btc_amount * sell_price + jpy_amount);
            bitbankutil_rs::order_manager::place_wanna_orders_concurrent(
                wanna_place_orders,
                current_orders,
                btc_free_amount,
                jpy_free_amount,
                self.bot_config.pair.clone(),
                self.bot_config.venue.clone(),
            )
            .await;
        }
        log::debug!(
            "update_orders has finished within {} ms",
            now_inst.elapsed().as_millis()
        );
    }
}

impl<V: TradingVenue> BotStrategy for MyBot<V> {
    type Event = MarketEvent;
    async fn handle_event(&mut self, event: Self::Event, _ctx: &BotContext<Self::Event>) {
        if let Err(err) = self.bot_config.venue.observe_market_event(&event) {
            log::error!("market event processing failed: {:?}", err);
            return;
        }
        match event {
            MarketEvent::Transactions { transactions, .. } => {
                log::debug!("transaction updated: {:?}", transactions);
                self.update_orders().await;
            }
            MarketEvent::DepthUpdated { depth, .. } => {
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
            MarketEvent::CircuitBreakInfo { info, .. } => {
                log::debug!("circuit break info updated: {:?}", info);
            }
            // Tickerイベントはこの戦略では意図的に無視される。
            MarketEvent::Ticker { .. } => {}
        }
    }
}

fn spawn_bot<V: TradingVenue>(
    venue: V,
    pair: String,
    tick_size: Decimal,
    refresh_cycle: u128,
    lot: Decimal,
    max_lot: Decimal,
    websocket_config: WebSocketConfig,
) -> BitbankBotRuntime<MarketEvent> {
    let bot = MyBot::new(venue, pair.clone(), tick_size, refresh_cycle, lot, max_lot);
    BitbankBotBuilder::new(bot)
        .add_pair(pair)
        .websocket_config(websocket_config)
        .spawn()
}

fn paper_venue(
    pair: &str,
    base_initial: Decimal,
    jpy_initial: Decimal,
) -> Result<PaperTradingVenue, String> {
    if base_initial < Decimal::ZERO || jpy_initial < Decimal::ZERO {
        return Err("paper initial balances must be non-negative".to_owned());
    }
    let config =
        PaperExecutionConfig::bitbank_spot_default(pair).map_err(|err| format!("{err:?}"))?;
    let base_asset = pair
        .strip_suffix("_jpy")
        .ok_or("paper requires a JPY pair")?;
    let balances = vec![
        BalanceSnapshot {
            asset: base_asset.to_owned(),
            free_amount: base_initial,
            locked_amount: Decimal::ZERO,
            onhand_amount: base_initial,
        },
        BalanceSnapshot {
            asset: "jpy".to_owned(),
            free_amount: jpy_initial,
            locked_amount: Decimal::ZERO,
            onhand_amount: jpy_initial,
        },
    ];
    let engine = PaperExecutionEngine::new(config, balances).map_err(|err| format!("{err:?}"))?;
    Ok(PaperTradingVenue::new(engine))
}

#[tokio::main]
async fn main() {
    env_logger::builder()
        .filter_level(LevelFilter::Info)
        .format_timestamp_millis()
        .init();

    let args: Vec<String> = env::args().collect();
    let mode = args.get(1).map(String::as_str);
    let expected_len = match mode {
        Some("--live") => 7,
        Some("--paper") => 9,
        _ => 0,
    };
    if args.len() != expected_len {
        log::error!(
            "usage: cargo run --example best_mm -- --live PAIR TICK_SIZE REFRESH_MS LOT MAX_LOT"
        );
        log::error!("   or: cargo run --example best_mm -- --paper PAIR TICK_SIZE REFRESH_MS LOT MAX_LOT BASE_INITIAL JPY_INITIAL");
        std::process::exit(2);
    }

    let pair = args[2].clone();
    let tick_size: Decimal = args[3].parse().expect("invalid tick size");
    let refresh_cycle: u128 = args[4].parse().expect("invalid refresh cycle");
    let lot: Decimal = args[5].parse().expect("invalid lot");
    let max_lot: Decimal = args[6].parse().expect("invalid max lot");
    assert!(tick_size > Decimal::ZERO && lot > Decimal::ZERO && lot <= max_lot);

    let mut websocket_config = WebSocketConfig::default();
    websocket_config.refresh_after = Duration::from_secs(3600);
    websocket_config.ignore_duplicate_during_reconnection = true;

    let _runtime = match mode {
        Some("--live") => {
            let key =
                env::var("BITBANK_API_KEY").expect("BITBANK_API_KEY is required for live mode");
            let secret = env::var("BITBANK_API_SECRET")
                .expect("BITBANK_API_SECRET is required for live mode");
            let venue = BitbankTradingVenue::new(BitbankPrivateApiClient::new(key, secret, None));
            spawn_bot(
                venue,
                pair,
                tick_size,
                refresh_cycle,
                lot,
                max_lot,
                websocket_config,
            )
        }
        Some("--paper") => {
            let base_initial: Decimal = args[7].parse().expect("invalid base initial balance");
            let jpy_initial: Decimal = args[8].parse().expect("invalid JPY initial balance");
            let venue = paper_venue(&pair, base_initial, jpy_initial).expect("invalid paper setup");
            spawn_bot(
                venue,
                pair,
                tick_size,
                refresh_cycle,
                lot,
                max_lot,
                websocket_config,
            )
        }
        _ => unreachable!(),
    };

    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitbankutil_rs::market_event::MarketTrade;
    use bitbankutil_rs::trading_venue::{PlacementRequest, TradingVenue};

    #[derive(Clone, Default)]
    struct ProbeVenue {
        calls: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    impl TradingVenue for ProbeVenue {
        fn account_snapshot<'a>(
            &'a self,
            _pair: &'a str,
        ) -> bitbankutil_rs::trading_venue::TradingVenueFuture<
            'a,
            bitbankutil_rs::trading_venue::AccountSnapshot,
        > {
            Box::pin(async move {
                self.calls.lock().unwrap().push("snapshot");
                Ok(bitbankutil_rs::trading_venue::AccountSnapshot {
                    open_orders: vec![],
                    balances: vec![
                        BalanceSnapshot {
                            asset: "btc".to_owned(),
                            free_amount: Decimal::ZERO,
                            locked_amount: Decimal::ZERO,
                            onhand_amount: Decimal::ZERO,
                        },
                        BalanceSnapshot {
                            asset: "jpy".to_owned(),
                            free_amount: Decimal::ZERO,
                            locked_amount: Decimal::ZERO,
                            onhand_amount: Decimal::ZERO,
                        },
                    ],
                })
            })
        }

        fn place_order(
            &self,
            _request: PlacementRequest,
        ) -> bitbankutil_rs::trading_venue::TradingVenueFuture<
            '_,
            bitbankutil_rs::trading_venue::PlacedOrder,
        > {
            Box::pin(async { panic!("unexpected order") })
        }

        fn cancel_orders<'a>(
            &'a self,
            _pair: &'a str,
            _order_ids: Vec<bitbankutil_rs::order_domain::OrderId>,
        ) -> bitbankutil_rs::trading_venue::TradingVenueFuture<'a, ()> {
            Box::pin(async { panic!("unexpected cancellation") })
        }

        fn observe_market_event(
            &self,
            _event: &MarketEvent,
        ) -> Result<(), bitbankutil_rs::trading_venue::TradingVenueError> {
            self.calls.lock().unwrap().push("observe");
            Ok(())
        }
    }

    #[tokio::test]
    async fn bot_observes_trade_before_reading_account() {
        let venue = ProbeVenue::default();
        let mut bot = MyBot::new(
            venue.clone(),
            "btc_jpy".to_owned(),
            Decimal::ONE,
            0,
            Decimal::new(1, 1),
            Decimal::ONE,
        );
        let asks = [(Decimal::new(6_000_000, 0), 1.0)].into();
        let bids = [(Decimal::new(5_000_000, 0), 1.0)].into();
        bot.depth = MarketDepthSnapshot::new(asks, bids, 0);
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);
        let context = BotContext::new(sender);
        bot.handle_event(
            MarketEvent::Transactions {
                pair: "btc_jpy".to_owned(),
                transactions: vec![],
            },
            &context,
        )
        .await;
        assert_eq!(*venue.calls.lock().unwrap(), vec!["observe", "snapshot"]);
    }

    #[tokio::test]
    async fn paper_mode_needs_no_credentials_and_exposes_initial_balances() {
        let venue = paper_venue("btc_jpy", Decimal::ZERO, Decimal::new(1_000_000, 0)).unwrap();
        let snapshot = venue.account_snapshot("btc_jpy").await.unwrap();
        assert!(snapshot.open_orders.is_empty());
        assert_eq!(
            snapshot
                .balances
                .iter()
                .find(|b| b.asset == "jpy")
                .unwrap()
                .free_amount,
            Decimal::new(1_000_000, 0)
        );
    }

    #[test]
    fn paper_setup_rejects_negative_balance_and_unsupported_pair() {
        assert!(paper_venue("btc_jpy", Decimal::new(-1, 0), Decimal::ZERO).is_err());
        assert!(paper_venue("btc_usdt", Decimal::ZERO, Decimal::ZERO).is_err());
    }

    #[tokio::test]
    async fn paper_trade_is_applied_before_next_account_read() {
        let venue = paper_venue("btc_jpy", Decimal::ZERO, Decimal::new(1_000_000, 0)).unwrap();
        let order = DesiredLimitOrder::limit(
            "btc_jpy".to_owned(),
            OrderSide::Buy,
            Decimal::new(1, 1),
            Decimal::new(5_000_000, 0),
        );
        venue
            .place_order(PlacementRequest::from(order))
            .await
            .unwrap();
        venue
            .observe_market_event(&MarketEvent::Transactions {
                pair: "btc_jpy".to_owned(),
                transactions: vec![MarketTrade {
                    amount: Decimal::new(5, 2),
                    executed_at: 1,
                    price: Decimal::new(5_000_000, 0),
                    side: OrderSide::Sell,
                    transaction_id: 1,
                }],
            })
            .unwrap();
        let snapshot = venue.account_snapshot("btc_jpy").await.unwrap();
        assert_eq!(snapshot.open_orders[0].remaining_amount, Decimal::new(5, 2));
        assert_eq!(
            snapshot
                .balances
                .iter()
                .find(|b| b.asset == "btc")
                .unwrap()
                .free_amount,
            Decimal::new(5, 2)
        );
        assert_eq!(
            snapshot
                .balances
                .iter()
                .find(|b| b.asset == "jpy")
                .unwrap()
                .locked_amount,
            Decimal::new(250_000, 0)
        );
    }
}
