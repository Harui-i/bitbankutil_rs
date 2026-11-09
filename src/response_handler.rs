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
            let mut count = 0;
            let mut track = serde_path_to_error::Track::new();
            let de = serde_path_to_error::Deserializer::new(de, &mut track);
            let ret = serde_ignored::deserialize(de, |path| {
                count += 1;
                if ignored.len() < 8 {
                    ignored.push(crate::telemetry::safe(&path.to_string()));
                }
            });

            match ret {
                Ok(ret) => {
                    // api_responseにあったフィールドが、retには含まれていなかった場合警告する
                    if !ignored.is_empty() {
                        tracing::warn!(event = "response_unknown_fields", api = %crate::telemetry::safe(api_name), count, fields = ?ignored, truncated = count > ignored.len());
                    }
                    Ok(ret)
                }
                Err(err) => {
                    tracing::error!(event = "response_decode_failed", api = %crate::telemetry::safe(api_name), reason_code = crate::telemetry::decode_reason(&err), field = %crate::telemetry::decode_path(&err, &track.path().to_string()), action = "reject_response");

                    Err(None)
                }
            }
        }
        Err(err) => match err {
            crypto_botters::http::RequestError::SendRequest(error) => {
                tracing::warn!(event = "response_failed", api = %crate::telemetry::safe(api_name), stage = "send", timeout = error.is_timeout(), reason_code = if error.is_timeout() { "timeout" } else if error.is_connect() { "connect" } else if error.is_request() { "request" } else { "transport" }, action = "return_error");

                Err(None)
            }
            crypto_botters::http::RequestError::ReceiveResponse(error) => {
                tracing::warn!(event = "response_failed", api = %crate::telemetry::safe(api_name), stage = "receive", timeout = error.is_timeout(), reason_code = if error.is_timeout() { "timeout" } else if error.is_body() { "body" } else if error.is_decode() { "decode" } else { "transport" }, action = "return_error");
                Err(None)
            }
            crypto_botters::http::RequestError::BuildRequestError(_error) => {
                tracing::warn!(event = "response_failed", api = %crate::telemetry::safe(api_name), stage = "build", reason_code = "request_build_failed", action = "return_error");
                Err(None)
            }
            crypto_botters::http::RequestError::ResponseHandleError(error) => {
                let (reason_code, exchange_code, security_relevant) =
                    crate::error_diagnostics::api_ref(&error);
                tracing::warn!(event = "response_failed", api = %crate::telemetry::safe(api_name), stage = "exchange", reason_code, exchange_code, security_relevant, action = "return_error");
                // Classify by reference so the error remains available to the caller.
                Err(Some(error))
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bitbank_structs::BitbankApiResponse, telemetry::tests::Capture};
    use serde_json::json;
    #[derive(serde::Deserialize)]
    struct Data {
        #[allow(dead_code)]
        orders: Vec<Order>,
    }
    #[derive(serde::Deserialize)]
    struct Order {
        #[allow(dead_code)]
        amount: u64,
    }
    fn response(
        data: Value,
    ) -> Result<
        BitbankApiResponse,
        crypto_botters::http::RequestError<
            &'static str,
            crypto_botters::bitbank::BitbankHandleError,
        >,
    > {
        Ok(serde_json::from_value(json!({"success":1,"data":data})).unwrap())
    }
    #[test]
    fn response_failures_explain_nested_type_and_missing_field_without_values() {
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        assert!(
            handle_response::<Data>(
                "test",
                response(json!({"orders":[{"amount":"secret-sentinel"}]}))
            )
            .is_err()
        );
        assert!(handle_response::<Data>("test", response(json!({"orders":[{}]}))).is_err());
        let events = capture.events();
        assert_eq!(events[0]["fields"]["reason_code"], "invalid_type");
        assert_eq!(events[0]["fields"]["field"], "orders[0].amount");
        assert_eq!(events[1]["fields"]["reason_code"], "missing_field");
        assert_eq!(events[1]["fields"]["field"], "orders[0].amount");
        assert!(!capture.text().contains("secret-sentinel"));
    }
    #[test]
    fn unknown_field_names_are_bounded_and_values_are_not_logged() {
        #[derive(serde::Deserialize)]
        struct Empty {}
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.subscriber());
        let data: serde_json::Map<_, _> = (0..20)
            .map(|i| (format!("extra{i}\n"), json!("secret-sentinel")))
            .collect();
        handle_response::<Empty>("test", response(Value::Object(data))).unwrap();
        let events = capture.events();
        assert_eq!(events[0]["fields"]["count"], 20);
        assert_eq!(events[0]["fields"]["truncated"], true);
        assert!(!capture.text().contains("secret-sentinel"));
    }
}
