use crate::bitbank_structs::{
    BitbankActiveOrdersResponse, BitbankApiResponse, BitbankAssetsData, BitbankCancelOrderResponse,
    BitbankCancelOrdersResponse, BitbankChannelAndTokenResponse, BitbankCreateOrderResponse,
    BitbankGetOrderResponse, BitbankSpotStatusResponse, BitbankTradeHistoryResponse,
};
use crypto_botters::{
    bitbank::{BitbankHandleError, BitbankHttpUrl, BitbankOption},
    Client, GetOptions,
};
use std::time::Instant;

#[derive(Clone)]
pub struct BitbankPrivateApiClient {
    client: Client,
}

impl BitbankPrivateApiClient {
    pub fn new(
        api_key: String,
        api_secret: String,
        options: Option<Vec<BitbankOption>>,
    ) -> BitbankPrivateApiClient {
        let mut client = Client::new();

        client.update_default_option(BitbankOption::HttpAuth(true));
        client.update_default_option(BitbankOption::HttpUrl(BitbankHttpUrl::Private));
        client.update_default_option(BitbankOption::Key(api_key));
        client.update_default_option(BitbankOption::Secret(api_secret));

        if let Some(options) = options {
            for option in options {
                client.update_default_option(option);
            }
        }

        // 認証オプションが設定されているか確認します。
        assert_eq!(<crypto_botters::Client as GetOptions<crypto_botters::bitbank::BitbankOptions>>::default_options(&client).http_auth, true);

        assert_eq!(<crypto_botters::Client as GetOptions<crypto_botters::bitbank::BitbankOptions>>::default_options(&client).http_url, BitbankHttpUrl::Private);

        assert_ne!(<crypto_botters::Client as GetOptions<crypto_botters::bitbank::BitbankOptions>>::default_options(&client).key, Some("".to_owned()));
        assert_ne!(<crypto_botters::Client as GetOptions<crypto_botters::bitbank::BitbankOptions>>::default_options(&client).secret, Some("".to_owned()));

        BitbankPrivateApiClient { client: client }
    }

    // ポジションを確認するために使用します。
    pub async fn get_assets(&self) -> Result<BitbankAssetsData, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<
                &str,
                crypto_botters::bitbank::BitbankHandleError,
            >,
        > = self
            .client
            .get_no_query("/user/assets", [BitbankOption::Default])
            .await;

        let duration = start_time.elapsed();
        log::debug!("get_assets request took {:?}", duration);

