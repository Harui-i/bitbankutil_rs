//! Offline recording and aggregation: cargo run --example record_execution -- /tmp/run.jsonl
use bitbankutil_rs::{
    execution_recording::{ExecutionRecorder, JsonlSink, read_records},
    market_event::{MarketEvent, MarketTrade},
    order_domain::{BalanceSnapshot, DesiredLimitOrder, OrderSide},
    paper_execution::{PaperExecutionConfig, PaperExecutionEngine, PaperTradingVenue},
    trading_venue::TradingVenue,
};
use rust_decimal::Decimal;
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
    config.fee_schedule.maker_fee_rate_quote = Decimal::new(1, 3);
    let balances = vec![
        BalanceSnapshot {
            asset: "btc".into(),
            free_amount: Decimal::ZERO,
            locked_amount: Decimal::ZERO,
            onhand_amount: Decimal::ZERO,
        },
        BalanceSnapshot {
            asset: "jpy".into(),
            free_amount: Decimal::from(1000),
            locked_amount: Decimal::ZERO,
            onhand_amount: Decimal::from(1000),
        },
    ];
    let venue = PaperTradingVenue::new(
        PaperExecutionEngine::new(config, balances, Some(recorder.clone())).unwrap(),
    );
    let order = DesiredLimitOrder::limit(
        "btc_jpy".into(),
        OrderSide::Buy,
        Decimal::from(2),
        Decimal::from(100),
    );
    let placed = venue.place_order(order.clone().into()).await.unwrap();
    venue
        .on_market_event(&MarketEvent::Transactions {
            pair: "btc_jpy".into(),
            transactions: vec![MarketTrade {
                amount: Decimal::ONE,
                executed_at: 1,
                price: Decimal::from(100),
                side: OrderSide::Sell,
                transaction_id: 1,
            }],
        })
        .unwrap();
    venue
        .cancel_orders("btc_jpy", vec![placed.order_id.unwrap()])
        .await
        .unwrap();
    let mut rejected = order;
    rejected.amount = Decimal::from(1000);
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
