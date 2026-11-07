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
    response_clock: Mutex<Option<crate::clock_observation::ClockObservation>>,
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
        *self.response_clock.lock().unwrap() =
            Some(crate::clock_observation::ClockObservation::now());
        *self.status.lock().unwrap() = Some(s.as_u16());
        *self.code.lock().unwrap() = serde_json::from_slice::<Value>(&b)
            .ok()
            .and_then(|v| v["data"]["code"].as_u64());
        self.inner.handle_response(s, h, b)
    }
}
fn query_strings(value: Value) -> Value {
    match value {
        Value::Number(n) => Value::String(n.to_string()),
        Value::Array(values) => Value::Array(values.into_iter().map(query_strings).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(k, v)| (k, query_strings(v)))
                .collect(),
        ),
        other => other,
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
            response_clock: Mutex::new(None),
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
            r.record_lazy("http_request_started", || {
                json!({
                    "request_id": request_id, "method": method.as_str(), "endpoint": path,
                    "request": request, "started_at_unix_us": total.started_at_unix_us(),
                })
            });
        }
        // arbitrary_precisionのNumberをURLエンコーダへ直接渡さず、
        // クエリの数値だけを正確な十進文字列にする（JSON本文は変更しない）。
        let query = query
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| RequestError::BuildRequestError("invalid query"))?
            .map(query_strings);
        for attempt in 1..=config.max_try {
            *observed.response_clock.lock().unwrap() = None;
            *observed.status.lock().unwrap() = None;
            *observed.code.lock().unwrap() = None;
            let request_clock = crate::clock_observation::ClockObservation::now();
            let measurement = Measurement::start();
            if let Some(r) = &self.recorder {
                r.record_lazy("http_started", || {
                    json!({
                        "request_id": request_id, "attempt": attempt,
                        "method": method.as_str(), "endpoint": path, "request": request,
                        "started_at_unix_us": measurement.started_at_unix_us(),
                    })
                });
            }
            let result = self
                .http
                .request(
                    method.clone(),
                    path,
                    query.as_ref(),
                    body.as_ref(),
                    &observed,
                )
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
                r.record_lazy("http_finished", || json!({
                    "request_id": request_id, "attempt": attempt,
                    "method": method.as_str(), "endpoint": path,
                    "success": result.is_ok(), "stage": stage,
                    "http_status": *observed.status.lock().unwrap(),
                    "exchange_code": *observed.code.lock().unwrap(),
                    "send_attempted": stage != "build", "timeout": timeout,
                    "will_retry": will_retry, "timing": timing,
                    "request_clock": request_clock,
                    "response_clock": *observed.response_clock.lock().unwrap(),
                    "result": result.as_ref().ok().map(|response| safe_result(path, &response.data)),
                }));
            }
            if will_retry {
                tokio::time::sleep(config.retry_cooldown).await;
            } else {
                let timing = total.finish();
                if let Some(r) = &self.recorder {
                    r.record_lazy("http_request_finished", || {
                        json!({
                            "request_id": request_id, "method": method.as_str(), "endpoint": path,
                            "success": result.is_ok(), "attempts": attempt, "timing": timing,
                            "timeout": timeout, "stage": stage,
                        })
                    });
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
    #[test]
    fn arbitrary_precision_query_numbers_remain_exact_strings() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"order_id":9007199254740993,"amount":90000000000.00000001}"#)
                .unwrap();
        let query = super::query_strings(value);
        assert_eq!(query["order_id"], "9007199254740993");
        assert_eq!(query["amount"], "90000000000.00000001");
    }

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

#[cfg(test)]
mod clock_tests {
    use super::*;
    use crate::clock_observation::{ClockObservation, private_offset};
    use crate::execution_recording::{JsonlSink, read_records, tests::Memory};
    use crypto_botters::bitbank::BitbankHttpUrl;
    use std::io::{Read, Write};
    #[tokio::test]
    async fn local_http_records_causal_order_and_cancel_timestamps() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for cancel in [false, true] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut b = [0; 4096];
                let mut request = Vec::new();
                loop {
                    let n = stream.read(&mut b).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&b[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let now = ClockObservation::now().unix_us / 1000;
                let data = if cancel {
                    json!({"order_id":42,"ordered_at":1,"canceled_at":now,"status":"CANCELED_UNFILLED"})
                } else {
                    json!({"order_id":42,"ordered_at":now,"status":"UNFILLED"})
                };
                let body = json!({"success":1,"data":data}).to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let m = Memory::default();
        let recorder = ExecutionRecorder::new("http-clock", json!({}), JsonlSink(m.clone()));
        let transport = Transport {
            recorder: Some(recorder.clone()),
            ..Default::default()
        };
        let mut options = BitbankOptions::default();
        options.http_url = BitbankHttpUrl::None;
        options.request_config.url_prefix = format!("http://{addr}");
        options.request_config.max_try = 1;
        for path in ["/user/spot/order", "/user/spot/cancel_order"] {
            transport
                .request(
                    &options,
                    Method::POST,
                    path,
                    None::<&()>,
                    Some(json!({"order_id":42})),
                )
                .await
                .unwrap();
        }
        server.join().unwrap();
        recorder.finish().unwrap();
        let records = read_records(std::io::Cursor::new(m.0.lock().unwrap().clone())).unwrap();
        let attempts: Vec<_> = records
            .iter()
            .filter(|r| r.kind == "http_finished")
            .collect();
        assert_eq!(attempts.len(), 2);
        for r in attempts {
            let start: ClockObservation =
                serde_json::from_value(r.data["request_clock"].clone()).unwrap();
            let response: ClockObservation =
                serde_json::from_value(r.data["response_clock"].clone()).unwrap();
            assert!(response.monotonic_us >= start.monotonic_us);
            assert!(
                response.unix_us as u64
                    <= r.data["timing"]["completed_at_unix_us"].as_u64().unwrap()
            );
            let interval = private_offset(&records, r, 1000).unwrap();
            assert!(interval.lower_us <= 0 && interval.upper_us >= 0);
        }
    }
    #[test]
    fn response_clock_precedes_inner_handler() {
        struct Slow;
        impl RequestHandler<()> for Slow {
            type Successful = ();
            type Unsuccessful = ();
            type BuildError = ();
            fn build_request(
                &self,
                b: RequestBuilder,
                _: &Option<()>,
                _: u8,
            ) -> Result<Request, ()> {
                b.build().map_err(|_| ())
            }
            fn handle_response(&self, _: StatusCode, _: HeaderMap, _: Bytes) -> Result<(), ()> {
                std::thread::sleep(std::time::Duration::from_millis(10));
                Ok(())
            }
        }
        let handler = Observed {
            inner: Slow,
            response_clock: Mutex::new(None),
            status: Mutex::new(None),
            code: Mutex::new(None),
        };
        handler
            .handle_response(StatusCode::OK, HeaderMap::new(), Bytes::from_static(b"{}"))
            .unwrap();
        assert!(
            ClockObservation::now().monotonic_us
                - handler.response_clock.lock().unwrap().unwrap().monotonic_us
                >= 10000
        );
    }
}
