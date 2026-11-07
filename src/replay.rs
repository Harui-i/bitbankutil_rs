//! 市場データv1の逐次読み込みと、backtestなどの決定的なオフライン実行。
//! 入力形式と実行手順は `docs/offline-replay.md` を参照。
use crate::bitbank_bot::{
    BitbankInboundMessage, BitbankMarketEventConverter, BotEvent, BotRunner, BotStrategy, InputId,
};
use crate::bitbank_structs::*;
use crate::trading_venue::TradingVenue;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    io::{self, BufRead, BufReader},
    path::Path,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
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

type DecodedBitbank = Option<(String, BitbankInboundMessage)>;
struct PreparedRecord {
    source: usize,
    record: CaptureRecord,
    bitbank: DecodedBitbank,
}

/// 検証とJSONの解析を一度だけ済ませ、複数のbacktestで再利用する入力。
///
/// 接続メタデータと型付きbitbank入力をRAMに保持する。準備時間は実行時間とは別に計測する。
/// bitbankの生JSONは解析後に破棄し、custom変換が必要な他取引所のJSONだけを保持する。
/// 他取引所のレコードは実行時のcustom変換へ渡す。接続切断・時刻・約定の処理は通常のrun_replayと共通。
pub struct PreparedReplay {
    records: Vec<PreparedRecord>,
    validation: ReplayValidation,
    tails: BTreeMap<usize, i64>,
}
impl PreparedReplay {
    /// 板の復元・市場入力の重複検証も一度だけ行う。時計モデルと先読み幅を固定する。
    /// custom変換は準備時に呼ばれ、結果を反復実行で共有する。
    pub fn compile<C: Clone + Send + 'static>(
        &self,
        config: crate::simulation::SimulationConfig,
        time_model: crate::paper_execution::PaperTimeModel,
        mut custom: impl FnMut(&CaptureRecord) -> io::Result<Option<C>>,
    ) -> io::Result<CompiledReplay<'_, C>> {
        let mut driver = crate::simulation::SimulationDriver::new(config, time_model)
            .map_err(|e| invalid(format!("{e:?}")))?;
        let mut admissions = Vec::with_capacity(self.records.len());
        for entry in &self.records {
            let record = &entry.record;
            let received = (record.received_at_micros / 1000) as i64;
            let id = Some(InputId {
                session_id: record.session_id.clone(),
                sequence: record.sequence,
            });
            let result = match &record.payload {
                CapturePayload::Message { .. } if record.exchange == "bitbank" => {
                    entry.bitbank.as_ref().map(|(pair, message)| {
                        driver.admit_bitbank(
                            received,
                            id,
                            format!("{}:{}", entry.source, record.connection_id),
                            pair.clone(),
                            message.clone(),
                        )
                    })
                }
                CapturePayload::Message { .. } => custom(record)?
                    .map(|event| driver.admit_event(received, id, BotEvent::Custom(event))),
                _ => None,
            };
            admissions.push(result.map(|result| driver.take_admission(result)));
        }
        driver.compile_markets(&mut admissions);
        Ok(CompiledReplay {
            input: self,
            config,
            time_model,
            admissions,
        })
    }
    pub fn new(
        input: impl IntoIterator<Item = io::Result<(usize, CaptureRecord)>>,
    ) -> io::Result<Self> {
        let mut result = Self {
            records: Vec::new(),
            validation: ReplayValidation::default(),
            tails: BTreeMap::new(),
        };
        for item in input {
            let (source, mut record) = item?;
            result.validation.validate(source, &record)?;
            let time = i64::try_from(record.received_at_micros / 1000).map_err(invalid)?;
            let bitbank = match &record.payload {
                CapturePayload::Message { message } if record.exchange == "bitbank" => {
                    decode_bitbank(message)?
                }
                _ => None,
            };
            if record.exchange == "bitbank"
                && let CapturePayload::Message { message } = &mut record.payload
            {
                // bitbankは実行時に必ず解析済み入力を使い、custom変換には渡さない。
                *message = Value::Null;
            }
            result.tails.insert(source, time);
            result.records.push(PreparedRecord {
                source,
                record,
                bitbank,
            });
        }
        Ok(result)
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// PaperTradingVenue等のsimulation対応venueを使うランナーで再生する。
    pub async fn run<S: BotStrategy<V>, V: TradingVenue>(
        &self,
        runner: &mut BotRunner<S, V>,
        options: ReplayOptions,
        cancel: &std::sync::atomic::AtomicBool,
        custom: impl FnMut(&CaptureRecord) -> io::Result<Option<S::CustomEvent>>,
    ) -> ReplayReport {
        if runner.venue().simulation_config().is_none() {
            return ReplayReport {
                error: Some("prepared replay requires a simulation venue".into()),
                incomplete: true,
                end_reason: ReplayEndReason::Error,
                ..Default::default()
            };
        }
        run_validated_simulation_replay(
            runner,
            self.records
                .iter()
                .map(|r| Ok(SimulationRecord::Prepared(r))),
            ReplaySetup {
                validation: &self.validation,
                tails: &self.tails,
                report: ReplayReport {
                    input_exhausted: true,
                    ..Default::default()
                },
                options,
                cancel,
            },
            custom,
            |_| None,
        )
        .await
    }
}

