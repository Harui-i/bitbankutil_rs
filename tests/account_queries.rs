use bitbankutil_rs::{
    account_domain::{
        HistoryOrder, LiquidityRole, OrderSnapshot, OrderStatus, OwnTrade, TradeHistoryQuery,
    },
    bitbank_structs::{BitbankGetOrderResponse, BitbankTradeHistoryDatum},
    market_event::{MarketEvent, MarketTrade},
    order_domain::{BalanceSnapshot, DesiredLimitOrder, OrderId, OrderSide, ParseOrderError},
    paper_execution::{PaperExecutionConfig, PaperExecutionEngine, PaperTradingVenue},
    trading_venue::{NoTradingVenue, TradingVenue, TradingVenueError},
};
use rust_decimal::Decimal;

#[test]
fn response_conversion_preserves_terminal_status_fees_precision_and_timestamps() {
    let response: BitbankGetOrderResponse =
        serde_json::from_str(include_str!("fixtures/order_snapshot.json")).unwrap();
    let state = OrderSnapshot::try_from(&response).unwrap();
    assert_eq!(state.status, OrderStatus::CanceledPartiallyFilled);
    assert_eq!(state.remaining_amount, Some(Decimal::new(1, 1)));
    assert_eq!(state.executed_amount, Decimal::new(1, 1));
    assert_eq!(
        state.average_price,
        "4999999.12345678".parse::<Decimal>().unwrap()
    );
    assert_eq!(state.canceled_at, Some(1710000000100));
    assert_eq!(state.triggered_at, Some(1710000000050));
    assert_eq!(state.post_only, None);
    let response: BitbankTradeHistoryDatum =
        serde_json::from_str(include_str!("fixtures/own_trade.json")).unwrap();
    let trade = OwnTrade::try_from(&response).unwrap();
    assert_eq!(trade.trade_id, 11);
    assert_eq!(trade.order_id, OrderId(7));
    assert_eq!(trade.liquidity, LiquidityRole::Maker);
    assert_eq!(
        trade.fee_amount_quote,
        "-99.9999824691356".parse::<Decimal>().unwrap()
    );
    assert_eq!(trade.profit_loss, None);
}

#[test]
fn invalid_responses_fail_instead_of_silently_losing_data() {
    let mut order: BitbankGetOrderResponse =
        serde_json::from_str(include_str!("fixtures/order_snapshot.json")).unwrap();
    order.status = "FUTURE_STATUS".into();
    assert!(matches!(
        OrderSnapshot::try_from(&order),
        Err(ParseOrderError::UnknownStatus(_))
    ));
    order.status = "REJECTED".into();
    assert_eq!(
        OrderSnapshot::try_from(&order).unwrap().status,
        OrderStatus::Rejected
    );
    for (name, expected) in [
        ("INACTIVE", OrderStatus::Inactive),
        ("UNFILLED", OrderStatus::Unfilled),
        ("PARTIALLY_FILLED", OrderStatus::PartiallyFilled),
        ("FULLY_FILLED", OrderStatus::FullyFilled),
        ("CANCELED_UNFILLED", OrderStatus::CanceledUnfilled),
        (
            "CANCELED_PARTIALLY_FILLED",
            OrderStatus::CanceledPartiallyFilled,
        ),
    ] {
        order.status = name.into();
        assert_eq!(OrderSnapshot::try_from(&order).unwrap().status, expected);
    }
    order.start_amount = None;
    order.remaining_amount = None;
    order.price = None;
    let nullable = OrderSnapshot::try_from(&order).unwrap();
    assert_eq!(nullable.start_amount, None);
    assert_eq!(nullable.remaining_amount, None);
    assert_eq!(nullable.price, None);
    order.executed_amount = "invalid".into();
    assert!(matches!(
        OrderSnapshot::try_from(&order),
        Err(ParseOrderError::InvalidDecimal(_))
    ));
    let mut trade: BitbankTradeHistoryDatum =
        serde_json::from_str(include_str!("fixtures/own_trade.json")).unwrap();
    trade.maker_taker = "unknown".into();
    assert!(matches!(
        OwnTrade::try_from(&trade),
        Err(ParseOrderError::UnknownLiquidityRole(_))
    ));
    trade.maker_taker = "taker".into();
    trade.trade_id = (-1).into();
    assert!(matches!(
        OwnTrade::try_from(&trade),
        Err(ParseOrderError::InvalidInteger(_))
    ));
}

fn venue() -> PaperTradingVenue {
    PaperTradingVenue::new(
        PaperExecutionEngine::new(
            PaperExecutionConfig::bitbank_spot_default("mona_jpy").unwrap(),
            ["mona", "jpy"]
                .into_iter()
                .map(|asset| BalanceSnapshot {
                    asset: asset.into(),
                    free_amount: Decimal::new(1000, 0),
                    onhand_amount: Decimal::new(1000, 0),
                    locked_amount: Decimal::ZERO,
                })
                .collect(),
        )
        .unwrap(),
    )
}
async fn place(venue: &PaperTradingVenue) -> OrderId {
    venue
        .place_order(
            DesiredLimitOrder::limit(
                "mona_jpy".into(),
                OrderSide::Buy,
                Decimal::ONE,
                Decimal::new(100, 0),
            )
            .into(),
        )
        .await
        .unwrap()
        .order_id
        .unwrap()
}
fn fill(venue: &PaperTradingVenue, time: i64, amount: Decimal) {
    venue
        .on_market_event(&MarketEvent::Transactions {
            pair: "mona_jpy".into(),
            transactions: vec![MarketTrade {
                amount,
                executed_at: time,
                price: Decimal::new(99, 0),
                side: OrderSide::Sell,
                transaction_id: time,
            }],
        })
        .unwrap();
}

