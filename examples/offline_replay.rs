//! backtestのサンプル: cargo run --example offline_replay -- /tmp/replay.jsonl
use bitbankutil_rs::book_units::literal;
use bitbankutil_rs::{
    bitbank_bot::*, depth::Depth, execution_recording::*, market_event::MarketEvent,
    order_domain::*, paper_execution::*, replay::*, trading_venue::TradingVenue,
};
use serde_json::json;
use std::sync::atomic::AtomicBool;
struct Strategy {
    last: Option<i64>,
}
impl BotStrategy<PaperTradingVenue> for Strategy {
    type CustomEvent = serde_json::Value;
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<Self::CustomEvent>,
        ctx: &BotContext<Self::CustomEvent, PaperTradingVenue>,
    ) -> Result<(), String> {
        if let BotEvent::Market(MarketEvent::DepthUpdated { depth, .. }) = event {
            if self.last.is_none_or(|t| ctx.now_ms() - t >= 50) {
                self.last = Some(ctx.now_ms());
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
                                *price,
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
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args()
        .nth(1)
        .unwrap_or("/tmp/replay.jsonl".into());
    println!("{:?}", demo(output).await?);
    Ok(())
}
async fn demo(
    output: String,
) -> Result<bitbankutil_rs::trading_venue::AccountSnapshot, Box<dyn std::error::Error>> {
    let manifest = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fixtures/market-data-v1/manifest.json"
    );
    let recorder = ExecutionRecorder::new(
        format!("replay-{}", timestamp_ms()),
        json!({"manifest":manifest,"input_manifest":serde_json::from_reader::<_,serde_json::Value>(std::fs::File::open(manifest)?)?,"merge_key":["received_at_micros","source_index","sequence"],"synthetic":true,"refresh_ms":50}),
        JsonlSink::open(output)?,
    );
    let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
    // E = L + δ。0は同期が検証済みという意味ではなく、この合成fixtureの仮定。
    config.time_model = PaperTimeModel::FixedOffset {
        exchange_minus_local_ms: 0,
    };
    config.fee_schedule = PaperFeeSchedule::new(literal("0.001"), literal("0.002"));
    let balances = [("btc", 0), ("jpy", 1000)]
        .into_iter()
        .map(|(asset, amount)| BalanceSnapshot {
            asset: asset.into(),
            free_amount: amount.into(),
            onhand_amount: amount.into(),
            locked_amount: literal("0"),
        })
        .collect();
    let venue = PaperTradingVenue::new(
        PaperExecutionEngine::new(
            config,
            bitbankutil_rs::paper_execution::PaperClock::Replay { now_ms: 0 },
            balances,
            Some(recorder.clone()),
        )
        .unwrap(),
    );
    let venue = venue
        .with_simulation_config(bitbankutil_rs::simulation::SimulationConfig { lookahead_ms: 30 })
        .unwrap();
    let mut runner = BotRunner::new(Strategy { last: None }, venue.clone());
    let mut input = ReplayInput::open(manifest)?
        .take_while(|r| !matches!(r,Ok((0,r)) if r.sequence>=6))
        .collect::<std::io::Result<Vec<_>>>()?;
    // 合成fixtureの末尾を延ばし、両sourceで評価終了+30msまで収録する。
    for source in 0..2 {
        let (_, last) = input.iter().rev().find(|(s, _)| *s == source).unwrap();
        let mut tail = last.clone();
        tail.sequence += 1;
        tail.received_at_micros = 1_793_113_200_060_000;
        tail.payload = CapturePayload::Message {
            message: json!(["heartbeat"]),
        };
        input.push((source, tail));
    }
    let report = run_replay(
        &mut runner,
        input.into_iter().map(Ok),
        false,
        &AtomicBool::new(false),
        |r| match &r.payload {
            CapturePayload::Message { message } => Ok(Some(message.clone())),
            _ => Ok(None),
        },
    )
    .await;
    // EOFで取消や操作期限を強制実行しない。最終口座は診断snapshotで読み取る。
    let diagnostics = runner.paper_diagnostics().unwrap();
    let final_account = bitbankutil_rs::trading_venue::AccountSnapshot {
        open_orders: diagnostics.open_orders(),
        balances: diagnostics.balances(),
        query: None,
    };
    venue.record_paper_settlement();
    recorder.record_lazy("replay_finished",|| json!({"error":report.error,"incomplete":report.incomplete,"interrupted":report.interrupted,"records":report.records,"final_account":final_account}));
    recorder.finish()?;
    if let Some(error) = report.error {
        return Err(std::io::Error::other(error).into());
    }
    Ok(final_account)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn deterministic_queue_prevents_fills_behind_displayed_volume() {
        let mut results = Vec::new();
        for n in 0..2 {
            let path =
                std::env::temp_dir().join(format!("replay-test-{}-{n}.jsonl", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let account = demo(path.to_string_lossy().into_owned()).await.unwrap();
            assert_eq!(account.open_orders.len(), 1);
            // 最終注文99.5の前には表示数量2があり、公開約定0.75では自分まで届かない。
            assert_eq!(account.balances[0].free_amount, literal("0"));
            assert_eq!(account.balances[1].onhand_amount, literal("1000"));
            let records =
                read_records(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).unwrap();
            let events: Vec<_> = records
                .iter()
                .filter(|r| r.kind == "paper_event")
                .map(|r| r.data.clone())
                .collect();
            assert_eq!(events.len(), 4);
            assert!(
                events
                    .iter()
                    .all(|v| v["event"].get("OrderFilled").is_none())
            );
            assert!(!records.iter().any(|r| r.kind == "paper_queue_diagnostic"));
            assert!(
                records
                    .iter()
                    .filter(|r| r.kind == "market_event" || r.kind == "custom_event")
                    .all(|r| r.input_id.is_some() && r.event_time_ms.is_some())
            );
            assert!(records.iter().any(|r| r.kind == "paper_settlement"));
            results.push((serde_json::to_value(account).unwrap(), events));
            std::fs::remove_file(path).unwrap();
        }
        assert_eq!(results[0], results[1]);
    }
}
