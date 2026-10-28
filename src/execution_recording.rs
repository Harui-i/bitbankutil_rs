#![doc = include_str!("../docs/execution-recording.md")]
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

pub trait EventSink: Send {
    fn append(&mut self, record: &ExecutionRecord) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}
pub struct JsonlSink<W: Write + Send>(pub W);
impl JsonlSink<std::io::BufWriter<std::fs::File>> {
    pub fn open(path: impl AsRef<std::path::Path>) -> io::Result<Self> {
        Ok(Self(std::io::BufWriter::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?,
        )))
    }
}
impl<W: Write + Send> EventSink for JsonlSink<W> {
    fn append(&mut self, record: &ExecutionRecord) -> io::Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        self.0.write_all(&line)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub version: u32,
    pub run_id: String,
    pub sequence: u64,
    pub recorded_at_ms: u64,
    pub event_time_ms: Option<i64>,
    #[serde(default)]
    pub input_id: Option<crate::bitbank_bot::InputId>,
    pub operation_id: Option<u64>,
    pub market_event_id: Option<u64>,
    pub kind: String,
    pub data: Value,
}
#[derive(Debug, Clone, Default)]
pub struct RecordingStatus {
    pub error: Option<String>,
    pub dropped_records: u64,
    pub persistence_uncertain: bool,
}
struct State {
    run_id: String,
    sequence: u64,
    next_id: u64,
    sink: Box<dyn EventSink>,
    status: RecordingStatus,
}
#[derive(Clone)]
pub struct ExecutionRecorder(Arc<Mutex<State>>);
impl std::fmt::Debug for ExecutionRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionRecorder").finish_non_exhaustive()
    }
}
#[derive(Clone, Default)]
pub struct RecordingContext {
    pub operation_id: Option<u64>,
    pub market_event_id: Option<u64>,
    pub event_time_ms: Option<i64>,
    pub input_id: Option<crate::bitbank_bot::InputId>,
}
tokio::task_local! { static CONTEXT: RecordingContext; }
pub fn current_context() -> RecordingContext {
    CONTEXT.try_with(Clone::clone).unwrap_or_default()
}
pub async fn scope<T>(
    context: RecordingContext,
    future: impl std::future::Future<Output = T>,
) -> T {
    CONTEXT.scope(context, future).await
}
pub fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
impl ExecutionRecorder {
    pub fn new(run_id: impl Into<String>, metadata: Value, sink: impl EventSink + 'static) -> Self {
        let r = Self(Arc::new(Mutex::new(State {
            run_id: run_id.into(),
            sequence: 0,
            next_id: 0,
            sink: Box::new(sink),
            status: RecordingStatus::default(),
        })));
        r.record("run_started", metadata);
        r
    }
    pub fn next_id(&self) -> u64 {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        s.next_id += 1;
        s.next_id
    }
    pub fn record(&self, kind: &str, data: Value) {
        let c = current_context();
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        s.sequence += 1;
        if s.status.error.is_some() {
            s.status.dropped_records += 1;
            return;
        }
        let record = ExecutionRecord {
            version: 1,
            run_id: s.run_id.clone(),
            sequence: s.sequence,
            recorded_at_ms: timestamp_ms(),
            event_time_ms: c.event_time_ms,
            input_id: c.input_id,
            operation_id: c.operation_id,
            market_event_id: c.market_event_id,
            kind: kind.into(),
            data,
        };
        if let Err(e) = s.sink.append(&record) {
            s.status.error = Some(e.to_string());
            s.status.dropped_records += 1;
            s.status.persistence_uncertain = true;
        }
    }
    pub fn status(&self) -> RecordingStatus {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }
    pub fn flush(&self) -> io::Result<()> {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if s.status.error.is_none() {
            if let Err(e) = s.sink.flush() {
                s.status.error = Some(e.to_string());
                s.status.persistence_uncertain = true;
            }
        }
        match &s.status.error {
            Some(e) => Err(io::Error::other(e.clone())),
            None => Ok(()),
        }
    }
    pub fn finish(&self) -> io::Result<()> {
        self.record("run_finished", json!({}));
        self.flush()
    }
}
pub fn read_records(reader: impl BufRead) -> io::Result<Vec<ExecutionRecord>> {
    reader
        .lines()
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect()
}
pub(crate) async fn operation<T: Serialize>(
    recorder: Option<&ExecutionRecorder>,
    name: &str,
    request: Value,
    future: impl std::future::Future<Output = Result<T, crate::trading_venue::TradingVenueError>>,
) -> Result<T, crate::trading_venue::TradingVenueError> {
    let Some(r) = recorder else {
        return future.await;
    };
    let mut c = current_context();
    c.operation_id = Some(r.next_id());
    scope(c, async {
        r.record(
            "operation_started",
            json!({"operation":name,"request":request}),
        );
        let result = future.await;
        let data = match &result {
            Ok(value) => json!({"operation": name, "success": true, "result": value}),
            Err(error) => {
                use crate::trading_venue::TradingVenueError;
                let (category, details) = match error {
                    TradingVenueError::Bitbank(_) => ("bitbank", Value::Null),
                    TradingVenueError::Parse(e) => ("parse", json!(e)),
                    TradingVenueError::Paper(e) => ("paper", json!(e)),
                    TradingVenueError::StatePoisoned => ("state_poisoned", Value::Null),
                    TradingVenueError::Unavailable => ("unavailable", Value::Null),
                };
                json!({"operation": name, "success": false, "error": category, "details": details})
            }
        };
        r.record("operation_finished", data);
        result
    })
    .await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[derive(Clone, Default)]
    pub(crate) struct Memory(pub Arc<Mutex<Vec<u8>>>);
    impl Write for Memory {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn paper_roundtrip() {
        let path = std::env::temp_dir().join(format!(
            "recording-{}-{}.jsonl",
            std::process::id(),
            timestamp_ms()
        ));
        let memory = Memory::default();
        let r = ExecutionRecorder::new("test", json!({}), JsonlSink(memory.clone()));
        use crate::{order_domain::*, paper_execution::*, trading_venue::TradingVenue};
        let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
        config.fee_schedule.maker_fee_rate_quote = rust_decimal::Decimal::new(1, 3);
        let engine = PaperExecutionEngine::new(
            config,
            vec![
                BalanceSnapshot {
                    asset: "btc".into(),
                    free_amount: 0.into(),
                    locked_amount: 0.into(),
                    onhand_amount: 0.into(),
                },
                BalanceSnapshot {
                    asset: "jpy".into(),
                    free_amount: 1000.into(),
                    locked_amount: 0.into(),
                    onhand_amount: 1000.into(),
                },
            ],
            Some(r.clone()),
        )
        .unwrap();
        let v = PaperTradingVenue::new(engine);
        let order =
            DesiredLimitOrder::limit("btc_jpy".into(), OrderSide::Buy, 2.into(), 100.into());
        let placed = v.place_order(order.into()).await.unwrap();
        v.on_market_event(&crate::market_event::MarketEvent::Transactions {
            pair: "btc_jpy".into(),
            transactions: vec![crate::market_event::MarketTrade {
                amount: 1.into(),
                price: 100.into(),
                executed_at: 123,
                side: OrderSide::Sell,
                transaction_id: 77,
            }],
        })
        .unwrap();
        v.cancel_orders("btc_jpy", vec![placed.order_id.unwrap()])
            .await
            .unwrap();
        let account = v.account_snapshot("btc_jpy").await.unwrap();
        assert!(account.open_orders.is_empty());
        assert_eq!(
            account
                .balances
                .iter()
                .find(|b| b.asset == "jpy")
                .unwrap()
                .free_amount,
            rust_decimal::Decimal::new(8999, 1)
        );
        assert!(v.account_snapshot("other").await.is_err());
        r.finish().unwrap();
        let bytes = memory.0.lock().unwrap().clone();
        std::fs::write(&path, &bytes).unwrap();
        let records =
            read_records(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).unwrap();
        std::fs::remove_file(path).unwrap();
        let starts: Vec<_> = records
            .iter()
            .filter(|x| x.kind == "operation_started")
            .collect();
        assert_eq!(starts.len(), 4);
        for start in starts {
            assert_eq!(
                records
                    .iter()
                    .filter(
                        |x| x.kind == "operation_finished" && x.operation_id == start.operation_id
                    )
                    .count(),
                1
            );
        }
        let fill = records
            .iter()
            .find(|r| r.data["event"].get("OrderFilled").is_some())
            .unwrap();
        assert_eq!(
            fill.data["event"]["OrderFilled"]["fee_amount_quote"],
            "0.100"
        );
        assert_eq!(
            fill.data["event"]["OrderFilled"]["trade"]["transaction_id"],
            77
        );
        let failed = ExecutionRecorder::new("broken", json!({}), Broken);
        let still_trading = PaperTradingVenue::new(
            PaperExecutionEngine::new(
                v.engine().lock().unwrap().config().clone(),
                account.balances.clone(),
                Some(failed.clone()),
            )
            .unwrap(),
        );
        assert!(still_trading.account_snapshot("btc_jpy").await.is_ok());
        assert!(failed.status().error.is_some());
        assert!(failed.flush().is_err());
        assert!(records.windows(2).all(|w| w[0].sequence < w[1].sequence));
        assert_eq!(
            records.iter().filter(|r| r.kind == "http_started").count(),
            0
        );
    }

    fn recorded_paper(recorder: ExecutionRecorder) -> crate::paper_execution::PaperExecutionEngine {
        use crate::{order_domain::BalanceSnapshot, paper_execution::*};
        let balances = [("btc", 10), ("jpy", 1000)]
            .into_iter()
            .map(|(asset, amount)| BalanceSnapshot {
                asset: asset.into(),
                free_amount: amount.into(),
                locked_amount: 0.into(),
                onhand_amount: amount.into(),
            })
            .collect();
        PaperExecutionEngine::new(
            PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap(),
            balances,
            Some(recorder),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn order_manager_tasks_inherit_market_context() {
        use crate::{
            order_domain::{DesiredLimitOrder, OrderSide},
            order_manager::{place_wanna_orders, place_wanna_orders_concurrent},
            paper_execution::PaperTradingVenue,
            trading_venue::TradingVenue,
        };
        let memory = Memory::default();
        let recorder = ExecutionRecorder::new("parallel", json!({}), JsonlSink(memory.clone()));
        let mut engine = recorded_paper(recorder.clone());
        let order =
            |price| DesiredLimitOrder::limit("btc_jpy".into(), OrderSide::Buy, 1.into(), price);
        engine.place_order(order(100.into())).unwrap();
        let venue = PaperTradingVenue::new(engine);
        scope(
            RecordingContext {
                market_event_id: Some(77),
                event_time_ms: Some(123),
                ..Default::default()
            },
            async {
                let account = venue.account_snapshot("btc_jpy").await.unwrap();
                // Budget forces one placement before cancellation and another after it.
                place_wanna_orders_concurrent(
                    vec![order(90.into()), order(80.into())],
                    account.open_orders,
                    0.into(),
                    90.into(),
                    "btc_jpy".into(),
                    venue.clone(),
                )
                .await;
                let account = venue.account_snapshot("btc_jpy").await.unwrap();
                assert_eq!(account.open_orders.len(), 2);
                place_wanna_orders(
                    [order(70.into())].into_iter().collect(),
                    account.open_orders,
                    "btc_jpy".into(),
                    venue.clone(),
                )
                .await;
            },
        )
        .await;
        recorder.finish().unwrap();
        let bytes = memory.0.lock().unwrap().clone();
        let records = read_records(&bytes[..]).unwrap();
        let operations: Vec<_> = records
            .iter()
            .filter(|r| r.kind.starts_with("operation_"))
            .collect();
        assert_eq!(operations.len(), 14); // 2 snapshots, 3 placements, 2 cancellations; start and finish.
        for record in operations {
            assert_eq!(record.market_event_id, Some(77));
            assert_eq!(record.event_time_ms, Some(123));
            if record.kind == "operation_finished" {
                assert_eq!(record.data["success"], true);
            }
        }
        let operation_ids: std::collections::BTreeSet<_> = records
            .iter()
            .filter(|r| r.kind == "operation_started")
            .map(|r| r.operation_id.unwrap())
            .collect();
        assert_eq!(operation_ids.len(), 7);
        for record in records
            .iter()
            .filter(|r| r.kind == "paper_event" && r.operation_id.is_some())
        {
            assert_eq!(record.market_event_id, Some(77));
            assert!(operation_ids.contains(&record.operation_id.unwrap()));
        }
    }

    #[tokio::test]
    async fn paper_clones_and_shared_handles_use_engine_recorder() {
        use crate::{
            order_domain::{DesiredLimitOrder, OrderSide},
            paper_execution::PaperTradingVenue,
            trading_venue::TradingVenue,
        };
        let memory = Memory::default();
        let recorder = ExecutionRecorder::new("shared", json!({}), JsonlSink(memory.clone()));
        let a = PaperTradingVenue::new(recorded_paper(recorder.clone()));
        let b = a.clone();
        let c = PaperTradingVenue::from_shared(a.engine());
        for venue in [&a, &b, &c] {
            assert!(venue.recorder().is_some());
            venue
                .place_order(
                    DesiredLimitOrder::limit(
                        "btc_jpy".into(),
                        OrderSide::Buy,
                        1.into(),
                        100.into(),
                    )
                    .into(),
                )
                .await
                .unwrap();
        }
        assert_eq!(
            a.account_snapshot("btc_jpy")
                .await
                .unwrap()
                .open_orders
                .len(),
            3
        );
        recorder.finish().unwrap();
        let bytes = memory.0.lock().unwrap().clone();
        let records = read_records(&bytes[..]).unwrap();
        assert!(records.iter().all(|r| r.run_id == "shared"));
        assert_eq!(
            records
                .iter()
                .filter(|r| r.kind == "paper_initial_state")
                .count(),
            1
        );
        let accepted: Vec<_> = records.iter().filter(|r| r.kind == "paper_event").collect();
        assert_eq!(accepted.len(), 3);
        for event in accepted {
            let id = event.operation_id.unwrap();
            assert_eq!(
                records
                    .iter()
                    .filter(|r| r.operation_id == Some(id) && r.kind.starts_with("operation_"))
                    .count(),
                2
            );
        }
    }
    #[test]
    fn paper_history_is_bounded_without_losing_persistent_records() {
        use crate::{
            order_domain::{DesiredLimitOrder, OrderSide},
            paper_execution::PAPER_EVENT_HISTORY_LIMIT,
        };
        let memory = Memory::default();
        let recorder = ExecutionRecorder::new("bounded", json!({}), JsonlSink(memory.clone()));
        for recording in [false, true] {
            let mut engine = if recording {
                recorded_paper(recorder.clone())
            } else {
                use crate::{order_domain::BalanceSnapshot, paper_execution::*};
                PaperExecutionEngine::new(
                    PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap(),
                    vec![
                        BalanceSnapshot {
                            asset: "btc".into(),
                            free_amount: 0.into(),
                            locked_amount: 0.into(),
                            onhand_amount: 0.into(),
                        },
                        BalanceSnapshot {
                            asset: "jpy".into(),
                            free_amount: 1000.into(),
                            locked_amount: 0.into(),
                            onhand_amount: 1000.into(),
                        },
                    ],
                    None,
                )
                .unwrap()
            };
            for index in 0..(PAPER_EVENT_HISTORY_LIMIT + 50) {
                // Rejections do not change the account, making a long run deterministic.
                assert!(
                    engine
                        .place_order(DesiredLimitOrder::limit(
                            "btc_jpy".into(),
                            OrderSide::Buy,
                            1.into(),
                            (10000 + index as i64).into()
                        ))
                        .is_err()
                );
            }
            assert_eq!(engine.discarded_event_count(), 50);
            let history = engine.drain_events();
            assert_eq!(history.len(), PAPER_EVENT_HISTORY_LIMIT);
            match (&history[0], history.last().unwrap()) {
                (
                    crate::paper_execution::PaperEvent::OrderRejected { order: first, .. },
                    crate::paper_execution::PaperEvent::OrderRejected { order: last, .. },
                ) => {
                    assert_eq!(first.price, 10050.into());
                    assert_eq!(
                        last.price,
                        (10000 + PAPER_EVENT_HISTORY_LIMIT as i64 + 49).into()
                    );
                }
                _ => panic!("expected rejected orders"),
            }
            assert!(engine.drain_events().is_empty());
            assert_eq!(engine.discarded_event_count(), 50);
        }
        recorder.finish().unwrap();
        let bytes = memory.0.lock().unwrap().clone();
        let records = read_records(&bytes[..]).unwrap();
        assert_eq!(
            records.iter().filter(|r| r.kind == "paper_event").count(),
            PAPER_EVENT_HISTORY_LIMIT + 50
        );
    }

    struct Broken;
    impl EventSink for Broken {
        fn append(&mut self, _: &ExecutionRecord) -> io::Result<()> {
            Err(io::Error::other("disk full"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn sticky_failure() {
        let r = ExecutionRecorder::new("test", json!({}), Broken);
        r.record("custom", json!({}));
        assert_eq!(r.status().dropped_records, 2);
        assert!(r.status().persistence_uncertain);
        assert!(r.finish().is_err());
    }
    struct FlushFailure;
    impl EventSink for FlushFailure {
        fn append(&mut self, _: &ExecutionRecord) -> io::Result<()> {
            Ok(())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
    }
    #[test]
    fn flush_failure() {
        let r = ExecutionRecorder::new("test", json!({}), FlushFailure);
        assert!(r.flush().is_err());
        r.record("later", json!({}));
        assert_eq!(r.status().dropped_records, 1);
    }
    #[tokio::test]
    async fn parallel_contexts() {
        let m = Memory::default();
        let r = ExecutionRecorder::new("test", json!({}), JsonlSink(m.clone()));
        let mut tasks = vec![];
        for id in 1..=10 {
            let r = r.clone();
            tasks.push(tokio::spawn(scope(
                RecordingContext {
                    operation_id: Some(id),
                    ..Default::default()
                },
                async move {
                    r.record("custom", json!({"id":id}));
                },
            )));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let records = read_records(&m.0.lock().unwrap()[..]).unwrap();
        for x in records.iter().skip(1) {
            assert_eq!(x.operation_id, x.data["id"].as_u64());
        }
    }
}
