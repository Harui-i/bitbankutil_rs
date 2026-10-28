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

fn validate_post_order_args(side: &str, r#type: &str, post_only: Option<bool>) {
    assert!(side == "buy" || side == "sell");
    assert!(r#type == "limit" || r#type == "market" || r#type == "stop" || r#type == "stop_limit");
    // post_onlyはlimit注文でのみ指定できる。
    assert!(post_only.is_none() || r#type == "limit");
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
    pub async fn get_assets(&self) -> Result<BitbankAssetsData, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<
                &str,
                crypto_botters::bitbank::BitbankHandleError,
            >,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::generic_api_client::http::Method::GET,
                "/user/assets",
                None::<&()>,
                None::<()>,
            )
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
            .transport
            .request(
                &self.options,
                crypto_botters::generic_api_client::http::Method::GET,
                "/user/spot/order",
                Some(&serde_json::json!({"pair": pair, "order_id": order_id})),
                None::<()>,
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
            .transport
            .request(
                &self.options,
                crypto_botters::generic_api_client::http::Method::POST,
                "/user/spot/order",
                None::<&()>,
                Some(&serde_json::Value::Object(body_map)),
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
            .transport
            .request(
                &self.options,
                crypto_botters::generic_api_client::http::Method::GET,
                "/user/spot/trade_history",
                Some(&serde_json::Value::Object(request_body)),
                None::<()>,
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
            .transport
            .request(
                &self.options,
                crypto_botters::generic_api_client::http::Method::POST,
                "/user/spot/cancel_order",
                None::<&()>,
                Some(&serde_json::json!({"pair": pair, "order_id": order_id})),
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
            .transport
            .request(
                &self.options,
                crypto_botters::generic_api_client::http::Method::POST,
                "/user/spot/cancel_orders",
                None::<&()>,
                Some(&serde_json::json!({"pair": pair, "order_ids": order_ids})),
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
            .transport
            .request(
                &self.options,
                crypto_botters::generic_api_client::http::Method::GET,
                "/user/spot/active_orders",
                Some(&serde_json::Value::Object(request_body)),
                None::<()>,
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
            .transport
            .request(
                &self.options,
                crypto_botters::generic_api_client::http::Method::GET,
                "/spot/status",
                None::<&()>,
                None::<()>,
            )
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
            .transport
            .request(
                &self.options,
                crypto_botters::generic_api_client::http::Method::GET,
                "/user/subscribe",
                None::<&()>,
                None::<()>,
            )
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
                    if body.contains("\"orders\":[],\"assets\":[]") {
                        body = if String::from_utf8_lossy(&request).contains("/user/assets") {
                            r#"{"success":1,"data":{"assets":[]}}"#
                        } else {
                            r#"{"success":1,"data":{"orders":[]}}"#
                        };
                    }
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
        let mut config = crypto_botters::generic_api_client::http::RequestConfig::default();
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
        // Same response supports both concurrent endpoint requests.
        let body = r#"{"success":1,"data":{"orders":[],"assets":[]}}"#;
        // strict-validation requires endpoint-specific fields, so select by request path below instead.
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
        let text = String::from_utf8(m.0.lock().unwrap().clone()).unwrap();
        assert!(!text.contains("token-sentinel"));
        assert!(!text.contains("secret-value-sentinel"));
    }
}
