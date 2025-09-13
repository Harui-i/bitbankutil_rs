use std::env;
use std::time::Duration;

use bitbankutil_rs::bitbank_multiple_bot::MultiBotTrait;
use bitbankutil_rs::bitbank_structs::BitbankDepth;
use crypto_botters::generic_api_client::websocket::WebSocketConfig;
use log::LevelFilter;

struct MyBot {}

impl MyBot {
    fn new() -> MyBot {
        MyBot {}
    }
}

impl MultiBotTrait<()> for MyBot {
    async fn on_depth_update(&self, pair: &str, depth: &BitbankDepth, _state: ()) -> () {
        log::info!("pair: {}, depth: {}", pair, depth);
        ()
    }
}

#[tokio::main]
async fn main() {
    env_logger::builder()
        .filter_level(LevelFilter::Debug)
        .format_timestamp_millis()
        .init();

    let args: Vec<String> = env::args().collect();

    if args.len() < 2_usize {
        log::error!("there should be one or more arguments: pairs (like `btc_jpy` `xrp_jpy`).");
        std::process::exit(-1);
    }

    let mut wsc = WebSocketConfig::default();
    wsc.refresh_after = Duration::from_secs(3600);
    wsc.ignore_duplicate_during_reconnection = true;
    let wsc = wsc; // make it immutable

    let pairs = args[1..].to_vec();
    let bot = MyBot::new();
    bot.run(pairs, vec![], wsc, ()).await;

    println!("end");
}
