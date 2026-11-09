use serde::de::IntoDeserializer;
use serde_json::Value;

pub fn handle_response<T: serde::de::DeserializeOwned>(
    api_name: &str,
    res: Result<
        crate::bitbank_structs::BitbankApiResponse,
        crypto_botters::http::RequestError<&str, crypto_botters::bitbank::BitbankHandleError>,
    >,
) -> Result<T, Option<crypto_botters::bitbank::BitbankHandleError>> {
    match res {
        Ok(api_response) => {
            // デシリアライズ
            let de: Value = api_response.data.clone().into_deserializer();
            let mut ignored = Vec::new();
            let ret = serde_ignored::deserialize(de, |path| {
                ignored.push(path.to_string());
            });

            match ret {
                Ok(ret) => {
                    // api_responseにあったフィールドが、retには含まれていなかった場合警告する
                    if !ignored.is_empty() {
                        tracing::warn!(event = "response_unknown_fields", api = %crate::telemetry::safe(api_name), count = ignored.len());
                    }
                    Ok(ret)
                }
                Err(err) => {
                    tracing::error!(event = "response_decode_failed", api = %crate::telemetry::safe(api_name), line = err.line(), column = err.column());

                    Err(None)
                }
            }
        }
        Err(err) => match err {
            crypto_botters::http::RequestError::SendRequest(error) => {
                tracing::warn!(event = "response_failed", api = %crate::telemetry::safe(api_name), stage = "send", timeout = error.is_timeout());

                Err(None)
            }
            crypto_botters::http::RequestError::ReceiveResponse(error) => {
                tracing::warn!(event = "response_failed", api = %crate::telemetry::safe(api_name), stage = "receive", timeout = error.is_timeout());
                Err(None)
            }
            crypto_botters::http::RequestError::BuildRequestError(_error) => {
                tracing::warn!(event = "response_failed", api = %crate::telemetry::safe(api_name), stage = "build");
                Err(None)
            }
            crypto_botters::http::RequestError::ResponseHandleError(error) => {
                tracing::warn!(event = "response_failed", api = %crate::telemetry::safe(api_name), stage = "exchange");
                Err(Some(error))
            }
        },
    }
}
