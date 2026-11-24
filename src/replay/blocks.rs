//! One compiled representation, supplied either from RAM or a bounded incremental
//! compiler. Exchange-order conversion uses a persistent lookahead frontier.
use super::*;
use crate::simulation::{PreparedEvents, SimulationDriver};
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, VecDeque},
    sync::{Arc, mpsc},
};

#[derive(Clone, Copy, Debug)]
pub struct BlockOptions {
    pub records_per_block: usize,
    /// Maximum not-yet-emitted input records, including the lookahead window.
    pub max_pending_records: usize,
}
impl Default for BlockOptions {
    fn default() -> Self {
        Self {
            records_per_block: 4096,
            max_pending_records: 65536,
        }
    }
}
/// Immutable strategy-independent events shared by all cases. Case state lives
/// exclusively in the consuming runners; blocks may be freed after last use.
pub struct CompiledBlock<C> {
    pub(super) records: Vec<(usize, CaptureRecord)>,
    pub(super) events: PreparedEvents<C>,
    pub(super) apply: fn(&Self, usize, &mut SimulationDriver<C>) -> Option<Result<(), String>>,
    config: crate::simulation::SimulationConfig,
    time_model: crate::paper_execution::PaperTimeModel,
}
impl<C> CompiledBlock<C> {
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}
struct Pending<C> {
    block: CompiledBlock<C>,
    remaining: usize,
}
/// Stateful compiler. The bound is on pending records, not on per-notification
/// size, converter history, or case state. Insufficient lookahead capacity fails
/// explicitly rather than dropping data or growing without a limit.
pub struct BlockCompiler<C, I: Iterator, D> {
    input: std::iter::Peekable<I>,
    custom: D,
    driver: SimulationDriver<C>,
    options: BlockOptions,
    config: crate::simulation::SimulationConfig,
    time_model: crate::paper_execution::PaperTimeModel,
    pending: VecDeque<Pending<C>>,
    heap: BinaryHeap<Reverse<((i64, u64, u64), u64, usize)>>,
    first_id: u64,
    next_id: u64,
    pending_records: usize,
    ended: bool,
    error: Option<io::Error>,
    failed: bool,
}
impl<C, I, D> BlockCompiler<C, I, D>
where
    C: Clone + Send + 'static,
    I: Iterator<Item = io::Result<(usize, CaptureRecord)>>,
    D: FnMut(&CaptureRecord) -> io::Result<Option<C>>,
{
    pub fn new(
        input: I,
        config: crate::simulation::SimulationConfig,
        time_model: crate::paper_execution::PaperTimeModel,
        custom: D,
        options: BlockOptions,
    ) -> io::Result<Self> {
        if options.records_per_block == 0
            || options.records_per_block > 65536
            || options.max_pending_records < options.records_per_block
        {
            return Err(invalid(
                "invalid compiled block batch size / pending record limits",
            ));
        }
        let driver =
            SimulationDriver::new(config, time_model).map_err(|e| invalid(format!("{e:?}")))?;
        Ok(Self {
            input: input.peekable(),
            custom,
            driver,
            options,
            config,
            time_model,
            pending: VecDeque::new(),
            heap: BinaryHeap::new(),
            first_id: 0,
            next_id: 0,
            pending_records: 0,
            ended: false,
            error: None,
            failed: false,
        })
    }
    fn compile_through(&mut self, limit: i64) {
        while self
            .heap
            .peek()
            .is_some_and(|Reverse((key, _, _))| key.0 <= limit)
        {
            let Reverse((_, id, index)) = self.heap.pop().unwrap();
            let pending = &mut self.pending[(id - self.first_id) as usize];
            self.driver
                .compile_market_at(&mut pending.block.events, index);
            pending.remaining -= 1;
        }
    }
    fn read_block(&mut self) -> io::Result<()> {
        let mut records = Vec::with_capacity(self.options.records_per_block);
        let mut events = PreparedEvents::new(self.options.records_per_block);
        for _ in 0..self.options.records_per_block {
            let item = match self.input.next() {
                Some(Ok(r)) => r,
                Some(Err(e)) => {
                    self.ended = true;
                    self.error = Some(e);
                    break;
                }
                None => {
                    self.ended = true;
                    break;
                }
            };
            if self.pending_records + records.len() >= self.options.max_pending_records {
                return Err(invalid(
                    "compiled block lookahead exceeds max_pending_records; increase the limit",
                ));
            }
            let (source, mut record) = item;
            let received = i64::try_from(record.received_at_micros / 1000).map_err(invalid)?;
            let id = Some(InputId {
                session_id: record.session_id.clone(),
                sequence: record.sequence,
            });
            let result = match &record.payload {
                CapturePayload::Message { message } if record.exchange == "bitbank" => {
                    decode_bitbank(message).map(|event| {
                        event.map(|(pair, message)| {
                            self.driver.admit_bitbank(
                                received,
                                id,
                                format!("{source}:{}", record.connection_id),
                                pair,
                                message,
                            )
                        })
                    })
                }
                CapturePayload::Message { .. } => (self.custom)(&record).map(|event| {
                    event.map(|event| {
                        self.driver
                            .admit_event(received, id, BotEvent::Custom(event))
                    })
                }),
                _ => Ok(None),
            };
            let admission = match result {
                Ok(result) => result.map(|r| self.driver.take_admission(r, &mut events)),
                Err(e) => {
                    self.ended = true;
                    self.error = Some(e);
                    break;
                }
            };
            events.admissions.push(admission);
            if matches!(record.payload, CapturePayload::Message { .. }) {
                record.payload = CapturePayload::Message {
                    message: Value::Null,
                };
            }
            records.push((source, record));
        }
        if !records.is_empty() {
            let count = events.market_keys().count();
            for (index, key) in events.market_keys() {
                self.heap.push(Reverse((key, self.next_id, index)));
            }
            self.next_id = self
                .next_id
                .checked_add(1)
                .ok_or_else(|| invalid("block id overflow"))?;
            self.pending_records += records.len();
            self.pending.push_back(Pending {
                remaining: count,
                block: CompiledBlock {
                    records,
                    events,
                    config: self.config,
                    time_model: self.time_model,
                    apply: |b, i, d| {
                        b.events.admissions[i]
                            .as_ref()
                            .map(|a| d.admit_prepared(a, &b.events))
                    },
                },
            });
        }
        let limit = if self.ended {
            i64::MAX
        } else {
            match self.input.peek() {
                None => {
                    self.ended = true;
                    i64::MAX
                }
                Some(Err(_)) => i64::MIN,
                Some(Ok((_, next))) => {
                    let received =
                        i64::try_from(next.received_at_micros / 1000).map_err(invalid)?;
                    // Unread events can occur exactly at received-lookahead;
                    // use strict inequality, including same-ms block boundaries.
                    self.config
                        .progress_limit(received)
                        .map_err(|e| invalid(format!("{e:?}")))?
                        .saturating_sub(1)
                }
            }
        };
        self.compile_through(limit);
        Ok(())
    }
}
impl<C, I, D> Iterator for BlockCompiler<C, I, D>
where
    C: Clone + Send + 'static,
    I: Iterator<Item = io::Result<(usize, CaptureRecord)>>,
    D: FnMut(&CaptureRecord) -> io::Result<Option<C>>,
{
    type Item = io::Result<Arc<CompiledBlock<C>>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            if self.pending.front().is_some_and(|p| p.remaining == 0) {
                let pending = self.pending.pop_front().unwrap();
                self.first_id += 1;
                self.pending_records -= pending.block.records.len();
                return Some(Ok(Arc::new(pending.block)));
            }
            if self.ended {
                self.compile_through(i64::MAX);
                if !self.pending.is_empty() {
                    continue;
                }
                self.failed = true;
                return self.error.take().map(Err);
            }
            if let Err(error) = self.read_block() {
                self.failed = true;
                return Some(Err(error));
            }
        }
    }
}
/// Full retention of exactly the same blocks consumed by streamed execution.
pub struct CompiledBlockCache<C> {
    blocks: Vec<Arc<CompiledBlock<C>>>,
}
impl<C> CompiledBlockCache<C> {
    pub fn collect(
        source: impl IntoIterator<Item = io::Result<Arc<CompiledBlock<C>>>>,
    ) -> io::Result<Self> {
        Ok(Self {
            blocks: source.into_iter().collect::<io::Result<_>>()?,
        })
    }
    pub fn iter(&self) -> impl Iterator<Item = io::Result<Arc<CompiledBlock<C>>>> + '_ {
        self.blocks.iter().map(|b| Ok(b.clone()))
    }
}
pub async fn run_compiled_block_source<S, V, I>(
    runner: &mut BotRunner<S, V>,
    source: I,
    metadata: &StreamingReplayMetadata,
    options: ReplayOptions,
    cancel: &std::sync::atomic::AtomicBool,
) -> ReplayReport
where
    S: BotStrategy<V>,
    V: TradingVenue,
    S::CustomEvent: Clone,
    I: IntoIterator<Item = io::Result<Arc<CompiledBlock<S::CustomEvent>>>>,
{
    let config = runner.venue().simulation_config();
    let model = runner.venue().simulation_time_model();
    if config.is_none() {
        return ReplayReport {
            error: Some("compiled blocks require simulation venue".into()),
            incomplete: true,
            end_reason: ReplayEndReason::Error,
            ..Default::default()
        };
    }
    let mut source = source.into_iter();
    let mut current = None::<Arc<CompiledBlock<S::CustomEvent>>>;
    let mut index = 0;
    let mut ended = false;
    let records = std::iter::from_fn(move || {
        loop {
            if let Some(block) = &current {
                if index < block.records.len() {
                    let result = SimulationRecord::Block {
                        block: block.clone(),
                        index,
                    };
                    index += 1;
                    return Some(Ok(result));
                }
            }
            if ended {
                return None;
            }
            match source.next() {
                Some(Ok(block)) => {
                    if Some(block.config) != config || block.time_model != model {
                        ended = true;
                        return Some(Err(invalid(
                            "compiled block simulation configuration mismatch",
                        )));
                    }
                    current = Some(block);
                    index = 0;
                }
                Some(Err(e)) => {
                    ended = true;
                    return Some(Err(e));
                }
                None => {
                    ended = true;
                    return None;
                }
            }
        }
    });
    run_validated_simulation_replay(
        runner,
        records,
        ReplaySetup {
            validation: &metadata.validation,
            tails: &metadata.tails,
            report: ReplayReport {
                input_exhausted: true,
                ..Default::default()
            },
            options,
            cancel,
        },
        |_| None,
    )
    .await
}

