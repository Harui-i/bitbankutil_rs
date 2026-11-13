use crate::bitbank_structs::{
    BitbankActiveOrdersResponse, BitbankApiResponse, BitbankAssetsData, BitbankCancelOrderResponse,
    BitbankCancelOrdersResponse, BitbankChannelAndTokenResponse, BitbankCreateOrderResponse,
    BitbankGetOrderResponse, BitbankSpotStatusResponse, BitbankTradeHistoryResponse,
};
use crypto_botters::{
    bitbank::{BitbankHandleError, BitbankHttpUrl, BitbankOption, BitbankOptions},
    traits::HandlerOptions,
};
use std::time::Instant;

fn assert_valid(condition: bool, field: &'static str, reason_code: &'static str) {
    if !condition {
        tracing::warn!(
            event = "input_validation_failed",
            field,
            reason_code,
            send_attempted = false,
            action = "panic"
        );
        panic!("invalid {field}: {reason_code}");
    }
}

fn validate_post_order_args(side: &str, r#type: &str, post_only: Option<bool>) {
    assert_valid(side == "buy" || side == "sell", "side", "unknown_side");
    assert_valid(
        r#type == "limit" || r#type == "market" || r#type == "stop" || r#type == "stop_limit",
        "type",
        "unknown_order_type",
    );
    // post_onlyはlimit注文でのみ指定できる。
    assert_valid(
        post_only.is_none() || r#type == "limit",
        "post_only",
        "requires_limit_order",
    );
}

#[derive(Clone)]
pub struct BitbankPrivateApiClient {
    options: BitbankOptions,
    transport: crate::recording_http::Transport,
}

impl BitbankPrivateApiClient {
    pub fn with_recorder(
        mut self,
        recorder: crate::execution_recording::ExecutionRecorder,
    ) -> Self {
        self.transport.recorder = Some(recorder);
        self
    }

    pub fn new(
        api_key: String,
        api_secret: String,
        options: Option<Vec<BitbankOption>>,
    ) -> BitbankPrivateApiClient {
        let mut client_options = BitbankOptions::default();

        client_options.update(BitbankOption::HttpAuth(true));
        client_options.update(BitbankOption::HttpUrl(BitbankHttpUrl::Private));
        client_options.update(BitbankOption::Key(api_key));
        client_options.update(BitbankOption::Secret(api_secret));

        if let Some(options) = options {
            for option in options {
                client_options.update(option);
            }
        }

        // 認証オプションが設定されているか確認する。
        assert!(client_options.http_auth);

        assert_eq!(client_options.http_url, BitbankHttpUrl::Private);

        assert_ne!(client_options.key, Some("".to_owned()));
        assert_ne!(client_options.secret, Some("".to_owned()));

        BitbankPrivateApiClient {
            options: client_options,
            transport: Default::default(),
        }
    }

