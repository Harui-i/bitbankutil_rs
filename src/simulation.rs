//! 発生時刻で内部市場を再生し、受信時刻で戦略へ配送する共通ドライバ。
use crate::{
    bitbank_bot::{
        BitbankInboundMessage, BitbankMarketEventConverter, BotEvent, BotRunner, BotStrategy,
        InputId,
    },
    market_event::{MarketDepthSnapshot, MarketEvent, MarketTrade},
    paper_execution::{PaperExecutionError, PaperTimeModel},
    trading_venue::TradingVenue,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SimulationConfig {
    pub lookahead_ms: i64,
}
impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            lookahead_ms: 10_000,
        }
    }
}
impl SimulationConfig {
    pub fn validate(self) -> Result<Self, PaperExecutionError> {
        if self.lookahead_ms < 0 {
            Err(PaperExecutionError::InvalidClock)
        } else {
            Ok(self)
        }
    }
    pub fn progress_limit(self, received_through_ms: i64) -> Result<i64, PaperExecutionError> {
        self.validate()?;
        received_through_ms
            .checked_sub(self.lookahead_ms)
            .ok_or(PaperExecutionError::InvalidClock)
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SimulationReport {
    pub config: SimulationConfig,
    pub received_through_ms: Option<i64>,
    pub evaluated_through_ms: Option<i64>,
    pub pending_market_events: usize,
    pub pending_deliveries: usize,
    pub issues: Vec<String>,
}

enum MarketInput {
    Raw {
        stream: String,
        pair: String,
        message: BitbankInboundMessage,
    },
    Event(MarketEvent),
}
struct InternalMarket {
    input_sequence: u64,
    id: Option<InputId>,
    received_at_ms: i64,
    exchange_at_ms: i64,
    input: MarketInput,
}
struct Delivery<C> {
    id: Option<InputId>,
    event: BotEvent<C>,
}
#[derive(Default)]
struct InternalBook {
    converter: Option<BitbankMarketEventConverter>,
    last_sequence: Option<u64>,
    initialized: bool,
}

/// 受信済み先読み入力を保持する。戦略からは所有せず、実行基盤だけが操作する。
/// `admit_*` を全て済ませてから、その受信範囲の `advance_to` を呼ぶ。
pub struct SimulationDriver<C> {
    config: SimulationConfig,
    time_model: PaperTimeModel,
    sequence: u64,
    market_sequence: u64,
    last_receipt_ms: Option<i64>,
    markets: BTreeMap<(i64, u64, u64), InternalMarket>,
    deliveries: BTreeMap<(i64, u64), Delivery<C>>,
    strategy_books: BTreeMap<(String, String), BitbankMarketEventConverter>,
    books: BTreeMap<(String, String), InternalBook>,
    seen_books: BTreeMap<(String, String, bool, u64), String>,
    seen_trades: BTreeMap<(String, i64), MarketTrade>,
    report: SimulationReport,
    reported_issues: usize,
    market_ids: BTreeMap<u64, u64>,
}
impl<C: Send + 'static> SimulationDriver<C> {
    pub fn new(
        config: SimulationConfig,
        time_model: PaperTimeModel,
    ) -> Result<Self, PaperExecutionError> {
        config.validate()?;
        Ok(Self {
            config,
            time_model,
            sequence: 0,
            market_sequence: 0,
            last_receipt_ms: None,
            markets: BTreeMap::new(),
            deliveries: BTreeMap::new(),
            strategy_books: BTreeMap::new(),
            books: BTreeMap::new(),
            seen_books: BTreeMap::new(),
            seen_trades: BTreeMap::new(),
            reported_issues: 0,
            market_ids: BTreeMap::new(),
            report: SimulationReport {
                config,
                ..Default::default()
            },
        })
    }
    pub fn report(&self) -> SimulationReport {
        SimulationReport {
            pending_market_events: self.markets.len(),
            pending_deliveries: self.deliveries.len(),
            ..self.report.clone()
        }
    }
    pub fn mark_incomplete(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        if !self.report.issues.contains(&reason) {
            self.report.issues.push(reason);
        }
    }
    fn sequence(&mut self, received: i64) -> Result<u64, String> {
        if self.last_receipt_ms.is_some_and(|t| received < t) {
            return Err("received time reversed".into());
        }
        self.last_receipt_ms = Some(received);
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("input sequence overflow")?;
        Ok(self.sequence)
    }
    fn market(
        &mut self,
        received: i64,
        exchange: i64,
        order: u64,
        id: Option<InputId>,
        input: MarketInput,
    ) -> Result<(), String> {
        let at = self
            .time_model
            .exchange_to_local_ms(exchange)
            .map_err(|e| format!("{e:?}"))?;
        if at > received {
            return Err("market occurrence is after receipt; check clock offset".into());
        }
        if received.checked_sub(at).ok_or("market delay overflow")? > self.config.lookahead_ms {
            return Err("market exceeded simulation lookahead".into());
        }
        if self.report.evaluated_through_ms.is_some_and(|t| at <= t) {
            return Err("unknown market event arrived behind simulation boundary".into());
        }
        self.market_sequence = self
            .market_sequence
            .checked_add(1)
            .ok_or("market sequence overflow")?;
        self.markets.insert(
            (at, order, self.market_sequence),
            InternalMarket {
                input_sequence: self.sequence,
                id,
                received_at_ms: received,
                exchange_at_ms: exchange,
                input,
            },
        );
        Ok(())
    }
    /// 生のbitbank板を別々のconverterへ渡す。streamは接続世代を含む識別子。
    pub fn admit_bitbank(
        &mut self,
        received: i64,
        id: Option<InputId>,
        stream: String,
        pair: String,
        message: BitbankInboundMessage,
    ) -> Result<(), String> {
        let result = self.admit_bitbank_inner(received, id, stream, pair, message);
        if let Err(error) = &result {
            self.mark_incomplete(error.clone());
        }
        result
    }
    pub fn admit_event(
        &mut self,
        received: i64,
        id: Option<InputId>,
        event: BotEvent<C>,
    ) -> Result<(), String> {
        let result = self.admit_event_inner(received, id, event);
        if let Err(error) = &result {
            self.mark_incomplete(error.clone());
        }
        result
    }
    fn admit_bitbank_inner(
        &mut self,
        received: i64,
        id: Option<InputId>,
        stream: String,
        pair: String,
        message: BitbankInboundMessage,
    ) -> Result<(), String> {
        let seq = self.sequence(received)?;
        let (exchange, book_seq, whole) = match &message {
            BitbankInboundMessage::DepthWhole(v) => (
                v.timestamp,
                Some(v.sequenceId.parse::<u64>().map_err(|e| e.to_string())?),
                true,
            ),
            BitbankInboundMessage::DepthDiff(v) => (
                v.t,
                Some(v.s.parse::<u64>().map_err(|e| e.to_string())?),
                false,
            ),
            BitbankInboundMessage::Ticker(v) => (
                v.timestamp.as_i64().ok_or("invalid ticker timestamp")?,
                None,
                false,
            ),
            BitbankInboundMessage::CircuitBreakInfo(v) => (
                v.timestamp
                    .as_i64()
                    .ok_or("invalid market mode timestamp")?,
                None,
                false,
            ),
            BitbankInboundMessage::Transactions(_) => (0, None, false),
        };
        if let Some(book_seq) = book_seq {
            let key = (stream.clone(), pair.clone(), whole, book_seq);
            let content = format!("{message:?}");
            if let Some(previous) = self.seen_books.get(&key) {
                return if previous == &content {
                    Ok(())
                } else {
                    Err("conflicting book sequence".into())
                };
            }
            self.seen_books.insert(key, content);
        }
        // このconverterは戦略向け。先読み中に生成しても配送時刻まで公開しない。
        let event = self
            .strategy_books
            .entry((stream.clone(), pair.clone()))
            .or_insert_with(|| BitbankMarketEventConverter::new(pair.clone()))
            .convert(message.clone())
            .map_err(|e| format!("{e:?}"))?;
        match message {
            BitbankInboundMessage::Transactions(trades) => {
                for trade in trades {
                    let trade = MarketTrade::try_from(trade).map_err(|e| format!("{e:?}"))?;
                    self.admit_trade(received, id.clone(), pair.clone(), trade)?;
                }
            }
            message => self.market(
                received,
                exchange,
                book_seq.unwrap_or(seq),
                id.clone(),
                MarketInput::Raw {
                    stream,
                    pair,
                    message,
                },
            )?,
        }
        if let Some(event) = event {
            self.deliveries.insert(
                (received, seq),
                Delivery {
                    id,
                    event: BotEvent::Market(event),
                },
            );
        }
        Ok(())
    }
    fn admit_trade(
        &mut self,
        received: i64,
        id: Option<InputId>,
        pair: String,
        trade: MarketTrade,
    ) -> Result<(), String> {
        let key = (pair.clone(), trade.transaction_id);
        if let Some(old) = self.seen_trades.get(&key) {
            return if old == &trade {
                Ok(())
            } else {
                Err("conflicting transaction ID".into())
            };
        }
        self.seen_trades.insert(key, trade.clone());
        // 複数約定は同じ入力sequenceを共有するため、transaction IDをtie-breakへ使う。
        let order = u64::try_from(trade.transaction_id).map_err(|_| "negative transaction ID")?;
        self.market(
            received,
            trade.executed_at,
            order,
            id,
            MarketInput::Event(MarketEvent::Transactions {
                pair,
                transactions: vec![trade],
            }),
        )
    }
    /// 外部データやカスタム入力も元の受信時刻で配送する。
    /// DepthUpdatedは独立snapshotとしてのみ利用する。生差分はadmit_bitbankを使用する。
    fn admit_event_inner(
        &mut self,
        received: i64,
        id: Option<InputId>,
        event: BotEvent<C>,
    ) -> Result<(), String> {
        let seq = self.sequence(received)?;
        if let BotEvent::Market(market) = &event {
            match market {
                MarketEvent::Transactions { pair, transactions } => {
                    for trade in transactions {
                        self.admit_trade(received, id.clone(), pair.clone(), trade.clone())?;
                    }
                }
                _ => {
                    let exchange = match market {
                        MarketEvent::DepthUpdated { depth, .. } => depth.last_timestamp(),
                        MarketEvent::Ticker { ticker, .. } => ticker
                            .timestamp
                            .as_i64()
                            .ok_or("invalid ticker timestamp")?,
                        MarketEvent::CircuitBreakInfo { info, .. } => {
                            info.timestamp.as_i64().ok_or("invalid mode timestamp")?
                        }
                        _ => unreachable!(),
                    };
                    self.market(
                        received,
                        exchange,
                        seq,
                        id.clone(),
                        MarketInput::Event(market.clone()),
                    )?;
                }
            }
        }
        self.deliveries
            .insert((received, seq), Delivery { id, event });
        Ok(())
    }
    fn convert_internal(&mut self, input: MarketInput) -> Result<Option<MarketEvent>, String> {
        match input {
            MarketInput::Event(event) => Ok(Some(event)),
            MarketInput::Raw {
                stream,
                pair,
                message,
            } => {
                let book = self.books.entry((stream, pair.clone())).or_default();
                let sequence = match &message {
                    BitbankInboundMessage::DepthWhole(v) => {
                        Some(v.sequenceId.parse::<u64>().map_err(|e| e.to_string())?)
                    }
                    BitbankInboundMessage::DepthDiff(v) => {
                        Some(v.s.parse::<u64>().map_err(|e| e.to_string())?)
                    }
                    _ => None,
                };
                if let Some(sequence) = sequence {
                    if book.last_sequence.is_some_and(|s| sequence < s) {
                        return Err("book sequence contradicts exchange timestamp order".into());
                    }
                    book.last_sequence = Some(sequence);
                    if matches!(message, BitbankInboundMessage::DepthWhole(_)) {
                        book.initialized = true;
                    }
                    if !book.initialized {
                        // snapshot以前の差分を保持して後から遡及適用しない。
                        return Ok(Some(MarketEvent::DepthUpdated {
                            pair,
                            depth: MarketDepthSnapshot::empty(),
                        }));
                    }
                }
                book.converter
                    .get_or_insert_with(|| BitbankMarketEventConverter::new(pair))
                    .convert(message)
                    .map_err(|e| format!("{e:?}"))
            }
        }
    }
    /// 入力を受信済みの範囲だけ進める。Paperもbacktestもこの関数を使用する。
    pub async fn advance_to<S: BotStrategy<V, CustomEvent = C>, V: TradingVenue>(
        &mut self,
        runner: &mut BotRunner<S, V>,
        received_through: i64,
        end: Option<i64>,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), String> {
        for issue in &self.report.issues[self.reported_issues..] {
            runner.venue().mark_market_incomplete(issue);
        }
        self.reported_issues = self.report.issues.len();
        let mut limit = self
            .config
            .progress_limit(received_through)
            .map_err(|e| format!("{e:?}"))?;
        if let Some(end) = end {
            limit = limit.min(end);
        }
        if self.report.evaluated_through_ms.is_some_and(|t| limit < t) {
            return Err("simulation limit reversed".into());
        }
        loop {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("simulation interrupted".into());
            }
            let next = self
                .markets
                .first_key_value()
                .map(|(k, _)| k.0)
                .into_iter()
                .chain(self.deliveries.first_key_value().map(|(k, _)| k.0))
                .chain(runner.next_internal_time())
                .min();
            let Some(time) = next.filter(|t| *t <= limit) else {
                break;
            };
            runner.advance_time(time).map_err(|e| format!("{e:?}"))?;
            while self
                .markets
                .first_key_value()
                .is_some_and(|(k, _)| k.0 == time)
            {
                let (_, entry) = self.markets.pop_first().unwrap();
                let market_id = runner.venue().recorder().map(|recorder| {
                    if self
                        .deliveries
                        .contains_key(&(entry.received_at_ms, entry.input_sequence))
                    {
                        *self
                            .market_ids
                            .entry(entry.input_sequence)
                            .or_insert_with(|| recorder.next_id())
                    } else {
                        recorder.next_id()
                    }
                });
                let context = crate::execution_recording::RecordingContext {
                    market_event_id: market_id,
                    event_time_ms: Some(time),
                    input_id: entry.id.clone(),
                    ..Default::default()
                };
                if let Some(recorder) = runner.venue().recorder() {
                    crate::execution_recording::sync_scope(context.clone(), || {
                        recorder.record_lazy("simulation_market", || serde_json::json!({
                        "received_at_ms":entry.received_at_ms,"exchange_at_ms":entry.exchange_at_ms,"simulation_local_ms":time,"input_id":entry.id
                    }))
                    });
                }
                let event = self.convert_internal(entry.input)?;
                if let Some(event) = event {
                    crate::execution_recording::sync_scope(context, || {
                        runner
                            .venue()
                            .on_simulation_market(&event, entry.received_at_ms)
                    })
                    .map_err(|e| format!("{e:?}"))?;
                }
            }
            runner
                .venue()
                .process_internal_events(time)
                .map_err(|e| format!("{e:?}"))?;
            while self
                .deliveries
                .first_key_value()
                .is_some_and(|(k, _)| k.0 == time)
            {
                let ((_, sequence), entry) = self.deliveries.pop_first().unwrap();
                let market_id = if matches!(entry.event, BotEvent::Market(_)) {
                    self.market_ids
                        .remove(&sequence)
                        .or_else(|| runner.venue().recorder().map(|r| r.next_id()))
                } else {
                    None
                };
                runner
                    .enqueue_simulation_delivery(time, entry.id, market_id, entry.event, true)
                    .map_err(|e| format!("{e:?}"))?;
            }
            // process_next_internalは予約イベントも処理する。市場・口座処理は上で完了済み。
            if runner.next_internal_time().is_some_and(|t| t <= time) {
                runner
                    .process_next_internal(true)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
            } else {
                runner.poll_replay().await.map_err(|e| format!("{e:?}"))?;
            }
            self.report.evaluated_through_ms = Some(time);
        }
        if runner.now_ms().is_some_and(|t| t <= limit) {
            runner.advance_time(limit).map_err(|e| format!("{e:?}"))?;
            runner
                .venue()
                .process_internal_events(limit)
                .map_err(|e| format!("{e:?}"))?;
            self.report.evaluated_through_ms = Some(limit);
        }
        self.report.received_through_ms = Some(received_through);
        Ok(())
    }
}
