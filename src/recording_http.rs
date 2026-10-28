use crate::{bitbank_structs::BitbankApiResponse, execution_recording::ExecutionRecorder};
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
        for attempt in 1..=config.max_try {
            *observed.status.lock().unwrap() = None;
            *observed.code.lock().unwrap() = None;
            if let Some(r) = &self.recorder {
                r.record("http_started",json!({"request_id":request_id,"attempt":attempt,"method":method.as_str(),"endpoint":path}));
            }
            let result = self
                .http
                .request(method.clone(), path, query, body.as_ref(), &observed)
                .await;
            let stage = match &result {
                Ok(_) => "success",
                Err(RequestError::SendRequest(_)) => "send",
                Err(RequestError::ReceiveResponse(_)) => "receive",
                Err(RequestError::BuildRequestError(_)) => "build",
                Err(RequestError::ResponseHandleError(_)) => "response",
            };
            if let Some(r) = &self.recorder {
                r.record("http_finished",json!({"request_id":request_id,"attempt":attempt,"method":method.as_str(),"endpoint":path,"success":result.is_ok(),"stage":stage,"http_status":*observed.status.lock().unwrap(),"exchange_code":*observed.code.lock().unwrap(),"sent":stage!="build", "result": result.as_ref().ok().map(|response| safe_result(path, &response.data))}));
            }
            if matches!(result, Err(RequestError::SendRequest(_))) && attempt < config.max_try {
                tokio::time::sleep(config.retry_cooldown).await;
            } else {
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
