#![allow(dead_code)]
//! 低水準kernel回帰テスト用の読み取り専用診断snapshot。
use bitbankutil_rs::{bitbank_bot::*, paper_execution::*};
use std::sync::{Arc, Mutex};
pub trait TestDiagnostics {
    fn engine(&self) -> Arc<Mutex<PaperDiagnosticSnapshot>>;
}
struct Noop;
impl BotStrategy<PaperTradingVenue> for Noop {
    type CustomEvent = ();
    type Error = std::convert::Infallible;
    async fn handle_event(
        &mut self,
        _: BotEvent<()>,
        _: &BotContext<(), PaperTradingVenue>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}
impl TestDiagnostics for PaperTradingVenue {
    fn engine(&self) -> Arc<Mutex<PaperDiagnosticSnapshot>> {
        Arc::new(Mutex::new(
            BotRunner::new(Noop, self.clone())
                .paper_diagnostics()
                .unwrap(),
        ))
    }
}
#[allow(dead_code)]
pub fn configure<V: bitbankutil_rs::trading_venue::TradingVenue>(venue: &V, lookahead_ms: i64) {
    if let Some(venue) = (venue as &dyn std::any::Any).downcast_ref::<PaperTradingVenue>() {
        venue
            .clone()
            .with_simulation_config(bitbankutil_rs::simulation::SimulationConfig { lookahead_ms })
            .unwrap();
    }
}

// Fixtures are already in memory. Preflight their immutable records once, then
// use the public incremental compiled entry point (no production spool adapter).
pub async fn run_replay<S, V, I, D>(
    runner: &mut BotRunner<S, V>,
    input: I,
    recovery: bool,
    cancel: &std::sync::atomic::AtomicBool,
    custom: D,
) -> bitbankutil_rs::replay::ReplayReport
where
    S: BotStrategy<V>,
    V: bitbankutil_rs::trading_venue::TradingVenue,
    S::CustomEvent: Clone + Sync,
    I: IntoIterator<Item = std::io::Result<(usize, bitbankutil_rs::replay::CaptureRecord)>>,
    D: FnMut(&bitbankutil_rs::replay::CaptureRecord) -> std::io::Result<Option<S::CustomEvent>>
        + Send
        + 'static,
{
    run_replay_with_options(
        runner,
        input,
        bitbankutil_rs::replay::ReplayOptions {
            recovery,
            end_time_ms: None,
        },
        cancel,
        custom,
    )
    .await
}
pub async fn run_replay_with_options<S, V, I, D>(
    runner: &mut BotRunner<S, V>,
    input: I,
    options: bitbankutil_rs::replay::ReplayOptions,
    cancel: &std::sync::atomic::AtomicBool,
    custom: D,
) -> bitbankutil_rs::replay::ReplayReport
where
    S: BotStrategy<V>,
    V: bitbankutil_rs::trading_venue::TradingVenue,
    S::CustomEvent: Clone + Sync,
    I: IntoIterator<Item = std::io::Result<(usize, bitbankutil_rs::replay::CaptureRecord)>>,
    D: FnMut(&bitbankutil_rs::replay::CaptureRecord) -> std::io::Result<Option<S::CustomEvent>>
        + Send
        + 'static,
{
    use bitbankutil_rs::replay::*;
    use std::sync::atomic::Ordering;
    if runner.venue().simulation_config().is_none() {
        return run_capture_replay_with_options(runner, input, options, cancel, custom).await;
    }
    let mut records = Vec::new();
    let mut metadata = StreamingReplayMetadata::default();
    for item in input {
        if cancel.load(Ordering::Relaxed) {
            runner
                .venue()
                .mark_market_incomplete("preflight interrupted");
            return ReplayReport {
                interrupted: true,
                incomplete: true,
                end_reason: ReplayEndReason::Interrupted,
                settlement: runner.venue().paper_settlement_status(),
                ..Default::default()
            };
        }
        let (source, record) = match item.and_then(|(source, record)| {
            metadata.observe(source, &record)?;
            Ok((source, record))
        }) {
            Ok(record) => record,
            Err(error) => {
                runner.venue().mark_market_incomplete("preflight failed");
                return ReplayReport {
                    error: Some(error.to_string()),
                    incomplete: true,
                    end_reason: ReplayEndReason::Error,
                    settlement: runner.venue().paper_settlement_status(),
                    ..Default::default()
                };
            }
        };
        records.push(Ok((source, record)));
    }
    bitbankutil_rs::replay::run_replay_with_options(
        runner, records, &metadata, options, cancel, custom,
    )
    .await
}
