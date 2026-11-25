use std::time::Instant;

use crypto_botters::{
    bitbank::{BitbankHandleError, BitbankHttpUrl, BitbankOption, BitbankOptions},
    traits::HandlerOptions,
};

use crate::bitbank_structs::{
    BitbankApiResponse, BitbankCandlestickResponse, BitbankCircuitBreakInfo, BitbankDepthWhole,
    BitbankSpotPairsResponse, BitbankSpotStatusResponse, BitbankTickerResponse,
    BitbankTickersDatum, BitbankTransactionsData,
};

#[derive(Clone)]
pub struct BitbankPublicApiClient {
    options: BitbankOptions,
    spot_options: BitbankOptions,
    transport: crate::recording_http::Transport,
}

impl Default for BitbankPublicApiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl BitbankPublicApiClient {
    pub fn with_recorder(
        mut self,
        recorder: crate::execution_recording::ExecutionRecorder,
    ) -> Self {
        self.transport.recorder = Some(recorder);
        self
    }

    pub fn new() -> BitbankPublicApiClient {
        let mut client_options = BitbankOptions::default();
        let opt = BitbankOption::HttpUrl(BitbankHttpUrl::Public);
        client_options.update(opt);
        BitbankPublicApiClient {
            options: client_options,
            spot_options: BitbankOptions {
                // この URL は api.bitbank.cc/v1 を指す。認証の有無は http_auth で設定する。
                http_url: BitbankHttpUrl::Private,
                http_auth: false,
                ..BitbankOptions::default()
            },
            transport: Default::default(),
        }
    }

