#![cfg(feature = "live-private-order-api")]

use std::{env, time::Duration};

use bitbankutil_rs::bitbank_private::BitbankPrivateApiClient;
use bitbankutil_rs::bitbank_structs::{
    BitbankCancelOrderResponse, BitbankCancelOrdersResponse, BitbankCreateOrderResponse,
};

fn logging_init() {
    let _ = env_logger::builder()
        .format_timestamp_millis()
        .is_test(true)
        .try_init();
}

fn init_client() -> BitbankPrivateApiClient {
    require_live_private_order_tests();

    let bitbank_key = env::var("BITBANK_API_KEY").unwrap();
    let bitbank_secret = env::var("BITBANK_API_SECRET").unwrap();

    BitbankPrivateApiClient::new(bitbank_key, bitbank_secret, None)
}

fn require_live_private_order_tests() {
    assert_eq!(
        env::var("BITBANKUTIL_RUN_PRIVATE_ORDER_TESTS").as_deref(),
        Ok("1"),
        "set BITBANKUTIL_RUN_PRIVATE_ORDER_TESTS=1 to run order-mutating live tests"
    );
}

#[tokio::test]
async fn test_private_get_order() {
    logging_init();
    let bb_client = init_client();

    let post_order_res: BitbankCreateOrderResponse = bb_client
        .post_order("btc_jpy", "1", Some("12"), "buy", "limit", Some(true), None)
        .await
        .unwrap();
    log::info!("post order: {:?}", post_order_res);

    tokio::time::sleep(Duration::from_millis(1000)).await;

    let get_order_res = bb_client
        .get_order("btc_jpy", post_order_res.order_id.as_u64().unwrap())
        .await
        .unwrap();

    log::info!("fetched order information: {:?}", get_order_res);

    let cancel_res = bb_client
        .post_cancel_order("btc_jpy", post_order_res.order_id.as_u64().unwrap())
        .await
        .unwrap();

    log::info!("cancelled response: {:?}", cancel_res);
}

// このテストは不安定だ！
#[tokio::test]
async fn test_private_post_cancel_order() {
    logging_init();
    let bb_client = init_client();

    let post_order_res: BitbankCreateOrderResponse = bb_client
        .post_order("btc_jpy", "1", Some("14"), "buy", "limit", Some(true), None)
        .await
        .unwrap();

    log::info!("order posted: {:?}", post_order_res);

    tokio::time::sleep(Duration::from_millis(1000)).await;

    let cancel_order_res: BitbankCancelOrderResponse = bb_client
        .post_cancel_order("btc_jpy", post_order_res.order_id.as_u64().unwrap())
        .await
        .unwrap();

    log::info!("cancel order response : {:?}", cancel_order_res);
}

#[tokio::test]
async fn test_private_post_cancel_orders() {
    logging_init();
    let bb_client = init_client();

    let post_order_res1: BitbankCreateOrderResponse = bb_client
        .post_order("btc_jpy", "1", Some("12"), "buy", "limit", Some(true), None)
        .await
        .unwrap();

    log::info!("post_order_res1: {:?}", post_order_res1);

    let post_order_res2: BitbankCreateOrderResponse = bb_client
        .post_order("btc_jpy", "1", Some("13"), "buy", "limit", Some(true), None)
        .await
        .unwrap();

    log::info!("post_order_res1: {:?}", post_order_res2);

    tokio::time::sleep(Duration::from_millis(1000)).await;

    let cancel_orders_res: BitbankCancelOrdersResponse = bb_client
        .post_cancel_orders(
            "btc_jpy",
            vec![
                post_order_res1.order_id.as_u64().unwrap(),
                post_order_res2.order_id.as_u64().unwrap(),
            ],
        )
        .await
        .unwrap();

    log::info!("cancel orders respones: {:?}", cancel_orders_res);
}

// 意図的にレート制限を超える。実行したい場合だけテスト名を指定してください。
#[tokio::test]
#[ignore]
async fn test_private_exceed_rate_limit() {
    logging_init();

    let bb_client = init_client();

    for _ in 0..10 {
        let res = bb_client
            .post_order("btc_jpy", "1", Some("12"), "buy", "limit", Some(true), None)
            .await
            .expect("post_order returned Err");

        println!("{:?}", res);
    }
}