/// 戦略に依存しない板イベントを共有する反復backtest用入力。
/// 残高・注文・約定待ち行列・戦略ハンドラーは毎回新たに実行する。
pub struct CompiledReplay<'a, C> {
    input: &'a PreparedReplay,
    config: crate::simulation::SimulationConfig,
    time_model: crate::paper_execution::PaperTimeModel,
    admissions: Vec<Option<crate::simulation::PreparedAdmission<C>>>,
}
impl<C: Clone + Send + 'static> CompiledReplay<'_, C> {
    pub async fn run<S: BotStrategy<V, CustomEvent = C>, V: TradingVenue>(
        &self,
        runner: &mut BotRunner<S, V>,
        options: ReplayOptions,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> ReplayReport {
        if runner.venue().simulation_config() != Some(self.config)
            || runner.venue().simulation_time_model() != self.time_model
        {
            return ReplayReport {
                error: Some("compiled replay simulation configuration mismatch".into()),
                incomplete: true,
                end_reason: ReplayEndReason::Error,
                ..Default::default()
            };
        }
        let mut admissions = self.admissions.iter();
        run_validated_simulation_replay(
            runner,
            self.input
                .records
                .iter()
                .map(|r| Ok(SimulationRecord::Prepared(r))),
            ReplaySetup {
                validation: &self.input.validation,
                tails: &self.input.tails,
                report: ReplayReport {
                    input_exhausted: true,
                    ..Default::default()
                },
                options,
                cancel,
            },
            |_| Ok(None),
            |driver| {
                admissions
                    .next()
                    .unwrap()
                    .as_ref()
                    .map(|admission| driver.admit_prepared(admission))
            },
        )
        .await
    }
}

enum SimulationRecord<'a> {
    Captured(usize, CaptureRecord),
    Prepared(&'a PreparedRecord),
}
impl<'a> SimulationRecord<'a> {
    fn received_at_micros(&self) -> u64 {
        match self {
            Self::Captured(_, r) => r.received_at_micros,
            Self::Prepared(r) => r.record.received_at_micros,
        }
    }
    fn parts(
        self,
    ) -> (
        usize,
        std::borrow::Cow<'a, CaptureRecord>,
        Option<&'a DecodedBitbank>,
    ) {
        match self {
            Self::Captured(s, r) => (s, std::borrow::Cow::Owned(r), None),
            Self::Prepared(r) => (
                r.source,
                std::borrow::Cow::Borrowed(&r.record),
                Some(&r.bitbank),
            ),
        }
    }
}
#[derive(Debug, Clone, Deserialize, Serialize)]
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
// ファイル入力と呼び出し元が渡す入力に共通するセッション・接続の検証。
#[derive(Default)]
struct SourceValidation {
    identity: Option<(String, String, String)>,
    sequence: u64,
    time: u64,
    active: HashSet<String>,
    subscriptions: BTreeMap<String, std::collections::BTreeSet<String>>,
}
impl SourceValidation {
    fn validate(&mut self, r: &CaptureRecord) -> io::Result<()> {
        if r.format_version != 1 {
            return Err(invalid("unsupported format version"));
        }
        if !matches!(
            (r.exchange.as_str(), r.stream.as_str()),
            ("bitbank", "public") | ("bybit", "linear")
        ) {
            return Err(invalid("unsupported exchange/stream"));
        }
        let identity = (r.session_id.clone(), r.exchange.clone(), r.stream.clone());
        if self
            .identity
            .as_ref()
            .is_some_and(|expected| expected != &identity)
        {
            return Err(invalid("source identity mismatch"));
        }
        if self.sequence.checked_add(1) != Some(r.sequence) {
            return Err(invalid("sequence mismatch"));
        }
        if r.received_at_micros < self.time {
            return Err(invalid("received time reversed"));
        }
        match &r.payload {
            CapturePayload::Connected { subscriptions, .. } => {
                if self.subscriptions.contains_key(&r.connection_id) {
                    return Err(invalid("reused connection"));
                }
                self.subscriptions.insert(
                    r.connection_id.clone(),
                    subscriptions.iter().cloned().collect(),
                );
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
                ) || !self.active.contains(&r.connection_id)
                {
                    return Err(invalid("invalid disconnect"));
                }
                self.active.remove(&r.connection_id);
            }
        }
        self.identity = Some(identity);
        self.sequence = r.sequence;
        self.time = r.received_at_micros;
        Ok(())
    }
}
#[derive(Default)]
struct ReplayValidation {
    sources: BTreeMap<usize, SourceValidation>,
    sessions: BTreeMap<String, usize>,
    last_key: Option<(u64, usize, u64)>,
}
impl ReplayValidation {
    fn validate(&mut self, source: usize, r: &CaptureRecord) -> io::Result<()> {
        let key = (r.received_at_micros, source, r.sequence);
        if self.last_key.is_some_and(|previous| key < previous) {
            return Err(invalid("merge order reversed"));
        }
        if self
            .sessions
            .get(&r.session_id)
            .is_some_and(|index| *index != source)
        {
            return Err(invalid("session assigned to multiple sources"));
        }
        self.sources.entry(source).or_default().validate(r)?;
        self.sessions.insert(r.session_id.clone(), source);
        self.last_key = Some(key);
        Ok(())
    }
}
struct Source {
    files: std::collections::VecDeque<String>,
    reader: Option<Box<dyn BufRead + Send>>,
    base: std::path::PathBuf,
    file: String,
    line: usize,
    validation: SourceValidation,
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
                self.validation.validate(&r)?;
                Ok(r)
            })();
            return result.map(Some).map_err(|e: io::Error| {
                invalid(format!(
                    "{}:{} session {}: {e}",
                    self.file,
                    self.line,
                    self.validation
                        .identity
                        .as_ref()
                        .map(|i| i.0.as_str())
                        .unwrap_or("unknown")
                ))
            });
        }
    }
}
fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}
/// manifestで指定した各sourceからレコードを逐次読み込み、受信時刻順にマージする。
/// 各sourceで保持する未処理レコードは1件で、ファイル境界でも状態を維持する。
pub struct ReplayInput {
    sources: Vec<Source>,
    failed: bool,
}
impl ReplayInput {
    /// manifestを開く。sourceのファイルパスはmanifestのディレクトリを基準に解決する。
    /// manifestを開けない場合や形式が不正な場合はエラーを返す。レコードは反復時に読み込み、検証する。
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
                reader: None,
                base: path.parent().unwrap_or(Path::new(".")).into(),
                file: String::new(),
                line: 0,
                validation: SourceValidation {
                    identity: Some((
                        spec.session_id.clone(),
                        spec.exchange.clone(),
                        spec.stream.clone(),
                    )),
                    ..Default::default()
                },
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
/// BitbankのコールバックJSONを通貨ペアとメッセージに変換する。
/// メッセージ以外や未知のチャネルは `Ok(None)`、必要なフィールドや既知チャネルのデータが不正ならエラーを返す。
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
    pub subscriptions: std::collections::BTreeSet<String>,
    pub start: InputId,
    pub end: Option<InputId>,
}
/// EOFは評価範囲の自動延長を行わない。EndTimeは指定境界まで評価済み。
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayEndReason {
    #[default]
    Eof,
    EndTime,
    Disconnected,
    Error,
    Interrupted,
}

