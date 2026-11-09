use bitbankutil_rs::book_units::{Amount, Price, Quantity, literal};
use std::env;
use std::time::Duration;

use bitbankutil_rs::bitbank_bot::{
    BitbankBotBuilder, BitbankBotRuntime, BotContext, BotEvent, BotStrategy,
};
use bitbankutil_rs::bitbank_private::BitbankPrivateApiClient;
use bitbankutil_rs::depth::Depth;
use bitbankutil_rs::market_event::{MarketDepthSnapshot, MarketEvent};
use bitbankutil_rs::order_domain::{BalanceSnapshot, DesiredLimitOrder, OrderSide, OrderType};
use bitbankutil_rs::paper_execution::{
    PaperExecutionConfig, PaperExecutionEngine, PaperTradingVenue,
};
use bitbankutil_rs::trading_venue::{BitbankTradingVenue, TradingVenue};
use crypto_botters::websocket::WebSocketConfig;

struct MyBot {
    bot_config: MyBotConfig,
    depth: MarketDepthSnapshot,
    last_updated: u128,
    last_bestbid: Price,
    last_bestask: Price,
    waiting_for_depth: bool,
}

struct MyBotConfig {
    pair: String,
    tick_size: Price,
    refresh_cycle: u128,
    lot: Quantity,
    max_lot: Quantity,
}

impl MyBot {
    fn new(
        pair: String,
        tick_size: Price,
        refresh_cycle: u128,
        lot: Quantity,
        max_lot: Quantity,
    ) -> MyBot {
        MyBot {
            bot_config: MyBotConfig {
                pair,
                tick_size,
                refresh_cycle,
                lot,
                max_lot,
            },
            depth: MarketDepthSnapshot::empty(),
            last_updated: 0,
            last_bestbid: literal("0"),
            last_bestask: literal("0"),
            waiting_for_depth: false,
        }
    }

    fn orders_for_account(
        &self,
        account: bitbankutil_rs::trading_venue::AccountSnapshot,
    ) -> Result<
        Option<(
            Vec<DesiredLimitOrder>,
            Vec<bitbankutil_rs::order_domain::OpenOrder>,
            Amount,
            Amount,
        )>,
        bitbankutil_rs::book_units::NumericError,
    > {
        let current_orders = account.open_orders;
        let asset_name = self.bot_config.pair.split('_').next().unwrap();
        let Some(btc_balance_snapshot) = account
            .balances
            .iter()
            .find(|asset| asset.asset == asset_name)
        else {
            tracing::error!("account snapshot is missing {}", asset_name);
            return Ok(None);
        };
        let Some(jpy_balance_snapshot) = account.balances.iter().find(|asset| asset.asset == "jpy")
        else {
            tracing::error!("account snapshot is missing jpy");
            return Ok(None);
        };
        tracing::debug!(
            event = "strategy_account",
            open_order_count = current_orders.len()
        );

        let mut btc_locked_jpy_amount: Amount = literal("0");
        // このペアのロックされたjpyを計算する
        for current_order in &current_orders {
            if current_order.order_type == OrderType::Limit && current_order.side == OrderSide::Buy
            {
                btc_locked_jpy_amount = btc_locked_jpy_amount.checked_add(
                    current_order
                        .price
                        .expect("limit order must have a price")
                        .notional(current_order.remaining_amount)?,
                )?;
            }
        }

        let btc_free_amount = btc_balance_snapshot.free_amount;
        let btc_locked_amount = btc_balance_snapshot.locked_amount;
        let btc_amount = btc_free_amount
            .checked_add(btc_locked_amount)?
            .to_quantity()?;
        let btc_amount_remainder = btc_amount.checked_remainder(self.bot_config.lot)?;
        let jpy_free_amount = jpy_balance_snapshot.free_amount;
        let jpy_amount = jpy_free_amount.checked_add(btc_locked_jpy_amount)?;

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
            if has_bestask_order
                || best_ask_price.checked_sub(self.bot_config.tick_size)? == best_bid_price
            {
                best_ask_price
            } else {
                best_ask_price.checked_sub(self.bot_config.tick_size)?
            }
        };