enum Transfer<C> {
    Block(Arc<CompiledBlock<C>>),
    End,
    Error(String),
}
pub struct SharedBlockResults<R> {
    pub cases: Vec<Result<(R, ReplayReport), String>>,
    pub source_error: Option<String>,
}
/// Broadcast a block once to fresh cases. One producer (calling thread), one
/// consumer thread per case, and at most one waiting block per consumer.
/// A failed/dropped consumer disconnects the source instead of deadlocking.
pub fn run_shared_blocks<S, V, I, F>(
    source: I,
    factories: Vec<F>,
    metadata: &StreamingReplayMetadata,
    options: ReplayOptions,
    cancel: &std::sync::atomic::AtomicBool,
) -> SharedBlockResults<BotRunner<S, V>>
where
    S: BotStrategy<V>,
    V: TradingVenue,
    S::CustomEvent: Clone + Sync,
    I: IntoIterator<Item = io::Result<Arc<CompiledBlock<S::CustomEvent>>>>,
    F: FnOnce() -> Result<BotRunner<S, V>, String> + Send,
{
    std::thread::scope(|scope| {
        let mut senders = Vec::new();
        let mut handles = Vec::new();
        for factory in factories {
            let (tx, rx) = mpsc::sync_channel(1);
            senders.push(tx);
            handles.push(scope.spawn(move || {
                let mut runner = factory()?;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                let mut ended = false;
                let blocks = std::iter::from_fn(move || {
                    if ended {
                        return None;
                    }
                    match rx.recv() {
                        Ok(Transfer::Block(b)) => Some(Ok(b)),
                        Ok(Transfer::End) => {
                            ended = true;
                            None
                        }
                        Ok(Transfer::Error(e)) => {
                            ended = true;
                            Some(Err(invalid(e)))
                        }
                        Err(_) => {
                            ended = true;
                            Some(Err(invalid("shared block source disconnected")))
                        }
                    }
                });
                let report = runtime.block_on(run_compiled_block_source(
                    &mut runner,
                    blocks,
                    metadata,
                    options,
                    cancel,
                ));
                Ok((runner, report))
            }));
        }
        let mut source_error = None;
        if !senders.is_empty() {
            for item in source {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    source_error = Some("shared block compilation interrupted".into());
                    break;
                }
                let block = match item {
                    Ok(b) => b,
                    Err(e) => {
                        source_error = Some(e.to_string());
                        break;
                    }
                };
                if senders
                    .iter()
                    .any(|tx| tx.send(Transfer::Block(block.clone())).is_err())
                {
                    source_error = Some("shared block consumer disconnected".into());
                    break;
                }
            }
            for tx in &senders {
                let _ = tx.send(match &source_error {
                    Some(e) => Transfer::Error(e.clone()),
                    None => Transfer::End,
                });
            }
        }
        drop(senders);
        let cases: Vec<Result<_, String>> = handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("shared block case panicked".into()))
            })
            .collect();
        if source_error.as_deref() == Some("shared block consumer disconnected")
            && cases.iter().all(|case| {
                case.as_ref().is_ok_and(|(_, r)| {
                    r.error.is_none()
                        && !r.interrupted
                        && options.end_time_ms.is_some_and(|end| {
                            r.simulation
                                .as_ref()
                                .and_then(|s| s.evaluated_through_ms)
                                .is_some_and(|t| t >= end)
                        })
                })
            })
        {
            source_error = None;
        }
        SharedBlockResults {
            cases,
            source_error,
        }
    })
}

