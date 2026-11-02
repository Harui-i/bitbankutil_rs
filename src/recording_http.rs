use crate::{
    bitbank_structs::BitbankApiResponse,
    execution_recording::{ExecutionRecorder, Measurement, current_context, scope},
};
use crypto_botters::{
    bitbank::{BitbankHandleError, BitbankOption, BitbankOptions},
    generic_api_client::http::*,
    traits::HttpOption,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Mutex;
#[derive(Clone, Default)]
pub(crate) struct Transport {
    pub recorder: Option<ExecutionRecorder>,
    http: crypto_botters::generic_api_client::http::Client,
}
struct Observed<H> {
    inner: H,
    status: Mutex<Option<u16>>,
    code: Mutex<Option<u64>>,
}
impl<B, H: RequestHandler<B>> RequestHandler<B> for Observed<H> {
    type Successful = H::Successful;
    type Unsuccessful = H::Unsuccessful;
    type BuildError = H::BuildError;
    fn request_config(&self) -> RequestConfig {
        let mut c = self.inner.request_config();
        c.max_try = 1;
        c
    }
    fn build_request(
        &self,
        b: RequestBuilder,
        body: &Option<B>,
        attempt: u8,
    ) -> Result<Request, Self::BuildError> {
        self.inner.build_request(b, body, attempt)
    }
    fn handle_response(
        &self,
        s: StatusCode,
        h: HeaderMap,
        b: Bytes,
    ) -> Result<Self::Successful, Self::Unsuccessful> {
        *self.status.lock().unwrap() = Some(s.as_u16());
        *self.code.lock().unwrap() = serde_json::from_slice::<Value>(&b)
            .ok()
            .and_then(|v| v["data"]["code"].as_u64());
        self.inner.handle_response(s, h, b)
    }
}
impl Transport {
    pub async fn request<Q: Serialize + ?Sized, B: Serialize>(
        &self,
        options: &BitbankOptions,
        method: Method,
        path: &str,
        query: Option<&Q>,
        body: Option<B>,
    ) -> Result<BitbankApiResponse, RequestError<&'static str, BitbankHandleError>> {
        let mut context = current_context();
        if let Some(r) = &self.recorder {
            if context.operation_id.is_none() {
                context.operation_id = Some(r.next_id());
            }
        }
        scope(
            context,
            self.request_inner(options, method, path, query, body),
        )
        .await
    }

    async fn request_inner<Q: Serialize + ?Sized, B: Serialize>(
        &self,
        options: &BitbankOptions,
        method: Method,
        path: &str,
        query: Option<&Q>,
        body: Option<B>,
    ) -> Result<BitbankApiResponse, RequestError<&'static str, BitbankHandleError>> {
        let handler = <BitbankOption as HttpOption<'_, BitbankApiResponse, &B>>::request_handler(
            options.clone(),
        );
        let config = <_ as RequestHandler<&B>>::request_config(&handler);
        assert!(config.max_try > 0);
        let observed = Observed {
            inner: handler,
            status: Mutex::new(None),
            code: Mutex::new(None),
        };
        let request_id = self.recorder.as_ref().map(|r| r.next_id());
        let request = self.recorder.as_ref().map(|_| {
            let value = body
                .as_ref()
                .and_then(|v| serde_json::to_value(v).ok())
                .or_else(|| query.and_then(|v| serde_json::to_value(v).ok()));
            safe_request(path, value.as_ref().unwrap_or(&Value::Null))
        });
        let total = Measurement::start();
        if let Some(r) = &self.recorder {
            r.record(
                "http_request_started",
                json!({
                    "request_id": request_id, "method": method.as_str(), "endpoint": path,
                    "request": request, "started_at_unix_us": total.started_at_unix_us(),
                }),
            );
        }
        for attempt in 1..=config.max_try {
            *observed.status.lock().unwrap() = None;
            *observed.code.lock().unwrap() = None;
            let measurement = Measurement::start();
            if let Some(r) = &self.recorder {
                r.record(
                    "http_started",
                    json!({
                        "request_id": request_id, "attempt": attempt,
                        "method": method.as_str(), "endpoint": path, "request": request,
                        "started_at_unix_us": measurement.started_at_unix_us(),
                    }),
                );
            }
            let result = self
                .http
                .request(method.clone(), path, query, body.as_ref(), &observed)
                .await;
            let timing = measurement.finish();
            let timeout = match &result {
                Err(RequestError::SendRequest(e) | RequestError::ReceiveResponse(e)) => {
                    e.is_timeout()
                }
                _ => false,
            };
            let will_retry =
                matches!(result, Err(RequestError::SendRequest(_))) && attempt < config.max_try;
            let stage = match &result {
                Ok(_) => "success",
                Err(RequestError::SendRequest(_)) => "send",
                Err(RequestError::ReceiveResponse(_)) => "receive",
                Err(RequestError::BuildRequestError(_)) => "build",
                Err(RequestError::ResponseHandleError(_)) => "response",
            };
            if let Some(r) = &self.recorder {
                r.record("http_finished", json!({
                    "request_id": request_id, "attempt": attempt,
                    "method": method.as_str(), "endpoint": path,
                    "success": result.is_ok(), "stage": stage,
                    "http_status": *observed.status.lock().unwrap(),
                    "exchange_code": *observed.code.lock().unwrap(),
                    "send_attempted": stage != "build", "timeout": timeout,
                    "will_retry": will_retry, "timing": timing,
                    "result": result.as_ref().ok().map(|response| safe_result(path, &response.data)),
                }));
            }
            if will_retry {
                tokio::time::sleep(config.retry_cooldown).await;
            } else {
                let timing = total.finish();
                if let Some(r) = &self.recorder {
                    r.record(
                        "http_request_finished",
                        json!({
                            "request_id": request_id, "method": method.as_str(), "endpoint": path,
                            "success": result.is_ok(), "attempts": attempt, "timing": timing,
                        }),
                    );
                }
                return result;
            }
        }
        unreachable!()
    }
}

