#![doc = include_str!("../docs/execution-recording.md")]
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
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
/// 記録する情報の範囲。既定は資金・注文を監査できる軽量記録。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "profile", rename_all = "snake_case")]
pub enum RecordingProfile {
    Disabled,
    #[default]
    Financial,
    Diagnostic {
        queue: QueueRecording,
    },
}
/// キュー診断の保存方法。周期は診断観測回数（最初は必ずsnapshot）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum QueueRecording {
    Full,
    Periodic {
        every: std::num::NonZeroU64,
    },
    Delta {
        snapshot_every: std::num::NonZeroU64,
    },
    Counters,
}
#[derive(Default)]
struct QueueHistory {
    observation: u64,
    sequence: u64,
    previous: Vec<Value>,
}
struct State {
    profile: RecordingProfile,
    queues: std::collections::BTreeMap<u64, QueueHistory>,
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
#[derive(Debug, Clone, Default)]
pub struct RecordingContext {
    pub operation_id: Option<u64>,
    pub market_event_id: Option<u64>,
    pub event_time_ms: Option<i64>,
    pub input_id: Option<crate::bitbank_bot::InputId>,
    /// 再生中の実行時計。ネストしたscopeも同じ時計を読み、開始時刻へ固定しない。
    pub replay_clock: Option<Arc<std::sync::atomic::AtomicI64>>,
}
tokio::task_local! { static CONTEXT: RecordingContext; }
pub fn current_context() -> RecordingContext {
    let mut context = CONTEXT.try_with(Clone::clone).unwrap_or_default();
    if let Some(clock) = &context.replay_clock {
        context.event_time_ms = Some(clock.load(std::sync::atomic::Ordering::SeqCst));
    }
    context
}
/// 同期した市場登録にも、入力の記録コンテキストを引き継ぐ。
pub fn sync_scope<T>(context: RecordingContext, f: impl FnOnce() -> T) -> T {
    CONTEXT.sync_scope(context, f)
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
/// ローカルUTCと単調時計を組にした計測。UTC差分から経過時間を求めない。
pub(crate) struct Measurement {
    started_at_unix_us: u64,
    started: Instant,
}
impl Measurement {
    pub(crate) fn start() -> Self {
        Self {
            started_at_unix_us: timestamp_us(),
            started: Instant::now(),
        }
    }
    pub(crate) fn started_at_unix_us(&self) -> u64 {
        self.started_at_unix_us
    }
    pub(crate) fn finish(self) -> Value {
        let elapsed_us = self.started.elapsed().as_micros();
        let completed_at_unix_us = timestamp_us();
        json!({"started_at_unix_us": self.started_at_unix_us,
            "completed_at_unix_us": completed_at_unix_us, "elapsed_us": elapsed_us})
    }
}
fn timestamp_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}
impl ExecutionRecorder {
    pub fn new(run_id: impl Into<String>, metadata: Value, sink: impl EventSink + 'static) -> Self {
        Self::with_profile(run_id, metadata, RecordingProfile::default(), sink)
    }
    /// profileと省略対象をrun_startedへ保存する。Disabledはsinkにも書き込まない。
    pub fn with_profile(
        run_id: impl Into<String>,
        metadata: Value,
        profile: RecordingProfile,
        sink: impl EventSink + 'static,
    ) -> Self {
        let r = Self(Arc::new(Mutex::new(State {
            profile,
            queues: Default::default(),
            run_id: run_id.into(),
            sequence: 0,
            next_id: 0,
            sink: Box::new(sink),
            status: RecordingStatus::default(),
        })));
        r.record_lazy("run_started", || json!({"metadata":metadata,"recording":profile,
            "queue_evidence":"only paper_queue_diagnostic contains queue snapshots/deltas; financial records omit queue internals",
            "omitted_events": if matches!(profile, RecordingProfile::Financial) { vec!["paper_queue_book", "paper_queue_trade"] } else { vec![] }}));
        r
    }
    pub fn next_id(&self) -> u64 {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        s.next_id += 1;
        s.next_id
    }
    pub fn profile(&self) -> RecordingProfile {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).profile
    }
    pub fn enabled(&self, kind: &str) -> bool {
        match self.profile() {
            RecordingProfile::Disabled => false,
            RecordingProfile::Financial => !matches!(
                kind,
                "paper_queue_book" | "paper_queue_trade" | "paper_queue_diagnostic"
            ),
            RecordingProfile::Diagnostic { .. } => true,
        }
    }
    /// filterを通った場合だけpayloadを構築する。closure内で同じrecorderを呼ばないこと。
    pub fn record_lazy(&self, kind: &str, data: impl FnOnce() -> Value) {
        if !self.enabled(kind) {
            return;
        }
        let c = current_context();
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if s.status.error.is_some() {
            s.sequence += 1;
            s.status.dropped_records += 1;
            return;
        }
        let data = data();
        Self::append(&mut s, c, kind, data);
    }
    pub fn record(&self, kind: &str, data: Value) {
        self.record_lazy(kind, || data);
    }
    /// 明示snapshot。通常の診断profileの周期を進めず、financialでも要求時は保存する。
    /// stream_idは同じrecorderのnext_idで採番し、engineごとに分ける。
    pub fn queue_snapshot(&self, stream_id: u64, queue: impl FnOnce() -> Vec<Value>) {
        self.queue_record(stream_id, true, queue, || Value::Null);
    }
    /// profile/周期を判定してからqueueまたはcounterのclosureだけを実行する。
    pub fn queue_record(
        &self,
        stream_id: u64,
        force: bool,
        queue: impl FnOnce() -> Vec<Value>,
        counters: impl FnOnce() -> Value,
    ) {
        let c = current_context();
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mode = match s.profile {
            RecordingProfile::Disabled => return,
            RecordingProfile::Financial if !force => return,
            RecordingProfile::Financial => QueueRecording::Full,
            RecordingProfile::Diagnostic { queue } => queue,
        };
        let history = s.queues.entry(stream_id).or_default();
        if !force {
            history.observation += 1;
        }
        let snapshot = force
            || match mode {
                QueueRecording::Full => true,
                QueueRecording::Periodic { every } => {
                    (history.observation - 1).is_multiple_of(every.get())
                }
                QueueRecording::Delta { snapshot_every } => {
                    history.sequence == 0
                        || (history.observation - 1).is_multiple_of(snapshot_every.get())
                }
                QueueRecording::Counters => false,
            };
        if !force && matches!(mode, QueueRecording::Periodic { .. }) && !snapshot {
            return;
        }
        if s.status.error.is_some() {
            s.sequence += 1;
            s.status.dropped_records += 1;
            return;
        }
        let history = s.queues.get_mut(&stream_id).unwrap();
        let base = history.sequence;
        history.sequence += 1;
        let mut data = json!({"format_version":1,"stream_id":stream_id,"sequence":history.sequence,"observation":history.observation});
        if snapshot {
            let current = queue();
            data["snapshot"] = json!(current);
            history.previous = if matches!(mode, QueueRecording::Delta { .. }) {
                current
            } else {
                vec![]
            };
        } else if matches!(mode, QueueRecording::Counters) {
            data["counters"] = counters();
        } else {
            let current = queue();
            let prefix = current
                .iter()
                .zip(&history.previous)
                .take_while(|(a, b)| a == b)
                .count();
            let suffix = current[prefix..]
                .iter()
                .rev()
                .zip(history.previous[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            data["base_sequence"] = json!(base);
            data["splice"] = json!({"start":prefix,"delete":history.previous.len()-prefix-suffix,"insert":current[prefix..current.len()-suffix]});
            history.previous = current;
        }
        Self::append(&mut s, c, "paper_queue_diagnostic", data);
    }
    fn append(s: &mut State, c: RecordingContext, kind: &str, data: Value) {
        s.sequence += 1;
        if s.status.error.is_some() {
            s.status.dropped_records += 1;
            return;
        }
        let record = ExecutionRecord {
            version: 3,
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
        if matches!(s.profile, RecordingProfile::Disabled) {
            return Ok(());
        }
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
        self.record_lazy("run_finished", || json!({}));
        self.flush()
    }
}
/// snapshot/delta列を復元する。streamごとに作り、欠損・逆順・形式違いをエラーにする。
#[derive(Debug, Default)]
pub struct QueueReconstruction {
    stream_id: Option<u64>,
    sequence: u64,
    pub levels: Vec<Value>,
}
impl QueueReconstruction {
    pub fn apply(&mut self, data: &Value) -> io::Result<()> {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid or missing queue diagnostic record",
            )
        };
        let stream = data["stream_id"].as_u64().ok_or_else(invalid)?;
        let sequence = data["sequence"].as_u64().ok_or_else(invalid)?;
        if data["format_version"] != 1
            || self.stream_id.is_some_and(|s| s != stream)
            || Some(sequence) != self.sequence.checked_add(1)
        {
            return Err(invalid());
        }
        let mut levels = self.levels.clone();
        if let Some(snapshot) = data["snapshot"].as_array() {
            levels = snapshot.clone();
        } else {
            if self.sequence == 0 || data["base_sequence"].as_u64() != Some(self.sequence) {
                return Err(invalid());
            }
            let splice = &data["splice"];
            let start = splice["start"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(invalid)?;
            let delete = splice["delete"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(invalid)?;
            let end = start
                .checked_add(delete)
                .filter(|end| *end <= levels.len())
                .ok_or_else(invalid)?;
            let insert = splice["insert"].as_array().ok_or_else(invalid)?;
            levels.splice(start..end, insert.iter().cloned());
        }
        self.levels = levels;
        self.sequence = sequence;
        self.stream_id = Some(stream);
        Ok(())
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
    let Some(r) = recorder.filter(|r| r.enabled("operation_started")) else {
        return future.await;
    };
    let mut c = current_context();
    c.operation_id = Some(r.next_id());
    scope(c, async {
        let measurement = Measurement::start();
        r.record_lazy(
            "operation_started",
            || json!({"operation":name,"request":request,"started_at_unix_us":measurement.started_at_unix_us()}),
        );
        let result = future.await;
        let timing = measurement.finish();
        r.record_lazy("operation_finished", || {
        let mut data = match &result {
            Ok(value) => json!({"operation": name, "success": true, "result": value}),
            Err(error) => {
                use crate::trading_venue::TradingVenueError;
                let (category, details) = match error {
                    TradingVenueError::Numeric(e)=>("numeric",json!(e)),
                    TradingVenueError::Bitbank(_) => ("bitbank", Value::Null),
                    TradingVenueError::Parse(e) => ("parse", json!(e)),
                    TradingVenueError::Paper(e) => ("paper", json!(e)),
                    TradingVenueError::StatePoisoned => ("state_poisoned", Value::Null),
                    TradingVenueError::Unavailable => ("unavailable", Value::Null),
                };
                json!({"operation": name, "success": false, "error": category, "details": details})
            }
        };
        data["timing"] = timing;
        data
        });
        result
    })
    .await
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::book_units::*;
    // 合成の時刻/資金fixture用。注文をspread内に置き、先行数量を0にする。
    fn seeded_engine(
        mut engine: crate::paper_execution::PaperExecutionEngine,
        exchange_ms: i64,
    ) -> crate::paper_execution::PaperExecutionEngine {
        engine.apply_market_event(&crate::market_event::MarketEvent::DepthUpdated {
            pair: "btc_jpy".into(),
            depth: crate::market_event::MarketDepthSnapshot::new(
                std::collections::BTreeMap::from([(1_000_000.into(), literal("1.0"))]),
                std::collections::BTreeMap::from([(1.into(), literal("1.0"))]),
                exchange_ms,
            ),
        });
        engine
    }

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
    #[test]
    fn elapsed_does_not_subtract_utc_timestamps() {
        let mut measurement = Measurement::start();
        // 開始UTCが完了UTCより未来でも、経過時間はInstantだけから計算する。
        measurement.started_at_unix_us = u64::MAX;
        let before = measurement.started.elapsed().as_micros();
        let timing = measurement.finish();
        assert_eq!(timing["started_at_unix_us"], u64::MAX);
        assert!(timing["completed_at_unix_us"].as_u64().unwrap() < u64::MAX);
        assert!(u128::from(timing["elapsed_us"].as_u64().unwrap()) >= before);
    }

    #[tokio::test]
    async fn operation_timing_includes_start_sink_wait_but_excludes_finish_sink_wait() {
        struct SlowSink(Memory);
        impl EventSink for SlowSink {
            fn append(&mut self, record: &ExecutionRecord) -> io::Result<()> {
                if record.kind == "operation_started" || record.kind == "operation_finished" {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                JsonlSink(self.0.clone()).append(record)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let memory = Memory::default();
        let recorder = ExecutionRecorder::new("slow", json!({}), SlowSink(memory.clone()));
        operation(Some(&recorder), "test", json!({}), async { Ok(()) })
            .await
            .unwrap();
        let after = timestamp_us();
        let records = read_records(&memory.0.lock().unwrap()[..]).unwrap();
        let timing = &records.last().unwrap().data["timing"];
        assert!(timing["elapsed_us"].as_u64().unwrap() >= 20_000);
        assert!(after - timing["completed_at_unix_us"].as_u64().unwrap() >= 20_000);
    }

    fn batch_recording_engine(
        profile: RecordingProfile,
        base: Amount,
        quote: Amount,
        fee: Rate,
    ) -> (crate::paper_execution::PaperExecutionEngine, Memory) {
        use crate::{order_domain::BalanceSnapshot, paper_execution::*};
        let memory = Memory::default();
        let recorder = ExecutionRecorder::with_profile(
            "batch-account",
            json!({}),
            profile,
            JsonlSink(memory.clone()),
        );
        let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
        config.fee_schedule.maker_fee_rate_quote = fee;
        let balances = [("btc", base), ("jpy", quote)]
            .into_iter()
            .map(|(asset, amount)| BalanceSnapshot {
                asset: asset.into(),
                free_amount: amount,
                locked_amount: Amount::ZERO,
                onhand_amount: amount,
            })
            .collect();
        (
            seeded_engine(
                PaperExecutionEngine::new(
                    config,
                    PaperClock::Replay { now_ms: 0 },
                    balances,
                    Some(recorder),
                )
                .unwrap(),
                0,
            ),
            memory,
        )
    }

    fn batch_events(memory: &Memory, variant: &str) -> Vec<ExecutionRecord> {
        read_records(&memory.0.lock().unwrap()[..])
            .unwrap()
            .into_iter()
            .filter(|r| r.kind == "paper_event" && r.data["event"].get(variant).is_some())
            .collect()
    }

    fn batch_balance(
        record: &ExecutionRecord,
        asset: &str,
    ) -> crate::order_domain::BalanceSnapshot {
        serde_json::from_value(
            record.data["balances"]
                .as_array()
                .unwrap()
                .iter()
                .find(|b| b["asset"] == asset)
                .unwrap()
                .clone(),
        )
        .unwrap()
    }

    #[test]
    fn batch_fills_record_each_intermediate_account() {
        use crate::{
            market_event::{MarketEvent, MarketTrade},
            order_domain::*,
        };
        for profile in [
            RecordingProfile::Financial,
            RecordingProfile::Diagnostic {
                queue: QueueRecording::Counters,
            },
        ] {
            for side in [OrderSide::Buy, OrderSide::Sell] {
                for fee in [literal::<Rate>("0.01"), literal::<Rate>("-0.01")] {
                    let (mut engine, memory) = batch_recording_engine(
                        profile,
                        if side == OrderSide::Buy {
                            Amount::ZERO
                        } else {
                            3.into()
                        },
                        if side == OrderSide::Buy {
                            1000.into()
                        } else {
                            Amount::ZERO
                        },
                        fee,
                    );
                    for quantity in [1, 2] {
                        engine
                            .place_order(DesiredLimitOrder::limit(
                                "btc_jpy".into(),
                                side,
                                quantity.into(),
                                100.into(),
                            ))
                            .unwrap();
                    }
                    engine.advance_replay_time(10).unwrap();
                    engine.apply_market_event(&MarketEvent::Transactions {
                        pair: "btc_jpy".into(),
                        transactions: vec![MarketTrade {
                            transaction_id: 1,
                            executed_at: 10,
                            side: if side == OrderSide::Buy {
                                OrderSide::Sell
                            } else {
                                OrderSide::Buy
                            },
                            price: 100.into(),
                            amount: literal("2.5"),
                        }],
                    });
                    assert_eq!(engine.advance_trade_watermark(10).unwrap().len(), 2);
                    let records = batch_events(&memory, "OrderFilled");
                    assert_eq!(records.len(), 2);
                    for (index, record) in records.iter().enumerate() {
                        let btc = batch_balance(record, "btc");
                        let jpy = batch_balance(record, "jpy");
                        let (base_free, base_locked, quote_free, quote_locked): (
                            Amount,
                            Amount,
                            Amount,
                            Amount,
                        ) = match (side, fee > Rate::ZERO, index) {
                            (OrderSide::Buy, true, 0) => {
                                (1.into(), 0.into(), 697.into(), 202.into())
                            }
                            (OrderSide::Buy, true, _) => {
                                (literal("2.5"), 0.into(), 697.into(), literal("50.5"))
                            }
                            (OrderSide::Buy, false, 0) => {
                                (1.into(), 0.into(), 701.into(), 200.into())
                            }
                            (OrderSide::Buy, false, _) => {
                                (literal("2.5"), 0.into(), literal("702.5"), 50.into())
                            }
                            (OrderSide::Sell, true, 0) => (0.into(), 2.into(), 99.into(), 0.into()),
                            (OrderSide::Sell, true, _) => {
                                (0.into(), literal("0.5"), literal("247.5"), 0.into())
                            }
                            (OrderSide::Sell, false, 0) => {
                                (0.into(), 2.into(), 101.into(), 0.into())
                            }
                            (OrderSide::Sell, false, _) => {
                                (0.into(), literal("0.5"), literal("252.5"), 0.into())
                            }
                        };
                        assert_eq!(
                            (btc.free_amount, btc.locked_amount),
                            (base_free, base_locked)
                        );
                        assert_eq!(
                            btc.onhand_amount,
                            base_free.checked_add(base_locked).unwrap()
                        );
                        assert_eq!(
                            (jpy.free_amount, jpy.locked_amount),
                            (quote_free, quote_locked)
                        );
                        assert_eq!(
                            jpy.onhand_amount,
                            quote_free.checked_add(quote_locked).unwrap()
                        );
                        let orders: Vec<OpenOrder> =
                            serde_json::from_value(record.data["open_orders"].clone()).unwrap();
                        assert_eq!(orders.len(), 1);
                        assert_eq!(orders[0].order_id, OrderId(2));
                        assert_eq!(
                            orders[0].remaining_amount,
                            if index == 0 { 2.into() } else { literal("0.5") }
                        );
                        assert_eq!(
                            record.data["event"]["OrderFilled"]["order_id"],
                            (index + 1) as u64
                        );
                    }
                    assert_eq!(
                        records.last().unwrap().data["balances"],
                        json!(engine.balances())
                    );
                    assert_eq!(
                        records.last().unwrap().data["open_orders"],
                        json!(engine.open_orders())
                    );
                }
            }
        }
    }

    #[test]
    fn batch_cancellations_record_each_release_and_keep_unrelated_orders() {
        use crate::order_domain::*;
        for profile in [
            RecordingProfile::Financial,
            RecordingProfile::Diagnostic {
                queue: QueueRecording::Counters,
            },
        ] {
            for side in [OrderSide::Buy, OrderSide::Sell] {
                for fee in [literal::<Rate>("0.01"), literal::<Rate>("-0.01")] {
                    let (mut engine, memory) = batch_recording_engine(
                        profile,
                        if side == OrderSide::Buy {
                            Amount::ZERO
                        } else {
                            4.into()
                        },
                        if side == OrderSide::Buy {
                            1000.into()
                        } else {
                            Amount::ZERO
                        },
                        fee,
                    );
                    for quantity in [1, 2, 1] {
                        engine
                            .place_order(DesiredLimitOrder::limit(
                                "btc_jpy".into(),
                                side,
                                quantity.into(),
                                100.into(),
                            ))
                            .unwrap();
                    }
                    engine.advance_replay_time(10).unwrap();
                    engine.cancel_orders("btc_jpy", vec![OrderId(1), OrderId(2)]);
                    assert_eq!(engine.advance_trade_watermark(10).unwrap().len(), 2);
                    let records = batch_events(&memory, "OrderCancelled");
                    assert_eq!(records.len(), 2);
                    for (index, record) in records.iter().enumerate() {
                        let asset = if side == OrderSide::Buy { "jpy" } else { "btc" };
                        let balance = batch_balance(record, asset);
                        let (free, locked): (Amount, Amount) = match (side, fee > Rate::ZERO, index)
                        {
                            (OrderSide::Buy, true, 0) => (697.into(), 303.into()),
                            (OrderSide::Buy, true, _) => (899.into(), 101.into()),
                            (OrderSide::Buy, false, 0) => (700.into(), 300.into()),
                            (OrderSide::Buy, false, _) => (900.into(), 100.into()),
                            (OrderSide::Sell, _, 0) => (1.into(), 3.into()),
                            (OrderSide::Sell, _, _) => (3.into(), 1.into()),
                        };
                        assert_eq!((balance.free_amount, balance.locked_amount), (free, locked));
                        assert_eq!(balance.onhand_amount, free.checked_add(locked).unwrap());
                        let orders: Vec<OpenOrder> =
                            serde_json::from_value(record.data["open_orders"].clone()).unwrap();
                        // 取消確定待ちは最初から公開注文一覧に含めず、無関係の注文だけ残す。
                        assert_eq!(orders.len(), 1);
                        assert_eq!(orders[0].order_id, OrderId(3));
                        assert_eq!(record.data["watermark_ms"], 10);
                        assert_eq!(
                            record.data["event"]["OrderCancelled"]["order_id"],
                            (index + 1) as u64
                        );
                    }
                    assert_eq!(
                        records.last().unwrap().data["balances"],
                        json!(engine.balances())
                    );
                    assert_eq!(
                        records.last().unwrap().data["open_orders"],
                        json!(engine.open_orders())
                    );
                }
            }
        }
    }

    #[test]
    fn batch_late_numeric_failure_does_not_emit_or_apply_earlier_fill() {
        use crate::{
            market_event::{MarketEvent, MarketTrade},
            order_domain::*,
            paper_execution::*,
        };
        let (mut engine, memory) = batch_recording_engine(
            RecordingProfile::Financial,
            2.into(),
            Amount::MAX.checked_sub(150.into()).unwrap(),
            Rate::ZERO,
        );
        for _ in 0..2 {
            engine
                .place_order(DesiredLimitOrder::limit(
                    "btc_jpy".into(),
                    OrderSide::Sell,
                    1.into(),
                    100.into(),
                ))
                .unwrap();
        }
        engine.advance_replay_time(10).unwrap();
        engine.apply_market_event(&MarketEvent::Transactions {
            pair: "btc_jpy".into(),
            transactions: vec![MarketTrade {
                transaction_id: 1,
                executed_at: 10,
                side: OrderSide::Buy,
                price: 100.into(),
                amount: 2.into(),
            }],
        });
        let balances = engine.balances();
        let orders = engine.open_orders();
        let status = engine.settlement_status();
        assert_eq!(
            engine.advance_trade_watermark(10),
            Err(PaperExecutionError::Numeric(NumericError::Overflow))
        );
        assert_eq!(engine.balances(), balances);
        assert_eq!(engine.open_orders(), orders);
        assert_eq!(engine.settlement_status(), status);
        assert!(batch_events(&memory, "OrderFilled").is_empty());
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
        config.fee_schedule.maker_fee_rate_quote = literal("0.001");
        let engine = PaperExecutionEngine::new(
            config,
            crate::paper_execution::PaperClock::Replay { now_ms: 0 },
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
        .map(|engine| seeded_engine(engine, 0))
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
        v.advance_trade_watermark(123).unwrap();
        v.cancel_orders("btc_jpy", vec![placed.order_id.unwrap()])
            .await
            .unwrap();
        v.advance_trade_watermark(124).unwrap();
        let account = v.account_snapshot("btc_jpy").await.unwrap();
        assert!(account.open_orders.is_empty());
        assert_eq!(
            account
                .balances
                .iter()
                .find(|b| b.asset == "jpy")
                .unwrap()
                .free_amount,
            literal("899.9")
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
        assert_eq!(fill.data["event"]["OrderFilled"]["fee_amount_quote"], "0.1");
        assert_eq!(
            fill.data["event"]["OrderFilled"]["trade"]["transaction_id"],
            77
        );
        let failed = ExecutionRecorder::new("broken", json!({}), Broken);
        let still_trading = PaperTradingVenue::new(
            PaperExecutionEngine::new(
                v.engine().lock().unwrap().config().clone(),
                crate::paper_execution::PaperClock::Replay { now_ms: 0 },
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
            crate::paper_execution::PaperClock::Replay { now_ms: 0 },
            balances,
            Some(recorder),
        )
        .map(|engine| seeded_engine(engine, 0))
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
                // 資金の制約により、取消の前後に1件ずつ発注する。
                place_wanna_orders_concurrent(
                    vec![order(90.into()), order(80.into())],
                    account.open_orders,
                    0.into(),
                    90.into(),
                    "btc_jpy".into(),
                    venue.clone(),
                )
                .await
                .unwrap();
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
                    crate::paper_execution::PaperClock::Replay { now_ms: 0 },
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
                .map(|engine| seeded_engine(engine, 0))
                .unwrap()
            };
            for index in 0..(PAPER_EVENT_HISTORY_LIMIT + 50) {
                // 注文拒否は口座を変更しないため、長時間の実行でも結果が決まる。
                assert!(
                    engine
                        .place_order(DesiredLimitOrder::limit(
                            "btc_jpy".into(),
                            OrderSide::Buy,
                            1.into(),
                            (10000 + index as i32).into()
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
                        (10000 + PAPER_EVENT_HISTORY_LIMIT as i32 + 49).into()
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

    #[test]
    fn filters_and_periods_do_not_build_payloads() {
        for profile in [RecordingProfile::Disabled, RecordingProfile::Financial] {
            let r = ExecutionRecorder::with_profile("lazy", json!({}), profile, Broken);
            r.record_lazy("paper_queue_book", || panic!("filtered JSON"));
            r.queue_record(1, false, || panic!("queue clone"), || panic!("counters"));
            if profile == RecordingProfile::Disabled {
                r.queue_snapshot(1, || panic!("disabled explicit snapshot"));
                r.finish().unwrap();
            }
        }
        let m = Memory::default();
        let r = ExecutionRecorder::with_profile(
            "periodic",
            json!({}),
            RecordingProfile::Diagnostic {
                queue: QueueRecording::Periodic {
                    every: 3.try_into().unwrap(),
                },
            },
            JsonlSink(m.clone()),
        );
        r.queue_record(1, false, Vec::new, || panic!("not counters"));
        for _ in 0..2 {
            r.queue_record(
                1,
                false,
                || panic!("skipped snapshot"),
                || panic!("not counters"),
            );
        }
        r.queue_record(1, false, || vec![json!(4)], || panic!("not counters"));
        let records = read_records(&m.0.lock().unwrap()[..]).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[2].data["observation"], 4);
        let r = ExecutionRecorder::with_profile(
            "counter",
            json!({}),
            RecordingProfile::Diagnostic {
                queue: QueueRecording::Counters,
            },
            JsonlSink(Memory::default()),
        );
        r.queue_record(
            1,
            false,
            || panic!("counter cloned queue"),
            || json!({"levels": 2}),
        );
    }

    #[test]
    fn deltas_restore_insertions_deletions_and_detect_missing_records() {
        let m = Memory::default();
        let r = ExecutionRecorder::with_profile(
            "delta",
            json!({}),
            RecordingProfile::Diagnostic {
                queue: QueueRecording::Delta {
                    snapshot_every: 4.try_into().unwrap(),
                },
            },
            JsonlSink(m.clone()),
        );
        let states = [
            vec![],
            vec![json!(1), json!(2)],
            vec![json!(1)],
            vec![json!(3), json!(1)],
            vec![],
            vec![json!(9)],
        ];
        for state in &states {
            r.queue_record(7, false, || state.clone(), || panic!("not counters"));
        }
        r.queue_snapshot(7, || states.last().unwrap().clone());
        let records: Vec<_> = read_records(&m.0.lock().unwrap()[..])
            .unwrap()
            .into_iter()
            .filter(|r| r.kind == "paper_queue_diagnostic")
            .collect();
        let mut reconstructed = QueueReconstruction::default();
        for (record, expected) in records.iter().zip(states.iter().chain(states.last())) {
            reconstructed.apply(&record.data).unwrap();
            assert_eq!(&reconstructed.levels, expected);
        }
        assert!(records[4].data.get("snapshot").is_some());
        let mut missing = QueueReconstruction::default();
        missing.apply(&records[0].data).unwrap();
        assert!(missing.apply(&records[2].data).is_err());
        assert!(missing.apply(&records[0].data).is_err());
        let mut wrong = records[1].data.clone();
        wrong["base_sequence"] = json!(999);
        assert!(missing.apply(&wrong).is_err());
        wrong = records[1].data.clone();
        wrong["format_version"] = json!(99);
        assert!(missing.apply(&wrong).is_err());
        assert!(
            QueueReconstruction::default()
                .apply(&records[1].data)
                .is_err()
        );
        missing.apply(&records[1].data).unwrap(); // 失敗時は復元状態を変更しない
    }

    #[test]
    fn failed_sink_skips_lazy_builders_and_reports_loss() {
        let r = ExecutionRecorder::with_profile(
            "fail",
            json!({}),
            RecordingProfile::Diagnostic {
                queue: QueueRecording::Full,
            },
            Broken,
        );
        r.record_lazy("custom", || panic!("failed sink payload"));
        r.queue_record(
            1,
            false,
            || panic!("failed sink queue"),
            || panic!("failed sink counter"),
        );
        assert_eq!(r.status().dropped_records, 3);
        assert!(r.status().persistence_uncertain);
        assert!(r.finish().is_err());
    }

    #[tokio::test]
    async fn profiles_preserve_finances_events_clock_and_query_metadata() {
        use crate::{
            market_event::*, order_domain::*, paper_execution::*, trading_venue::TradingVenue,
        };
        let mut baseline = None;
        for profile in [
            None,
            Some(RecordingProfile::Disabled),
            Some(RecordingProfile::Financial),
            Some(RecordingProfile::Diagnostic {
                queue: QueueRecording::Full,
            }),
            Some(RecordingProfile::Diagnostic {
                queue: QueueRecording::Periodic {
                    every: 3.try_into().unwrap(),
                },
            }),
            Some(RecordingProfile::Diagnostic {
                queue: QueueRecording::Delta {
                    snapshot_every: 4.try_into().unwrap(),
                },
            }),
            Some(RecordingProfile::Diagnostic {
                queue: QueueRecording::Counters,
            }),
        ] {
            let m = Memory::default();
            let recorder = profile.map(|p| {
                ExecutionRecorder::with_profile("invariant", json!({}), p, JsonlSink(m.clone()))
            });
            let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
            config.fee_schedule.maker_fee_rate_quote = literal("0.001");
            let balances = [("btc", 0), ("jpy", 1000)]
                .into_iter()
                .map(|(asset, n)| BalanceSnapshot {
                    asset: asset.into(),
                    free_amount: n.into(),
                    locked_amount: 0.into(),
                    onhand_amount: n.into(),
                })
                .collect();
            let v = PaperTradingVenue::new(seeded_engine(
                PaperExecutionEngine::new(
                    config,
                    PaperClock::Replay { now_ms: 0 },
                    balances,
                    recorder.clone(),
                )
                .unwrap(),
                0,
            ));
            let initial = v.account_snapshot("btc_jpy").await.unwrap();
            let order =
                DesiredLimitOrder::limit("btc_jpy".into(), OrderSide::Buy, 2.into(), 100.into());
            let placed = v.place_order(order.clone().into()).await.unwrap();
            let locked = v.account_snapshot("btc_jpy").await.unwrap();
            v.on_market_event(&MarketEvent::Transactions {
                pair: "btc_jpy".into(),
                transactions: vec![MarketTrade {
                    amount: 1.into(),
                    price: 100.into(),
                    executed_at: 123,
                    side: OrderSide::Sell,
                    transaction_id: 77,
                }],
            })
            .unwrap();
            v.advance_trade_watermark(123).unwrap();
            let filled = v.account_snapshot("btc_jpy").await.unwrap();
            v.cancel_orders("btc_jpy", vec![placed.order_id.unwrap()])
                .await
                .unwrap();
            v.advance_trade_watermark(124).unwrap();
            v.advance_replay_time(200).unwrap();
            assert!(
                v.place_order(
                    DesiredLimitOrder {
                        amount: 100.into(),
                        ..order
                    }
                    .into()
                )
                .await
                .is_err()
            );
            let final_account = v.account_snapshot("btc_jpy").await.unwrap();
            let events = v.engine().lock().unwrap().drain_events();
            let result = json!({"snapshots":[initial,locked,filled,final_account], "events":events,
                "settlement":v.paper_settlement_status()});
            if let Some(expected) = &baseline {
                assert_eq!(&result, expected);
            } else {
                baseline = Some(result);
            }
            v.record_paper_settlement();
            if let Some(r) = recorder {
                r.finish().unwrap();
                let records = read_records(&m.0.lock().unwrap()[..]).unwrap();
                if profile == Some(RecordingProfile::Disabled) {
                    assert!(records.is_empty());
                    continue;
                }
                let audit: Vec<_> = records.iter().filter(|r| r.kind == "paper_event").collect();
                assert_eq!(audit.len(), 5); // accepted, partial fill, cancel pending, cancelled, rejected
                assert_eq!(
                    audit[1].data["event"]["OrderFilled"]["fee_amount_quote"],
                    "0.1"
                );
                assert_eq!(audit[0].data["balances"][1]["locked_amount"], "200.2");
                assert_eq!(
                    audit[3].data["balances"][1]["locked_amount"]
                        .as_str()
                        .unwrap()
                        .parse::<Amount>()
                        .unwrap(),
                    Amount::ZERO
                );
                assert_eq!(audit[3].data["balances"][1]["free_amount"], "899.9");
                assert!(records.last().is_some_and(|r| r.kind == "run_finished"));
                assert!(
                    records
                        .iter()
                        .filter(|r| r.kind == "paper_settlement")
                        .all(|r| r.data.get("queue").is_none())
                );
            }
        }
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
