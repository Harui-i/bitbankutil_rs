use bitbankutil_rs::book_units::literal;
use bitbankutil_rs::simulation::SimulationConfig;
use bitbankutil_rs::{
    bitbank_bot::*, depth::Depth, execution_recording::*, market_event::MarketEvent,
    order_domain::*, paper_execution::*, replay::*, trading_venue::TradingVenue,
};
use serde_json::json;
use std::sync::atomic::AtomicBool;
use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};
pub const MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/shared-replay/manifest.json"
);
#[derive(Clone, Copy)]
pub struct Case {
    pub id: usize,
    pub fail: bool,
    pub response_ms: i64,
    pub cancel_on_event: bool,
}
impl Case {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            fail: false,
            response_ms: 0,
            cancel_on_event: false,
        }
    }
}
pub fn prepare(manifest: &str) -> io::Result<PreparedReplay> {
    PreparedReplay::new(ReplayInput::open(manifest)?)
}
pub fn compile(prepared: &PreparedReplay) -> io::Result<CompiledReplay<'_, serde_json::Value>> {
    prepared.compile(
        SimulationConfig { lookahead_ms: 30 },
        PaperTimeModel::FixedOffset {
            exchange_minus_local_ms: 0,
        },
        |r| match &r.payload {
            CapturePayload::Message { message } => Ok(Some(message.clone())),
            _ => Ok(None),
        },
    )
}
struct Sink(Arc<Mutex<Vec<serde_json::Value>>>);
impl EventSink for Sink {
    fn append(&mut self, r: &ExecutionRecord) -> io::Result<()> {
        // 壁時計の計測値を除外。入力ID・仮想時刻・順序・判断・約定は比較に含める。
        let mut value = serde_json::to_value(r)?;
        value.as_object_mut().unwrap().remove("recorded_at_ms");
        // プロセス全体で採番される診断ストリームIDはケースの意味に影響しない。
        if let Some(data) = value["data"].as_object_mut() {
            if r.kind == "paper_initial_state" {
                data.remove("queue_stream_id");
            }
            if r.kind == "operation_started" {
                data.remove("started_at_unix_us");
            }
            if r.kind == "operation_finished" {
                data.remove("timing");
            }
        }
        self.0.lock().unwrap().push(value);
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Strategy {
    last: Option<i64>,
    refresh_ms: i64,
    fail: bool,
    cancel_on_event: Option<Arc<AtomicBool>>,
}
impl BotStrategy<PaperTradingVenue> for Strategy {
    type CustomEvent = serde_json::Value;
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<Self::CustomEvent>,
        ctx: &BotContext<Self::CustomEvent, PaperTradingVenue>,
    ) -> Result<(), String> {
        if let Some(cancel) = &self.cancel_on_event {
            cancel.store(true, Ordering::Relaxed);
        }
        if self.fail {
            return Err("injected case failure".into());
        }
        if let BotEvent::Market(MarketEvent::DepthUpdated { depth, .. }) = event {
            if self
                .last
                .is_none_or(|t| ctx.now_ms() - t >= self.refresh_ms)
            {
                self.last = Some(ctx.now_ms());
                ctx.venue().recorder().unwrap().record_lazy("strategy_decision", || json!({"now_ms":ctx.now_ms(),"refresh_ms":self.refresh_ms,"best_bid":depth.best_bid()}));
                let account = ctx
                    .venue()
                    .account_snapshot("btc_jpy")
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                ctx.venue()
                    .cancel_orders(
                        "btc_jpy",
                        account.open_orders.iter().map(|o| o.order_id).collect(),
                    )
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                if let Some((price, _)) = depth.best_bid() {
                    ctx.venue()
                        .place_order(
                            DesiredLimitOrder::limit(
                                "btc_jpy".into(),
                                OrderSide::Buy,
                                literal("1"),
                                price
                                    .checked_add(literal("0.1"))
                                    .map_err(|e| format!("{e:?}"))?,
                            )
                            .into(),
                        )
                        .await
                        .map_err(|e| format!("{e:?}"))?;
                }
            }
        }
        Ok(())
    }
}

async fn run_case(
    compiled: &CompiledReplay<'_, serde_json::Value>,
    case: Case,
    cancel: &Arc<AtomicBool>,
    end_time_ms: Option<i64>,
) -> io::Result<serde_json::Value> {
    let id = case.id;
    let records = Arc::new(Mutex::new(Vec::new()));
    let recorder = ExecutionRecorder::new(
        format!("case-{id}"),
        json!({"case":id}),
        Sink(records.clone()),
    );
    let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
    config.time_model = PaperTimeModel::FixedOffset {
        exchange_minus_local_ms: 0,
    };
    let balances = [("btc", 0), ("jpy", 1000)]
        .into_iter()
        .map(|(asset, n)| BalanceSnapshot {
            asset: asset.into(),
            free_amount: n.into(),
            onhand_amount: n.into(),
            locked_amount: 0.into(),
        })
        .collect();
    let engine = PaperExecutionEngine::new(
        config,
        PaperClock::Replay { now_ms: 0 },
        balances,
        Some(recorder.clone()),
    )
    .unwrap();
    let venue = PaperTradingVenue::with_latency(
        engine,
        PaperLatencyConfig {
            placement: PaperOperationLatency {
                entry_ms: 0,
                response_ms: case.response_ms,
            },
            ..Default::default()
        },
    )
    .unwrap()
    .with_simulation_config(SimulationConfig { lookahead_ms: 30 })
    .unwrap();
    let mut runner = BotRunner::new(
        Strategy {
            last: None,
            refresh_ms: 10 + (id % 4) as i64 * 20,
            fail: case.fail,
            cancel_on_event: case.cancel_on_event.then(|| cancel.clone()),
        },
        venue.clone(),
    );
    let report = compiled
        .run(
            &mut runner,
            ReplayOptions {
                recovery: true,
                end_time_ms,
            },
            cancel,
        )
        .await;
    let diagnostics = runner.paper_diagnostics().unwrap();
    let account = bitbankutil_rs::trading_venue::AccountSnapshot {
        open_orders: diagnostics.open_orders(),
        balances: diagnostics.balances(),
        query: None,
    };
    venue.record_paper_settlement();
    recorder.finish()?;
    let log = records.lock().unwrap().clone();
    Ok(json!({"case":id,"report":report,"account":account,"log":log}))
}
/// スレッドは上限個だけ作り、借用したcompiledを共有する。futureのSendは要求しない。
/// cancelは実行中と未開始のケースの両方へ渡す。エラーもケースの結果として残す。
pub fn run_cases(
    compiled: &CompiledReplay<'_, serde_json::Value>,
    cases: &[Case],
    workers: usize,
    iterations: usize,
    cancel: &Arc<AtomicBool>,
) -> Vec<serde_json::Value> {
    assert!(workers > 0 && iterations > 0);
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        let handles: Vec<_> = (0..workers.min(cases.len()))
            .map(|_| {
                let next = &next;
                scope.spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .build()
                        .unwrap();
                    let mut results = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&case) = cases.get(index) else { break };
                        let id = case.id;
                        let mut result = serde_json::Value::Null;
                        for _ in 0..iterations {
                            result = runtime
                                .block_on(run_case(compiled, case, cancel, None))
                                .unwrap_or_else(|e| json!({"case":id,"error":e.to_string()}));
                            if cancel.load(Ordering::Relaxed)
                                || result.get("error").is_some()
                                || result["report"]["error"].is_string()
                            {
                                break;
                            }
                        }
                        results.push((index, result));
                    }
                    results
                })
            })
            .collect();
        let mut results: Vec<_> = handles
            .into_iter()
            .flat_map(|h| h.join().expect("worker panicked"))
            .collect();
        results.sort_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parallel_and_reordered_cases_match_sequential() {
        let prepared = prepare(MANIFEST).unwrap();
        let compiled = compile(&prepared).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let ids: Vec<_> = (0..8).map(Case::new).collect();
        let sequential = run_cases(&compiled, &ids, 1, 1, &cancel);
        assert!(sequential.iter().all(|r| r["report"]["error"].is_null()));
        assert!(
            sequential
                .iter()
                .any(|r| !r["account"]["open_orders"].as_array().unwrap().is_empty())
        );
        assert!(
            sequential
                .iter()
                .any(|r| r["account"]["balances"][0]["onhand_amount"] != "0")
        );
        assert!(sequential.iter().all(|r| {
            r["log"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["kind"] == "strategy_decision")
        }));
        for workers in [2, 4, 20] {
            let parallel = run_cases(&compiled, &ids, workers, 1, &cancel);
            for (expected, actual) in sequential.iter().zip(&parallel) {
                assert!(
                    expected == actual,
                    "case {} differs at workers={workers}",
                    expected["case"]
                );
            }
            let mut reversed = run_cases(
                &compiled,
                &ids.iter().copied().rev().collect::<Vec<_>>(),
                workers,
                1,
                &cancel,
            );
            reversed.reverse();
            assert!(sequential == reversed);
        }
    }
    #[test]
    fn failure_incomplete_and_cancellation_remain_visible() {
        let prepared = prepare(MANIFEST).unwrap();
        let compiled = compile(&prepared).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let failed = runtime
            .block_on(run_case(
                &compiled,
                Case {
                    fail: true,
                    ..Case::new(0)
                },
                &cancel,
                None,
            ))
            .unwrap();
        assert!(failed["report"]["error"].is_string());
        let short = runtime
            .block_on(run_case(
                &compiled,
                Case {
                    response_ms: 100,
                    ..Case::new(1)
                },
                &cancel,
                Some(1_793_113_200_031),
            ))
            .unwrap();
        assert_eq!(short["report"]["incomplete"], true);
        let mixed = run_cases(
            &compiled,
            &[
                Case::new(0),
                Case {
                    fail: true,
                    ..Case::new(1)
                },
                Case {
                    response_ms: 100,
                    ..Case::new(2)
                },
                Case::new(3),
            ],
            2,
            1,
            &cancel,
        );
        assert!(mixed[0]["report"]["error"].is_null());
        assert!(mixed[1]["report"]["error"].is_string());
        assert_eq!(mixed[2]["report"]["incomplete"], true);
        assert!(mixed[3]["report"]["error"].is_null());
        let active = run_cases(
            &compiled,
            &[Case {
                cancel_on_event: true,
                ..Case::new(0)
            }],
            1,
            1,
            &cancel,
        );
        assert_eq!(active[0]["report"]["interrupted"], true);
        assert!(active[0]["report"]["records"].as_u64().unwrap() > 0);
        let stopped = run_cases(
            &compiled,
            &[Case::new(0), Case::new(1), Case::new(2)],
            2,
            1,
            &cancel,
        );
        assert!(stopped.iter().all(|r| r["report"]["interrupted"] == true));
        cancel.store(false, Ordering::Relaxed);
        assert!(
            run_cases(&compiled, &[Case::new(3)], 1, 1, &cancel)[0]["report"]["error"].is_null()
        );
    }
}
