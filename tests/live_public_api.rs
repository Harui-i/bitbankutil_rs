#![cfg(feature = "live-public-api")]

use bitbankutil_rs::bitbank_public::BitbankPublicApiClient;
use bitbankutil_rs::bitbank_structs::BitbankDepth;

fn logging_init() {
    let _ = env_logger::builder()
        .format_timestamp_millis()
        .is_test(true)
        .try_init();
}

#[tokio::test]
async fn test_public_get_ticker() {
    logging_init();
    let public_client = BitbankPublicApiClient::new();
    let res = public_client.get_ticker("eth_jpy").await;

    log::debug!("{:?}", res);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_public_get_tickers() {
    logging_init();
    let public_client = BitbankPublicApiClient::new();
    let res = public_client.get_tickers().await;
    log::debug!("{:?}", res);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_public_get_tickers_jpy() {
    logging_init();
    let public_client = BitbankPublicApiClient::new();
    let res = public_client.get_tickers_jpy().await;
    log::debug!("{:?}", res);
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_public_get_transactions() {
    logging_init();
    let public_client = BitbankPublicApiClient::new();

    let res_without_date = public_client.get_transactions("btc_jpy", None).await;
    log::debug!("{:?}", res_without_date);
    assert!(res_without_date.is_ok());

    let res_with_date = public_client
        .get_transactions("btc_jpy", Some("20241127"))
        .await;
    log::debug!("{:?}", res_with_date);
    assert!(res_with_date.is_ok());
}

#[tokio::test]
async fn test_public_get_candlestick() {
    logging_init();
    let public_client = BitbankPublicApiClient::new();
    let res = public_client
        .get_candlestick("btc_jpy", "1day", "2024")
        .await;
    log::debug!("{:?}", res);
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
    depth.update_whole(res.unwrap());
    log::debug!("{}", depth);
}

#[tokio::test]
async fn test_public_get_circuit_break_info() {
    logging_init();
    let public_client = BitbankPublicApiClient::new();
    let res = public_client.get_circuit_break_info("eth_jpy").await;
    log::debug!("{:?}", res);
    assert!(res.is_ok());
    let circuit_break_info = res.unwrap();
    log::debug!("Circuit Break Info: {:?}", circuit_break_info);
}