/// 終了時刻はローカル仮想時計のms単位。境界と同時刻のイベントを含む。
#[derive(Debug, Default, Clone, Copy)]
pub struct ReplayOptions {
    pub recovery: bool,
    pub end_time_ms: Option<i64>,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct ReplayReport {
    pub settlement: Option<crate::paper_execution::PaperSettlementStatus>,
    pub simulation: Option<crate::simulation::SimulationReport>,
    pub end_reason: ReplayEndReason,
    pub input_exhausted: bool,
    pub final_time_ms: Option<i64>,
    pub pending_scheduled: Vec<crate::bitbank_bot::ScheduledEventInfo>,
    pub pending_handler: bool,
    pub pending_deliveries: usize,
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
/// 市場データを検証し、受信時刻に従って戦略を逐次実行する。
/// `input` は `(受信時刻、source番号、sequence)` 順に渡す。`custom` はBitbank以外のメッセージを変換する。
/// 切断時は `recovery` がfalseなら停止し、trueなら欠損を記録して継続する。
/// `cancel` は入力の境界で確認する。入力や戦略のエラー、欠損、中断は戻り値に記録する。
/// 詳しい入力形式と完了条件は `docs/offline-replay.md` を参照。
pub async fn run_replay<S: BotStrategy<V>, V: TradingVenue>(
    runner: &mut BotRunner<S, V>,
    input: impl IntoIterator<Item = io::Result<(usize, CaptureRecord)>>,
    recovery: bool,
    cancel: &std::sync::atomic::AtomicBool,
    custom: impl FnMut(&CaptureRecord) -> io::Result<Option<S::CustomEvent>>,
) -> ReplayReport {
    run_replay_with_options(
        runner,
        input,
        ReplayOptions {
            recovery,
            end_time_ms: None,
        },
        cancel,
        custom,
    )
    .await
}

/// 予約イベントと市場入力を交互に実行する。失敗・中断では予約を停止し、結果に残す。
/// 同じms内は市場入力を元のマージ順で優先し、その後に予約を採番順で実行する。
pub async fn run_replay_with_options<S: BotStrategy<V>, V: TradingVenue>(
    runner: &mut BotRunner<S, V>,
    input: impl IntoIterator<Item = io::Result<(usize, CaptureRecord)>>,
    options: ReplayOptions,
    cancel: &std::sync::atomic::AtomicBool,
    mut custom: impl FnMut(&CaptureRecord) -> io::Result<Option<S::CustomEvent>>,
) -> ReplayReport {
    if runner.venue().simulation_config().is_some() {
        return run_simulation_replay(runner, input, options, cancel, custom).await;
    }
    let recovery = options.recovery;
    let mut report = ReplayReport::default();
    let mut adapter = ReplayAdapter::default();
    let mut validation = ReplayValidation::default();
    if options
        .end_time_ms
        .zip(runner.now_ms())
        .is_some_and(|(end, now)| end < now)
    {
        report.error = Some("end time precedes runner clock".into());
    }
    let mut input = input.into_iter();
    while report.error.is_none() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            report.interrupted = true;
            break;
        }
        let Some(item) = input.next() else {
            report.input_exhausted = true;
            break;
        };
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
        if let Err(error) = validation.validate(source, &record) {
            report.last_input = Some(id.clone());
            report.error = Some(format!("source {source} {id:?}: {error}"));
            break;
        }
        let time = match i64::try_from(record.received_at_micros / 1000) {
            Ok(t) => t,
            Err(_) => {
                report.error = Some("time overflow".into());
                break;
            }
        };
        let boundary = options.end_time_ms.map_or(time, |end| end.min(time));
        while runner
            .next_internal_time()
            .is_some_and(|t| t < time && t <= boundary)
        {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                report.interrupted = true;
                break;
            }
            if let Err(e) = runner.process_next_internal(recovery).await {
                report.error = Some(format!("{e:?}"));
                break;
            }
        }
        // 最後の予約で中断された場合も、次入力へ進む前に観測する。
        report.interrupted |= cancel.load(std::sync::atomic::Ordering::Relaxed);
        if report.interrupted || report.error.is_some() {
            break;
        }
        if options.end_time_ms.is_some_and(|end| time > end) {
            report.end_reason = ReplayEndReason::EndTime;
            break;
        }
        report.last_input = Some(id.clone());
        if let Err(e) = runner.advance_time(time) {
            report.error = Some(format!("{e:?}"));
            break;
        }

