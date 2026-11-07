//! 独立プロセスで旧/新QueueModelを測る合成benchmark。docs/queue-history.md参照。
#![allow(dead_code)]
use bitbankutil_rs::book_units::{Price, Quantity, literal};
pub use bitbankutil_rs::{bitbank_bot, book_units, order_domain};
use bitbankutil_rs::{
    depth, market_event,
    market_event::{MarketDepthSnapshot, MarketEvent, MarketTrade},
    order_domain::{DesiredLimitOrder, OpenOrder, OrderId, OrderSide},
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    collections::BTreeMap,
    hint::black_box,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::Instant,
};
#[path = "../src/paper_execution/queue/legacy.rs"]
mod legacy;
#[path = "../src/paper_execution/queue.rs"]
mod queue;
use queue::{PaperBookEvidence, PaperModelUnavailable, PaperQueueLevel, PaperQueueSegment};
#[derive(Debug)]
enum PaperRejectReason {
    ExecutionModelUnavailable(PaperModelUnavailable),
    PostOnlyWouldTakeLiquidity,
}
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
trait Model: Default {
    fn book(&mut self, b: &MarketDepthSnapshot, at: i64);
    fn accept(&mut self, o: &DesiredLimitOrder, id: OrderId, at: i64);
    fn remove(&mut self, id: OrderId);
    fn trade(&mut self, t: &MarketTrade);
    fn state(&self) -> Vec<PaperQueueLevel>;
    fn diagnostics(&self) -> queue::PaperQueueDiagnostics;
}
macro_rules! model {
    ($ty:ty, $diagnostics:expr) => {
        impl Model for $ty {
            fn book(&mut self, b: &MarketDepthSnapshot, at: i64) {
                self.observe_book(b, at, at);
            }
            fn accept(&mut self, o: &DesiredLimitOrder, id: OrderId, at: i64) {
                self.accept(o, id, at);
            }
            fn remove(&mut self, id: OrderId) {
                self.remove(id);
            }
            fn trade(&mut self, t: &MarketTrade) {
                black_box(self.allocations(t, &BTreeMap::new(), &BTreeMap::new()));
            }
            fn state(&self) -> Vec<PaperQueueLevel> {
                self.state()
            }
            fn diagnostics(&self) -> queue::PaperQueueDiagnostics {
                ($diagnostics)(self)
            }
        }
    };
}
model!(queue::QueueModel, |q: &queue::QueueModel| q.diagnostics());
model!(legacy::QueueModel, |q: &legacy::QueueModel| {
    let state = q.state();
    queue::PaperQueueDiagnostics {
        active_levels: state
            .iter()
            .filter(|l| l.segments.iter().any(|s| s.order_id.is_some()))
            .count(),
        retained_levels: state.len(),
        queue_levels: state.len(),
        segments: state.iter().map(|l| l.segments.len()).sum(),
        live_orders: state
            .iter()
            .flat_map(|l| &l.segments)
            .filter(|s| s.order_id.is_some())
            .count(),
        removed_segments: 0,
    }
});
fn measure(label: &str, f: impl FnOnce()) {
    let count = ALLOCS.load(Relaxed);
    let bytes = BYTES.load(Relaxed);
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let start = Instant::now();
    f();
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    let count = ALLOCS.load(Relaxed) - count;
    let bytes = BYTES.load(Relaxed) - bytes;
    let live = LIVE.load(Relaxed);
    let peak = PEAK.load(Relaxed);
    println!("{label},{elapsed:.3},{count},{bytes},{live},{peak}");
}
fn depth(t: i64, p: i32, q: Quantity) -> MarketDepthSnapshot {
    MarketDepthSnapshot::new(
        [(1_000_000.into(), literal("1.0"))].into(),
        [(Price::from(p as i32), q)].into(),
        t,
    )
}
fn run<Q: Model>(n: i64, churn: i64) {
    let mut q = Q::default();
    q.book(&depth(0, 100, literal("1.0")), 0);
    q.accept(
        &DesiredLimitOrder::limit(
            "btc_jpy".into(),
            OrderSide::Sell,
            1.into(),
            1_000_000.into(),
        ),
        OrderId(1),
        0,
    );
    println!("phase,ms,allocations,allocated_bytes,live_heap_bytes,peak_heap_bytes");
    measure("build", || {
        for i in 0..n {
            let p = 100 + i;
            for j in 0..churn {
                let t = (i * churn + j) * 3 + 1;
                q.book(
                    &depth(
                        t,
                        p as i32,
                        if j % 2 == 0 {
                            literal("2.0")
                        } else {
                            literal("1.0")
                        },
                    ),
                    t,
                );
                q.accept(
                    &DesiredLimitOrder::limit(
                        "btc_jpy".into(),
                        OrderSide::Buy,
                        1.into(),
                        Price::from(p as i32),
                    ),
                    OrderId(2),
                    t,
                );
                q.remove(OrderId(2));
                q.trade(&MarketTrade {
                    transaction_id: t,
                    executed_at: t + 1,
                    side: OrderSide::Sell,
                    price: Price::from(p as i32),
                    amount: 100.into(),
                });
            }
        }
    });
    let t = n * churn * 3 + 10;
    measure("observe_book_200", || {
        for j in 0..200 {
            q.book(
                &depth(
                    t + j,
                    (100 + n) as i32,
                    if j % 2 == 0 {
                        literal("1.0")
                    } else {
                        literal("2.0")
                    },
                ),
                t + j,
            );
        }
    });
    let o = DesiredLimitOrder::limit(
        "btc_jpy".into(),
        OrderSide::Buy,
        1.into(),
        Price::from((100 + n) as i32),
    );
    measure("accept_remove_200", || {
        for j in 0..200 {
            q.accept(&o, OrderId(2), t + 200 + j);
            q.remove(OrderId(2));
        }
    });
    measure("allocations_200", || {
        for j in 0..200 {
            q.trade(&MarketTrade {
                transaction_id: t + j,
                executed_at: t + 500 + j,
                side: OrderSide::Sell,
                price: 1.into(),
                amount: 100.into(),
            });
        }
    });
    measure("state_200", || {
        for _ in 0..200 {
            black_box(q.state());
        }
    });
    println!("diagnostics={:?}", q.diagnostics());
    // Linuxのプロセス最大RSS。allocator外のメモリも含む。
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines().filter(|l| l.starts_with("VmHWM:")) {
            println!("{line}");
        }
    }
}
fn main() {
    // cargo test --all-targets では測定しない。
    let args: Vec<_> = std::env::args().collect();
    let Some(mode) = args.get(1) else {
        return;
    };
    if !matches!(mode.as_str(), "old" | "new") {
        return;
    }
    let n = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(1000);
    let churn = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(8);
    assert!((1..900_000).contains(&n) && churn > 0);
    if mode == "old" {
        run::<legacy::QueueModel>(n, churn);
    } else {
        run::<queue::QueueModel>(n, churn);
    }
}