        crate::response_handler::handle_response("get_assets", res)
    }

    // 注文情報を取得します。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-order-information
    pub async fn get_order(
        &self,
        pair: &str,
        order_id: u64,
    ) -> Result<BitbankGetOrderResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get(
                "/user/spot/order",
                Some(&serde_json::json!({"pair": pair, "order_id": order_id})),
                [BitbankOption::Default],
            )
            .await;

        let duration = start_time.elapsed();
        log::debug!("get_order request took {:?}", duration);

        crate::response_handler::handle_response("get_order", res)
    }

    // 新規注文を作成します。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#create-new-order
    pub async fn post_order(
        &self,
        pair: &str,
        amount: &str,
        price: Option<&str>,
        side: &str,
        r#type: &str,
        post_only: Option<bool>,
        trigger_price: Option<&str>,
    ) -> Result<BitbankCreateOrderResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        assert!(side == "buy" || side == "sell");
        assert!(
            r#type == "limit" || r#type == "market" || r#type == "stop" || r#type == "stop_limit"
        );
        // post_onlyがtrue => r#typeは "limit"
        assert!(post_only.is_none() || (post_only.unwrap() == true && r#type == "limit"));

        let mut body_map = serde_json::Map::new();

        body_map.insert("pair".to_string(), serde_json::json!(pair));
        body_map.insert("amount".to_string(), serde_json::json!(amount));

        if price.is_some() {
            body_map.insert("price".to_string(), serde_json::json!(price));
        }

        body_map.insert("side".to_string(), serde_json::json!(side));
        body_map.insert("type".to_string(), serde_json::json!(r#type));

        if post_only.is_some() {
            body_map.insert("post_only".to_string(), serde_json::json!(post_only));
        }

        if trigger_price.is_some() {
            body_map.insert(
                "trigger_price".to_string(),
                serde_json::json!(trigger_price),
            );
        }

        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .post(
                "/user/spot/order",
                Some(&serde_json::Value::Object(body_map)),
                [BitbankOption::Default],
            )
            .await;

        let duration = start_time.elapsed();
        log::debug!("post_order request took {:?}", duration);

        crate::response_handler::handle_response("post_order", res)
    }

    // 取引履歴を取得します: https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-trade-history
    pub async fn get_trade_history(
        &self,
        pair: Option<&str>,    // ペア
        count: Option<i64>,    // 取得件数 (最大1000)
        order_id: Option<i64>, // 注文ID
        since: Option<i64>,    // 開始Unixタイムスタンプ
        end: Option<i64>,      // 終了Unixタイムスタンプ
        order: Option<&str>,   // 履歴の順序 (`asc`または`desc`、デフォルトは`desc`)
    ) -> Result<BitbankTradeHistoryResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let mut request_body = serde_json::Map::new();

        if let Some(pair) = pair {
            request_body.insert("pair".to_string(), serde_json::json!(pair));
        }
        if let Some(count) = count {
            request_body.insert("count".to_string(), serde_json::json!(count));
        }

        if let Some(order_id) = order_id {
            request_body.insert("order_id".to_string(), serde_json::json!(order_id));
        }

        if let Some(since) = since {
            request_body.insert("since".to_string(), serde_json::json!(since));
        }
        if let Some(end) = end {
            request_body.insert("end".to_string(), serde_json::json!(end));
        }

        if let Some(order) = order {
            request_body.insert("order".to_string(), serde_json::json!(order));
        }

        let request_body = request_body; // 不変にします

        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get(
                "/user/spot/trade_history",
                Some(&serde_json::Value::Object(request_body)),
                [BitbankOption::Default],
            )
            .await;

        let duration = start_time.elapsed();
        log::debug!("trade_history request took {:?}", duration);

        crate::response_handler::handle_response("get_trade_history", res)
    }

    // 注文をキャンセルします。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#cancel-order
    pub async fn post_cancel_order(
        &self,
        pair: &str,
        order_id: u64,
    ) -> Result<BitbankCancelOrderResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .post(
                "/user/spot/cancel_order",
                Some(&serde_json::json!({"pair": pair, "order_id": order_id})),
                [BitbankOption::Default],
            )
            .await;

        let duration = start_time.elapsed();
        log::debug!("post_cancel_order request took {:?}", duration);

        crate::response_handler::handle_response("post_cancel_order", res)
    }

    // 複数の注文をキャンセルします。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#cancel-multiple-orders
    pub async fn post_cancel_orders(
        &self,
        pair: &str,
        order_ids: Vec<u64>,
    ) -> Result<BitbankCancelOrdersResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        assert!(0 < order_ids.len() && order_ids.len() <= 30_usize);

        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .post(
                "/user/spot/cancel_orders",
                Some(&serde_json::json!({"pair": pair, "order_ids": order_ids})),
                [BitbankOption::Default],
            )
            .await;

        let duration = start_time.elapsed();
        log::debug!("post_cancel_orders request took {:?}", duration);

        crate::response_handler::handle_response("post_cancel_orders", res)
    }

    // TODO
    // 複数の注文を取得します。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-multiple-orders
    pub fn post_orders_info(&self) {
        todo!();
    }

    // 有効な注文を取得します。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-active-orders
    pub async fn get_active_orders(
        &self,
        pair: Option<&str>,
        count: Option<&str>,
        from_id: Option<u64>,
        end_id: Option<u64>,
        since: Option<u64>,
        end: Option<u64>,
    ) -> Result<BitbankActiveOrdersResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let mut request_body = serde_json::Map::new();
        if let Some(pair) = pair {
            request_body.insert("pair".to_string(), serde_json::json!(pair));
        }
        if let Some(count) = count {
            request_body.insert("count".to_string(), serde_json::json!(count));
        }
        if let Some(from_id) = from_id {
            request_body.insert("from_id".to_string(), serde_json::json!(from_id));
        }
        if let Some(end_id) = end_id {
            request_body.insert("end_id".to_string(), serde_json::json!(end_id));
        }
        if let Some(since) = since {
            request_body.insert("since".to_string(), serde_json::json!(since));
        }
        if let Some(end) = end {
            request_body.insert("end".to_string(), serde_json::json!(end));
        }

        let request_body = request_body; // 不変にします

        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get(
                "/user/spot/active_orders",
                Some(&serde_json::Value::Object(request_body)),
                [BitbankOption::Default],
            )
            .await;

        let duration = start_time.elapsed();
        log::debug!("get_active_orders request took {:?}", duration);

        crate::response_handler::handle_response("get_active_orders", res)
    }

    // 取引所のステータスを取得します。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#get-exchange-status
    pub async fn get_status(
        &self,
    ) -> Result<BitbankSpotStatusResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();

        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get_no_query("/spot/status", [BitbankOption::Default])
            .await;

        let duration = start_time.elapsed();
        log::debug!("get_status request took {:?}", duration);

        crate::response_handler::handle_response("get_status", res)
    }

    // プライベートストリーム用のチャンネルとトークンを取得します。 cf: https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#private-stream
    pub async fn get_channel_and_token(
        &self,
    ) -> Result<BitbankChannelAndTokenResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();

        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get_no_query("/user/subscribe", [BitbankOption::Default])
            .await;
        let duration = start_time.elapsed();
        log::debug!("get_channel_and_token request took {:?}", duration);

        crate::response_handler::handle_response("get_channel_and_token", res)
    }
}

