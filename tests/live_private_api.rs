#![cfg(feature = "live-private-api")]

use std::env;

use bitbankutil_rs::bitbank_private::BitbankPrivateApiClient;
use bitbankutil_rs::bitbank_structs::{
    BitbankActiveOrdersResponse, BitbankAssetsData, BitbankChannelAndTokenResponse,
    BitbankTradeHistoryResponse,
};

fn logging_init() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
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
    tracing::info!("{:?}", assets);
}

#[tokio::test]
async fn test_private_get_active_orders() {
    logging_init();
    let bb_client = init_client();

    let active_orders_res: BitbankActiveOrdersResponse = bb_client
        .get_active_orders(Some("btc_jpy"), None, None, None, None, None)
        .await
        .unwrap();

    tracing::info!("active orders response: {:?}", active_orders_res);
}

#[tokio::test]
async fn test_private_get_trade_history() {
    logging_init();
    let bb_client = init_client();

    let history: BitbankTradeHistoryResponse = bb_client
        .get_trade_history(Some("eth_jpy"), None, None, None, None, Some("asc"))
        .await
        .unwrap();
    tracing::info!("Bitbank trade history: {:?}", history);
}

#[tokio::test]
async fn test_private_get_deposits() {
    logging_init();
    let bb_client = init_client();

    let crypto_history = bb_client
        .get_deposit_history(None, Some(1), None, None)
        .await;
    let jpy_history = bb_client
        .get_deposit_history(Some("jpy"), Some(1), None, None)
        .await;
    let unconfirmed = bb_client.get_unconfirmed_deposits().await;
    let originators = bb_client.get_deposit_originators().await;

    let mut failed = Vec::new();
    for (api, result) in [
        (
            "get_deposit_history (crypto)",
            crypto_history.map(|data| !data.deposits.is_empty()),
        ),
        (
            "get_deposit_history (jpy)",
            jpy_history.map(|data| !data.deposits.is_empty()),
        ),
        (
            "get_unconfirmed_deposits",
            unconfirmed.map(|data| !data.deposits.is_empty()),
        ),
        (
            "get_deposit_originators",
            originators.map(|data| !data.originators.is_empty()),
        ),
    ] {
        if let Ok(has_records) = result {
            tracing::info!(api, has_records);
        } else {
            failed.push(api);
        }
    }
    // APIエラーの本文や口座データをテスト出力に表示しない。
    assert!(failed.is_empty(), "deposit API requests failed: {failed:?}");
}

#[tokio::test]
async fn test_private_get_channel_and_token() {
    logging_init();
    let bb_client = init_client();

    let _channel_and_token: BitbankChannelAndTokenResponse =
        bb_client.get_channel_and_token().await.unwrap();
    tracing::info!(event = "channel_token_received", success = true);
}
