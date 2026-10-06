use std::{future::Future, pin::Pin};

use crate::{
    bitbank_private::BitbankPrivateApiClient,
    bitbank_structs::{BitbankActiveOrdersResponse, BitbankAssetsData},
    market_event::MarketEvent,
    order_domain::{
        BalanceSnapshot, DesiredLimitOrder, OpenOrder, OrderId, OrderType, ParseOrderError,
    },
    paper_execution::PaperExecutionError,
};

pub type TradingVenueFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TradingVenueError>> + Send + 'a>>;

#[derive(Debug)]
pub enum TradingVenueError {
    Bitbank(Option<crypto_botters::bitbank::BitbankHandleError>),
    Parse(ParseOrderError),
    Paper(PaperExecutionError),
    StatePoisoned,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountSnapshot {
    pub open_orders: Vec<OpenOrder>,
    pub balances: Vec<BalanceSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementRequest {
    pub order: DesiredLimitOrder,
}

impl From<DesiredLimitOrder> for PlacementRequest {
    fn from(order: DesiredLimitOrder) -> Self {
        Self { order }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedOrder {
    pub order_id: Option<OrderId>,
}

pub trait TradingVenue: Clone + Send + Sync + 'static {
    fn account_snapshot<'a>(&'a self, pair: &'a str) -> TradingVenueFuture<'a, AccountSnapshot>;

    fn place_order(&self, request: PlacementRequest) -> TradingVenueFuture<'_, PlacedOrder>;

    fn cancel_orders<'a>(
        &'a self,
        pair: &'a str,
        order_ids: Vec<OrderId>,
    ) -> TradingVenueFuture<'a, ()>;

    fn observe_market_event(&self, event: &MarketEvent) -> Result<(), TradingVenueError>;
}

fn convert_account_snapshot(
    orders: BitbankActiveOrdersResponse,
    assets: BitbankAssetsData,
) -> Result<AccountSnapshot, TradingVenueError> {
    Ok(AccountSnapshot {
        open_orders: orders
            .orders
            .iter()
            .map(OpenOrder::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(TradingVenueError::Parse)?,
        balances: assets
            .assets
            .iter()
            .map(BalanceSnapshot::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(TradingVenueError::Parse)?,
    })
}

#[derive(Clone)]
pub struct BitbankTradingVenue {
    api_client: BitbankPrivateApiClient,
}

impl BitbankTradingVenue {
    pub fn new(api_client: BitbankPrivateApiClient) -> Self {
        Self { api_client }
    }
}

impl TradingVenue for BitbankTradingVenue {
    fn account_snapshot<'a>(&'a self, pair: &'a str) -> TradingVenueFuture<'a, AccountSnapshot> {
        Box::pin(async move {
            let (orders, assets) = tokio::join!(
                self.api_client
                    .get_active_orders(Some(pair), None, None, None, None, None),
                self.api_client.get_assets()
            );
            let orders = orders.map_err(TradingVenueError::Bitbank)?;
            let assets = assets.map_err(TradingVenueError::Bitbank)?;
            convert_account_snapshot(orders, assets)
        })
    }

    fn place_order(&self, request: PlacementRequest) -> TradingVenueFuture<'_, PlacedOrder> {
        Box::pin(async move {
            let order = request.order;
            let response = self
                .api_client
                .post_order(
                    &order.pair,
                    &order.amount.to_string(),
                    Some(&order.price.to_string()),
                    order.side.as_str(),
                    OrderType::Limit.as_str(),
                    order.post_only,
                    None,
                )
                .await
                .map_err(TradingVenueError::Bitbank)?;

            Ok(PlacedOrder {
                order_id: response.order_id.as_u64().map(OrderId),
            })
        })
    }

    fn cancel_orders<'a>(
        &'a self,
        pair: &'a str,
        order_ids: Vec<OrderId>,
    ) -> TradingVenueFuture<'a, ()> {
        Box::pin(async move {
            self.api_client
                .post_cancel_orders(
                    pair,
                    order_ids.into_iter().map(|order_id| order_id.0).collect(),
                )
                .await
                .map_err(TradingVenueError::Bitbank)?;
            Ok(())
        })
    }

    fn observe_market_event(&self, _event: &MarketEvent) -> Result<(), TradingVenueError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::order_domain::OrderSide;
    use rust_decimal::Decimal;
    use serde_json::json;

    #[test]
    fn live_snapshot_converts_orders_and_balances_together() {
        let orders: BitbankActiveOrdersResponse = serde_json::from_value(json!({
            "orders": [{
                "order_id": 7,
                "pair": "btc_jpy",
                "side": "buy",
                "position_side": null,
                "type": "limit",
                "start_amount": "0.2",
                "remaining_amount": "0.1",
                "executed_amount": "0.1",
                "price": "5000000",
                "post_only": true,
                "user_cancelable": true,
                "average_price": "0",
                "ordered_at": 1710000000000_u64,
                "expire_at": null,
                "trigger_price": null,
                "status": "PARTIALLY_FILLED"
            }]
        }))
        .unwrap();
        let assets: BitbankAssetsData = serde_json::from_value(json!({
            "assets": [{
                "asset": "jpy",
                "free_amount": "500000",
                "amount_precision": 0,
                "onhand_amount": "1000000",
                "locked_amount": "500000",
                "withdrawing_amount": "0",
                "withdrawal_fee": "0",
                "stop_deposit": false,
                "stop_withdrawal": false,
                "network_list": null,
                "collateral_ratio": "0"
            }]
        }))
        .unwrap();
        let snapshot = convert_account_snapshot(orders, assets).unwrap();
        assert_eq!(snapshot.open_orders[0].side, OrderSide::Buy);
        assert_eq!(snapshot.open_orders[0].remaining_amount, Decimal::new(1, 1));
        assert_eq!(snapshot.balances[0].locked_amount, Decimal::new(500_000, 0));
    }
}
