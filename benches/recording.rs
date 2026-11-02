//! cargo bench --bench recording -- <disabled|financial|full|periodic|delta|counters> [updates] [levels]
use bitbankutil_rs::{
    execution_recording::*, market_event::*, order_domain::*, paper_execution::*,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::Instant,
};
struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(layout.size() as u64, Relaxed);
            let live = LIVE.fetch_add(layout.size() as u64, Relaxed) + layout.size() as u64;
            PEAK.fetch_max(live, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as u64, Relaxed);
        unsafe { System.dealloc(p, layout) };
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[derive(Default)]
struct Metrics {
    bytes: u64,
    serialize_ns: u128,
    records: u64,
}
struct Sink(Arc<Mutex<Metrics>>);
impl EventSink for Sink {
    fn append(&mut self, record: &ExecutionRecord) -> io::Result<()> {
        let start = Instant::now();
        let bytes = serde_json::to_vec(record)?;
        let elapsed = start.elapsed().as_nanos();
        let mut m = self.0.lock().unwrap();
        m.bytes += bytes.len() as u64 + 1;
        m.serialize_ns += elapsed;
        m.records += 1;
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let Some(mode) = args.get(1) else {
        return;
    };
    let profile = match mode.as_str() {
        "disabled" => None,
        "financial" => Some(RecordingProfile::Financial),
        "full" => Some(RecordingProfile::Diagnostic {
            queue: QueueRecording::Full,
        }),
        "periodic" => Some(RecordingProfile::Diagnostic {
            queue: QueueRecording::Periodic {
                every: 100.try_into().unwrap(),
            },
        }),
        "delta" => Some(RecordingProfile::Diagnostic {
            queue: QueueRecording::Delta {
                snapshot_every: 100.try_into().unwrap(),
            },
        }),
        "counters" => Some(RecordingProfile::Diagnostic {
            queue: QueueRecording::Counters,
        }),
        _ => return,
    };
    let n: i64 = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(2000);
    let levels: i64 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(128);
    assert!(n > 0 && levels > 0 && levels < 10000);
    // 同じ深い板を繰り返し更新し、入力生成コストは計測範囲から外す。
    let events: Vec<_> = (0..n)
        .map(|i| MarketEvent::DepthUpdated {
            pair: "btc_jpy".into(),
            depth: MarketDepthSnapshot::new(
                (0..levels).map(|p| ((20000 + p).into(), 1.0)).collect(),
                (0..levels)
                    .map(|p| {
                        (
                            (10000 - p).into(),
                            if p == 0 { 1.0 + (i % 2) as f64 } else { 1.0 },
                        )
                    })
                    .collect(),
                i,
            ),
        })
        .collect();
    let m = Arc::new(Mutex::new(Metrics::default()));
    let count = ALLOCS.load(Relaxed);
    let allocated = BYTES.load(Relaxed);
    let start = Instant::now();
    let recorder = profile.map(|p| {
        ExecutionRecorder::with_profile("benchmark", serde_json::json!({}), p, Sink(m.clone()))
    });
    let balances = [("btc", 10), ("jpy", 100000)]
        .into_iter()
        .map(|(asset, n)| BalanceSnapshot {
            asset: asset.into(),
            free_amount: n.into(),
            locked_amount: 0.into(),
            onhand_amount: n.into(),
        })
        .collect();
    let mut engine = PaperExecutionEngine::new(
        PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap(),
        PaperClock::Replay { now_ms: 0 },
        balances,
        recorder.clone(),
    )
    .unwrap();
    engine.apply_market_event(&events[0]);
    engine
        .place_order(DesiredLimitOrder::limit(
            "btc_jpy".into(),
            OrderSide::Buy,
            1.into(),
            10000.into(),
        ))
        .unwrap();
    for (i, event) in events.iter().enumerate() {
        engine.advance_replay_time(i as i64).unwrap();
        engine.apply_market_event(event);
        engine.advance_trade_watermark(i as i64).unwrap();
    }
    let venue = PaperTradingVenue::new(engine);
    bitbankutil_rs::trading_venue::TradingVenue::record_paper_settlement(&venue);
    if let Some(r) = recorder {
        r.finish().unwrap();
    }
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    let allocations = ALLOCS.load(Relaxed) - count;
    let allocated_bytes = BYTES.load(Relaxed) - allocated;
    let m = m.lock().unwrap();
    println!(
        "mode,updates,levels,total_ms,serialize_ms,allocations,allocated_bytes,jsonl_bytes,records"
    );
    println!(
        "{mode},{n},{levels},{ms:.3},{:.3},{allocations},{allocated_bytes},{},{}",
        m.serialize_ns as f64 / 1e6,
        m.bytes,
        m.records
    );
}
