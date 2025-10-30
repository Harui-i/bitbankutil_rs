use std::env;
use std::time::Duration;

use bitbankutil_rs::bitbank_bot::BitbankBotBuilder;
use bitbankutil_rs::strategies::best_mm::MyBot;
use crypto_botters::generic_api_client::websocket::WebSocketConfig;
use log::LevelFilter;
use rust_decimal::prelude::*;

#[tokio::main]
async fn main() {
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
    let wsc = wsc;

    let pair = args[1].clone();
    let tick_size: Decimal = args[2].parse().unwrap();
    let refresh_cycle: u128 = args[3].parse().unwrap();
    let lot: Decimal = args[4].parse().unwrap();
    let max_lot: Decimal = args[5].parse().unwrap();

    assert!(lot <= max_lot);

    let bot = MyBot::new(
        bitbank_key,
        bitbank_secret,
        pair.clone(),
        tick_size,
        refresh_cycle,
        lot,
        max_lot,
    );

    let _runtime = BitbankBotBuilder::new(bot)
        .add_pair(pair)
        .websocket_config(wsc)
        .spawn();

    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