        if matches!(record.payload, CapturePayload::Disconnected { .. }) {
            runner
                .venue()
                .mark_market_incomplete("replay input disconnected");
            report.incomplete = true;
            let reason = match &record.payload {
                CapturePayload::Disconnected { reason } => reason.clone(),
                _ => unreachable!(),
            };
            report.gaps.push(ReplayGap {
                source_index: source,
                connection_id: record.connection_id.clone(),
                reason,
                subscriptions: validation.sources[&source].subscriptions[&record.connection_id]
                    .clone(),
                start: id.clone(),
                end: None,
            });
            if !recovery {
                report.end_reason = ReplayEndReason::Disconnected;
                break;
            }
        }
        if let CapturePayload::Connected { subscriptions, .. } = &record.payload {
            let subscriptions: std::collections::BTreeSet<_> =
                subscriptions.iter().cloned().collect();
            let candidates: Vec<_> = report
                .gaps
                .iter_mut()
                .filter(|gap| {
                    gap.source_index == source
                        && gap.end.is_none()
                        && !subscriptions.is_empty()
                        && gap.subscriptions == subscriptions
                })
                .collect();
            // 同じ購読集合の切断が複数ある場合は対応を特定できない。
            // 再接続との対応が一意な欠損だけを終了する。
            if candidates.len() == 1 {
                candidates.into_iter().next().unwrap().end = Some(id.clone());
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
            let result = runner.submit_replay(time, Some(id), event, recovery).await;
            if let Err(e) = result {
                report.error = Some(format!("{e:?}"));
                break;
            }
        }
        report.records += 1;
    }
    // EOFだけでは最終入力を越えない。明示境界だけが市場のない区間を延長する。
    if report.error.is_none()
        && !report.interrupted
        && report.end_reason != ReplayEndReason::Disconnected
    {
        let end = options.end_time_ms.or(runner.now_ms());
        if let Some(end) = end {
            while runner.next_internal_time().is_some_and(|t| t <= end) {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    report.interrupted = true;
                    break;
                }
                if let Err(e) = runner.process_next_internal(recovery).await {
                    report.error = Some(format!("{e:?}"));
                    break;
                }
            }
            report.interrupted |= cancel.load(std::sync::atomic::Ordering::Relaxed);
            if report.error.is_none() && !report.interrupted {
                if let Err(e) = runner.advance_time(end) {
                    report.error = Some(format!("{e:?}"));
                } else if options.end_time_ms.is_some() {
                    report.end_reason = ReplayEndReason::EndTime;
                }
            }
        }
    }
    if report.error.is_some() {
        report.end_reason = ReplayEndReason::Error;
    } else if report.interrupted {
        report.end_reason = ReplayEndReason::Interrupted;
    }
    report.final_time_ms = runner.now_ms();
    report.pending_scheduled = runner.pending_scheduled();
    report.pending_handler = runner.handler_pending();
    report.pending_deliveries = runner.pending_delivery_count();
    report.incomplete |= report.pending_handler || report.pending_deliveries > 0;
    runner.stop_pending_handler();
    report.incomplete |= report.error.is_some() || report.interrupted;
    if report.error.is_some() || report.interrupted {
        runner
            .venue()
            .mark_market_incomplete("replay error or interruption");
    }
    report.settlement = runner.venue().paper_settlement_status();
    report.incomplete |= report.settlement.as_ref().is_some_and(|s| s.unsettled);
    if let Some(recorder) = runner.venue().recorder() {
        recorder.record_lazy("replay_outcome", || serde_json::json!({
            "settlement":report.settlement.as_ref().map(|s| s.recording_summary()),
            "end_reason":report.end_reason,"input_exhausted":report.input_exhausted,
            "final_time_ms":report.final_time_ms,"pending_scheduled":report.pending_scheduled,
            "pending_handler":report.pending_handler,"pending_deliveries":report.pending_deliveries,
            "records":report.records,"incomplete":report.incomplete,"interrupted":report.interrupted,
            "gaps":report.gaps,"last_input":report.last_input,"error":report.error}));
    }
    report
}

