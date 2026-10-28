//! Streaming market-data-v1 reader and deterministic offline execution.
use crate::bitbank_bot::{
    BitbankInboundMessage, BitbankMarketEventConverter, BotEvent, BotRunner, BotStrategy, InputId,
};
use crate::bitbank_structs::*;
use crate::trading_venue::TradingVenue;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    io::{self, BufRead, BufReader},
    path::Path,
};

#[derive(Debug, Clone, Deserialize)]
pub struct CaptureRecord {
    pub format_version: u32,
    pub exchange: String,
    pub stream: String,
    pub session_id: String,
    pub connection_id: String,
    pub sequence: u64,
    pub received_at_micros: u64,
    #[serde(flatten)]
    pub payload: CapturePayload,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CapturePayload {
    Connected {
        endpoint: String,
        subscriptions: Vec<String>,
    },
    Message {
        message: Value,
    },
    Disconnected {
        reason: String,
    },
}
#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub sources: Vec<SourceSpec>,
}
#[derive(Debug, Deserialize)]
pub struct SourceSpec {
    pub session_id: String,
    pub exchange: String,
    pub stream: String,
    pub files: Vec<String>,
}
struct Source {
    spec: SourceSpec,
    files: std::collections::VecDeque<String>,
    reader: Option<Box<dyn BufRead + Send>>,
    base: std::path::PathBuf,
    file: String,
    line: usize,
    sequence: u64,
    time: u64,
    active: HashSet<String>,
    used: HashSet<String>,
    head: Option<CaptureRecord>,
}
impl Source {
    fn next(&mut self) -> io::Result<Option<CaptureRecord>> {
        loop {
            if self.reader.is_none() {
                let Some(file) = self.files.pop_front() else {
                    return Ok(None);
                };
                self.reader = Some(Box::new(BufReader::new(std::fs::File::open(
                    self.base.join(&file),
                )?)));
                self.file = file;
                self.line = 0;
            }
            let mut line = String::new();
            if self.reader.as_mut().unwrap().read_line(&mut line)? == 0 {
                self.reader = None;
                continue;
            }
            self.line += 1;
            let result = (|| {
                if !line.ends_with('\n') {
                    return Err(invalid("missing final newline"));
                }
                let r: CaptureRecord = serde_json::from_str(&line).map_err(invalid)?;
                if r.format_version != 1
                    || r.session_id != self.spec.session_id
                    || r.exchange != self.spec.exchange
                    || r.stream != self.spec.stream
                    || r.sequence != self.sequence + 1
                    || r.received_at_micros < self.time
                {
                    return Err(invalid("version, source, sequence or time mismatch"));
                }
                if !matches!(
                    (r.exchange.as_str(), r.stream.as_str()),
                    ("bitbank", "public") | ("bybit", "linear")
                ) {
                    return Err(invalid("unsupported exchange/stream"));
                }
                match &r.payload {
                    CapturePayload::Connected { .. } => {
                        if !self.used.insert(r.connection_id.clone()) {
                            return Err(invalid("reused connection"));
                        }
                        self.active.insert(r.connection_id.clone());
                    }
                    CapturePayload::Message { .. } => {
                        if !self.active.contains(&r.connection_id) {
                            return Err(invalid("message outside connection"));
                        }
                    }
                    CapturePayload::Disconnected { reason } => {
                        if !matches!(
                            reason.as_str(),
                            "transport_closed" | "scheduled_refresh" | "shutdown" | "capture_gap"
                        ) || !self.active.remove(&r.connection_id)
                        {
                            return Err(invalid("invalid disconnect"));
                        }
                    }
                }
                self.sequence = r.sequence;
                self.time = r.received_at_micros;
                Ok(r)
            })();
            return result.map(Some).map_err(|e: io::Error| {
                invalid(format!(
                    "{}:{} session {}: {e}",
                    self.file, self.line, self.spec.session_id
                ))
            });
        }
    }
}
fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}
/// Keeps one pending record per source; file boundaries do not reset state.
pub struct ReplayInput {
    sources: Vec<Source>,
    failed: bool,
}
impl ReplayInput {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let manifest: Manifest =
            serde_json::from_reader(std::fs::File::open(path)?).map_err(invalid)?;
        if manifest.format_version != 1 {
            return Err(invalid("unsupported manifest version"));
        }
        let mut ids = HashSet::new();
        let mut sources = Vec::new();
        for spec in manifest.sources {
            if !ids.insert(spec.session_id.clone()) {
                return Err(invalid("duplicate session"));
            }
            sources.push(Source {
                files: spec.files.clone().into(),
                spec,
                reader: None,
                base: path.parent().unwrap_or(Path::new(".")).into(),
                file: String::new(),
                line: 0,
                sequence: 0,
                time: 0,
                active: HashSet::new(),
                used: HashSet::new(),
                head: None,
            });
        }
        Ok(Self {
            sources,
            failed: false,
        })
    }
}
impl Iterator for ReplayInput {
    type Item = io::Result<(usize, CaptureRecord)>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        for source in &mut self.sources {
            if source.head.is_none() {
                match source.next() {
                    Ok(head) => source.head = head,
                    Err(e) => {
                        self.failed = true;
                        return Some(Err(e));
                    }
                }
            }
        }
        let index = self
            .sources
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                s.head
                    .as_ref()
                    .map(|r| ((r.received_at_micros, i, r.sequence), i))
            })
            .min()
            .map(|(_, i)| i)?;
        Some(Ok((index, self.sources[index].head.take().unwrap())))
    }
}
/// Decode the complete callback JSON without losing the pair in the room name.
pub fn decode_bitbank(raw: &Value) -> io::Result<Option<(String, BitbankInboundMessage)>> {
    if raw.get(0).and_then(Value::as_str) != Some("message") {
        return Ok(None);
    }
    let room = raw
        .get(1)
        .and_then(|v| v.get("room_name"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing room_name"))?;
    let data = raw
        .get(1)
        .and_then(|v| v.get("message"))
        .and_then(|v| v.get("data"))
        .ok_or_else(|| invalid("missing message.data"))?;
    macro_rules! parse {
        ($t:ty) => {
            serde_json::from_value::<$t>(data.clone()).map_err(invalid)?
        };
    }
    let (prefix, message) = if room.starts_with("depth_whole_") {
        (
            "depth_whole_",
            BitbankInboundMessage::DepthWhole(parse!(BitbankDepthWhole)),
        )
    } else if room.starts_with("depth_diff_") {
        (
            "depth_diff_",
            BitbankInboundMessage::DepthDiff(parse!(BitbankDepthDiff)),
        )
    } else if room.starts_with("transactions_") {
        (
            "transactions_",
            BitbankInboundMessage::Transactions(parse!(BitbankTransactionsData).transactions),
        )
    } else if room.starts_with("ticker_") {
        (
            "ticker_",
            BitbankInboundMessage::Ticker(parse!(BitbankTickerResponse)),
        )
    } else if room.starts_with("circuit_break_info_") {
        (
            "circuit_break_info_",
            BitbankInboundMessage::CircuitBreakInfo(parse!(BitbankCircuitBreakInfo)),
        )
    } else {
        return Ok(None);
    };
    Ok(Some((room[prefix.len()..].to_owned(), message)))
}
#[derive(Debug, serde::Serialize)]
pub struct ReplayGap {
    pub source_index: usize,
    pub connection_id: String,
    pub reason: String,
    pub start: InputId,
    pub end: Option<InputId>,
}
#[derive(Debug, Default, serde::Serialize)]
pub struct ReplayReport {
    pub records: usize,
    pub incomplete: bool,
    pub interrupted: bool,
    pub gaps: Vec<ReplayGap>,
    pub last_input: Option<InputId>,
    pub error: Option<String>,
}
#[derive(Default)]
pub struct ReplayAdapter {
    books: BTreeMap<(usize, String, String), BitbankMarketEventConverter>,
    trades: BTreeMap<(String, i64), crate::market_event::MarketTrade>,
}
impl ReplayAdapter {
    pub fn adapt<C>(
        &mut self,
        source: usize,
        record: &CaptureRecord,
        custom: &mut impl FnMut(&CaptureRecord) -> io::Result<Option<C>>,
    ) -> io::Result<Option<BotEvent<C>>> {
        match &record.payload {
            CapturePayload::Disconnected { .. } => {
                self.books
                    .retain(|(s, c, _), _| *s != source || c != &record.connection_id);
                Ok(None)
            }
            CapturePayload::Connected { .. } => Ok(None),
            CapturePayload::Message { message } if record.exchange == "bitbank" => {
                let Some((pair, message)) = decode_bitbank(message)? else {
                    return Ok(None);
                };
                let converter = self
                    .books
                    .entry((source, record.connection_id.clone(), pair.clone()))
                    .or_insert_with(|| BitbankMarketEventConverter::new(pair.clone()));
                let mut event = converter
                    .convert(message)
                    .map_err(|e| invalid(format!("{e:?}")))?;
                if let Some(crate::market_event::MarketEvent::Transactions {
                    transactions, ..
                }) = &mut event
                {
                    let mut filtered = Vec::new();
                    for trade in transactions.drain(..) {
                        let key = (pair.clone(), trade.transaction_id);
                        if let Some(old) = self.trades.get(&key) {
                            if old != &trade {
                                return Err(invalid("conflicting transaction ID"));
                            }
                        } else {
                            self.trades.insert(key, trade.clone());
                            filtered.push(trade);
                        }
                    }
                    *transactions = filtered;
                    if transactions.is_empty() {
                        return Ok(None);
                    }
                }
                Ok(event.map(BotEvent::Market))
            }
            CapturePayload::Message { .. } => custom(record).map(|c| c.map(BotEvent::Custom)),
        }
    }
}
/// Stop normally only at EOF. Disconnects stop evaluation unless recovery is explicit.
pub async fn run_replay<S: BotStrategy<V>, V: TradingVenue>(
    runner: &mut BotRunner<S, V>,
    input: impl IntoIterator<Item = io::Result<(usize, CaptureRecord)>>,
    recovery: bool,
    cancel: &std::sync::atomic::AtomicBool,
    mut custom: impl FnMut(&CaptureRecord) -> io::Result<Option<S::CustomEvent>>,
) -> ReplayReport {
    let mut report = ReplayReport::default();
    let mut adapter = ReplayAdapter::default();
    for item in input {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            report.interrupted = true;
            break;
        }
        let (source, record) = match item {
            Ok(v) => v,
            Err(e) => {
                report.error = Some(e.to_string());
                break;
            }
        };
        let id = InputId {
            session_id: record.session_id.clone(),
            sequence: record.sequence,
        };
        report.last_input = Some(id.clone());
        let time = match i64::try_from(record.received_at_micros / 1000) {
            Ok(t) => t,
            Err(_) => {
                report.error = Some("time overflow".into());
                break;
            }
        };
        if let Err(e) = runner.advance_time(time) {
            report.error = Some(format!("{e:?}"));
            break;
        }

        if matches!(record.payload, CapturePayload::Disconnected { .. }) {
            report.incomplete = true;
            let reason = match &record.payload {
                CapturePayload::Disconnected { reason } => reason.clone(),
                _ => unreachable!(),
            };
            report.gaps.push(ReplayGap {
                source_index: source,
                connection_id: record.connection_id.clone(),
                reason,
                start: id.clone(),
                end: None,
            });
            if !recovery {
                break;
            }
        }
        if matches!(record.payload, CapturePayload::Connected { .. }) {
            for gap in &mut report.gaps {
                if gap.source_index == source && gap.end.is_none() {
                    gap.end = Some(id.clone());
                }
            }
        }
        let event = match adapter.adapt(source, &record, &mut custom) {
            Ok(e) => e,
            Err(e) => {
                report.error = Some(format!("{id:?}: {e}"));
                break;
            }
        };
        if let Some(event) = event {
            if let Err(e) = runner.process(time, Some(id), event).await {
                report.error = Some(format!("{e:?}"));
                break;
            }
        }
        report.records += 1;
    }
    if let Some(recorder) = runner.venue().recorder() {
        recorder.record("replay_outcome", serde_json::json!(&report));
    }
    report
}
