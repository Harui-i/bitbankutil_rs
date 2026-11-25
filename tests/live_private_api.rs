#![cfg(feature = "live-private-api")]

use std::{collections::HashMap, env};

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
async fn test_private_post_orders_info() {
    let client = init_client();
    let history = client
        .get_trade_history(None, Some(100), None, None, None, Some("desc"))
        .await
        .unwrap_or_else(|_| panic!("failed to fetch trade history for orders_info"));

    // 同じ注文の分割約定が履歴に並ぶことがあるため、異なる注文IDを選ぶ。
    let mut first_id_by_pair = HashMap::new();
    let (pair, mut requested_ids) = history
        .trades
        .into_iter()
        .find_map(|trade| {
            let id = trade
                .order_id
                .as_u64()
                .expect("expected a numeric order ID");
            let first = *first_id_by_pair.entry(trade.pair.clone()).or_insert(id);
            (first != id).then_some((trade.pair, [first, id]))
        })
        .expect("orders_info live test requires two recent filled orders for one pair");

    let result = client
        .post_orders_info(&pair, &requested_ids)
        .await
        .unwrap_or_else(|_| panic!("orders_info request failed"));
    assert!(
        result.orders.iter().all(|order| order.pair == pair),
        "orders_info returned an unexpected pair"
    );
    let mut returned_ids = result
        .orders
        .iter()
        .map(|order| {
            order
                .order_id
                .as_u64()
                .expect("expected a numeric order ID")
        })
        .collect::<Vec<_>>();
    requested_ids.sort_unstable();
    returned_ids.sort_unstable();
    assert!(
        returned_ids == requested_ids,
        "orders_info did not return both requested orders"
    );
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

#[tokio::test]
async fn test_private_withdrawal_accounts() {
    let client = init_client();
    for asset in ["btc", "xrp"] {
        let accounts = client.get_withdrawal_accounts(asset).await.unwrap();
        println!("{asset} withdrawal accounts: {}", accounts.accounts.len());
    }

    let jpy = client.get_withdrawal_accounts("jpy").await.unwrap();
    assert!(
        jpy.accounts
            .iter()
            .all(|account| account.account_number.is_some()
                && account.account_owner.is_some()
                && account.address.is_none())
    );
    println!("JPY withdrawal accounts: {}", jpy.accounts.len());
}

#[tokio::test]
async fn test_private_withdrawal_history() {
    let client = init_client();
    let crypto = client
        .get_withdrawal_history(None, Some(100), None, None)
        .await
        .unwrap();
    assert!(crypto.withdrawals.iter().all(|entry| entry.asset != "jpy"));
    println!("crypto withdrawals: {}", crypto.withdrawals.len());

    let jpy = client
        .get_withdrawal_history(Some("jpy"), Some(100), None, None)
        .await
        .unwrap();
    assert!(jpy.withdrawals.iter().all(|entry| entry.asset == "jpy"));
    println!("JPY withdrawals: {}", jpy.withdrawals.len());
}

#[tokio::test]
#[ignore = "sends an intentionally invalid request to the production withdrawal endpoint"]
async fn test_private_withdrawal_rejects_invalid_request() {
    let client = init_client();
    let error = client
        .request_withdrawal("xrp", "", "0", None, None)
        .await
        .unwrap_err();
    let code = error
        .exchange_code
        .expect("expected an error code from bitbank");
    println!(
        "invalid withdrawal request rejected: {} ({code})",
        error.reason_code
    );
}