// sourceごとの安全な終端を先に確定するための、一時ファイルによる2-pass入力。
// 一般のIteratorを全件メモリ保持せず、元JSONも戦略への配送まで保持する。
struct SimulationSpool {
    file: Option<std::io::BufWriter<std::fs::File>>,
    path: std::path::PathBuf,
}
impl SimulationSpool {
    fn new() -> io::Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "bb-simulation-{}-{}.jsonl",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        file: Some(std::io::BufWriter::new(file)),
                        path,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
    fn append(&mut self, source: usize, record: &CaptureRecord) -> io::Result<()> {
        use std::io::Write;
        let file = self.file.as_mut().unwrap();
        serde_json::to_writer(&mut *file, &(source, record)).map_err(invalid)?;
        file.write_all(b"\n")
    }
    fn reader(
        &mut self,
    ) -> io::Result<impl Iterator<Item = io::Result<(usize, CaptureRecord)>> + '_> {
        use std::io::{Seek, Write};
        let file = self.file.as_mut().unwrap();
        file.flush()?;
        file.get_mut().rewind()?;
        Ok(BufReader::new(file.get_ref())
            .lines()
            .map(|line| serde_json::from_str(&line?).map_err(invalid)))
    }
}
impl Drop for SimulationSpool {
    fn drop(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// captureを検証してsourceごとの末尾を確定し、同じ先読みドライバへ渡す。
async fn run_simulation_replay<S: BotStrategy<V>, V: TradingVenue>(
    runner: &mut BotRunner<S, V>,
    input: impl IntoIterator<Item = io::Result<(usize, CaptureRecord)>>,
    options: ReplayOptions,
    cancel: &std::sync::atomic::AtomicBool,
    custom: impl FnMut(&CaptureRecord) -> io::Result<Option<S::CustomEvent>>,
) -> ReplayReport {
    let mut report = ReplayReport::default();
    let mut validation = ReplayValidation::default();
    let mut spool = match SimulationSpool::new() {
        Ok(spool) => spool,
        Err(error) => {
            runner
                .venue()
                .mark_market_incomplete("simulation spool creation failed");
            report.error = Some(error.to_string());
            report.end_reason = ReplayEndReason::Error;
            report.incomplete = true;
            return report;
        }
    };
    let mut tails = BTreeMap::new();
    // EOFのsource別時刻を調べる。入力は一時ファイルへ退避し、メモリへ蓄積しない。
    for item in input {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            report.interrupted = true;
            break;
        }
        match item.and_then(|(source, record)| {
            validation.validate(source, &record)?;
            Ok((source, record))
        }) {
            Ok((source, record)) => {
                let Ok(time) = i64::try_from(record.received_at_micros / 1000) else {
                    report.error = Some("time overflow".into());
                    break;
                };
                tails.insert(source, time);
                if let Err(error) = spool.append(source, &record) {
                    report.error = Some(error.to_string());
                    break;
                }
            }
            Err(e) => {
                report.error = Some(e.to_string());
                break;
            }
        }
    }
    report.input_exhausted = report.error.is_none() && !report.interrupted;
    let reader = spool.reader();
    if let Err(error) = &reader {
        report.error = Some(error.to_string());
    }
    let records = reader
        .into_iter()
        .flatten()
        .map(|r| r.map(|(source, record)| SimulationRecord::Captured(source, record)));
    run_validated_simulation_replay(
        runner,
        records,
        ReplaySetup {
            validation: &validation,
            tails: &tails,
            report,
            options,
            cancel,
        },
        custom,
        |_| None,
    )
    .await
}

struct ReplaySetup<'a> {
    validation: &'a ReplayValidation,
    tails: &'a BTreeMap<usize, i64>,
    report: ReplayReport,
    options: ReplayOptions,
    cancel: &'a std::sync::atomic::AtomicBool,
}

