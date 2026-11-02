//! backtestのサンプル: cargo run --example offline_replay -- /tmp/replay.jsonl
use bitbankutil_rs::{
    bitbank_bot::*, depth::Depth, execution_recording::*, market_event::MarketEvent,
    order_domain::*, paper_execution::*, replay::*, trading_venue::TradingVenue,
};
use rust_decimal::Decimal;
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
                                Decimal::ONE,
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
    config.fee_schedule = PaperFeeSchedule::new(Decimal::new(1, 3), Decimal::new(2, 3));
    let balances = [("btc", 0), ("jpy", 1000)]
        .into_iter()
        .map(|(asset, amount)| BalanceSnapshot {
            asset: asset.into(),
            free_amount: amount.into(),
            onhand_amount: amount.into(),
            locked_amount: Decimal::ZERO,
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
    let mut runner = BotRunner::new(Strategy { last: None }, venue.clone());
    let input =
        ReplayInput::open(manifest)?.take_while(|r| !matches!(r,Ok((0,r)) if r.sequence>=6));
    let report = run_replay(
        &mut runner,
        input,
        false,
        &AtomicBool::new(false),
        |r| match &r.payload {
            CapturePayload::Message { message } => Ok(Some(message.clone())),
            _ => Ok(None),
        },
    )
    .await;
    // fixtureのこの区間では、ここまでに成立した取引がすべて収録済みと分かっている。
    // 一般のcaptureのEOFにこの保証はない。
    venue
        .advance_trade_watermark(1_793_113_200_030)
        .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    let account = venue
        .account_snapshot("btc_jpy")
        .await
        .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    venue
        .cancel_orders(
            "btc_jpy",
            account.open_orders.iter().map(|o| o.order_id).collect(),
        )
        .await
        .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    // 同msの確定後に出した取消は次の未確定msに有効になる。
    venue
        .advance_trade_watermark(1_793_113_200_031)
        .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    let final_account = venue
        .account_snapshot("btc_jpy")
        .await
        .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    recorder.record("replay_finished",json!({"error":report.error,"incomplete":report.incomplete,"interrupted":report.interrupted,"records":report.records,"final_account":final_account,"settlement":venue.paper_settlement_status()}));
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
            assert!(account.open_orders.is_empty());
            // 最終注文99.5の前には表示数量2があり、公開約定0.75では自分まで届かない。
            assert_eq!(account.balances[0].free_amount, Decimal::ZERO);
            assert_eq!(account.balances[1].free_amount, Decimal::from(1000));
            let records =
                read_records(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).unwrap();
            let events: Vec<_> = records
                .iter()
                .filter(|r| r.kind == "paper_event")
                .map(|r| r.data.clone())
                .collect();
            assert_eq!(events.len(), 6);
            assert!(
                events
                    .iter()
                    .all(|v| v["event"].get("OrderFilled").is_none())
            );
            assert!(records.iter().any(|r| r.kind == "paper_queue_trade"));
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
