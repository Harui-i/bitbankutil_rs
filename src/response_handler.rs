pub fn handle_response<T: serde::de::DeserializeOwned>(
    api_name: &str,
    res: Result<
        crate::bitbank_structs::BitbankApiResponse,
        crypto_botters::generic_api_client::http::RequestError<
            &str,
            crypto_botters::bitbank::BitbankHandleError,
        >,
    >,
) -> Result<T, Option<crypto_botters::bitbank::BitbankHandleError>> {
    match res {
        Ok(api_response) => {
            // デシリアライズ
            match serde_json::from_value::<T>(api_response.data.clone()) {
                Ok(ret) => Ok(ret),
                Err(err) => {
                    log::error!(
                        "failed to convert api_response into certain type.\
                            api_response: {:?}, Error: {:?}",
                        api_response.clone(),
                        err
                    );

                    Err(None)
                }
            }
        }
        Err(err) => match err {
            crypto_botters::generic_api_client::http::RequestError::SendRequest(error) => {
                log::error!("Send request error on {}. error: {:?}", api_name, error);

                Err(None)
            }
            crypto_botters::generic_api_client::http::RequestError::ReceiveResponse(error) => {
                log::error!("Receive response error on {}. error: {:?}", api_name, error);
                Err(None)
            }
            crypto_botters::generic_api_client::http::RequestError::BuildRequestError(error) => {
                log::error!("Build request error on {}. error: {:?}", api_name, error);
                Err(None)
            }
            crypto_botters::generic_api_client::http::RequestError::ResponseHandleError(error) => {
                log::error!("Bitbank handle error on {}. error : {:?}", api_name, error);
                Err(Some(error))
            }
        },
    }
}