fn safe_result(path: &str, data: &Value) -> Value {
    fn fields(v: &Value, keys: &[&str]) -> Value {
        let mut map = serde_json::Map::new();
        for key in keys {
            if let Some(value) = v.get(*key) {
                map.insert((*key).into(), value.clone());
            }
        }
        Value::Object(map)
    }
    let order_keys = [
        "order_id",
        "pair",
        "side",
        "type",
        "start_amount",
        "remaining_amount",
        "executed_amount",
        "price",
        "average_price",
        "ordered_at",
        "canceled_at",
        "status",
    ];
    match path {
        "/user/assets" => {
            json!({"assets":data["assets"].as_array().map(|items|items.iter().map(|v| fields(v,&["asset","free_amount","locked_amount","onhand_amount"])).collect::<Vec<_>>())})
        }
        "/user/spot/active_orders" | "/user/spot/cancel_orders" => {
            json!({"orders":data["orders"].as_array().map(|items|items.iter().map(|v|fields(v,&order_keys)).collect::<Vec<_>>())})
        }
        "/user/spot/order" | "/user/spot/cancel_order" => fields(data, &order_keys),
        _ => Value::Null,
    }
}

// 認証前の引数も丸ごと保存せず、注文操作に必要なフィールドだけを選ぶ。
fn safe_request(path: &str, value: &Value) -> Value {
    let keys: &[&str] = match path {
        "/user/spot/order"
        | "/user/spot/cancel_order"
        | "/user/spot/cancel_orders"
        | "/user/spot/active_orders" => &[
            "pair",
            "order_id",
            "order_ids",
            "amount",
            "price",
            "side",
            "type",
            "post_only",
            "trigger_price",
        ],
        _ => &[],
    };
    let mut result = serde_json::Map::new();
    for key in keys {
        if let Some(v) = value.get(*key) {
            result.insert((*key).into(), v.clone());
        }
    }
    Value::Object(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchange_times_stay_missing_or_null_and_only_allowlisted_fields_survive() {
        let result = safe_result(
            "/user/spot/cancel_orders",
            &json!({"orders":[
                {"order_id":1,"ordered_at":100,"canceled_at":null,"status":"FULLY_FILLED","secret":"sentinel"},
                {"order_id":2,"status":"CANCELED_UNFILLED"}
            ]}),
        );
        let first = &result["orders"][0];
        assert_eq!(first["ordered_at"], 100);
        assert!(first.get("canceled_at").unwrap().is_null());
        assert_eq!(first["status"], "FULLY_FILLED");
        let second = &result["orders"][1];
        assert!(second.get("ordered_at").is_none());
        assert!(second.get("canceled_at").is_none());
        assert!(!result.to_string().contains("sentinel"));
        let request = safe_request(
            "/user/spot/cancel_order",
            &json!({
                "order_id":1,"pair":"btc_jpy","ACCESS-KEY":"sentinel","signature":"sentinel"
            }),
        );
        assert_eq!(request, json!({"order_id":1,"pair":"btc_jpy"}));
        assert!(safe_result("/user/subscribe", &json!({"token":"sentinel"})).is_null());
    }
}
