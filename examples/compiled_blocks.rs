//! cargo run --example compiled_blocks -- stream
//! cargo run --example compiled_blocks -- compile MANIFEST CACHE IDENTITY
//! cargo run --example compiled_blocks -- replay MANIFEST CACHE IDENTITY
use bitbankutil_rs::{
    bitbank_bot::*, book_units::literal, order_domain::BalanceSnapshot, paper_execution::*,
    replay::*, simulation::SimulationConfig,
};
use std::{io, sync::atomic::AtomicBool};
#[derive(Default)]
struct Count {
    events: usize,
}
impl BotStrategy<PaperTradingVenue> for Count {
    type CustomEvent = serde_json::Value;
    type Error = String;
    async fn handle_event(
        &mut self,
        _: BotEvent<Self::CustomEvent>,
        _: &BotContext<Self::CustomEvent, PaperTradingVenue>,
    ) -> Result<(), String> {
        self.events += 1;
        Ok(())
    }
}
fn decoder(record: &CaptureRecord) -> io::Result<Option<serde_json::Value>> {
    Ok(match &record.payload {
        CapturePayload::Message { message } => Some(message.clone()),
        _ => None,
    })
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("stream");
    let manifest = args.get(2).map(String::as_str).unwrap_or(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/shared-replay/manifest.json"
    ));
    let metadata = StreamingReplayMetadata::scan(ReplayInput::open(manifest)?)?;
    let config = SimulationConfig { lookahead_ms: 30 };
    let cancel = AtomicBool::new(false);
    let cache = || -> Result<(&str, &str), Box<dyn std::error::Error>> {
        Ok((
            args.get(3).ok_or("CACHE path required")?,
            args.get(4)
                .ok_or("input/settings/decoder/build IDENTITY required")?,
        ))
    };
    if mode == "compile" {
        let (path, identity) = cache()?;
        let compiler = BlockCompiler::new(
            ReplayInput::open(manifest)?,
            config,
            Default::default(),
            decoder,
            BlockOptions::default(),
        )?;
        let stats = write_compiled_blocks(path, identity, compiler)?;
        println!(
            "{}",
            serde_json::json!({"blocks":stats.blocks,"records":stats.records,"bytes":stats.bytes})
        );
        return Ok(());
    }
    let balances = ["btc", "jpy"]
        .into_iter()
        .map(|asset| BalanceSnapshot {
            asset: asset.into(),
            free_amount: literal("0"),
            locked_amount: literal("0"),
            onhand_amount: literal("0"),
        })
        .collect();
    let engine = PaperExecutionEngine::new(
        PaperExecutionConfig::bitbank_spot_default("btc_jpy").map_err(|e| format!("{e:?}"))?,
        PaperClock::Replay { now_ms: 0 },
        balances,
        None,
    )
    .map_err(|e| format!("{e:?}"))?;
    let venue = PaperTradingVenue::new(engine)
        .with_simulation_config(config)
        .map_err(|e| format!("{e:?}"))?;
    let mut runner = BotRunner::new(Count::default(), venue);
    let report = match mode {
        "stream" => {
            run_replay(
                &mut runner,
                ReplayInput::open(manifest)?,
                &metadata,
                false,
                &cancel,
                decoder,
            )
            .await
        }
        "replay" => {
            let (path, identity) = cache()?;
            run_prefetched_compiled(
                &mut runner,
                CompiledBlockReader::open(path, identity)?,
                &metadata,
                ReplayOptions::default(),
                &cancel,
            )
            .await
        }
        _ => return Err("mode must be stream, compile or replay".into()),
    };
    println!("{}", serde_json::to_string(&report)?);
    if report.error.is_some() || report.interrupted || report.incomplete {
        return Err("replay did not complete cleanly".into());
    }
    Ok(())
}