async fn run_validated_simulation_replay<'a, S: BotStrategy<V>, V: TradingVenue>(
    runner: &mut BotRunner<S, V>,
    records: impl Iterator<Item = io::Result<SimulationRecord<'a>>>,
    setup: ReplaySetup<'_>,
    mut custom: impl FnMut(&CaptureRecord) -> io::Result<Option<S::CustomEvent>>,
    mut compiled: impl FnMut(
        &mut crate::simulation::SimulationDriver<S::CustomEvent>,
    ) -> Option<Result<(), String>>,
) -> ReplayReport {
    let ReplaySetup {
        validation,
        tails,
        mut report,
        options,
        cancel,
    } = setup;
    let config = runner.venue().simulation_config().unwrap();
    let mut driver = match crate::simulation::SimulationDriver::new(
        config,
        runner.venue().simulation_time_model(),
    ) {
        Ok(driver) => driver,
        Err(e) => {
            report.error = Some(format!("{e:?}"));
            report.end_reason = ReplayEndReason::Error;
            report.incomplete = true;
            runner
                .venue()
                .mark_market_incomplete("invalid simulation configuration");
            report.settlement = runner.venue().paper_settlement_status();
            return report;
        }
    };
    if let Err(e) = runner.venue().begin_simulation() {
        report.error = Some(format!("{e:?}"));
    }
    let end = tails
        .values()
        .min()
        .and_then(|t| config.progress_limit(*t).ok())
        .map(|end| {
            options
                .end_time_ms
                .map_or(end, |requested| requested.min(end))
        });
    if end.zip(runner.now_ms()).is_some_and(|(end, now)| end < now) {
        report.error = Some("evaluation end precedes runner clock".into());
    }
    if report.error.is_none() && !report.interrupted {
        let mut records = records.peekable();
        while let Some(item) = records.next() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                report.interrupted = true;
                break;
            }
            let (source, record, decoded) = match item {
                Ok(item) => item.parts(),
                Err(error) => {
                    report.error = Some(error.to_string());
                    break;
                }
            };
            let received = (record.received_at_micros / 1000) as i64;
            if end
                .and_then(|t| t.checked_add(config.lookahead_ms))
                .is_some_and(|through| received > through)
            {
                break;
            }
            let id = InputId {
                session_id: record.session_id.clone(),
                sequence: record.sequence,
            };
            report.last_input = Some(id.clone());
            let admitted = if let Some(result) = compiled(&mut driver) {
                result
            } else {
                match &record.payload {
                    CapturePayload::Connected { subscriptions, .. } => {
                        let subscriptions = subscriptions
                            .iter()
                            .cloned()
                            .collect::<std::collections::BTreeSet<_>>();
                        let candidates = report
                            .gaps
                            .iter_mut()
                            .filter(|g| {
                                g.source_index == source
                                    && g.end.is_none()
                                    && !subscriptions.is_empty()
                                    && g.subscriptions == subscriptions
                            })
                            .collect::<Vec<_>>();
                        if candidates.len() == 1 {
                            candidates.into_iter().next().unwrap().end = Some(id.clone());
                        }
                        Ok(())
                    }
                    CapturePayload::Disconnected { reason } => {
                        driver.mark_incomplete(format!("source {source} disconnected: {reason}"));
                        if !options.recovery {
                            runner.venue().mark_market_incomplete(reason);
                        }
                        report.gaps.push(ReplayGap {
                            source_index: source,
                            connection_id: record.connection_id.clone(),
                            reason: reason.clone(),
                            subscriptions: validation.sources[&source].subscriptions
                                [&record.connection_id]
                                .clone(),
                            start: id.clone(),
                            end: None,
                        });
                        if !options.recovery {
                            report.end_reason = ReplayEndReason::Disconnected;
                        }
                        Ok(())
                    }
                    CapturePayload::Message { message } if record.exchange == "bitbank" => {
                        match decoded
                            .map_or_else(|| decode_bitbank(message), |message| Ok(message.clone()))
                        {
                            Ok(Some((pair, message))) => driver.admit_bitbank(
                                received,
                                Some(id),
                                format!("{source}:{}", record.connection_id),
                                pair,
                                message,
                            ),
                            Ok(None) => Ok(()),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                    CapturePayload::Message { .. } => match custom(&record) {
                        Ok(Some(event)) => {
                            driver.admit_event(received, Some(id), BotEvent::Custom(event))
                        }
                        Ok(None) => Ok(()),
                        Err(e) => Err(e.to_string()),
                    },
                }
            };
            report.records += 1;
            if let Err(e) = admitted {
                report.error = Some(e);
                break;
            }
            if report.end_reason == ReplayEndReason::Disconnected {
                break;
            }
            // 同msの全入力を登録してから境界を進める。
            if records.peek().is_none_or(|item| {
                item.as_ref()
                    .is_ok_and(|r| r.received_at_micros() / 1000 > received as u64)
            }) {
                if let Err(e) = driver.advance_to(runner, received, end, cancel).await {
                    report.interrupted = cancel.load(std::sync::atomic::Ordering::Relaxed);
                    if !report.interrupted {
                        report.error = Some(e);
                    }
                    break;
                }
            }
        }
    }
    if report.error.is_none()
        && !report.interrupted
        && report.end_reason != ReplayEndReason::Disconnected
    {
        if let Some(end) = end {
            match end.checked_add(config.lookahead_ms) {
                Some(through) => {
                    if let Err(error) = driver.advance_to(runner, through, Some(end), cancel).await
                    {
                        report.interrupted = cancel.load(std::sync::atomic::Ordering::Relaxed);
                        if !report.interrupted {
                            report.error = Some(error);
                        }
                    }
                }
                None => report.error = Some("simulation horizon overflow".into()),
            }
        }
    }
    if report.error.is_some() || report.interrupted {
        runner
            .venue()
            .mark_market_incomplete("simulation error or interruption");
    }
    if report.error.is_some() {
        report.end_reason = ReplayEndReason::Error;
    } else if report.interrupted {
        report.end_reason = ReplayEndReason::Interrupted;
    } else if report.end_reason != ReplayEndReason::Disconnected
        && options
            .end_time_ms
            .zip(end)
            .is_some_and(|(requested, end)| requested == end)
    {
        report.end_reason = ReplayEndReason::EndTime;
    }
    report.final_time_ms = runner.now_ms();
    report.pending_scheduled = runner.pending_scheduled();
    report.pending_handler = runner.handler_pending();
    report.pending_deliveries = runner.pending_delivery_count();
    report.simulation = Some(driver.report());
    report.settlement = runner.venue().paper_settlement_status();
    report.incomplete = report.error.is_some()
        || report.interrupted
        || !report.gaps.is_empty()
        || report.pending_handler
        || report.pending_deliveries > 0
        || report.settlement.as_ref().is_some_and(|s| s.unsettled);
    runner.stop_pending_handler();
    runner.venue().record_paper_settlement();
    if let Some(recorder) = runner.venue().recorder() {
        recorder.record_lazy("replay_outcome", || {
            let mut value = serde_json::to_value(&report).unwrap();
            value["settlement"] = report
                .settlement
                .as_ref()
                .map(|s| s.recording_summary())
                .unwrap_or(Value::Null);
            value
        });
    }
    report
}