    /// API キーなしで全ペアの取引所ステータスと最小発注量を取得する。
    ///
    /// 仕様: <https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#get-exchange-status>
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty))]
    pub async fn get_status(
        &self,
    ) -> Result<BitbankSpotStatusResponse, Option<BitbankHandleError>> {
        self.get_spot("/spot/status", "get_status").await
    }

    /// API キーなしで全ペアの数量・価格精度、手数料、注文停止フラグ、信用取引設定を取得する。
    ///
    /// 仕様: <https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#get-all-pairs-info>
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty))]
    pub async fn get_pairs(&self) -> Result<BitbankSpotPairsResponse, Option<BitbankHandleError>> {
        self.get_spot("/spot/pairs", "get_pairs").await
    }

    async fn get_spot<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        api: &str,
    ) -> Result<T, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res = self
            .transport
            .request(
                &self.spot_options,
                crypto_botters::http::Method::GET,
                path,
                None::<&()>,
                None::<()>,
            )
            .await;

        let result = crate::response_handler::handle_response(api, res);
        tracing::debug!(
            event = "api_call_finished",
            api,
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
    }

    // https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#ticker
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty, pair = %crate::telemetry::safe(pair)))]
    pub async fn get_ticker(
        &self,
        pair: &str,
    ) -> Result<BitbankTickerResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, crypto_botters::bitbank::BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::GET,
                &format!("/{}/ticker", pair),
                Some(&serde_json::json!({"pair": pair})),
                None::<()>,
            )
            .await;

        let result = crate::response_handler::handle_response("get_ticker", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "get_ticker",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
    }

    // https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#tickers
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty))]
    pub async fn get_tickers(
        &self,
    ) -> Result<Vec<BitbankTickersDatum>, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::GET,
                "/tickers",
                None::<&()>,
                None::<()>,
            )
            .await;

        let result = crate::response_handler::handle_response("get_tickers", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "get_tickers",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
    }

    // https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#tickersjpy
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty))]
    pub async fn get_tickers_jpy(
        &self,
    ) -> Result<Vec<BitbankTickersDatum>, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::GET,
                "/tickers_jpy",
                None::<&()>,
                None::<()>,
            )
            .await;

        let result = crate::response_handler::handle_response("get_tickers_jpy", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "get_tickers_jpy",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
    }

    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty, pair = %crate::telemetry::safe(pair)))]
    pub async fn get_transactions(
        &self,
        pair: &str,
        yyyymmdd: Option<&str>,
    ) -> Result<BitbankTransactionsData, Option<BitbankHandleError>> {
        let start_time = Instant::now();

        let url = {
            if let Some(yyyymmdd) = yyyymmdd {
                format!("/{}/transactions/{}", pair, yyyymmdd)
            } else {
                format!("/{}/transactions", pair)
            }
        };

        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::GET,
                &url,
                None::<&()>,
                None::<()>,
            )
            .await;

        let result = crate::response_handler::handle_response("get_transactions", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "get_transactions",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
    }

    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty, pair = %crate::telemetry::safe(pair)))]
    pub async fn get_depth(
        &self,
        pair: &str,
    ) -> Result<BitbankDepthWhole, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::GET,
                &format!("/{}/depth", pair),
                Some(&serde_json::json!({"pair": pair})),
                None::<()>,
            )
            .await;

        let result = crate::response_handler::handle_response("get_depth", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "get_depth",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
    }

    // https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#candlestick
    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty, pair = %crate::telemetry::safe(pair)))]
    pub async fn get_candlestick(
        &self,
        pair: &str,
        candle_type: &str,
        yyyy: &str,
    ) -> Result<BitbankCandlestickResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let url = format!("/{}/candlestick/{}/{}", pair, candle_type, yyyy);
        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::GET,
                &url,
                None::<&()>,
                None::<()>,
            )
            .await;

        let result = crate::response_handler::handle_response("get_candlestick", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "get_candlestick",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
    }

    #[tracing::instrument(skip_all, fields(api_call_id = crate::telemetry::next_id(), venue_request_id = tracing::field::Empty, pair = %crate::telemetry::safe(pair)))]
    pub async fn get_circuit_break_info(
        &self,
        pair: &str,
    ) -> Result<BitbankCircuitBreakInfo, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::http::RequestError<&str, BitbankHandleError>,
        > = self
            .transport
            .request(
                &self.options,
                crypto_botters::http::Method::GET,
                &format!("/{}/circuit_break_info", pair),
                Some(&serde_json::json!({"pair": pair})),
                None::<()>,
            )
            .await;
        let result = crate::response_handler::handle_response("get_circuit_break_info", res);
        tracing::debug!(
            event = "api_call_finished",
            api = "get_circuit_break_info",
            success = result.is_ok(),
            elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0
        );
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crypto_botters::{http::RequestHandler, traits::HttpOption};
    use std::time::Duration;
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
    };

    #[tokio::test]
    async fn spot_endpoints_use_unauthenticated_rest_and_decode_responses() {
        let mut client = BitbankPublicApiClient::new();
        let handler = <BitbankOption as HttpOption<'_, BitbankApiResponse, &()>>::request_handler(
            client.spot_options.clone(),
        );
        let mut config = <_ as RequestHandler<&()>>::request_config(&handler);
        assert_eq!(config.url_prefix, "https://api.bitbank.cc/v1");

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // 実際の API と同じパスを保ち、送信先だけをテストサーバーに切り替える。
        config.url_prefix = format!("http://{addr}/v1");
        config.max_try = 1;
        client.spot_options.http_url = BitbankHttpUrl::None;
        client.spot_options.request_config = config;

        let server = async move {
            for (path, body) in [
                (
                    "/v1/spot/status",
                    r#"{"success":1,"data":{"statuses":[{"pair":"btc_jpy","status":"BUSY","min_amount":"0.001"}]}}"#,
                ),
                (
                    "/v1/spot/pairs",
                    include_str!("../fixtures/spot_pairs_response.json"),
                ),
            ] {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                assert_eq!(line.trim_end(), format!("GET {path} HTTP/1.1"));
                loop {
                    line.clear();
                    assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                    let (name, _) = line.split_once(':').unwrap();
                    let name = name.to_ascii_lowercase();
                    assert!(!name.starts_with("access-") && name != "authorization");
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            }
        };

        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(server, async {
                let status = &client.get_status().await.unwrap().statuses[0];
                assert_eq!(status.pair, "btc_jpy");
                assert_eq!(status.status, "BUSY");
                assert_eq!(status.min_amount, "0.001");

                let response = client.get_pairs().await.unwrap();
                let [margin, spot] = response.pairs.as_slice() else {
                    panic!("expected two pair settings");
                };
                assert_eq!(margin.name, "btc_jpy");
                assert_eq!(margin.quote_asset, "jpy");
                assert_eq!(margin.price_digits, 0);
                assert_eq!(margin.amount_digits, 4);
                assert!(margin.is_enabled && margin.stop_margin_short_order);
                assert_eq!(
                    margin.margin_open_taker_fee_rate_quote.as_deref(),
                    Some("0.001")
                );
                assert_eq!(margin.margin_current_company_until, Some(1796137199999));
                assert_eq!(margin.market_in_cb_allowance_rate.as_deref(), Some("0.2"));
                assert_eq!(spot.maker_fee_rate_quote, "-0.0002");
                assert_eq!(spot.margin_open_taker_fee_rate_quote, None);
                assert_eq!(spot.margin_current_company_until, None);
                assert_eq!(spot.market_in_cb_allowance_rate, None);
            });
        })
        .await
        .unwrap();
    }
}