        let buy_price = {
            if has_bestbid_order
                || best_bid_price.checked_add(self.bot_config.tick_size)? == best_ask_price
            {
                best_bid_price
            } else {
                best_bid_price.checked_add(self.bot_config.tick_size)?
            }
        };

        tracing::debug!("target spread: {}", sell_price.checked_sub(buy_price)?);

        let can_buy = jpy_amount >= buy_price.notional(self.bot_config.lot)?
            && btc_amount.checked_add(self.bot_config.lot)? <= self.bot_config.max_lot;
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
                self.bot_config.lot.checked_add(btc_amount_remainder)?,
                sell_price,
            ));
        }

        tracing::debug!(event = "strategy_evaluated", reason = "best_quote_market_making",
            best_bid = %best_bid_price, best_ask = %best_ask_price,
            buy_price = %buy_price, sell_price = %sell_price,
            lot = %self.bot_config.lot, max_lot = %self.bot_config.max_lot,
            tick_size = %self.bot_config.tick_size, refresh_cycle_ms = self.bot_config.refresh_cycle,
            market_time_ms = self.depth.last_timestamp(),
            base_free = %btc_free_amount, quote_free = %jpy_free_amount,
            can_buy, can_sell, target_order_count = wanna_place_orders.len());
        if wanna_place_orders.is_empty() {
            tracing::debug!(
                event = "strategy_skipped",
                reason = "balance_or_inventory_limit"
            );
        }
        for order in &wanna_place_orders {
            tracing::info!(event = "strategy_target_order", side = order.side.as_str(), price = %order.price, amount = %order.amount);
        }
        tracing::debug!(
            "evaluated asset: {}",
            sell_price.notional(btc_amount)?.checked_add(jpy_amount)?
        );
        Ok(Some((
            wanna_place_orders,
            current_orders,
            btc_free_amount,
            jpy_free_amount,
        )))
    }
    #[tracing::instrument(skip_all, fields(pair = %self.bot_config.pair.escape_default(), decision_time_ms = now))]
    async fn update_orders<V: TradingVenue>(
        &mut self,
        venue: &V,
        now: u128,
    ) -> Result<(), bitbankutil_rs::book_units::NumericError> {
        let now_inst = std::time::Instant::now();

        assert!(self.last_updated <= now);

        if now - self.last_updated >= self.bot_config.refresh_cycle {
            tracing::debug!(
                "{} milliseconds have passed since the last order update",
                now - self.last_updated
            );

            if !self.depth.is_complete() {
                if !self.waiting_for_depth {
                    tracing::info!(event = "strategy_waiting", reason = "depth_incomplete");
                    self.waiting_for_depth = true;
                } else {
                    tracing::debug!(event = "strategy_waiting", reason = "depth_incomplete");
                }
                return Ok(());
            }

            if self.waiting_for_depth {
                tracing::info!(event = "strategy_ready", reason = "depth_complete");
                self.waiting_for_depth = false;
            }
            // APIの呼び出し頻度が高くなりすぎないように、ここで `self.last_updated` を更新する
            // ここでは可変参照が必要である。
            self.last_updated = now;

            let account = match venue.account_snapshot(&self.bot_config.pair).await {
                Ok(account) => account,
                Err(_err) => {
                    tracing::warn!(
                        event = "strategy_skipped",
                        reason = "account_snapshot_failed"
                    );
                    return Ok(());
                }
            };
            let Some((wanna_place_orders, current_orders, btc_free_amount, jpy_free_amount)) =
                self.orders_for_account(account)?
            else {
                return Ok(());
            };
            bitbankutil_rs::order_manager::place_wanna_orders_concurrent(
                wanna_place_orders,
                current_orders,
                btc_free_amount,
                jpy_free_amount,
                self.bot_config.pair.clone(),
                venue.clone(),
            )
            .await?;
        }
        tracing::debug!(
            "update_orders has finished within {} ms",
            now_inst.elapsed().as_millis()
        );
        Ok(())
    }
}

