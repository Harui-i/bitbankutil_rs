#![doc = include_str!("../docs/clock-observation.md")]
use crate::execution_recording::{ExecutionRecord, ExecutionRecorder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// 実UTCとプロセス共通起点の単調時計。別プロセスのmonotonic_usは比較できない。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClockObservation {
    pub unix_us: i64,
    pub monotonic_us: i64,
}
impl ClockObservation {
    pub fn now() -> Self {
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        let origin = ORIGIN.get_or_init(Instant::now);
        let unix_us = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_micros() as i64,
            Err(e) => -(e.duration().as_micros() as i64),
        };
        Self {
            unix_us,
            monotonic_us: origin.elapsed().as_micros() as i64,
        }
    }
}
/// 取引所が付与した時刻とその出所。時刻の意味はfieldで区別する。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeTimeObservation {
    pub field: String,
    pub unix_ms: i64,
    pub transaction_id: Option<Value>,
    pub sequence_id: Option<Value>,
}
/// Δ = 取引所時計 − ローカル時計（マイクロ秒）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct OffsetInterval {
    pub lower_us: i128,
    pub upper_us: i128,
}
impl OffsetInterval {
    pub fn intersection(self, other: Self) -> Option<Self> {
        let interval = Self {
            lower_us: self.lower_us.max(other.lower_us),
            upper_us: self.upper_us.min(other.upper_us),
        };
        (interval.lower_us <= interval.upper_us).then_some(interval)
    }
    /// WSの見かけの時刻差に時計差を加えた到着遅延区間。負値も隠さない。
    pub fn arrival_delay(self, received_unix_us: i64, exchange_unix_ms: i64) -> Self {
        let apparent = i128::from(received_unix_us) - i128::from(exchange_unix_ms) * 1000;
        Self {
            lower_us: apparent + self.lower_us,
            upper_us: apparent + self.upper_us,
        }
    }
}
/// 要求で生成された時刻から区間を計算する。UTCの飛び・逆順は拒否する。
/// 量子化への余裕として両側1ms広げる。取引所時刻の正確さは保証しない。
pub fn offset_interval(
    start: ClockObservation,
    response: ClockObservation,
    exchange_ms: i64,
    tolerance_us: u64,
) -> Option<OffsetInterval> {
    let wall = i128::from(response.unix_us) - i128::from(start.unix_us);
    let mono = i128::from(response.monotonic_us) - i128::from(start.monotonic_us);
    if wall < 0 || mono < 0 || (wall - mono).abs() > i128::from(tolerance_us) {
        return None;
    }
    let exchange = i128::from(exchange_ms) * 1000;
    Some(OffsetInterval {
        lower_us: exchange - i128::from(response.unix_us) - 1000,
        upper_us: exchange - i128::from(start.unix_us) + 1000,
    })
}
/// 成功した単一試行のPOST発注・取消だけを推定対象にする。
/// 全レコードを使い、後の再試行や同じrequest_idの重複がないことも確認する。
pub fn private_offset(
    records: &[ExecutionRecord],
    record: &ExecutionRecord,
    tolerance_us: u64,
) -> Option<OffsetInterval> {
    let d = &record.data;
    if record.kind != "http_finished"
        || d["method"] != "POST"
        || d["success"] != true
        || d["attempt"] != 1
        || d["will_retry"] != false
    {
        return None;
    }
    let request = d["request_id"].as_u64()?;
    let related: Vec<_> = records
        .iter()
        .filter(|r| {
            r.run_id == record.run_id
                && r.kind == "http_finished"
                && r.data["request_id"].as_u64() == Some(request)
        })
        .collect();
    if related.len() != 1 {
        return None;
    }
    let completed = records
        .iter()
        .filter(|r| {
            r.run_id == record.run_id
                && r.kind == "http_request_finished"
                && r.data["request_id"].as_u64() == Some(request)
        })
        .collect::<Vec<_>>();
    if completed.len() != 1
        || completed[0].data["success"] != true
        || completed[0].data["attempts"] != 1
    {
        return None;
    }
    let field = match d["endpoint"].as_str()? {
        "/user/spot/order" => "ordered_at",
        "/user/spot/cancel_order"
            if matches!(
                d["result"]["status"].as_str(),
                Some("CANCELED_UNFILLED" | "CANCELED_PARTIALLY_FILLED")
            ) =>
        {
            "canceled_at"
        }
        _ => return None,
    };
    offset_interval(
        serde_json::from_value(d["request_clock"].clone()).ok()?,
        serde_json::from_value(d["response_clock"].clone()).ok()?,
        d["result"][field].as_i64()?,
        tolerance_us,
    )
}

