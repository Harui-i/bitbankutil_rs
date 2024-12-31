use std::env;
use std::time::Duration;

use bitbankutil_rs::bitbank_bot::BotTrait;
use crypto_botters::generic_api_client::websocket::WebSocketConfig;
use log::LevelFilter;

struct MyBot {}

impl MyBot {
    fn new() -> MyBot {
        MyBot {}
    }
}

impl BotTrait for MyBot {
    async fn on_transactions(
        &mut self,
        _transactions: &Vec<bitbankutil_rs::bitbank_structs::BitbankTransactionDatum>,
    ) {
    }

    async fn on_depth_update(&mut self, depth: &bitbankutil_rs::bitbank_structs::BitbankDepth) {
        log::info!("{}", depth);
    }
}

#[tokio::main]
async fn main() {
    env_logger::builder()
        .filter_level(LevelFilter::Debug)
        .format_timestamp_millis()
        .init();

    let args: Vec<String> = env::args().collect();

    if args.len() != 2_usize {
        log::error!("there should be one arguments: pair(like `btc_jpy`).");
        std::process::exit(-1);
    }

    let mut wsc = WebSocketConfig::default();
    wsc.refresh_after = Duration::from_secs(3600);
    wsc.ignore_duplicate_during_reconnection = true;
    let wsc = wsc; // make it immutable

    let pair = args[1].clone();
    let mut bot = MyBot::new();

    let _bot_task = tokio::spawn(async move {
        bot.run(pair.clone(), vec![], wsc).await;
    })
    .await
    .unwrap();

    println!("end");
}
