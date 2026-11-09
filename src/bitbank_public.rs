use std::time::Instant;

use crypto_botters::{
    bitbank::{BitbankHandleError, BitbankHttpUrl, BitbankOption, BitbankOptions},
    traits::HandlerOptions,
};

use crate::bitbank_structs::{
    BitbankApiResponse, BitbankCandlestickResponse, BitbankCircuitBreakInfo, BitbankDepthWhole,
    BitbankTickerResponse, BitbankTickersDatum, BitbankTransactionsData,
};

#[derive(Clone)]
pub struct BitbankPublicApiClient {
    options: BitbankOptions,
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
            transport: Default::default(),
        }
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
