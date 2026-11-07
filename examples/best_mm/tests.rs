use super::*;
use bitbankutil_rs::book_units::literal;
use bitbankutil_rs::trading_venue::{PlacementRequest, TradingVenue};

#[derive(Clone, Default)]
struct ProbeVenue {
    calls: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    snapshot_read: std::sync::Arc<tokio::sync::Notify>,
}

impl TradingVenue for ProbeVenue {
    fn account_snapshot<'a>(
        &'a self,
        _pair: &'a str,
    ) -> bitbankutil_rs::trading_venue::TradingVenueFuture<
        'a,
        bitbankutil_rs::trading_venue::AccountSnapshot,
    > {
        Box::pin(async move {
            self.calls.lock().unwrap().push("snapshot");
            self.snapshot_read.notify_one();
            Ok(bitbankutil_rs::trading_venue::AccountSnapshot {
                query: None,
                open_orders: vec![],
                balances: vec![
                    BalanceSnapshot {
                        asset: "btc".to_owned(),
                        free_amount: literal("0"),
                        locked_amount: literal("0"),
                        onhand_amount: literal("0"),
                    },
                    BalanceSnapshot {
                        asset: "jpy".to_owned(),
                        free_amount: literal("0"),
                        locked_amount: literal("0"),
                        onhand_amount: literal("0"),
                    },
                ],
            })
        })
    }

    fn place_order(
        &self,
        _request: PlacementRequest,
    ) -> bitbankutil_rs::trading_venue::TradingVenueFuture<
        '_,
        bitbankutil_rs::trading_venue::PlacedOrder,
    > {
        Box::pin(async { panic!("unexpected order") })
    }

    fn cancel_orders<'a>(
        &'a self,
        _pair: &'a str,
        _order_ids: Vec<bitbankutil_rs::order_domain::OrderId>,
    ) -> bitbankutil_rs::trading_venue::TradingVenueFuture<'a, ()> {
        Box::pin(async { panic!("unexpected cancellation") })
    }

    fn on_market_event(
        &self,
        _event: &MarketEvent,
    ) -> Result<(), bitbankutil_rs::trading_venue::TradingVenueError> {
        self.calls.lock().unwrap().push("apply");
        Ok(())
    }
}

#[tokio::test]
async fn runtime_applies_trade_before_bot_reads_account() {
    let venue = ProbeVenue::default();
    let mut bot = MyBot::new(
        "btc_jpy".to_owned(),
        literal("1"),
        0,
        literal("0.1"),
        literal("1"),
    );
    let asks = [(literal("6000000"), literal("1.0"))].into();
    let bids = [(literal("5000000"), literal("1.0"))].into();
    bot.depth = MarketDepthSnapshot::new(asks, bids, 0);
    let runtime = BitbankBotBuilder::new(bot, venue.clone()).spawn();
    runtime
        .event_sender()
        .send(BotEvent::Market(MarketEvent::Transactions {
            pair: "btc_jpy".to_owned(),
            transactions: vec![],
        }))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), venue.snapshot_read.notified())
        .await
        .unwrap();
    runtime.shutdown().await.unwrap();
    assert_eq!(*venue.calls.lock().unwrap(), vec!["apply", "snapshot"]);
}

#[tokio::test]
async fn paper_mode_needs_no_credentials_and_exposes_initial_balances() {
    let (venue, _) = paper_venue("btc_jpy", literal("0"), literal("1000000"), 1000).unwrap();
    let snapshot = venue.account_snapshot("btc_jpy").await.unwrap();
    assert!(snapshot.open_orders.is_empty());
    assert_eq!(
        snapshot
            .balances
            .iter()
            .find(|b| b.asset == "jpy")
            .unwrap()
            .free_amount,
        literal("1000000")
    );
}

#[test]
fn paper_setup_rejects_negative_balance_and_unsupported_pair() {
    assert!(paper_venue("btc_jpy", literal("-1"), literal("0"), 1000).is_err());
    assert!(paper_venue("btc_usdt", literal("0"), literal("0"), 1000).is_err());
}
