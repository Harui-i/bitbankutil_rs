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

// Pin<Box<dyn Future<Output=Result<T,TradingVenueError>> + Send + 'a>>ってなんやねん
/*
Pin<T>やUnpinトレイトについてRust the bookを読んで勉強した.

The book: https://doc.rust-lang.org/book/ch17-05-traits-for-async.html
勉強記録: https://zenn.dev/harui/scraps/b191888e1cb762

返り値がResult<T, TradingVenueError>であるFutureをBoxでヒープに確保しつつPinで固定することでFutureのアドレスが変わらないようになる(Unpinトレイトを実装していないなら)
*/
pub type TradingVenueFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TradingVenueError>> + Send + 'a>>;

#[derive(Debug)]
pub enum TradingVenueError {
    Bitbank(Option<crypto_botters::bitbank::BitbankHandleError>),
    Parse(ParseOrderError),
    Paper(PaperExecutionError),
    StatePoisoned,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AccountSnapshot {
    pub open_orders: Vec<OpenOrder>,
    pub balances: Vec<BalanceSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlacementRequest {
    pub order: DesiredLimitOrder,
}

impl From<DesiredLimitOrder> for PlacementRequest {
    fn from(order: DesiredLimitOrder) -> Self {
        Self { order }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlacedOrder {
    pub order_id: Option<OrderId>,
}

/// 注文操作と口座取得の共通インターフェース。
/// Cloneした値も同じ口座・注文状態を共有すること。
pub trait TradingVenue: Clone + Send + Sync + 'static {
    fn recorder(&self) -> Option<crate::execution_recording::ExecutionRecorder> {
        None
    }
    fn account_snapshot<'a>(&'a self, pair: &'a str) -> TradingVenueFuture<'a, AccountSnapshot>;

    fn place_order(&self, request: PlacementRequest) -> TradingVenueFuture<'_, PlacedOrder>;

    fn cancel_orders<'a>(
        &'a self,
        pair: &'a str,
        order_ids: Vec<OrderId>,
    ) -> TradingVenueFuture<'a, ()>;

    /// ボットが戦略へ市場データを渡す前に呼ぶ処理。
    /// paperでは市場データから仮想約定を判定し、注文と残高を更新する。
    /// 実取引では約定・残高を取引所が管理するため、ここでは何もしない。
    fn on_market_event(&self, _event: &MarketEvent) -> Result<(), TradingVenueError> {
        Ok(())
    }
}

/// 市場データ閲覧専用。注文・口座操作は利用できない。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTradingVenue;

impl TradingVenue for NoTradingVenue {
    fn account_snapshot<'a>(&'a self, _pair: &'a str) -> TradingVenueFuture<'a, AccountSnapshot> {
        Box::pin(async { Err(TradingVenueError::Unavailable) })
    }

    fn place_order(&self, _request: PlacementRequest) -> TradingVenueFuture<'_, PlacedOrder> {
        Box::pin(async { Err(TradingVenueError::Unavailable) })
    }

    fn cancel_orders<'a>(
        &'a self,
        _pair: &'a str,
        _order_ids: Vec<OrderId>,
    ) -> TradingVenueFuture<'a, ()> {
        Box::pin(async { Err(TradingVenueError::Unavailable) })
    }
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
    recorder: Option<crate::execution_recording::ExecutionRecorder>,
    api_client: BitbankPrivateApiClient,
}

impl BitbankTradingVenue {
    pub fn with_recorder(
        mut self,
        recorder: crate::execution_recording::ExecutionRecorder,
    ) -> Self {
        self.api_client = self.api_client.with_recorder(recorder.clone());
        self.recorder = Some(recorder);
        self
    }

    pub fn new(api_client: BitbankPrivateApiClient) -> Self {
        Self {
            api_client,
            recorder: None,
        }
    }
}

impl TradingVenue for BitbankTradingVenue {
    fn recorder(&self) -> Option<crate::execution_recording::ExecutionRecorder> {
        self.recorder.clone()
    }

    fn account_snapshot<'a>(&'a self, pair: &'a str) -> TradingVenueFuture<'a, AccountSnapshot> {
        Box::pin(crate::execution_recording::operation(
            self.recorder.as_ref(),
            "account_snapshot",
            serde_json::json!({"pair":pair}),
            async move {
                let (orders, assets) = tokio::join!(
                    self.api_client
                        .get_active_orders(Some(pair), None, None, None, None, None),
                    self.api_client.get_assets()
                );
                let orders = orders.map_err(TradingVenueError::Bitbank)?;
                let assets = assets.map_err(TradingVenueError::Bitbank)?;
                convert_account_snapshot(orders, assets)
            },
        ))
    }

    fn place_order(&self, request: PlacementRequest) -> TradingVenueFuture<'_, PlacedOrder> {
        Box::pin(crate::execution_recording::operation(
            self.recorder.as_ref(),
            "place_order",
            serde_json::json!(&request),
            async move {
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
            },
        ))
    }

    fn cancel_orders<'a>(
        &'a self,
        pair: &'a str,
        order_ids: Vec<OrderId>,
    ) -> TradingVenueFuture<'a, ()> {
        Box::pin(crate::execution_recording::operation(
            self.recorder.as_ref(),
            "cancel_orders",
            serde_json::json!({"pair":pair,"order_ids":order_ids}),
            async move {
                self.api_client
                    .post_cancel_orders(
                        pair,
                        order_ids.into_iter().map(|order_id| order_id.0).collect(),
                    )
                    .await
                    .map_err(TradingVenueError::Bitbank)?;
                Ok(())
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::order_domain::OrderSide;
    use rust_decimal::Decimal;
    use serde_json::json;

    #[tokio::test]
    async fn market_data_venue_has_no_order_or_account_operations() {
        let event = MarketEvent::Transactions {
            pair: "btc_jpy".into(),
            transactions: vec![],
        };
        NoTradingVenue.on_market_event(&event).unwrap();
        assert!(matches!(
            NoTradingVenue.account_snapshot("btc_jpy").await,
            Err(TradingVenueError::Unavailable)
        ));
        let request =
            DesiredLimitOrder::limit("btc_jpy".into(), OrderSide::Buy, Decimal::ONE, Decimal::ONE)
                .into();
        assert!(matches!(
            NoTradingVenue.place_order(request).await,
            Err(TradingVenueError::Unavailable)
        ));
        assert!(matches!(
            NoTradingVenue.cancel_orders("btc_jpy", vec![]).await,
            Err(TradingVenueError::Unavailable)
        ));
        let live = BitbankTradingVenue::new(BitbankPrivateApiClient::new(
            "unused".into(),
            "unused".into(),
            None,
        ));
        live.on_market_event(&event).unwrap();
    }

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
