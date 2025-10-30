use std::env;
use std::path::PathBuf;

use bitbankutil_rs::backtest::{BacktestConfig, BacktestEngine};
use bitbankutil_rs::strategies::best_mm::MyBot;
use log::LevelFilter;
use rust_decimal::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::builder()
        .filter_level(LevelFilter::Warn)
        .format_timestamp_millis()
        .init();

    let args: Vec<String> = env::args().collect();
    if args.len() < 9 {
        eprintln!("usage: cargo run --example best_mm_backtest <pair> <capture_path> <tick_size> <refresh_cycle_ms> <lot> <max_lot> <initial_base> <initial_quote> [speed_multiplier]");
        std::process::exit(1);
    }

    let pair = args[1].clone();
    let capture_path = PathBuf::from(&args[2]);
    let tick_size: Decimal = args[3].parse()?;
    let refresh_cycle: u128 = args[4].parse()?;
    let lot: Decimal = args[5].parse()?;
    let max_lot: Decimal = args[6].parse()?;
    let initial_base: Decimal = args[7].parse()?;
    let initial_quote: Decimal = args[8].parse()?;
    let speed_multiplier: f64 = if args.len() > 9 {
        args[9].parse().unwrap_or(500.0)
    } else {
        500.0
    };

    let config = BacktestConfig::builder(pair.clone(), capture_path)
        .initial_base(initial_base)
        .initial_quote(initial_quote)
        .maker_fee_rate(Decimal::from_str("-0.0002").unwrap()) // -2 bps maker fee
        .taker_fee_rate(Decimal::from_str("0.0012").unwrap()) // 12 bps taker fee
        .speed_multiplier(speed_multiplier)
        .build();

    let engine = BacktestEngine::new(config)?;
    let api_client = engine.api_client();

    let bot = MyBot::with_api(
        api_client,
        pair.clone(),
        tick_size,
        refresh_cycle,
        lot,
        max_lot,
    );

    let report = engine.run(bot).await?;

    println!("--- backtest summary ---");
    println!("pair: {}", pair);
    println!("orders placed: {}", report.total_orders);
    println!("fills executed: {}", report.filled_trades);
    println!(
        "final PnL: {} quote (fees: {})",
        report.final_pnl, report.total_fees
    );
    println!("max drawdown: {}", report.max_drawdown);
    println!(
        "ending inventory: base={}, quote={}",
        report.ending_base, report.ending_quote
    );

    Ok(())
}
