use std::time::Instant;

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
    ) -> Result<Vec<BitbankTickerResponse>, Option<BitbankHandleError>> {
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
    pub async fn get_tickers_jpy(
        &self,
    ) -> Result<Vec<BitbankTickerResponse>, Option<BitbankHandleError>> {
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

#[cfg(test)]
mod tests {
    use crate::bitbank_structs::BitbankDepth;

    use super::*;

    fn logging_init() {
        let _ = env_logger::builder()
            .format_timestamp_millis()
            .is_test(true)
            .try_init();
    }

    #[tokio::test]
    async fn test_public_get_ticker() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_ticker("eth_jpy").await;

        log::debug!("{:?}", res);
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_public_get_tickers() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_tickers().await;
        log::debug!("{:?}", res);
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_public_get_tickers_jpy() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_tickers_jpy().await;
        log::debug!("{:?}", res);
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_public_get_transactions() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();

        let res_without_date = public_client.get_transactions("btc_jpy", None).await;
        log::debug!("{:?}", res_without_date);
        assert!(res_without_date.is_ok());

        let res_with_date = public_client
            .get_transactions("btc_jpy", Some("20241127"))
            .await;
        log::debug!("{:?}", res_with_date);
        assert!(res_with_date.is_ok());
    }

    #[tokio::test]
    async fn test_public_get_depth() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_depth("eth_jpy").await;

        let mut depth = BitbankDepth::new();
        depth.update_whole(res.unwrap());
        log::debug!("{}", depth);
    }

    #[tokio::test]
    async fn test_public_get_circuit_break_info() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_circuit_break_info("eth_jpy").await;
        log::debug!("{:?}", res);
        assert!(res.is_ok());
        let circuit_break_info = res.unwrap();
        log::debug!("Circuit Break Info: {:?}", circuit_break_info);
    }
}
