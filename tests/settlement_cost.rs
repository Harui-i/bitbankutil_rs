//! 合成キュー履歴に対する固定回数の採取・照会コスト。時間は参考値のみ。
use bitbankutil_rs::{
    market_event::{MarketDepthSnapshot, MarketEvent},
    order_domain::{BalanceSnapshot, DesiredLimitOrder, OrderSide},
    paper_execution::*,
    trading_venue::TradingVenue,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
    time::Instant,
};

struct CountingAllocator;
static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Relaxed) {
            ALLOCATIONS.fetch_add(1, Relaxed);
            BYTES.fetch_add(layout.size(), Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Relaxed) {
            ALLOCATIONS.fetch_add(1, Relaxed);
            BYTES.fetch_add(size, Relaxed);
        }
        unsafe { System.realloc(ptr, layout, size) }
    }
}
fn measure(label: &str, mut f: impl FnMut()) -> (usize, usize) {
    ALLOCATIONS.store(0, Relaxed);
    BYTES.store(0, Relaxed);
    let start = Instant::now();
    ENABLED.store(true, Relaxed);
    for _ in 0..100 {
        f();
    }
    ENABLED.store(false, Relaxed);
    let counts = (ALLOCATIONS.load(Relaxed), BYTES.load(Relaxed));
    eprintln!(
        "{label}: allocations={}, bytes={}, elapsed={:?}",
        counts.0,
        counts.1,
        start.elapsed()
    );
    counts
}
fn engine(history: usize) -> PaperExecutionEngine {
    let mut e = PaperExecutionEngine::new(
        PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap(),
        PaperClock::Replay { now_ms: 1000 },
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
    .unwrap();
    for i in 0..=history {
        e.apply_market_event(&MarketEvent::DepthUpdated {
            pair: "btc_jpy".into(),
            depth: MarketDepthSnapshot::new(
                [(101.into(), 1.0)].into(),
                [(99.into(), (i + 1) as f64)].into(),
                1000,
            ),
        });
        if i == 0 {
            e.place_order(DesiredLimitOrder::limit(
                "btc_jpy".into(),
                OrderSide::Buy,
                1.into(),
                99.into(),
            ))
            .unwrap();
        }
    }
    e.advance_trade_watermark(1000).unwrap();
    e
}
#[test]
fn fixed_query_cost_does_not_grow_with_queue_history() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut query_baseline = None;
    let mut diagnostic_bytes = 0;
    for history in [10, 1000, 10000] {
        let e = engine(history);
        let full = e.settlement_status();
        assert_eq!(full.queue.len(), 1);
        assert_eq!(full.queue[0].segments.len(), history + 2);
        let observation = e.settlement_observation();
        assert_eq!(observation.watermark_ms, full.watermark_ms);
        assert_eq!(observation.unsettled, full.unsettled);
        assert_eq!(observation.issues, full.issues);
        assert_eq!(observation.model_issues, full.model_issues);
        eprintln!("history={history}");
        assert_eq!(
            measure("borrowed metadata", || {
                black_box(e.settlement_observation());
            }),
            (0, 0)
        );
        let (_, bytes) = measure("previous diagnostic metadata path", || {
            black_box(e.settlement_status());
        });
        assert!(bytes > diagnostic_bytes);
        diagnostic_bytes = bytes;
        let v = PaperTradingVenue::new(e);
        assert_eq!(
            measure("pending check", || {
                black_box(v.has_pending_operations());
            }),
            (0, 0)
        );
        // warm up runtime and lazy state before measuring actual account queries.
        runtime.block_on(v.account_snapshot("btc_jpy")).unwrap();
        let counts = measure("account query", || {
            black_box(runtime.block_on(v.account_snapshot("btc_jpy")).unwrap());
        });
        assert_eq!(*query_baseline.get_or_insert(counts), counts);
    }
}