pub(crate) struct WsObserver {
    recorder: ExecutionRecorder,
    stream_id: u64,
    sequence: AtomicU64,
}
impl WsObserver {
    pub(crate) fn new(recorder: ExecutionRecorder) -> Self {
        let stream_id = recorder.next_id();
        Self {
            recorder,
            stream_id,
            sequence: AtomicU64::new(0),
        }
    }
    pub(crate) fn observe(&self, raw: &Value, received: ClockObservation, valid: bool) {
        if !self.recorder.enabled("ws_clock_observed") {
            return;
        }
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let id = self.recorder.next_id();
        let room = raw.get(1).and_then(|v| v["room_name"].as_str());
        let data = &raw[1]["message"]["data"];
        let prefixes = [
            "transactions_",
            "depth_whole_",
            "depth_diff_",
            "ticker_",
            "circuit_break_info_",
        ];
        let prefix = room.and_then(|r| prefixes.into_iter().find(|p| r.starts_with(p)));
        let mut times = Vec::new();
        let mut add = |v: &Value, field: &str| {
            if let Some(unix_ms) = v[field].as_i64() {
                times.push(ExchangeTimeObservation {
                    field: field.into(),
                    unix_ms,
                    transaction_id: v.get("transaction_id").cloned(),
                    sequence_id: v.get("sequenceId").or_else(|| v.get("s")).cloned(),
                });
            }
        };
        if prefix == Some("transactions_") {
            if let Some(trades) = data["transactions"].as_array() {
                for trade in trades {
                    add(trade, "executed_at");
                }
            }
        } else if prefix == Some("depth_diff_") {
            add(data, "t");
        } else if prefix.is_some() {
            add(data, "timestamp");
        }
        self.recorder.record_lazy("ws_clock_observed", || {
            json!({
                "observation_id": id, "stream_id": self.stream_id, "receive_sequence": sequence,
                "received_clock": received, "channel": room,
                "pair": room.zip(prefix).map(|(r,p)| &r[p.len()..]),
                "valid": valid, "exchange_times": times,
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_recording::{JsonlSink, read_records, tests::Memory};
    fn clock(unix_us: i64, monotonic_us: i64) -> ClockObservation {
        ClockObservation {
            unix_us,
            monotonic_us,
        }
    }
    #[test]
    fn signed_bounds_quantization_and_clock_steps() {
        let a = offset_interval(clock(0, 0), clock(80000, 80000), 50, 1000).unwrap();
        assert_eq!(
            a,
            OffsetInterval {
                lower_us: -31000,
                upper_us: 51000
            }
        );
        assert_eq!(
            offset_interval(clock(100000, 0), clock(180000, 80000), 50, 1000)
                .unwrap()
                .upper_us,
            -49000
        );
        assert!(offset_interval(clock(0, 0), clock(80000, 70000), 50, 1000).is_none());
        assert!(offset_interval(clock(10, 10), clock(0, 20), 0, 1000).is_none());
        assert!(
            a.intersection(OffsetInterval {
                lower_us: 52000,
                upper_us: 53000
            })
            .is_none()
        );
        assert_eq!(
            a.intersection(OffsetInterval {
                lower_us: 0,
                upper_us: 1000
            })
            .unwrap()
            .lower_us,
            0
        );
        assert_eq!(
            a.arrival_delay(100000, 90),
            OffsetInterval {
                lower_us: -21000,
                upper_us: 61000
            }
        );
    }
    #[test]
    fn recorded_private_and_ws_observations_roundtrip() {
        let memory = Memory::default();
        let recorder = ExecutionRecorder::new("clock", json!({}), JsonlSink(memory.clone()));
        recorder.record("http_finished",json!({"method":"POST","endpoint":"/user/spot/order","request_id":1,"attempt":1,"will_retry":false,"success":true,"request_clock":clock(0,0),"response_clock":clock(80000,80000),"result":{"ordered_at":50}}));
        recorder.record(
            "http_request_finished",
            json!({"request_id":1,"attempts":1,"success":true}),
        );
        let ws = WsObserver::new(recorder.clone());
        ws.observe(&json!(["message",{"room_name":"transactions_btc_jpy","message":{"data":{"transactions":[{"transaction_id":1,"side":"buy","price":"1","amount":"1","executed_at":90},{"transaction_id":2,"side":"sell","price":"1","amount":"1","executed_at":91}]}}}]),clock(100000,100000),true);
        ws.observe(
            &json!(["message",{"room_name":"ticker_btc_jpy","message":{"data":{}}}]),
            clock(101000, 101000),
            false,
        );
        recorder.finish().unwrap();
        let records = read_records(std::io::Cursor::new(memory.0.lock().unwrap().clone())).unwrap();
        let http = records.iter().find(|r| r.kind == "http_finished").unwrap();
        let offset = private_offset(&records, http, 1000).unwrap();
        let event = records
            .iter()
            .find(|r| r.kind == "ws_clock_observed")
            .unwrap();
        assert_eq!(event.data["received_clock"]["unix_us"], 100000);
        assert_eq!(event.data["exchange_times"].as_array().unwrap().len(), 2);
        assert_eq!(event.data["valid"], true);
        assert_eq!(offset.arrival_delay(100000, 90).upper_us, 61000);
        assert_eq!(
            records
                .iter()
                .filter(|r| r.kind == "ws_clock_observed")
                .last()
                .unwrap()
                .data["valid"],
            false
        );
        let mut bad = http.clone();
        for (key, value) in [
            ("method", json!("GET")),
            ("success", json!(false)),
            ("attempt", json!(2)),
            ("response_clock", Value::Null),
        ] {
            bad.data = http.data.clone();
            bad.data[key] = value;
            assert!(private_offset(&records, &bad, 1000).is_none());
        }
        bad.data = http.data.clone();
        bad.data["endpoint"] = json!("/user/spot/cancel_order");
        assert!(private_offset(&records, &bad, 1000).is_none());
        bad.data["result"] = json!({"status":"CANCELED_UNFILLED","canceled_at":50});
        assert!(private_offset(&records, &bad, 1000).is_some());
        let mut retried = records.clone();
        retried.push(http.clone());
        assert!(private_offset(&retried, http, 1000).is_none());
    }
}
