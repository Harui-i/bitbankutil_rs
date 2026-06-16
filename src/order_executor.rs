use std::{future::Future, pin::Pin};

use crate::{
    bitbank_private::BitbankPrivateApiClient,
    order_domain::{DesiredLimitOrder, OrderId, OrderType},
};

pub type OrderExecutorFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, OrderExecutionError>> + Send + 'a>>;

#[derive(Debug)]
pub enum OrderExecutionError {
    Bitbank(Option<crypto_botters::bitbank::BitbankHandleError>),
    Other(String),
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

pub trait OrderExecutor: Clone + Send + Sync + 'static {
    fn place_order(&self, request: PlacementRequest) -> OrderExecutorFuture<'_, PlacedOrder>;

    fn cancel_orders<'a>(
        &'a self,
        pair: &'a str,
        order_ids: Vec<OrderId>,
    ) -> OrderExecutorFuture<'a, ()>;
}

#[derive(Clone)]
pub struct BitbankOrderExecutor {
    api_client: BitbankPrivateApiClient,
}

impl BitbankOrderExecutor {
    pub fn new(api_client: BitbankPrivateApiClient) -> Self {
        Self { api_client }
    }
}

impl From<BitbankPrivateApiClient> for BitbankOrderExecutor {
    fn from(api_client: BitbankPrivateApiClient) -> Self {
        Self::new(api_client)
    }
}

impl OrderExecutor for BitbankOrderExecutor {
    fn place_order(&self, request: PlacementRequest) -> OrderExecutorFuture<'_, PlacedOrder> {
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
                .map_err(OrderExecutionError::Bitbank)?;

            Ok(PlacedOrder {
                order_id: response.order_id.as_u64().map(OrderId),
            })
        })
    }

    fn cancel_orders<'a>(
        &'a self,
        pair: &'a str,
        order_ids: Vec<OrderId>,
    ) -> OrderExecutorFuture<'a, ()> {
        Box::pin(async move {
            self.api_client
                .post_cancel_orders(
                    pair,
                    order_ids.into_iter().map(|order_id| order_id.0).collect(),
                )
                .await
                .map_err(OrderExecutionError::Bitbank)?;

            Ok(())
        })
    }
}

impl OrderExecutor for BitbankPrivateApiClient {
    fn place_order(&self, request: PlacementRequest) -> OrderExecutorFuture<'_, PlacedOrder> {
        Box::pin(async move {
            let response = self
                .post_order(
                    &request.order.pair,
                    &request.order.amount.to_string(),
                    Some(&request.order.price.to_string()),
                    request.order.side.as_str(),
                    OrderType::Limit.as_str(),
                    request.order.post_only,
                    None,
                )
                .await
                .map_err(OrderExecutionError::Bitbank)?;

            Ok(PlacedOrder {
                order_id: response.order_id.as_u64().map(OrderId),
            })
        })
    }

    fn cancel_orders<'a>(
        &'a self,
        pair: &'a str,
        order_ids: Vec<OrderId>,
    ) -> OrderExecutorFuture<'a, ()> {
        Box::pin(async move {
            self.post_cancel_orders(
                pair,
                order_ids.into_iter().map(|order_id| order_id.0).collect(),
            )
            .await
            .map_err(OrderExecutionError::Bitbank)?;

            Ok(())
        })
    }
}
