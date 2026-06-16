#![cfg(feature = "live-private-api")]

use std::env;

use bitbankutil_rs::bitbank_private::BitbankPrivateApiClient;
use bitbankutil_rs::bitbank_structs::{
    BitbankActiveOrdersResponse, BitbankAssetsData, BitbankChannelAndTokenResponse,
    BitbankSpotStatusResponse, BitbankTradeHistoryResponse,
};

fn logging_init() {
    let _ = env_logger::builder()
        .format_timestamp_millis()
        .is_test(true)
        .try_init();
}

fn init_client() -> BitbankPrivateApiClient {
    let bitbank_key = env::var("BITBANK_API_KEY").unwrap();
    let bitbank_secret = env::var("BITBANK_API_SECRET").unwrap();

    BitbankPrivateApiClient::new(bitbank_key, bitbank_secret, None)
}

#[tokio::test]
async fn test_private_get_assets() {
    logging_init();

    let bb_client = init_client();
    let assets: BitbankAssetsData = bb_client.get_assets().await.unwrap();
    log::info!("{:?}", assets);
}

#[tokio::test]
async fn test_private_get_active_orders() {
    logging_init();
    let bb_client = init_client();

    let active_orders_res: BitbankActiveOrdersResponse = bb_client
        .get_active_orders(Some("btc_jpy"), None, None, None, None, None)
        .await
        .unwrap();

    log::info!("active orders response: {:?}", active_orders_res);
}

#[tokio::test]
async fn test_private_get_trade_history() {
    logging_init();
    let bb_client = init_client();

    let history: BitbankTradeHistoryResponse = bb_client
        .get_trade_history(Some("eth_jpy"), None, None, None, None, Some("asc"))
        .await
        .unwrap();
    log::info!("Bitbank trade history: {:?}", history);
}

#[tokio::test]
async fn test_private_get_status() {
    logging_init();
    let bb_client = init_client();

    let status: BitbankSpotStatusResponse = bb_client.get_status().await.unwrap();
    log::info!("Bitbank spot status: {:?}", status);
}

#[tokio::test]
async fn test_private_get_channel_and_token() {
    logging_init();
    let bb_client = init_client();

    let channel_and_token: BitbankChannelAndTokenResponse =
        bb_client.get_channel_and_token().await.unwrap();
    log::info!("Bitbank channel and token: {:?}", channel_and_token);
}
