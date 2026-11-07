//! オフラインでの記録と集計: cargo run --example record_execution -- /tmp/run.jsonl
use bitbankutil_rs::book_units::literal;
use bitbankutil_rs::{
    execution_recording::{ExecutionRecorder, JsonlSink, read_records},
    market_event::{MarketEvent, MarketTrade},
    order_domain::{BalanceSnapshot, DesiredLimitOrder, OrderSide},
    paper_execution::{PaperExecutionConfig, PaperExecutionEngine, PaperTradingVenue},
    trading_venue::TradingVenue,
};
use serde_json::json;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).unwrap_or("execution.jsonl".into());
    let run_id = format!(
        "demo-{}-{}",
        std::process::id(),
        bitbankutil_rs::execution_recording::timestamp_ms()
    );
    let recorder = ExecutionRecorder::new(
        run_id.clone(),
        json!({"strategy":"offline_demo"}),
        JsonlSink::open(&path)?,
    );
    let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
    config.fee_schedule.maker_fee_rate_quote = literal("0.001");
    let balances = vec![
        BalanceSnapshot {
            asset: "btc".into(),
            free_amount: literal("0"),
            locked_amount: literal("0"),
            onhand_amount: literal("0"),
        },
        BalanceSnapshot {
            asset: "jpy".into(),
            free_amount: literal("1000"),
            locked_amount: literal("0"),
            onhand_amount: literal("1000"),
        },
    ];
    let venue = PaperTradingVenue::new(
        PaperExecutionEngine::new(
            config,
            bitbankutil_rs::paper_execution::PaperClock::Replay { now_ms: 0 },
            balances,
            Some(recorder.clone()),
        )
        .unwrap(),
    );
    // 合成fixtureの可視板。注文価格100はspread内で、先行数量は0。
    venue
        .on_market_event(&MarketEvent::DepthUpdated {
            pair: "btc_jpy".into(),
            depth: bitbankutil_rs::market_event::MarketDepthSnapshot::new(
                std::collections::BTreeMap::from([(1_000_000.into(), literal("1.0"))]),
                std::collections::BTreeMap::from([(1.into(), literal("1.0"))]),
                0,
            ),
        })
        .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    let order = DesiredLimitOrder::limit(
        "btc_jpy".into(),
        OrderSide::Buy,
        literal("2"),
        literal("100"),
    );
    let placed = venue.place_order(order.clone().into()).await.unwrap();
    venue
        .on_market_event(&MarketEvent::Transactions {
            pair: "btc_jpy".into(),
            transactions: vec![MarketTrade {
                amount: literal("1"),
                executed_at: 1,
                price: literal("100"),
                side: OrderSide::Sell,
                transaction_id: 1,
            }],
        })
        .unwrap();
    venue.advance_trade_watermark(1).unwrap(); // 合成入力の完了境界
    venue
        .cancel_orders("btc_jpy", vec![placed.order_id.unwrap()])
        .await
        .unwrap();
    venue.advance_trade_watermark(2).unwrap(); // 取消を含む次のmsまで合成入力は完了
    let mut rejected = order;
    rejected.amount = literal("1000");
    assert!(venue.place_order(rejected.into()).await.is_err());
    venue.account_snapshot("btc_jpy").await.unwrap();
    recorder.finish()?;
    let records = read_records(std::io::BufReader::new(std::fs::File::open(path)?))?;
    let mut counts = std::collections::BTreeMap::new();
    for r in records.into_iter().filter(|r| r.run_id == run_id) {
        if r.kind == "operation_finished" {
            *counts
                .entry((
                    r.data["operation"].to_string(),
                    r.data["success"].to_string(),
                ))
                .or_insert(0) += 1;
        }
        if r.kind == "paper_event" {
            println!("{}", r.data);
        }
        if r.kind == "operation_finished" && r.data["operation"] == "account_snapshot" {
            println!("final account: {}", r.data["result"]);
        }
    }
    println!("operation counts: {counts:?}");
    Ok(())
}