// テスト成功時に標準出力を表示したい場合は、`RUST_LOG=debug cargo test -- --nocapture` を実行してください。
// 並列実行を避けるには、`--` の後に `--test-threads=1` を追加する必要があります。
// 推奨される形式: `cargo test XXX -- --test-threads=1`
#[cfg(test)]
mod tests {
    use std::{env, time::Duration};

    use super::*;

    fn logging_init() {
        let _a = env_logger::builder()
            .format_timestamp_millis()
            .is_test(true)
            .try_init()
            .ok();
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
        let assets = bb_client.get_assets().await;
        println!("{:?}", assets);
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

    // このテストは不安定です！
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

    #[tokio::test]
    async fn test_private_get_trade_history() {
        logging_init();
        let bitbank_key = env::var("BITBANK_API_KEY").unwrap();
        let bitbank_secret = env::var("BITBANK_API_SECRET").unwrap();
        let bb_client = BitbankPrivateApiClient::new(bitbank_key, bitbank_secret, None);

        let history = bb_client
            .get_trade_history(Some("eth_jpy"), None, None, None, None, Some("asc"))
            .await
            .unwrap();
        log::info!("Bitbank trade history: {:?}", history);
    }

    #[tokio::test]
    async fn test_private_get_status() {
        logging_init();
        let bitbank_key = env::var("BITBANK_API_KEY").unwrap();
        let bitbank_secret = env::var("BITBANK_API_SECRET").unwrap();
        let bb_client = BitbankPrivateApiClient::new(bitbank_key, bitbank_secret, None);

        let status = bb_client.get_status().await.unwrap();
        log::info!("Bitbank spot status: {:?}", status);
    }

    #[tokio::test]
    async fn test_private_get_channel_and_token() {
        logging_init();
        let bitbank_key = env::var("BITBANK_API_KEY").unwrap();
        let bitbank_secret = env::var("BITBANK_API_SECRET").unwrap();
        let bb_client = BitbankPrivateApiClient::new(bitbank_key, bitbank_secret, None);

        let channel_and_token = bb_client.get_channel_and_token().await;
        log::info!("Bitbank channel and token: {:?}", channel_and_token);
        assert!(channel_and_token.is_ok());
    }

    // 意図的にレート制限を超えます。実行したい場合は、`cargo test -- --ignored` のように `-- --ignored` オプションを追加してください。
    #[tokio::test]
    #[ignore]
    async fn test_private_exceed_rate_limit() {
        logging_init();

        let bitbank_key = env::var("BITBANK_API_KEY").unwrap();
        let bitbank_secret = env::var("BITBANK_API_SECRET").unwrap();
        let bb_client = BitbankPrivateApiClient::new(bitbank_key, bitbank_secret, None);

        for _ in 0..10 {
            let res = bb_client
                .post_order("btc_jpy", "1", Some("12"), "buy", "limit", Some(true), None)
                .await
                .expect("post_order returned Err");

            println!("{:?}", res);
        }
    }
}
