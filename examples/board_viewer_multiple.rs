use std::env;
use std::time::Duration;

use bitbankutil_rs::bitbank_bot::{BitbankBotBuilder, BotContext, BotEvent, BotStrategy};
use crypto_botters::websocket::WebSocketConfig;

struct MyBot {}

impl MyBot {
    fn new() -> MyBot {
        MyBot {}
    }
}

impl BotStrategy for MyBot {
    type CustomEvent = std::convert::Infallible;
    type Error = String;

    async fn handle_event(
        &mut self,
        event: BotEvent<Self::CustomEvent>,
        _ctx: &BotContext<Self::CustomEvent>,
    ) -> Result<(), Self::Error> {
        if let BotEvent::Market(bitbankutil_rs::market_event::MarketEvent::DepthUpdated {
            pair,
            depth,
        }) = event
        {
            tracing::info!("pair: {}, depth: {}", pair, depth);
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() {
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

    let args: Vec<String> = env::args().collect();

    if args.len() < 2_usize {
        tracing::error!("there should be one or more arguments: pairs (like `btc_jpy` `xrp_jpy`).");
        std::process::exit(-1);
    }

    let mut wsc = WebSocketConfig::default();
    wsc.refresh_after = Duration::from_secs(3600);
    wsc.ignore_duplicate_during_reconnection = true;
    let wsc = wsc; // 不変にする

    let pairs = args[1..].to_vec();
    let bot = MyBot::new();
    let _runtime = BitbankBotBuilder::market_data(bot)
        .run_metadata(bitbankutil_rs::telemetry::RunMetadata {
            application: "board_viewer_multiple".into(),
            application_version: env!("CARGO_PKG_VERSION").into(),
            ..Default::default()
        })
        .with_pairs(pairs)
        .websocket_config(wsc)
        .spawn();

    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