/// Single-case streamed supply of the same compiled blocks as a full cache.
pub async fn run_streaming_compiled<S, V, I, D>(
    runner: &mut BotRunner<S, V>,
    input: I,
    metadata: &StreamingReplayMetadata,
    options: ReplayOptions,
    cancel: &std::sync::atomic::AtomicBool,
    custom: D,
    limits: BlockOptions,
) -> ReplayReport
where
    S: BotStrategy<V>,
    V: TradingVenue,
    S::CustomEvent: Clone + Sync,
    I: IntoIterator<Item = io::Result<(usize, CaptureRecord)>> + Send + 'static,
    I::IntoIter: Send,
    D: FnMut(&CaptureRecord) -> io::Result<Option<S::CustomEvent>> + Send + 'static,
{
    let Some(config) = runner.venue().simulation_config() else {
        return ReplayReport {
            error: Some("compiled blocks require simulation venue".into()),
            incomplete: true,
            end_reason: ReplayEndReason::Error,
            ..Default::default()
        };
    };
    let model = runner.venue().simulation_time_model();
    let compiler = match BlockCompiler::new(input.into_iter(), config, model, custom, limits) {
        Ok(compiler) => compiler,
        Err(e) => {
            return ReplayReport {
                error: Some(e.to_string()),
                incomplete: true,
                end_reason: ReplayEndReason::Error,
                ..Default::default()
            };
        }
    };
    run_prefetched_compiled(runner, compiler, metadata, options, cancel).await
}
/// Supply cached or generated blocks on one producer thread with one queued
/// block. The exact same simulation consumer executes both sources.
pub async fn run_prefetched_compiled<S, V, I>(
    runner: &mut BotRunner<S, V>,
    source: I,
    metadata: &StreamingReplayMetadata,
    options: ReplayOptions,
    cancel: &std::sync::atomic::AtomicBool,
) -> ReplayReport
where
    S: BotStrategy<V>,
    V: TradingVenue,
    S::CustomEvent: Clone + Sync,
    I: IntoIterator<Item = io::Result<Arc<CompiledBlock<S::CustomEvent>>>> + Send + 'static,
    I::IntoIter: Send,
{
    let (tx, rx) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = (|| {
            for block in source {
                if tx.send(Transfer::Block(block?)).is_err() {
                    return Ok(());
                }
            }
            Ok::<(), io::Error>(())
        })();
        let _ = tx.send(match result {
            Ok(()) => Transfer::End,
            Err(e) => Transfer::Error(e.to_string()),
        });
    });
    let mut ended = false;
    let source = std::iter::from_fn(move || {
        if ended {
            return None;
        }
        match rx.recv() {
            Ok(Transfer::Block(b)) => Some(Ok(b)),
            Ok(Transfer::End) => {
                ended = true;
                None
            }
            Ok(Transfer::Error(e)) => {
                ended = true;
                Some(Err(invalid(e)))
            }
            Err(_) => {
                ended = true;
                Some(Err(invalid("compiled block worker disconnected")))
            }
        }
    });
    let mut report = run_compiled_block_source(runner, source, metadata, options, cancel).await;
    if worker.join().is_err() {
        report.error = Some("compiled block worker panicked".into());
        report.incomplete = true;
        report.end_reason = ReplayEndReason::Error;
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> Vec<io::Result<(usize, CaptureRecord)>> {
        ReplayInput::open(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/shared-replay/manifest.json"
        ))
        .unwrap()
        .collect()
    }
    struct Noop;
    impl BotStrategy<crate::paper_execution::PaperTradingVenue> for Noop {
        type CustomEvent = ();
        type Error = String;
        async fn handle_event(
            &mut self,
            _: BotEvent<()>,
            _: &crate::bitbank_bot::BotContext<(), crate::paper_execution::PaperTradingVenue>,
        ) -> Result<(), String> {
            Ok(())
        }
    }
    fn runner() -> BotRunner<Noop, crate::paper_execution::PaperTradingVenue> {
        use crate::paper_execution::*;
        let config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
        let balances = ["btc", "jpy"]
            .into_iter()
            .map(|asset| crate::order_domain::BalanceSnapshot {
                asset: asset.into(),
                free_amount: crate::book_units::literal("0"),
                locked_amount: crate::book_units::literal("0"),
                onhand_amount: crate::book_units::literal("0"),
            })
            .collect();
        let engine =
            PaperExecutionEngine::new(config, PaperClock::Replay { now_ms: 0 }, balances, None)
                .unwrap();
        BotRunner::new(
            Noop,
            PaperTradingVenue::new(engine)
                .with_simulation_config(crate::simulation::SimulationConfig { lookahead_ms: 30 })
                .unwrap(),
        )
    }
    fn compiler() -> impl Iterator<Item = io::Result<Arc<CompiledBlock<()>>>> {
        BlockCompiler::new(
            input().into_iter(),
            crate::simulation::SimulationConfig { lookahead_ms: 30 },
            Default::default(),
            |_| Ok::<Option<()>, io::Error>(None),
            BlockOptions {
                records_per_block: 1,
                max_pending_records: 65536,
            },
        )
        .unwrap()
    }
    #[test]
    fn factory_failure_and_consumer_panic_do_not_deadlock() {
        let metadata = StreamingReplayMetadata::default();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let failed =
            || -> Result<BotRunner<Noop, crate::paper_execution::PaperTradingVenue>, String> {
                Err("factory failure".into())
            };
        let result = run_shared_blocks(
            compiler(),
            vec![failed],
            &metadata,
            ReplayOptions::default(),
            &cancel,
        );
        assert_eq!(result.cases[0].as_ref().err().unwrap(), "factory failure");
        assert!(result.source_error.is_some());
        let panics =
            || -> Result<BotRunner<Noop, crate::paper_execution::PaperTradingVenue>, String> {
                panic!("factory panic")
            };
        let result = run_shared_blocks(
            compiler(),
            vec![panics],
            &metadata,
            ReplayOptions::default(),
            &cancel,
        );
        assert!(result.cases[0].as_ref().err().unwrap().contains("panicked"));
    }
    #[tokio::test]
    async fn configuration_mismatch_and_source_failure_are_reported() {
        let metadata = StreamingReplayMetadata::default();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut runner = runner();
        let mut blocks = compiler().collect::<io::Result<Vec<_>>>().unwrap();
        Arc::get_mut(&mut blocks[0]).unwrap().config.lookahead_ms = 31;
        let report = run_compiled_block_source(
            &mut runner,
            blocks.into_iter().map(Ok),
            &metadata,
            ReplayOptions::default(),
            &cancel,
        )
        .await;
        assert!(report.error.unwrap().contains("configuration mismatch"));
        let mut runner = super::tests::runner();
        let report = run_prefetched_compiled(
            &mut runner,
            vec![Err(invalid("disk corrupt"))],
            &metadata,
            ReplayOptions::default(),
            &cancel,
        )
        .await;
        assert!(report.error.unwrap().contains("disk corrupt"));
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut runner = super::tests::runner();
        let report = run_prefetched_compiled(
            &mut runner,
            compiler().collect::<Vec<_>>(),
            &metadata,
            ReplayOptions::default(),
            &cancel,
        )
        .await;
        assert!(report.interrupted);
    }
    #[tokio::test]
    async fn empty_cancel_and_inconsistent_metadata_are_explicit() {
        let metadata = StreamingReplayMetadata::default();
        let cancel = std::sync::atomic::AtomicBool::new(true);
        let mut target = runner();
        let report = run_prefetched_compiled(
            &mut target,
            Vec::new(),
            &metadata,
            ReplayOptions::default(),
            &cancel,
        )
        .await;
        assert!(report.interrupted && report.incomplete);
        cancel.store(false, std::sync::atomic::Ordering::Relaxed);
        let records = input().into_iter().map(|item| {
            item.map(|(source, mut record)| {
                if record.sequence == 6 && source == 0 {
                    record.payload = CapturePayload::Disconnected {
                        reason: "gap".into(),
                    };
                }
                (source, record)
            })
        });
        let blocks = BlockCompiler::new(
            records,
            crate::simulation::SimulationConfig { lookahead_ms: 30 },
            Default::default(),
            |_| Ok::<Option<()>, io::Error>(None),
            BlockOptions::default(),
        )
        .unwrap();
        let mut target = runner();
        let report = run_compiled_block_source(
            &mut target,
            blocks,
            &metadata,
            ReplayOptions::default(),
            &cancel,
        )
        .await;
        assert!(
            report
                .error
                .as_deref()
                .is_some_and(|e| e.contains("preflight subscriptions")),
            "{report:?}"
        );
    }
    #[test]
    fn oversized_batch_is_an_error_before_allocation() {
        let result = BlockCompiler::new(
            input().into_iter(),
            Default::default(),
            Default::default(),
            |_| Ok::<Option<()>, io::Error>(None),
            BlockOptions {
                records_per_block: usize::MAX,
                max_pending_records: usize::MAX,
            },
        );
        assert!(result.is_err());
    }
    #[test]
    fn boundaries_and_late_exchange_order_match_full_compile() {
        let prepared = PreparedReplay::new(input()).unwrap();
        let config = crate::simulation::SimulationConfig { lookahead_ms: 30 };
        let full = prepared
            .compile::<()>(config, Default::default(), |_| Ok(None))
            .unwrap();
        let expected = full.events.compiled_signature();
        assert!(!expected.is_empty());
        for size in [1, 2, 3, 8, 4096] {
            let compiler = BlockCompiler::new(
                input().into_iter(),
                config,
                Default::default(),
                |_| Ok::<Option<()>, io::Error>(None),
                BlockOptions {
                    records_per_block: size,
                    max_pending_records: 65536,
                },
            )
            .unwrap();
            let mut actual = Vec::new();
            let mut count = 0;
            for block in compiler {
                let block = block.unwrap();
                count += block.len();
                actual.extend(block.events.compiled_signature());
            }
            actual.sort_by_key(|v| v.0);
            assert_eq!(actual, expected, "block size {size}");
            assert_eq!(count, prepared.len());
        }
    }
    #[test]
    fn insufficient_lookahead_capacity_fails_explicitly() {
        let compiler = BlockCompiler::new(
            input().into_iter(),
            crate::simulation::SimulationConfig {
                lookahead_ms: 1000000,
            },
            Default::default(),
            |_| Ok::<Option<()>, io::Error>(None),
            BlockOptions {
                records_per_block: 1,
                max_pending_records: 1,
            },
        )
        .unwrap();
        assert!(
            compiler
                .collect::<io::Result<Vec<_>>>()
                .err()
                .unwrap()
                .to_string()
                .contains("max_pending_records")
        );
    }
    #[test]
    fn malformed_input_never_becomes_successful_eof() {
        let mut data = input();
        data.push(Err(invalid("corrupt input")));
        let compiler = BlockCompiler::new(
            data.into_iter(),
            crate::simulation::SimulationConfig { lookahead_ms: 30 },
            Default::default(),
            |_| Ok::<Option<()>, io::Error>(None),
            BlockOptions::default(),
        )
        .unwrap();
        assert!(compiler.collect::<io::Result<Vec<_>>>().is_err());
    }
}

mod store;
pub use store::{CompiledBlockReader, CompiledBlockStoreStats, write_compiled_blocks};