    // ポジションを確認するために使用する。
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty))]
    pub async fn get_assets(&self) -> Result<BitbankAssetsData, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, crypto_botters::bitbank::BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::GET,
                "/user/assets",
                None::<&()>,
                None::<()>,
            )
            .await;

        let result = crate::response_handler::handle_response("get_assets", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "get_assets",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
    }

    // 注文情報を取得する。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-order-information
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty, pair = %crate::telemetry::safe(pair), order_id = order_id))]
    pub async fn get_order(
        &self,
        pair: &str,
        order_id: u64,
    ) -> Result<BitbankGetOrderResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::GET,
                "/user/spot/order",
                Some(&serde_json::json!({"pair": pair, "order_id": order_id})),
                None::<()>,
            )
            .await;

        let result = crate::response_handler::handle_response("get_order", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "get_order",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
    }

    // 新規注文を作成する。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#create-new-order
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty, pair = %crate::telemetry::safe(pair)))]
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
        tracing::info!(event = "order_api_started", operation = "place", side = %crate::telemetry::safe(side),
            order_type = %crate::telemetry::safe(r#type), price = price.map(crate::telemetry::safe),
            amount = %crate::telemetry::safe(amount), post_only, trigger_price = trigger_price.map(crate::telemetry::safe));

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

        let request_started_at_ms = crate::execution_recording::timestamp_ms();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::POST,
                "/user/spot/order",
                None::<&()>,
                Some(&serde_json::Value::Object(body_map)),
            )
            .await;
        let response_completed_at_ms = crate::execution_recording::timestamp_ms();

        let result: Result<BitbankCreateOrderResponse, _> =
            crate::response_handler::handle_response("post_order", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "post_order",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        tracing::info!(
            event = "order_api_timing",
            request_started_at_ms,
            response_completed_at_ms,
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0,
            success = result.is_ok(),
        );
        match &result {
            Ok(response) => {
                tracing::info!(event = "order_api_finished", outcome = "accepted", order_id = response.order_id.as_u64(), ordered_at = response.ordered_at.as_u64(), status = %crate::telemetry::safe(&response.status));
            }
            Err(error) => crate::telemetry::api_failure(error),
        }
        result
    }

    // 取引履歴を取得する: https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-trade-history
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty, pair = pair.map(crate::telemetry::safe), order_id = order_id))]
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
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::GET,
                "/user/spot/trade_history",
                Some(&serde_json::Value::Object(request_body)),
                None::<()>,
            )
            .await;

        let result = crate::response_handler::handle_response("get_trade_history", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "trade_history",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
    }

    // 注文をキャンセルする。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#cancel-order
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty, pair = %crate::telemetry::safe(pair), order_id = order_id))]
    pub async fn post_cancel_order(
        &self,
        pair: &str,
        order_id: u64,
    ) -> Result<BitbankCancelOrderResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        tracing::info!(event = "order_api_started", operation = "cancel", order_id);
        let request_started_at_ms = crate::execution_recording::timestamp_ms();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::POST,
                "/user/spot/cancel_order",
                None::<&()>,
                Some(&serde_json::json!({"pair": pair, "order_id": order_id})),
            )
            .await;
        let response_completed_at_ms = crate::execution_recording::timestamp_ms();

        let result: Result<BitbankCancelOrderResponse, _> =
            crate::response_handler::handle_response("post_cancel_order", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "post_cancel_order",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        tracing::info!(
            event = "order_api_timing",
            request_started_at_ms,
            response_completed_at_ms,
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0,
            success = result.is_ok(),
        );
        match &result {
            Ok(response) => {
                tracing::info!(event = "order_api_finished", outcome = "accepted", order_id = response.order_id.as_u64(), canceled_at = response.canceled_at.as_ref().and_then(serde_json::Number::as_u64), status = %crate::telemetry::safe(&response.status));
            }
            Err(error) => crate::telemetry::api_failure(error),
        }
        result
    }

    // 複数の注文をキャンセルする。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#cancel-multiple-orders
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty, pair = %crate::telemetry::safe(pair)))]
    pub async fn post_cancel_orders(
        &self,
        pair: &str,
        order_ids: Vec<u64>,
    ) -> Result<BitbankCancelOrdersResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        assert_valid(
            !order_ids.is_empty() && order_ids.len() <= 30_usize,
            "order_ids",
            "batch_size_out_of_range",
        );
        tracing::info!(event = "order_api_started", operation = "cancel_batch", order_ids = ?order_ids);

        let request_started_at_ms = crate::execution_recording::timestamp_ms();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::POST,
                "/user/spot/cancel_orders",
                None::<&()>,
                Some(&serde_json::json!({"pair": pair, "order_ids": order_ids})),
            )
            .await;
        let response_completed_at_ms = crate::execution_recording::timestamp_ms();

        let result: Result<BitbankCancelOrdersResponse, _> =
            crate::response_handler::handle_response("post_cancel_orders", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "post_cancel_orders",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        tracing::info!(
            event = "order_api_timing",
            request_started_at_ms,
            response_completed_at_ms,
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0,
            success = result.is_ok(),
        );
        match &result {
            Ok(response) => {
                tracing::info!(
                    event = "order_api_finished",
                    outcome = "accepted",
                    returned_order_count = response.orders.len()
                );
                for order in &response.orders {
                    tracing::info!(event = "cancel_order_observed", order_id = order.order_id.as_u64(), canceled_at = order.canceled_at.as_ref().and_then(serde_json::Number::as_u64), status = %crate::telemetry::safe(&order.status));
                }
            }
            Err(error) => crate::telemetry::api_failure(error),
        }
        result
    }

    // TODO
    // 複数の注文を取得する。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-multiple-orders
    pub fn post_orders_info(&self) {
        todo!();
    }

    // 有効な注文を取得する。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-active-orders
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty, pair = pair.map(crate::telemetry::safe)))]
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
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::GET,
                "/user/spot/active_orders",
                Some(&serde_json::Value::Object(request_body)),
                None::<()>,
            )
            .await;

        let result = crate::response_handler::handle_response("get_active_orders", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "get_active_orders",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
    }

    // 取引所のステータスを取得する。 https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#get-exchange-status
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty))]
    pub async fn get_status(
        &self,
    ) -> Result<BitbankSpotStatusResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();

        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::GET,
                "/spot/status",
                None::<&()>,
                None::<()>,
            )
            .await;

        let result = crate::response_handler::handle_response("get_status", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "get_status",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
    }

    // プライベートストリーム用のチャンネルとトークンを取得する。 cf: https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#private-stream
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty))]
    pub async fn get_channel_and_token(
        &self,
    ) -> Result<BitbankChannelAndTokenResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();

        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::GET,
                "/user/subscribe",
                None::<&()>,
                None::<()>,
            )
            .await;
        let result = crate::response_handler::handle_response("get_channel_and_token", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "get_channel_and_token",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
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
    #[test]
    fn invalid_order_arguments_have_a_safe_reason_before_panicking() {
        let capture = crate::telemetry::tests::Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        assert!(
            std::panic::catch_unwind(|| validate_post_order_args("secret-sentinel", "limit", None))
                .is_err()
        );
        let events = capture.events();
        assert_eq!(events[0]["fields"]["reason_code"], "unknown_side");
        assert_eq!(events[0]["fields"]["field"], "side");
        assert_eq!(events[0]["fields"]["send_attempted"], false);
        assert!(!capture.text().contains("secret-sentinel"));
    }
}

