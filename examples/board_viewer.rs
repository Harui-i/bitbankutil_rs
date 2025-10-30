use std::env;
use std::time::Duration;

use bitbankutil_rs::bitbank_bot::{BitbankBotBuilder, BitbankEvent, BotContext, BotStrategy};
use crypto_botters::generic_api_client::websocket::WebSocketConfig;
use log::LevelFilter;

struct MyBot {}

impl MyBot {
    fn new() -> MyBot {
        MyBot {}
    }
}

impl BotStrategy for MyBot {
    type Event = BitbankEvent;

    async fn handle_event(&mut self, event: Self::Event, _ctx: &BotContext<Self::Event>) {
        if let BitbankEvent::DepthUpdated { pair, depth } = event {
            log::info!("pair: {}\n{}", pair, depth);
        }
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
    let wsc = wsc; // 不変にします

    let pair = args[1].clone();
    let bot = MyBot::new();

    let _runtime = BitbankBotBuilder::new(bot)
        .add_pair(pair)
        .websocket_config(wsc)
        .spawn();

    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