impl<V: TradingVenue> BotStrategy<V> for MyBot {
    type CustomEvent = std::convert::Infallible;
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<Self::CustomEvent>,
        ctx: &BotContext<Self::CustomEvent, V>,
    ) -> Result<(), Self::Error> {
        let event = match event {
            BotEvent::Market(event) => event,
            BotEvent::Custom(never) => match never {},
        };
        match event {
            MarketEvent::Transactions { transactions, .. } => {
                tracing::trace!(event = "market_transactions", count = transactions.len());
                self.update_orders(ctx.venue(), ctx.now_ms() as u128)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            MarketEvent::DepthUpdated { depth, .. } => {
                tracing::debug!("depth updated");

                if depth.is_complete() {
                    let bestask = depth.best_ask().unwrap().0.clone();
                    let bestbid = depth.best_bid().unwrap().0.clone();

                    if bestask != self.last_bestask || bestbid != self.last_bestbid {
                        tracing::debug!(
                            "best ask diff: {}, best bid diff: {}",
                            bestask
                                .checked_sub(self.last_bestask)
                                .map_err(|e| e.to_string())?,
                            bestbid
                                .checked_sub(self.last_bestbid)
                                .map_err(|e| e.to_string())?
                        );
                        self.last_bestask = bestask;
                        self.last_bestbid = bestbid;
                    }
                }

                self.depth = depth;
                if self.depth.is_complete() {
                    self.update_orders(ctx.venue(), ctx.now_ms() as u128)
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
            MarketEvent::CircuitBreakInfo { info, .. } => {
                tracing::debug!(event = "circuit_break_updated", mode = %info.mode.chars().take(128).flat_map(char::escape_default).collect::<String>(), market_time_ms = info.timestamp.as_i64());
            }
            // Tickerイベントはこの戦略では意図的に無視される。
            MarketEvent::Ticker { .. } => {}
        }
        Ok(())
    }
}

fn spawn_bot<V: TradingVenue>(
    venue: V,
    pair: String,
    tick_size: Price,
    refresh_cycle: u128,
    lot: Quantity,
    max_lot: Quantity,
    websocket_config: WebSocketConfig,
) -> BitbankBotRuntime<std::convert::Infallible> {
    let bot = MyBot::new(pair.clone(), tick_size, refresh_cycle, lot, max_lot);
    BitbankBotBuilder::new(bot, venue)
        .run_metadata(bitbankutil_rs::telemetry::RunMetadata {
            application: "best_mm".into(),
            application_version: env!("CARGO_PKG_VERSION").into(),
            ..Default::default()
        })
        .add_pair(pair)
        .websocket_config(websocket_config)
        .spawn()
}

fn paper_venue(
    pair: &str,
    base_initial: Amount,
    jpy_initial: Amount,
    lookahead_ms: i64,
) -> Result<
    (
        PaperTradingVenue,
        bitbankutil_rs::paper_execution::PaperDiagnostics,
    ),
    String,
> {
    if base_initial < literal("0") || jpy_initial < literal("0") {
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
            locked_amount: literal("0"),
            onhand_amount: base_initial,
        },
        BalanceSnapshot {
            asset: "jpy".to_owned(),
            free_amount: jpy_initial,
            locked_amount: literal("0"),
            onhand_amount: jpy_initial,
        },
    ];
    let engine = PaperExecutionEngine::new(
        config,
        bitbankutil_rs::paper_execution::PaperClock::Realtime,
        balances,
        None,
    )
    .map_err(|err| format!("{err:?}"))?;
    tracing::info!("paper lookahead {lookahead_ms} ms; stream completeness is assumed");
    let (venue, diagnostics) = PaperTradingVenue::with_diagnostics(engine);
    let venue = venue
        .with_simulation_config(bitbankutil_rs::simulation::SimulationConfig { lookahead_ms })
        .map_err(|e| format!("{e:?}"))?;
    Ok((venue, diagnostics))
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let mode = args.get(1).map(String::as_str);
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    tracing::info!(
        event = "logging_initialized",
        format = "text",
        writer = "stderr"
    );

    let expected_len = match mode {
        Some("--live") => 7,
        Some("--paper") => 10,
        Some("--backtest" | "--backtest-commands") => {
            if args.len() == 12 {
                12
            } else {
                11
            }
        }
        _ => 0,
    };
    if args.len() != expected_len {
        tracing::error!(
            "usage: cargo run --example best_mm -- --live PAIR TICK_SIZE REFRESH_MS LOT MAX_LOT"
        );
        tracing::error!(
            "   or: cargo run --example best_mm -- --paper PAIR TICK_SIZE REFRESH_MS LOT MAX_LOT BASE_INITIAL JPY_INITIAL LOOKAHEAD_MS"
        );
        tracing::error!(
            "   or: cargo run --release --example best_mm -- --backtest PAIR TICK_SIZE REFRESH_MS LOT MAX_LOT BASE_INITIAL JPY_INITIAL LOOKAHEAD_MS MANIFEST [REPEATS]"
        );
        tracing::error!(
            "   --backtest-commands uses the experimental synchronous event/command API with the same arguments"
        );
        std::process::exit(2);
    }
    if matches!(mode, Some("--backtest" | "--backtest-commands")) {
        if let Err(error) = run_backtest(&args).await {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    let pair = args[2].clone();
    let tick_size: Price = args[3].parse().expect("invalid tick size");
    let refresh_cycle: u128 = args[4].parse().expect("invalid refresh cycle");
    let lot: Quantity = args[5].parse().expect("invalid lot");
    let max_lot: Quantity = args[6].parse().expect("invalid max lot");
    assert!(tick_size > literal("0") && lot > literal("0") && lot <= max_lot);

    let mut websocket_config = WebSocketConfig::default();
    websocket_config.refresh_after = Duration::from_secs(3600);
    websocket_config.ignore_duplicate_during_reconnection = true;

    let (_runtime, paper_engine) = match mode {
        Some("--live") => {
            let key =
                env::var("BITBANK_API_KEY").expect("BITBANK_API_KEY is required for live mode");
            let secret = env::var("BITBANK_API_SECRET")
                .expect("BITBANK_API_SECRET is required for live mode");
            let venue = BitbankTradingVenue::new(BitbankPrivateApiClient::new(key, secret, None));
            let runtime = spawn_bot(
                venue,
                pair,
                tick_size,
                refresh_cycle,
                lot,
                max_lot,
                websocket_config,
            );
            (runtime, None)
        }
        Some("--paper") => {
            let base_initial: Amount = args[7].parse().expect("invalid base initial balance");
            let jpy_initial: Amount = args[8].parse().expect("invalid JPY initial balance");
            let lookahead_ms = args[9].parse().expect("invalid max trade delay");
            let (venue, engine) = paper_venue(&pair, base_initial, jpy_initial, lookahead_ms)
                .expect("invalid paper setup");
            let runtime = spawn_bot(
                venue,
                pair,
                tick_size,
                refresh_cycle,
                lot,
                max_lot,
                websocket_config,
            );
            (runtime, Some(engine))
        }
        _ => unreachable!(),
    };

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some(engine) = &paper_engine {
            // 各イベントは記録時にログ出力済み。取得済みの履歴を定期的に破棄する。
            let engine = engine.snapshot();
            tracing::debug!(
                "paper settlement (provisional): {:?}",
                engine.settlement_status()
            );
        }
    }
}

async fn run_backtest(args: &[String]) -> Result<(), String> {
    use bitbankutil_rs::{bitbank_bot::BotRunner, replay::*, simulation::SimulationConfig};
    use std::{sync::atomic::AtomicBool, time::Instant};
    let pair = &args[2];
    let tick_size: Price = args[3]
        .parse()
        .map_err(|e| format!("invalid tick size: {e}"))?;
    let refresh: u128 = args[4]
        .parse()
        .map_err(|e| format!("invalid refresh: {e}"))?;
    let lot: Quantity = args[5].parse().map_err(|e| format!("invalid lot: {e}"))?;
    let max_lot: Quantity = args[6]
        .parse()
        .map_err(|e| format!("invalid max lot: {e}"))?;
    let base_initial = args[7]
        .parse()
        .map_err(|e| format!("invalid initial balance: {e}"))?;
    let jpy_initial = args[8]
        .parse()
        .map_err(|e| format!("invalid initial balance: {e}"))?;
    let lookahead_ms = args[9]
        .parse()
        .map_err(|e| format!("invalid lookahead: {e}"))?;
    let repeats: usize = args
        .get(11)
        .map(|s| s.parse())
        .transpose()
        .map_err(|e| format!("invalid repeats: {e}"))?
        .unwrap_or(1);
    if tick_size <= literal("0") || lot <= literal("0") || lot > max_lot || repeats == 0 {
        return Err("requires positive tick, lot, repeats and lot <= max_lot".into());
    }
    // 入力準備より先に設定を検証する。
    paper_venue(pair, base_initial, jpy_initial, lookahead_ms)?;
    let started = Instant::now();
    let prepared = PreparedReplay::new(ReplayInput::open(&args[10]).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let parsed_seconds = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let compiled = prepared
        .compile(
            SimulationConfig { lookahead_ms },
            Default::default(),
            |_| Ok(None::<std::convert::Infallible>),
        )
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::json!({"prepare_seconds":parsed_seconds,"compile_seconds":started.elapsed().as_secs_f64(),"records":prepared.len()})
    );
    for repeat in 0..repeats {
        if args[1] == "--backtest-commands" {
            use bitbankutil_rs::{
                event_strategy::EventRunner,
                paper_execution::{OwnedPaperExecution, PaperClock},
            };
            let config =
                PaperExecutionConfig::bitbank_spot_default(pair).map_err(|e| format!("{e:?}"))?;
            let engine = PaperExecutionEngine::new(
                config,
                PaperClock::Replay { now_ms: 0 },
                vec![
                    BalanceSnapshot {
                        asset: pair.split('_').next().unwrap().into(),
                        free_amount: base_initial,
                        locked_amount: literal("0"),
                        onhand_amount: base_initial,
                    },
                    BalanceSnapshot {
                        asset: "jpy".into(),
                        free_amount: jpy_initial,
                        locked_amount: literal("0"),
                        onhand_amount: jpy_initial,
                    },
                ],
                None,
            )
            .map_err(|e| format!("{e:?}"))?;
            let execution = OwnedPaperExecution::new(
                engine,
                Default::default(),
                SimulationConfig { lookahead_ms },
            )
            .map_err(|e| format!("{e:?}"))?;
            let mut runner = EventRunner::new(
                CommandBot::new(MyBot::new(pair.clone(), tick_size, refresh, lot, max_lot)),
                execution,
            );
            let started = Instant::now();
            let report = compiled
                .run_commands(
                    &mut runner,
                    ReplayOptions {
                        recovery: true,
                        end_time_ms: None,
                    },
                    &AtomicBool::new(false),
                )
                .await;
            let elapsed = started.elapsed().as_secs_f64();
            println!(
                "{}",
                serde_json::json!({"repeat":repeat,"seconds":elapsed,"records":report.records,"error":report.error,"incomplete":report.incomplete,"final_time_ms":report.final_time_ms,"balances":runner.execution().balances(),"orders":runner.execution().open_orders()})
            );
            if let Some(error) = report.error {
                return Err(error);
            }
            continue;
        }
        let (venue, _) = paper_venue(pair, base_initial, jpy_initial, lookahead_ms)?;
        let mut runner = BotRunner::new(
            MyBot::new(pair.clone(), tick_size, refresh, lot, max_lot),
            venue,
        );
        let started = Instant::now();
        let report = compiled
            .run(
                &mut runner,
                ReplayOptions {
                    recovery: true,
                    end_time_ms: None,
                },
                &AtomicBool::new(false),
            )
            .await;
        let elapsed = started.elapsed().as_secs_f64();
        let diagnostics = runner.paper_diagnostics().unwrap();
        println!(
            "{}",
            serde_json::json!({"repeat":repeat,"seconds":elapsed,"records":report.records,"error":report.error,"incomplete":report.incomplete,"final_time_ms":report.final_time_ms,"balances":diagnostics.balances(),"orders":diagnostics.open_orders()})
        );
        if let Some(error) = report.error {
            return Err(error);
        }
    }
    Ok(())
}

#[path = "best_mm/commands.rs"]
mod commands;
use commands::CommandBot;

#[cfg(test)]
#[path = "best_mm/tests.rs"]
mod tests;
