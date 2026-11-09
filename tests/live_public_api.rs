#![cfg(feature = "live-public-api")]

use bitbankutil_rs::bitbank_public::BitbankPublicApiClient;
use bitbankutil_rs::bitbank_structs::BitbankDepth;

fn logging_init() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
}

#[tokio::test]
async fn test_public_get_ticker() {
    logging_init();
    let public_client = BitbankPublicApiClient::new();
    let res = public_client.get_ticker("eth_jpy").await;

    tracing::debug!("{:?}", res);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_public_get_tickers() {
    logging_init();
    let public_client = BitbankPublicApiClient::new();
    let res = public_client.get_tickers().await;
    tracing::debug!("{:?}", res);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_public_get_tickers_jpy() {
    logging_init();
    let public_client = BitbankPublicApiClient::new();
    let res = public_client.get_tickers_jpy().await;
    tracing::debug!("{:?}", res);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_public_get_transactions() {
    logging_init();
    let public_client = BitbankPublicApiClient::new();

    let res_without_date = public_client.get_transactions("btc_jpy", None).await;
    tracing::debug!("{:?}", res_without_date);
    assert!(res_without_date.is_ok());

    let res_with_date = public_client
        .get_transactions("btc_jpy", Some("20241127"))
        .await;
    tracing::debug!("{:?}", res_with_date);
    assert!(res_with_date.is_ok());
}

#[tokio::test]
async fn test_public_get_candlestick() {
    logging_init();
    let public_client = BitbankPublicApiClient::new();
    let res = public_client
        .get_candlestick("btc_jpy", "1day", "2024")
        .await;
    tracing::debug!("{:?}", res);
    assert!(res.is_ok());

    let candlestick = res.unwrap();
    assert!(!candlestick.candlestick.is_empty());
    assert_eq!(candlestick.candlestick[0].r#type, "1day");
}

#[tokio::test]
async fn test_public_get_depth() {
    logging_init();
    let public_client = BitbankPublicApiClient::new();
    let res = public_client.get_depth("eth_jpy").await;

    let mut depth = BitbankDepth::new();
    depth.update_whole(res.unwrap()).unwrap();
    tracing::debug!("{}", depth);
}

#[tokio::test]
async fn test_public_get_circuit_break_info() {
    logging_init();
    let public_client = BitbankPublicApiClient::new();
    let res = public_client.get_circuit_break_info("eth_jpy").await;
    tracing::debug!("{:?}", res);
    assert!(res.is_ok());
    let circuit_break_info = res.unwrap();
    tracing::debug!("Circuit Break Info: {:?}", circuit_break_info);
}
