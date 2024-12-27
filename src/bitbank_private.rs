use crate::bitbank_structs::{
    BitbankActiveOrdersResponse, BitbankAssetsData, BitbankCancelOrderResponse,
    BitbankCancelOrdersResponse, BitbankCreateOrderResponse, BitbankGetOrderResponse,
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
    pub fn new(api_key: String, api_secret: String, options: Option<Vec<BitbankOption>>) -> BitbankPrivateApiClient {
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

        // check whether authentication option is set.
        assert_eq!(client.default_options().http_auth, true);

        assert_eq!(client.default_options().http_url, BitbankHttpUrl::Private);

        assert_ne!(client.default_options().key, Some("".to_owned()));
        assert_ne!(client.default_options().secret, Some("".to_owned()));

        BitbankPrivateApiClient { client: client }
    }

    // you will use it in order to check your position
    pub async fn get_assets(&self) -> Result<BitbankAssetsData, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            serde_json::Value,
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

        match res {
            Ok(res) => match serde_json::from_value::<BitbankAssetsData>(res["data"].clone()) {
                Ok(bbad) => Ok(bbad),
                Err(err) => {
                    log::error!(
                        "failed to convert res into BitbankAssetData. res: {:?}, err: {:?}",
                        res,
                        err
                    );
                    Err(None)
                }
            },

            Err(x) => match x {
                crypto_botters::generic_api_client::http::RequestError::SendRequest(error) => {
                    log::error!("Send request error on get_assets: {:?}", error);
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ReceiveResponse(error) => {
                    log::error!("Receive response error on get_assets: {:?}", error);
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::BuildRequestError(
                    error,
                ) => {
                    log::error!("Build request error on get_assets: {:?}", error);

                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ResponseHandleError(
                    err,
                ) => {
                    log::error!("Response handle error on get_assets: {:?}", err);

                    Err(Some(err))
                }
            },
        }
    }

    // Fetch order information. https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-order-information
    pub async fn get_order(
        &self,
        pair: &str,
        order_id: u64,
    ) -> Result<BitbankGetOrderResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            serde_json::Value,
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

        match res {
            Ok(res) => {
                match serde_json::from_value::<BitbankGetOrderResponse>(res["data"].clone()) {
                    Ok(bbgor) => Ok(bbgor),
                    Err(e) => {
                        log::error!("failed to convert res into BitbankCreateOrderResponse. response: {:?}, Error: {}", res, e);
                        Err(None)
                    }
                }
            }
            Err(x) => match x {
                crypto_botters::generic_api_client::http::RequestError::ResponseHandleError(x) => {
                    log::error!("error on get_order: {:?}", x);
                    Err(Some(x))
                }
                crypto_botters::generic_api_client::http::RequestError::BuildRequestError(e) => {
                    println!("BuildRequestError : {}", e);

                    Err(None)
                }

                crypto_botters::generic_api_client::http::RequestError::ReceiveResponse(er) => {
                    println!("ReceiveResponse: {}", er);
                    Err(None)
                }

                crypto_botters::generic_api_client::http::RequestError::SendRequest(e) => {
                    println!("SendRequest: {}", e);
                    Err(None)
                }
            },
        }
    }

    // Create new order. https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#create-new-order
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
        // post_only is true => r#type is "limit"
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
            serde_json::Value,
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

        match res {
            Ok(res) => {
                match serde_json::from_value::<BitbankCreateOrderResponse>(res["data"].clone()) {
                    Ok(bbcor) => Ok(bbcor),
                    Err(e) => {
                        log::error!("failed to convert res into BitbankCreateOrderResponse. response: {:?}, Error: {}", res, e);
                        Err(None)
                    }
                }
            }
            Err(x) => match x {
                crypto_botters::generic_api_client::http::RequestError::ResponseHandleError(x) => {
                    log::error!("error on post_order: {:?}", x);
                    Err(Some(x))
                }
                crypto_botters::generic_api_client::http::RequestError::BuildRequestError(e) => {
                    println!("BuildRequestError : {}", e);

                    Err(None)
                }

                crypto_botters::generic_api_client::http::RequestError::ReceiveResponse(er) => {
                    println!("ReceiveResponse: {}", er);
                    Err(None)
                }

                crypto_botters::generic_api_client::http::RequestError::SendRequest(e) => {
                    println!("SendRequest: {}", e);
                    Err(None)
                }
            },
        }
    }

    // Cancel order. https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#cancel-order
    pub async fn post_cancel_order(
        &self,
        pair: &str,
        order_id: u64,
    ) -> Result<BitbankCancelOrderResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        let res: Result<
            serde_json::Value,
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

        match res {
            Ok(res_val) => {
                match serde_json::from_value::<BitbankCancelOrderResponse>(res_val["data"].clone())
                {
                    Ok(bbcor) => Ok(bbcor),

                    Err(err) => {
                        log::error!("failed to convert response value into BitbankCancelOrderResponse. res_val: {:?}, Error: {:?}", res_val.clone(), err);
                        Err(None)
                    }
                }
            }

            Err(err) => match err {
                crypto_botters::generic_api_client::http::RequestError::ResponseHandleError(
                    bhe,
                ) => {
                    log::error!("error on post_cancel_order: {:?}", bhe);
                    Err(Some(bhe))
                }
                crypto_botters::generic_api_client::http::RequestError::BuildRequestError(er) => {
                    println!("BuildRequestError : {}", er);

                    Err(None)
                }

                crypto_botters::generic_api_client::http::RequestError::ReceiveResponse(er) => {
                    println!("ReceiveResponse: {}", er);
                    Err(None)
                }

                crypto_botters::generic_api_client::http::RequestError::SendRequest(er) => {
                    println!("SendRequest: {}", er);
                    Err(None)
                }
            },
        }
    }

    // Cancel multiple orders. https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#cancel-multiple-orders
    pub async fn post_cancel_orders(
        &self,
        pair: &str,
        order_ids: Vec<u64>,
    ) -> Result<BitbankCancelOrdersResponse, Option<BitbankHandleError>> {
        let start_time = Instant::now();
        assert!(0 < order_ids.len() && order_ids.len() <= 30_usize);

        let res: Result<
            serde_json::Value,
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

        match res {
            Ok(res) => {
                match serde_json::from_value::<BitbankCancelOrdersResponse>(res["data"].clone()) {
                    Ok(bbcor) => Ok(bbcor),
                    Err(err) => {
                        log::error!(
                            "failed to convert res into BitbankCancelOrdersResponse: {:?}",
                            err
                        );
                        Err(None)
                    }
                }
            }

            Err(err) => match err {
                crypto_botters::generic_api_client::http::RequestError::SendRequest(error) => {
                    log::error!(
                        "Send request error on post_cancel_orders. error: {:?}",
                        error
                    );

                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ReceiveResponse(error) => {
                    log::error!("Receive response error on post_cancel_orders: {:?}", error);
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::BuildRequestError(
                    error,
                ) => {
                    log::error!("Build request error : {:?}", error);
                    Err(None)
                }
                crypto_botters::generic_api_client::http::RequestError::ResponseHandleError(
                    error,
                ) => {
                    log::error!("Bitbank handle error : {:?}", error);
                    Err(Some(error))
                }
            },
        }
    }

    // TODO
    // Fetch multiple orders. https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-multiple-orders
    pub fn post_orders_info(&self) {
        todo!();
    }

    // TODO
    // get exchange status. https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#get-exchange-status
    pub async fn get_status(&self) {
        todo!();
    }

    // Fetch active orders. https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-active-orders
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

        let request_body = request_body; // immutalize

        let res: Result<
            serde_json::Value,
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

        match res {
            Ok(res_val) => {
                match serde_json::from_value::<BitbankActiveOrdersResponse>(res_val["data"].clone())
                {
                    Ok(bbcor) => Ok(bbcor),

                    Err(err) => {
                        log::error!("failed to convert response value into BitbankActiveOrdersResponse. res_val: {:?}, Error: {:?}", res_val.clone(), err);
                        Err(None)
                    }
                }
            }

            Err(err) => match err {
                crypto_botters::generic_api_client::http::RequestError::ResponseHandleError(
                    bhe,
                ) => {
                    log::error!("error on post_order: {:?}", bhe);
                    Err(Some(bhe))
                }
                crypto_botters::generic_api_client::http::RequestError::BuildRequestError(er) => {
                    println!("BuildRequestError : {}", er);

                    Err(None)
                }

                crypto_botters::generic_api_client::http::RequestError::ReceiveResponse(er) => {
                    println!("ReceiveResponse: {}", er);
                    Err(None)
                }

                crypto_botters::generic_api_client::http::RequestError::SendRequest(er) => {
                    println!("SendRequest: {}", er);
                    Err(None)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::*;

    fn logging_init() {
        let _ = env_logger::builder()
            .format_timestamp_millis()
            .is_test(true)
            .try_init();
    }

    fn init_client() -> BitbankPrivateApiClient {
        let bitbank_key = env::var("BITBANK_API_KEY").unwrap();
        let bitbank_secret = env::var("BITBANK_API_SECRET").unwrap();

        BitbankPrivateApiClient::new(bitbank_key, bitbank_secret, None)
    }

    #[tokio::test]
    async fn test_get_assets() {
        logging_init();

        let bb_client = init_client();
        let assets = bb_client.get_assets().await;
        println!("{:?}", assets);
    }

    #[tokio::test]
    async fn test_post_order() {
        logging_init();

        log::debug!("dbg!");
        log::info!("info!");
        log::warn!("warrn!");

        let bb_client = init_client();
        let res = bb_client
            .post_order("btc_jpy", "1", Some("12"), "buy", "limit", Some(true), None)
            .await
            .expect("post_order returned Err");

        println!("{:?}", res);
    }

    #[tokio::test]
    async fn test_get_order() {
        logging_init();
        let bb_client = init_client();

        let post_order_res: BitbankCreateOrderResponse = bb_client
            .post_order("btc_jpy", "1", Some("12"), "buy", "limit", Some(true), None)
            .await
            .unwrap();
        log::info!("post order: {:?}", post_order_res);

        let get_order_res = bb_client
            .get_order("btc_jpy", post_order_res.order_id.as_u64().unwrap())
            .await
            .unwrap();

        log::info!("fetched order information: {:?}", get_order_res);
    }

    #[tokio::test]
    async fn test_post_cancel_order() {
        logging_init();
        let bb_client = init_client();

        let post_order_res: BitbankCreateOrderResponse = bb_client
            .post_order("btc_jpy", "1", Some("12"), "buy", "limit", Some(true), None)
            .await
            .unwrap();

        log::info!("order posted: {:?}", post_order_res);

        let cancel_order_res: BitbankCancelOrderResponse = bb_client
            .post_cancel_order("btc_jpy", post_order_res.order_id.as_u64().unwrap())
            .await
            .unwrap();

        log::info!("cancel order response : {:?}", cancel_order_res);
    }

    #[tokio::test]
    async fn test_get_active_orders() {
        logging_init();
        let bb_client = init_client();

        let active_orders_res: BitbankActiveOrdersResponse = bb_client
            .get_active_orders(Some("btc_jpy"), None, None, None, None, None)
            .await
            .unwrap();

        log::info!("active orders response: {:?}", active_orders_res);
    }

    #[tokio::test]
    async fn test_post_cancel_orders() {
        logging_init();
        let bb_client = init_client();

        let active_orders_res: BitbankActiveOrdersResponse = bb_client
            .get_active_orders(Some("btc_jpy"), None, None, None, None, None)
            .await
            .unwrap();
        log::info!("active orders response: {:?}", active_orders_res);

        let active_orders_id: Vec<u64> = active_orders_res
            .orders
            .iter()
            .map(|order| order.order_id.as_u64().unwrap())
            .collect();

        let cancel_orders_res: BitbankCancelOrdersResponse = bb_client
            .post_cancel_orders("btc_jpy", active_orders_id)
            .await
            .unwrap();

        log::info!("cancel orders respones: {:?}", cancel_orders_res);
    }

    // intentionaly exceed Rate Limit. if you want to run it, you should add `-- --ignored` option like: `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn test_exceed_rate_limit() {
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