#[cfg(test)]
mod recording_tests {
    use super::*;
    use crate::{
        execution_recording::{ExecutionRecorder, JsonlSink, read_records, tests::Memory},
        trading_venue::{BitbankTradingVenue, TradingVenue},
    };
    use std::io::{Read, Write};
    fn client(
        replies: Vec<Option<(u16, &'static str)>>,
        retries: u8,
    ) -> (
        BitbankPrivateApiClient,
        Memory,
        ExecutionRecorder,
        std::thread::JoinHandle<()>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut b = [0; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut b).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&b[..n]);
                }
                if let Some((status, mut body)) = reply {
                    if body == "__timeout__" {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        continue;
                    }
                    if body.contains("\"orders\":[],\"assets\":[]") {
                        body = if String::from_utf8_lossy(&request).contains("/user/assets") {
                            r#"{"success":1,"data":{"assets":[]}}"#
                        } else {
                            r#"{"success":1,"data":{"orders":[]}}"#
                        };
                    }
                    let body = if body == "__order__" || body == "__latency_order__" {
                        let request = String::from_utf8_lossy(&request);
                        let cancel = request.contains("/cancel_");
                        let mut order = serde_json::json!({
                            "order_id":if cancel {72} else {71}, "pair":"btc_jpy",
                            "side":"buy", "type":"limit", "start_amount":"1",
                            "remaining_amount":"1", "executed_amount":"0", "price":"100",
                            "user_cancelable":true, "average_price":"0", "ordered_at":1234,
                            "status":if cancel {"CANCELED_UNFILLED"} else {"UNFILLED"}
                        });
                        if cancel {
                            order["canceled_at"] = serde_json::json!(2345);
                        }
                        if request.contains("/cancel_orders") {
                            // 73は返却されない。要求にあったという理由だけで成功扱いしない。
                            if body == "__latency_order__" {
                                let mut second = order.clone();
                                second["order_id"] = serde_json::json!(74);
                                second["canceled_at"] = serde_json::json!(3456);
                                let mut missing = order.clone();
                                missing["order_id"] = serde_json::json!(75);
                                missing.as_object_mut().unwrap().remove("canceled_at");
                                order = serde_json::json!({"orders":[order, second, missing]});
                            } else {
                                order = serde_json::json!({"orders":[order]});
                            }
                        }
                        serde_json::json!({"success":1,"data":order}).to_string()
                    } else {
                        body.to_owned()
                    };
                    write!(stream,"HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                }
            }
        });
        let mut c = BitbankPrivateApiClient::new(
            "secret-key-sentinel".into(),
            "secret-value-sentinel".into(),
            None,
        );
        c.options
            .update(BitbankOption::HttpUrl(BitbankHttpUrl::None));
        let mut config = crypto_botters::http::RequestConfig::default();
        config.url_prefix = format!("http://{addr}");
        config.max_try = retries;
        config.retry_cooldown = std::time::Duration::ZERO;
        c.options.update(BitbankOption::RequestConfig(config));
        let m = Memory::default();
        let r = ExecutionRecorder::new("http-test", serde_json::json!({}), JsonlSink(m.clone()));
        (c.with_recorder(r.clone()), m, r, server)
    }
    #[tokio::test]
    async fn snapshot_is_one_operation_two_http_calls() {
        // 同じ応答を並行する両方のエンドポイント要求に返す。
        let body = r#"{"success":1,"data":{"orders":[],"assets":[]}}"#;
        let (c, m, r, server) = client(vec![Some((200, body)); 2], 1);
        let venue = BitbankTradingVenue::new(c).with_recorder(r.clone());
        let result = venue.account_snapshot("btc_jpy").await;
        assert!(result.is_ok());
        r.finish().unwrap();
        server.join().unwrap();
        let records = read_records(&m.0.lock().unwrap()[..]).unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|r| r.kind == "operation_started")
                .count(),
            1
        );
        let calls: Vec<_> = records
            .iter()
            .filter(|r| r.kind == "http_finished")
            .collect();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|r| r.operation_id == Some(1)));
        assert_ne!(calls[0].data["request_id"], calls[1].data["request_id"]);
        let text = String::from_utf8(m.0.lock().unwrap().clone()).unwrap();
        assert!(!text.contains("secret-key-sentinel"));
        assert!(!text.contains("secret-value-sentinel"));
        assert!(!text.contains("ACCESS-"));
    }
    #[tokio::test]
    async fn order_latency_logs_keep_exchange_times_and_request_correlation() {
        use crate::telemetry::tests::{Capture, span_field};
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        let (c, _, _, server) = client(
            vec![
                Some((200, "__order__")),
                Some((200, "__order__")),
                Some((200, "__latency_order__")),
            ],
            1,
        );
        let before = crate::execution_recording::timestamp_ms();
        c.post_order("btc_jpy", "1", Some("100"), "buy", "limit", None, None)
            .await
            .unwrap();
        c.post_cancel_order("btc_jpy", 72).await.unwrap();
        c.post_cancel_orders("btc_jpy", vec![72, 74, 75])
            .await
            .unwrap();
        let after = crate::execution_recording::timestamp_ms();
        server.join().unwrap();
        let events = capture.events();
        let timings: Vec<_> = events
            .iter()
            .filter(|e| e["fields"]["event"] == "order_api_timing")
            .collect();
        assert_eq!(timings.len(), 3);
        assert_eq!(
            events
                .iter()
                .filter(|e| e["fields"]["event"] == "cancel_order_observed")
                .count(),
            3
        );
        let mut ids = std::collections::BTreeSet::new();
        for timing in timings {
            let fields = &timing["fields"];
            assert!(fields["request_started_at_ms"].as_u64().unwrap() >= before);
            assert!(fields["response_completed_at_ms"].as_u64().unwrap() <= after);
            assert!(fields["elapsed_ms"].as_f64().unwrap() >= 0.0);
            let api_id = span_field(timing, "api_call_id").unwrap();
            ids.insert(api_id.as_u64().unwrap());
            let request_id = span_field(timing, "venue_request_id").unwrap();
            let observed: Vec<_> = events
                .iter()
                .filter(|e| {
                    span_field(e, "api_call_id") == Some(api_id.clone())
                        && (e["fields"]["event"] == "order_api_finished"
                            || e["fields"]["event"] == "cancel_order_observed")
                })
                .collect();
            for event in &observed {
                assert_eq!(
                    span_field(event, "venue_request_id"),
                    Some(request_id.clone())
                );
            }
            if observed.iter().any(|e| e["fields"]["order_id"] == 71) {
                assert_eq!(observed[0]["fields"]["ordered_at"], 1234);
            } else {
                let orders: Vec<_> = observed
                    .iter()
                    .filter(|e| e["fields"]["order_id"].is_number())
                    .collect();
                assert_eq!(orders[0]["fields"]["canceled_at"], 2345);
                if orders.len() > 1 {
                    assert_eq!(orders.len(), 3);
                    assert_eq!(orders[1]["fields"]["order_id"], 74);
                    assert_eq!(orders[1]["fields"]["canceled_at"], 3456);
                    assert_eq!(orders[2]["fields"]["order_id"], 75);
                    assert!(orders[2]["fields"].get("canceled_at").is_none());
                }
            }
        }
        assert_eq!(ids.len(), 3);
    }
    #[tokio::test]
    async fn parallel_order_and_cancel_keep_ids_timestamps_and_missing_results() {
        use crate::order_domain::{DesiredLimitOrder, OrderId, OrderSide};
        let (c, m, r, server) = client(vec![Some((200, "__order__")); 4], 1);
        let venue = BitbankTradingVenue::new(c.clone()).with_recorder(r.clone());
        let (placed, canceled) = tokio::join!(
            venue.place_order(
                DesiredLimitOrder::limit("btc_jpy".into(), OrderSide::Buy, 1.into(), 100.into())
                    .into()
            ),
            venue.cancel_orders("btc_jpy", vec![OrderId(72), OrderId(73)]),
        );
        assert_eq!(placed.unwrap().order_id, Some(OrderId(71)));
        canceled.unwrap();
        c.post_cancel_order("btc_jpy", 72).await.unwrap();
        c.get_order("btc_jpy", 71).await.unwrap();
        server.join().unwrap();
        let records = read_records(&m.0.lock().unwrap()[..]).unwrap();
        let calls: Vec<_> = records
            .iter()
            .filter(|r| r.kind == "http_finished")
            .collect();
        assert_eq!(calls.len(), 4);
        let ids: std::collections::BTreeSet<_> =
            calls.iter().map(|r| r.operation_id.unwrap()).collect();
        assert_eq!(ids.len(), 4);
        for call in calls {
            let start = records
                .iter()
                .find(|s| {
                    s.kind == "http_started" && s.data["request_id"] == call.data["request_id"]
                })
                .unwrap();
            assert_eq!(start.operation_id, call.operation_id);
            let timing = &call.data["timing"];
            assert_eq!(
                timing["started_at_unix_us"],
                start.data["started_at_unix_us"]
            );
            assert!(timing["elapsed_us"].as_u64().is_some());
            assert!(
                timing["completed_at_unix_us"].as_u64().unwrap()
                    <= call.recorded_at_ms * 1000 + 999
            );
            let result = &call.data["result"];
            match call.data["endpoint"].as_str().unwrap() {
                "/user/spot/cancel_orders" => {
                    assert_eq!(
                        start.data["request"]["order_ids"],
                        serde_json::json!([72, 73])
                    );
                    assert_eq!(result["orders"].as_array().unwrap().len(), 1);
                    assert_eq!(result["orders"][0]["order_id"], 72);
                    assert_eq!(result["orders"][0]["canceled_at"], 2345);
                    let op = records
                        .iter()
                        .find(|s| {
                            s.kind == "operation_started" && s.operation_id == call.operation_id
                        })
                        .unwrap();
                    assert_eq!(op.data["operation"], "cancel_orders");
                }
                "/user/spot/cancel_order" => {
                    assert_eq!(start.data["request"]["order_id"], 72);
                    assert_eq!(result["canceled_at"], 2345);
                }
                "/user/spot/order" => {
                    assert_eq!(result["order_id"], 71);
                    assert_eq!(result["ordered_at"], 1234);
                    assert!(result.get("canceled_at").is_none());
                    if start.data["method"] == "GET" {
                        assert_eq!(start.data["request"]["order_id"], 71);
                    }
                }
                _ => panic!(),
            }
        }
    }

    #[tokio::test]
    async fn timeout_is_a_failed_attempt_and_recording_does_not_change_retry_policy() {
        let capture = crate::telemetry::tests::Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        for recording in [false, true] {
            let (mut c, m, _, server) = client(
                vec![
                    Some((200, "__timeout__")),
                    Some((200, r#"{"success":1,"data":{"assets":[]}}"#)),
                ],
                2,
            );
            c.options.request_config.timeout = std::time::Duration::from_millis(30);
            // 最初の接続を閉じた後に2回目を受け付ける。
            c.options.request_config.retry_cooldown = std::time::Duration::from_millis(120);
            if !recording {
                c.transport.recorder = None;
            }
            assert!(c.get_assets().await.is_ok());
            server.join().unwrap();
            let records = read_records(&m.0.lock().unwrap()[..]).unwrap();
            let calls: Vec<_> = records
                .iter()
                .filter(|r| r.kind == "http_finished")
                .collect();
            let events = capture.events();
            assert!(
                events
                    .iter()
                    .any(|e| e["fields"]["event"] == "venue_http_attempt_finished"
                        && e["fields"]["timeout"] == true
                        && e["fields"]["will_retry"] == true)
            );
            if !recording {
                assert!(calls.is_empty());
                continue;
            }
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].data["timeout"], true);
            assert_eq!(calls[0].data["success"], false);
            assert_eq!(calls[0].data["will_retry"], true);
            assert_eq!(calls[1].data["timeout"], false);
            assert_eq!(calls[1].data["success"], true);
            assert_eq!(calls[0].operation_id, calls[1].operation_id);
            let total = records
                .iter()
                .find(|r| r.kind == "http_request_finished")
                .unwrap();
            assert_eq!(total.data["attempts"], 2);
            assert_eq!(total.data["success"], true);
            assert_eq!(total.data["timeout"], false);
            assert_eq!(total.data["stage"], "success");
            assert!(total.data["timing"]["elapsed_us"].as_u64().unwrap() >= 120_000);
        }
    }

    #[tokio::test]
    async fn exhausted_timeouts_are_recorded_as_request_timeout() {
        let (mut c, m, _, server) = client(vec![Some((200, "__timeout__")); 2], 2);
        c.options.request_config.timeout = std::time::Duration::from_millis(30);
        c.options.request_config.retry_cooldown = std::time::Duration::from_millis(120);
        assert!(c.get_assets().await.is_err());
        server.join().unwrap();
        let records = read_records(&m.0.lock().unwrap()[..]).unwrap();
        let calls: Vec<_> = records
            .iter()
            .filter(|r| r.kind == "http_finished")
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].data["will_retry"], true);
        assert_eq!(calls[1].data["will_retry"], false);
        let total = records
            .iter()
            .find(|r| r.kind == "http_request_finished")
            .unwrap();
        assert_eq!(total.data["request_id"], calls[1].data["request_id"]);
        assert_eq!(total.operation_id, calls[1].operation_id);
        assert_eq!(total.data["attempts"], 2);
        assert_eq!(total.data["success"], false);
        assert_eq!(total.data["timeout"], true);
        assert_eq!(total.data["stage"], "send");
    }

    #[tokio::test]
    async fn build_failure_is_not_sent_or_retried() {
        let memory = Memory::default();
        let recorder =
            ExecutionRecorder::new("build", serde_json::json!({}), JsonlSink(memory.clone()));
        let mut c = BitbankPrivateApiClient::new("key".into(), "secret".into(), None)
            .with_recorder(recorder);
        c.options.key = None;
        c.options.request_config.max_try = 3;
        assert!(c.post_cancel_order("btc_jpy", 99).await.is_err());
        let records = read_records(&memory.0.lock().unwrap()[..]).unwrap();
        let calls: Vec<_> = records
            .iter()
            .filter(|r| r.kind == "http_finished")
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].data["stage"], "build");
        assert_eq!(calls[0].data["send_attempted"], false);
        assert_eq!(calls[0].data["will_retry"], false);
        assert!(calls[0].data["http_status"].is_null());
        assert!(calls[0].data["result"].is_null());
        let total = records
            .iter()
            .find(|r| r.kind == "http_request_finished")
            .unwrap();
        assert_eq!(total.data["success"], false);
        assert_eq!(total.data["timeout"], false);
        assert_eq!(total.data["stage"], "build");
    }

    #[tokio::test]
    async fn errors_retries_and_token_are_recorded_safely() {
        let (c, m, r, server) = client(
            vec![
                None,
                Some((
                    429,
                    r#"{"success":0,"data":{"code":10009,"echo":"secret-value-sentinel"}}"#,
                )),
                Some((200, "invalid json")),
                Some((
                    200,
                    r#"{"success":1,"data":{"pubnub_channel":"channel","pubnub_token":"token-sentinel"}}"#,
                )),
            ],
            2,
        );
        assert!(c.get_assets().await.is_err());
        assert!(c.get_assets().await.is_err());
        let _ = c.get_channel_and_token().await;
        r.finish().unwrap();
        server.join().unwrap();
        let records = read_records(&m.0.lock().unwrap()[..]).unwrap();
        let calls: Vec<_> = records
            .iter()
            .filter(|r| r.kind == "http_finished")
            .collect();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0].data["request_id"], calls[1].data["request_id"]);
        assert_eq!(calls[1].data["attempt"], 2);
        assert_eq!(calls[1].data["http_status"], 429);
        assert_eq!(calls[1].data["exchange_code"], 10009);
        let totals: Vec<_> = records
            .iter()
            .filter(|r| r.kind == "http_request_finished")
            .collect();
        assert_eq!(totals.len(), 3);
        for (total, last_attempt) in totals.iter().zip([calls[1], calls[2], calls[3]]) {
            assert_eq!(total.data["request_id"], last_attempt.data["request_id"]);
            assert_eq!(total.data["success"], last_attempt.data["success"]);
            assert_eq!(total.data["timeout"], false);
            assert_eq!(total.data["stage"], last_attempt.data["stage"]);
        }
        assert_eq!(totals[0].data["stage"], "response");
        assert_eq!(totals[1].data["stage"], "response");
        assert_eq!(totals[2].data["stage"], "success");
        let text = String::from_utf8(m.0.lock().unwrap().clone()).unwrap();
        assert!(!text.contains("token-sentinel"));
        assert!(!text.contains("secret-value-sentinel"));
    }
    #[tokio::test]
    async fn tracing_http_ids_errors_and_secrets() {
        use crate::telemetry::tests::{Capture, span_field};
        for recording in [false, true] {
            let capture = Capture::default();
            let _guard = tracing::subscriber::set_default(capture.subscriber());
            let (mut c, memory, _, server) = client(
                vec![
                    Some((
                        429,
                        r#"{"success":0,"data":{"code":10009,"echo":"secret-value-sentinel"}}"#,
                    )),
                    Some((
                        200,
                        r#"{"success":1,"data":{"pubnub_channel":"channel","pubnub_token":"token-sentinel"}}"#,
                    )),
                ],
                1,
            );
            if !recording {
                c.transport.recorder = None;
            }
            assert!(c.get_assets().await.is_err());
            let _ = c.get_channel_and_token().await;
            server.join().unwrap();
            let events = capture.events();
            let calls: Vec<_> = events
                .iter()
                .filter(|e| e["fields"]["event"] == "venue_http_attempt_finished")
                .collect();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0]["fields"]["http_status"], 429);
            assert_eq!(calls[0]["fields"]["exchange_code"], 10009);
            assert_eq!(calls[0]["fields"]["stage"], "response");
            assert_ne!(
                calls[0]["fields"]["venue_request_id"],
                calls[1]["fields"]["venue_request_id"]
            );
            for call in &calls {
                let op = span_field(call, "operation_id").unwrap();
                let id = &call["fields"]["venue_request_id"];
                assert!(
                    events
                        .iter()
                        .any(|e| e["fields"]["event"] == "venue_http_attempt_started"
                            && e["fields"]["venue_request_id"] == *id)
                );
                // The transport has its own ID; both IDs must survive in nested spans.
                assert!(
                    events
                        .iter()
                        .any(|e| e["fields"]["event"] == "http_request_finished"
                            && span_field(e, "venue_request_id").as_ref() == Some(id)
                            && span_field(e, "request_id").is_some()
                            && span_field(e, "venue_attempt") == Some(serde_json::json!(1)))
                );
                if recording {
                    let records = read_records(&memory.0.lock().unwrap()[..]).unwrap();
                    assert!(records.iter().any(|r| r.kind == "http_finished"
                        && r.data["request_id"] == *id
                        && r.operation_id == op.as_u64()));
                }
            }
            let text = capture.text();
            for secret in [
                "secret-key-sentinel",
                "secret-value-sentinel",
                "token-sentinel",
                "ACCESS-SIGNATURE",
            ] {
                assert!(!text.contains(secret), "leaked {secret}");
            }
        }
    }
    #[tokio::test]
    async fn read_failures_keep_http_correlation_and_authentication_transitions() {
        use crate::telemetry::tests::{Capture, span_field};
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        let (c, _, _, server) = client(
            vec![
                Some((
                    200,
                    r#"{"success":0,"data":{"code":20005,"secret":"secret-sentinel"}}"#,
                )),
                Some((200, r#"{"success":1,"data":{"assets":[]}}"#)),
                Some((200, r#"{"success":1,"data":{"assets":[]}}"#)),
                Some((200, r#"{"success":1,"data":{"secret":"secret-sentinel"}}"#)),
            ],
            1,
        );
        assert!(c.get_assets().await.is_err());
        assert!(c.get_assets().await.is_ok());
        assert!(c.get_assets().await.is_ok());
        assert!(c.get_order("trx_jpy", 71).await.is_err());
        server.join().unwrap();
        let events = capture.events();
        let auth: Vec<_> = events
            .iter()
            .filter(|e| e["fields"]["event"] == "api_authentication_state")
            .collect();
        assert_eq!(auth.len(), 2);
        assert_eq!(auth[0]["fields"]["reason_code"], "invalid_access_signature");
        assert_eq!(auth[1]["fields"]["outcome"], "authenticated");
        let failed = events
            .iter()
            .find(|e| e["fields"]["event"] == "response_decode_failed")
            .unwrap();
        assert_eq!(
            span_field(failed, "pair"),
            Some(serde_json::json!("trx_jpy"))
        );
        assert_eq!(span_field(failed, "order_id"), Some(serde_json::json!(71)));
        let request = span_field(failed, "venue_request_id").unwrap();
        assert!(
            events
                .iter()
                .any(|e| e["fields"]["event"] == "venue_http_attempt_finished"
                    && e["fields"]["venue_request_id"] == request)
        );
        assert!(span_field(failed, "api_call_id").is_some());
        assert!(!capture.text().contains("secret-sentinel"));
    }
}
