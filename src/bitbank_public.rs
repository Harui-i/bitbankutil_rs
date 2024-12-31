use std::time::Instant;

use crypto_botters::{
    bitbank::{BitbankHandleError, BitbankHttpUrl, BitbankOption},
    Client,
};

use crate::bitbank_structs::{
    BitbankDepthWhole, BitbankTickerResponse, BitbankTickersDatum, BitbankTransactionDatum,
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
            serde_json::Value,
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

        match res {
            Ok(res_val) => {
                match serde_json::from_value::<BitbankTickerResponse>(res_val["data"].clone()) {
                    Ok(bbtr) => Ok(bbtr),
                    Err(err) => {
                        log::error!(
                            "failed to convert response value into BitbankTickerResponse.\
                            res_val: {:?}, Error: {:?}",
                            res_val.clone(),
                            err
                        );

                        Err(None)
                    }
                }
            }
            Err(err) => match err {
                crypto_botters::generic_api_client::http::RequestError::SendRequest(error) => {
                    log::error!("Send request error on get_ticker. error: {:?}", error);

                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ReceiveResponse(error) => {
                    log::error!("Receive response error on get_ticker. error: {:?}", error);
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::BuildRequestError(
                    error,
                ) => {
                    log::error!("Build request error on get_ticker. error: {:?}", error);
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ResponseHandleError(
                    error,
                ) => {
                    log::error!("Bitbank handle error on get_ticker. error : {:?}", error);
                    Err(Some(error))
                }
            },
        }
    }

    // https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#tickers
    pub async fn get_tickers(
        &self,
    ) -> Result<Vec<BitbankTickersDatum>, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            serde_json::Value,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get_no_query("/tickers", [BitbankOption::Default])
            .await;

        let duration = start_time.elapsed();
        log::debug!("get_tickers request took {:?}", duration);

        match res {
            Ok(res_val) => {
                match serde_json::from_value::<Vec<BitbankTickersDatum>>(res_val["data"].clone()) {
                    Ok(vecbbtr) => Ok(vecbbtr),
                    Err(err) => {
                        log::error!(
                            "failed to convert response value into Vec<BitbankTickersDatum>. \
                            res_val: {:?}, Error: {:?}",
                            res_val.clone(),
                            err
                        );

                        Err(None)
                    }
                }
            }
            Err(err) => match err {
                crypto_botters::generic_api_client::http::RequestError::SendRequest(error) => {
                    log::error!("Send request error on get_tickers. error: {:?}", error);

                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ReceiveResponse(error) => {
                    log::error!("Receive response error on get_tickers. error: {:?}", error);
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::BuildRequestError(
                    error,
                ) => {
                    log::error!("Build request error on get_tickers. error: {:?}", error);
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ResponseHandleError(
                    error,
                ) => {
                    log::error!("Bitbank handle error on get_tickers. error: {:?}", error);
                    Err(Some(error))
                }
            },
        }
    }

    //https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#tickersjpy
    pub async fn get_tickers_jpy(
        &self,
    ) -> Result<Vec<BitbankTickersDatum>, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            serde_json::Value,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get_no_query("/tickers_jpy", [BitbankOption::Default])
            .await;

        let duration = start_time.elapsed();
        log::debug!("get_tickers_jpy request took {:?}", duration);

        match res {
            Ok(res_val) => {
                match serde_json::from_value::<Vec<BitbankTickersDatum>>(res_val["data"].clone()) {
                    Ok(vecbbtr) => Ok(vecbbtr),
                    Err(err) => {
                        log::error!(
                            "failed to convert response value into Vec<BitbankTickersDatum>. \
                            res_val: {:?}, Error: {:?}",
                            res_val.clone(),
                            err
                        );

                        Err(None)
                    }
                }
            }
            Err(err) => match err {
                crypto_botters::generic_api_client::http::RequestError::SendRequest(error) => {
                    log::error!("Send request error on get_tickers_jpy. error: {:?}", error);

                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ReceiveResponse(error) => {
                    log::error!(
                        "Receive response error on get_tickers_jpy. error: {:?}",
                        error
                    );
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::BuildRequestError(
                    error,
                ) => {
                    log::error!("Build request error on get_tickers_jpy. error: {:?}", error);
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ResponseHandleError(
                    error,
                ) => {
                    log::error!(
                        "Bitbank handle error on get_tickers_jpy. error: {:?}",
                        error
                    );
                    Err(Some(error))
                }
            },
        }
    }

    pub async fn get_transactions(
        &self,
        pair: &str,
        yyyymmdd: Option<&str>,
    ) -> Result<Vec<BitbankTransactionDatum>, Option<BitbankHandleError>> {
        let start_time = Instant::now();

        let url = {
            if let Some(yyyymmdd) = yyyymmdd {
                format!("/{}/transactions/{}", pair, yyyymmdd)
            } else {
                format!("/{}/transactions", pair)
            }
        };

        let res: Result<
            serde_json::Value,
            crypto_botters::generic_api_client::http::RequestError<&str, BitbankHandleError>,
        > = self
            .client
            .get_no_query(&url, [BitbankOption::Default])
            .await;

        let duration = start_time.elapsed();
        log::debug!("get_transactions request took {:?}", duration);

        match res {
            Ok(res_val) => {
                match serde_json::from_value::<Vec<BitbankTransactionDatum>>(
                    res_val["data"]["transactions"].clone(),
                ) {
                    Ok(transactions) => Ok(transactions),
                    Err(err) => {
                        log::error!(
                            "failed to convert response value into Vec<BitbankTransactionDatum>. \
                            res_val: {:?}, Error: {:?}",
                            res_val.clone(),
                            err
                        );
                        Err(None)
                    }
                }
            }
            Err(err) => match err {
                crypto_botters::generic_api_client::http::RequestError::SendRequest(error) => {
                    log::error!("Send request error on get_transactions. error: {:?}", error);
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ReceiveResponse(error) => {
                    log::error!(
                        "Receive response error on get_transactions. error: {:?}",
                        error
                    );
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::BuildRequestError(
                    error,
                ) => {
                    log::error!(
                        "Build request error on get_transactions. error: {:?}",
                        error
                    );
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ResponseHandleError(
                    error,
                ) => {
                    log::error!(
                        "Bitbank handle error on get_transactions. error: {:?}",
                        error
                    );
                    Err(Some(error))
                }
            },
        }
    }

    pub async fn get_depth(
        &self,
        pair: &str,
    ) -> Result<BitbankDepthWhole, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            serde_json::Value,
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

        match res {
            Ok(res_val) => {
                match serde_json::from_value::<BitbankDepthWhole>(res_val["data"].clone()) {
                    Ok(bbdw) => Ok(bbdw),
                    Err(err) => {
                        log::error!(
                            "failed to convert response value into BitbankDepthWhole.\
                            res_val: {:?}, Error: {:?}",
                            res_val.clone(),
                            err
                        );

                        Err(None)
                    }
                }
            }
            Err(err) => match err {
                crypto_botters::generic_api_client::http::RequestError::SendRequest(error) => {
                    log::error!("Send request error on get_depth. error: {:?}", error);

                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ReceiveResponse(error) => {
                    log::error!("Receive response error on get_depth. error: {:?}", error);
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::BuildRequestError(
                    error,
                ) => {
                    log::error!("Build request error on get_depth. error: {:?}", error);
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ResponseHandleError(
                    error,
                ) => {
                    log::error!("Bitbank handle error on get_depth. error: {:?}", error);
                    Err(Some(error))
                }
            },
        }
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
    async fn test_get_ticker() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_ticker("eth_jpy").await;

        log::debug!("{:?}", res);
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_get_tickers() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_tickers().await;
        log::debug!("{:?}", res);
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_get_tickers_jpy() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_tickers_jpy().await;
        log::debug!("{:?}", res);
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_get_transactions() {
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
    async fn test_get_depth() {
        logging_init();
        let public_client = BitbankPublicApiClient::new();
        let res = public_client.get_depth("eth_jpy").await;

        let mut depth = BitbankDepth::new();
        depth.update_whole(res.unwrap());
        log::debug!("{}", depth);
    }
}
