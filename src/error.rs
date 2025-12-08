use crypto_botters::bitbank::BitbankHandleError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BitbankUtilError {
    #[error("send request error on {api_name}: {error}")]
    SendRequest {
        api_name: &'static str,
        error: String,
    },

    #[error("receive response error on {api_name}: {error}")]
    ReceiveResponse {
        api_name: &'static str,
        error: String,
    },

    #[error("build request error on {api_name}: {error}")]
    BuildRequest {
        api_name: &'static str,
        error: String,
    },

    #[error("bitbank handle error on {api_name}: {error:?}")]
    ResponseHandle {
        api_name: &'static str,
        error: BitbankHandleError,
    },

    #[error("failed to deserialize response for {api_name}: {source}")]
    Deserialize {
        api_name: &'static str,
        #[source]
        source: serde_json::Error,
    },
}
