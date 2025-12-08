use crate::error::BitbankUtilError;
use crypto_botters::{bitbank::BitbankHandleError, generic_api_client::http::RequestError};
use serde::de::DeserializeOwned;

pub fn handle_response<T: DeserializeOwned>(
    api_name: &'static str,
    res: Result<crate::bitbank_structs::BitbankApiResponse, RequestError<&str, BitbankHandleError>>,
) -> Result<T, BitbankUtilError> {
    match res {
        Ok(api_response) => serde_json::from_value::<T>(api_response.data.clone())
            .map_err(|source| BitbankUtilError::Deserialize { api_name, source }),
        Err(RequestError::SendRequest(error)) => Err(BitbankUtilError::SendRequest {
            api_name,
            error: error.to_string(),
        }),
        Err(RequestError::ReceiveResponse(error)) => Err(BitbankUtilError::ReceiveResponse {
            api_name,
            error: error.to_string(),
        }),
        Err(RequestError::BuildRequestError(error)) => Err(BitbankUtilError::BuildRequest {
            api_name,
            error: error.to_string(),
        }),
        Err(RequestError::ResponseHandleError(error)) => {
            Err(BitbankUtilError::ResponseHandle { api_name, error })
        }
    }
}
