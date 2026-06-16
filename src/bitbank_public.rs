use std::time::Instant;

use crypto_botters::{
    bitbank::{BitbankHandleError, BitbankHttpUrl, BitbankOption},
    Client,
};

use crate::bitbank_structs::{
    BitbankApiResponse, BitbankCandlestickResponse, BitbankCircuitBreakInfo, BitbankDepthWhole,
    BitbankTickerResponse, BitbankTickersDatum, BitbankTransactionsData,
};

#[derive(Clone)]
pub struct BitbankPublicApiClient {
    client: Client,
}

impl Default for BitbankPublicApiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl BitbankPublicApiClient {
    pub fn new() -> BitbankPublicApiClient {
        let mut client = Client::new();
        let opt = BitbankOption::HttpUrl(BitbankHttpUrl::Public);
        client.update_default_option(opt);
        BitbankPublicApiClient { client }
    }

    // https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#ticker
    pub async fn get_ticker(
        &self,
        pair: &str,
    ) -> Result<BitbankTickerResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<
                &str,
                crypto_botters::bitbank::BitbankHandleError,
            >,
        > = self
            .client
            .get(
                &format!("/{}/ticker", pair),
                Some(&serde_json::json!({"pair": pair})),
                [BitbankOption::Default],
            )
            .await;

        let duration = start_time.elapsed();
        log::debug!("get_ticker request took {:?}", duration);

        crate::response_handler::handle_response("get_ticker", res)
    }

    // https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#tickers
    pub async fn get_tickers(
        &self,
    ) -> Result<Vec<BitbankTickersDatum>, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get_no_query("/tickers", [BitbankOption::Default])
            .await;

        let duration = start_time.elapsed();
        log::debug!("get_tickers request took {:?}", duration);

        crate::response_handler::handle_response("get_tickers", res)
    }

    // https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#tickersjpy
    pub async fn get_tickers_jpy(
        &self,
    ) -> Result<Vec<BitbankTickersDatum>, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get_no_query("/tickers_jpy", [BitbankOption::Default])
            .await;

        let duration = start_time.elapsed();
        log::debug!("get_tickers_jpy request took {:?}", duration);

        crate::response_handler::handle_response("get_tickers_jpy", res)
    }

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
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get_no_query(&url, [BitbankOption::Default])
            .await;

        let duration = start_time.elapsed();
        log::debug!("get_transactions request took {:?}", duration);

        crate::response_handler::handle_response("get_transactions", res)
    }

    pub async fn get_depth(
        &self,
        pair: &str,
    ) -> Result<BitbankDepthWhole, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get(
                &format!("/{}/depth", pair),
                Some(&serde_json::json!({"pair": pair})),
                [BitbankOption::Default],
            )
            .await;

        let duration = start_time.elapsed();
        log::debug!("get_depth request took {:?}", duration);

        crate::response_handler::handle_response("get_depth", res)
    }

    // https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#candlestick
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
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get_no_query(&url, [BitbankOption::Default])
            .await;

        let duration = start_time.elapsed();
        log::debug!("get_candlestick request took {:?}", duration);

        crate::response_handler::handle_response("get_candlestick", res)
    }

    pub async fn get_circuit_break_info(
        &self,
        pair: &str,
    ) -> Result<BitbankCircuitBreakInfo, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            BitbankApiResponse,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get(
                &format!("/{}/circuit_break_info", pair),
                Some(&serde_json::json!({"pair": pair})),
                [BitbankOption::Default],
            )
            .await;
        let duration = start_time.elapsed();
        log::debug!("get_circuit_break_info request took {:?}", duration);

        crate::response_handler::handle_response("get_circuit_break_info", res)
    }
}
