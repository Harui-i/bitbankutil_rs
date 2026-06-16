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

fn validate_post_order_args(side: &str, r#type: &str, post_only: Option<bool>) {
    assert!(side == "buy" || side == "sell");
    assert!(r#type == "limit" || r#type == "market" || r#type == "stop" || r#type == "stop_limit");
    // post_onlyはlimit注文でのみ指定できる。
    assert!(post_only.is_none() || r#type == "limit");
}

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

        // 認証オプションが設定されているか確認する。
        assert!(<crypto_botters::Client as GetOptions<crypto_botters::bitbank::BitbankOptions>>::default_options(&client).http_auth);

        assert_eq!(<crypto_botters::Client as GetOptions<crypto_botters::bitbank::BitbankOptions>>::default_options(&client).http_url, BitbankHttpUrl::Private);

        assert_ne!(<crypto_botters::Client as GetOptions<crypto_botters::bitbank::BitbankOptions>>::default_options(&client).key, Some("".to_owned()));
        assert_ne!(<crypto_botters::Client as GetOptions<crypto_botters::bitbank::BitbankOptions>>::default_options(&client).secret, Some("".to_owned()));

        BitbankPrivateApiClient { client }
    }

    // ポジションを確認するために使用する。
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

    // 注文情報を取得する。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-order-information
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

    // 新規注文を作成する。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#create-new-order
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
        validate_post_order_args(side, r#type, post_only);

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

    // 取引履歴を取得する: https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-trade-history
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

        let request_body = request_body; // 不変にする

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

    // 注文をキャンセルする。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#cancel-order
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

    // 複数の注文をキャンセルする。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#cancel-multiple-orders
    pub async fn post_cancel_orders(
        &self,
        pair: &str,
        order_ids: Vec<u64>,
    ) -> Result<BitbankCancelOrdersResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        assert!(!order_ids.is_empty() && order_ids.len() <= 30_usize);

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
    // 複数の注文を取得する。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-multiple-orders
    pub fn post_orders_info(&self) {
        todo!();
    }

    // 有効な注文を取得する。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-active-orders
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

        let request_body = request_body; // 不変にする

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

    // 取引所のステータスを取得する。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#get-exchange-status
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

    // プライベートストリーム用のチャンネルとトークンを取得する。 cf: https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#private-stream
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
// 並列実行を避けるには、`--` の後に `--test-threads=1` を追加する必要がある。
// 推奨される形式: `cargo test XXX -- --test-threads=1`
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_post_order_args_accepts_false_post_only_for_limit_order() {
        validate_post_order_args("buy", "limit", Some(false));
    }

    #[test]
    #[should_panic]
    fn validate_post_order_args_rejects_post_only_for_non_limit_order() {
        validate_post_order_args("buy", "market", Some(false));
    }
}
