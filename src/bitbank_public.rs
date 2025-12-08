use std::time::Instant;

use crate::error::BitbankUtilError;
use crypto_botters::{
    bitbank::{BitbankHandleError, BitbankHttpUrl, BitbankOption},
    Client,
};

use crate::bitbank_structs::{
    BitbankApiResponse, BitbankCircuitBreakInfo, BitbankDepthWhole, BitbankTickerResponse,
    BitbankTransactionsData,
};

#[derive(Clone)]
pub struct BitbankPublicApiClient {
    client: Client,
}

impl BitbankPublicApiClient {
    pub fn new() -> BitbankPublicApiClient {
        let mut client = Client::new();
        let opt = BitbankOption::HttpUrl(BitbankHttpUrl::Public);
        client.update_default_option(opt);
        BitbankPublicApiClient { client }
    }

    // https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#ticker
    pub async fn get_ticker(&self, pair: &str) -> Result<BitbankTickerResponse, BitbankUtilError> {
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
    pub async fn get_tickers(&self) -> Result<Vec<BitbankTickerResponse>, BitbankUtilError> {
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

    //https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#tickersjpy
    pub async fn get_tickers_jpy(&self) -> Result<Vec<BitbankTickerResponse>, BitbankUtilError> {
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
    ) -> Result<BitbankTransactionsData, BitbankUtilError> {
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

    pub async fn get_depth(&self, pair: &str) -> Result<BitbankDepthWhole, BitbankUtilError> {
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

    pub async fn get_circuit_break_info(
        &self,
        pair: &str,
    ) -> Result<BitbankCircuitBreakInfo, BitbankUtilError> {
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

#[cfg(test)]
mod tests {
    use crate::bitbank_structs::BitbankDepth;
    use crate::error::BitbankUtilError;

    use super::*;

    fn logging_init() {
        let _ = env_logger::builder()
            .format_timestamp_millis()
            .is_test(true)
            .try_init();
    }

    /// If the request fails due to network conditions in the CI environment,
    /// skip the test rather than failing hard. Only request/response transport
    /// errors are treated as skippable; all other errors will still fail.
    fn assume_network_available<T>(res: Result<T, BitbankUtilError>) -> Option<T> {
        match res {
            Ok(value) => Some(value),
            Err(BitbankUtilError::SendRequest { api_name, error }) => {
                log::warn!(
                    "skipping test because network is unavailable ({}: {})",
                    api_name,
                    error
                );
                None
            }
            Err(BitbankUtilError::ReceiveResponse { api_name, error }) => {
                log::warn!(
                    "skipping test because response could not be received ({}: {})",
                    api_name,
                    error
                );
                None
            }
            Err(err) => panic!("unexpected public API error: {err:?}"),
        }
    }

    #[tokio::test]
    async fn test_public_get_ticker() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_ticker("eth_jpy").await;
        if assume_network_available(res).is_none() {
            return;
        }
    }

    #[tokio::test]
    async fn test_public_get_tickers() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_tickers().await;
        if assume_network_available(res).is_none() {
            return;
        }
    }

    #[tokio::test]
    async fn test_public_get_tickers_jpy() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_tickers_jpy().await;
        if assume_network_available(res).is_none() {
            return;
        }
    }

    #[tokio::test]
    async fn test_public_get_transactions() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();

        let res_without_date = public_client.get_transactions("btc_jpy", None).await;
        let Some(_) = assume_network_available(res_without_date) else {
            return;
        };

        let res_with_date = public_client
            .get_transactions("btc_jpy", Some("20241127"))
            .await;
        if assume_network_available(res_with_date).is_none() {
            return;
        }
    }

    #[tokio::test]
    async fn test_public_get_depth() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_depth("eth_jpy").await;
        let Some(depth_whole) = assume_network_available(res) else {
            return;
        };

        let mut depth = BitbankDepth::new();
        depth.update_whole(depth_whole);
        log::debug!("{}", depth);
    }

    #[tokio::test]
    async fn test_public_get_circuit_break_info() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_circuit_break_info("eth_jpy").await;
        let Some(circuit_break_info) = assume_network_available(res) else {
            return;
        };

        log::debug!("Circuit Break Info: {:?}", circuit_break_info);
    }
}