#[tokio::test]
async fn paper_keeps_partial_full_and_canceled_states_after_events_are_drained() {
    let venue = venue();
    let id = place(&venue).await;
    assert_eq!(
        venue.get_order("mona_jpy", id).await.unwrap().status,
        OrderStatus::Unfilled
    );
    fill(&venue, 10, Decimal::new(4, 1));
    let state = venue.get_order("mona_jpy", id).await.unwrap();
    assert_eq!(state.status, OrderStatus::PartiallyFilled);
    assert_eq!(state.executed_amount, Decimal::new(4, 1));
    assert_eq!(state.average_price, Decimal::new(100, 0));
    venue.cancel_orders("mona_jpy", vec![id]).await.unwrap();
    let state = venue.get_order("mona_jpy", id).await.unwrap();
    assert_eq!(state.status, OrderStatus::CanceledPartiallyFilled);
    assert_eq!(state.remaining_amount, Some(Decimal::new(6, 1)));
    assert!(!state.user_cancelable);
    let full = place(&venue).await;
    fill(&venue, 20, Decimal::ONE);
    assert_eq!(
        venue.get_order("mona_jpy", full).await.unwrap().status,
        OrderStatus::FullyFilled
    );
    let canceled = place(&venue).await;
    venue
        .cancel_orders("mona_jpy", vec![canceled])
        .await
        .unwrap();
    assert_eq!(
        venue.get_order("mona_jpy", canceled).await.unwrap().status,
        OrderStatus::CanceledUnfilled
    );
    venue.engine().lock().unwrap().drain_events();
    let trades = venue
        .trade_history(TradeHistoryQuery::default())
        .await
        .unwrap();
    assert_eq!(trades.len(), 2);
    assert!(trades[0].fee_amount_quote < Decimal::ZERO);
    assert_eq!(trades[0].order_id, full);
    assert_eq!(trades[1].order_id, id);
    assert_eq!(
        venue
            .clone()
            .get_order("mona_jpy", full)
            .await
            .unwrap()
            .status,
        OrderStatus::FullyFilled
    );
    assert!(matches!(
        venue.get_order("mona_jpy", OrderId(999)).await,
        Err(TradingVenueError::OrderNotFound { .. })
    ));
    assert!(matches!(
        venue.get_order("btc_jpy", id).await,
        Err(TradingVenueError::OrderNotFound { .. })
    ));
}

#[tokio::test]
async fn paper_history_filters_sorts_limits_and_assigns_unique_fill_ids() {
    let venue = venue();
    let first = place(&venue).await;
    let second = place(&venue).await;
    fill(&venue, 10, Decimal::new(15, 1)); // 同じ公開約定が2つの自分の注文に約定
    fill(&venue, 20, Decimal::ONE);
    let asc = venue
        .trade_history(TradeHistoryQuery {
            order: HistoryOrder::Asc,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        asc.iter().map(|t| t.trade_id).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        asc.iter().map(|t| t.order_id).collect::<Vec<_>>(),
        vec![first, second, second]
    );
    let query = TradeHistoryQuery {
        pair: Some("mona_jpy".into()),
        order_id: Some(second),
        since: Some(10),
        end: Some(20),
        count: Some(1),
        ..Default::default()
    };
    assert_eq!(
        venue.trade_history(query.clone()).await.unwrap()[0].trade_id,
        3
    );
    assert_eq!(
        venue
            .trade_history(TradeHistoryQuery {
                order: HistoryOrder::Asc,
                ..query
            })
            .await
            .unwrap()[0]
            .trade_id,
        2
    );
    assert!(
        venue
            .trade_history(TradeHistoryQuery {
                pair: Some("btc_jpy".into()),
                ..Default::default()
            })
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        venue
            .trade_history(TradeHistoryQuery {
                since: Some(20),
                end: Some(20),
                ..Default::default()
            })
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn invalid_queries_and_unavailable_venues_are_reported() {
    let venue = venue();
    for query in [
        TradeHistoryQuery {
            count: Some(0),
            ..Default::default()
        },
        TradeHistoryQuery {
            count: Some(1001),
            ..Default::default()
        },
        TradeHistoryQuery {
            order_id: Some(OrderId(1)),
            ..Default::default()
        },
        TradeHistoryQuery {
            since: Some(20),
            end: Some(10),
            ..Default::default()
        },
        TradeHistoryQuery {
            since: Some(-1),
            ..Default::default()
        },
        TradeHistoryQuery {
            pair: Some("".into()),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            venue.trade_history(query).await,
            Err(TradingVenueError::InvalidQuery(_))
        ));
    }
    assert!(matches!(
        NoTradingVenue.get_order("btc_jpy", OrderId(1)).await,
        Err(TradingVenueError::Unavailable)
    ));
    assert!(matches!(
        NoTradingVenue
            .trade_history(TradeHistoryQuery::default())
            .await,
        Err(TradingVenueError::Unavailable)
    ));
}
