use async_trait::async_trait;

use crate::bitbank_structs::{
    BitbankActiveOrdersResponse, BitbankAssetsData, BitbankCancelOrdersResponse,
    BitbankCreateOrderResponse,
};

/// Common trait for Bitbank trading API clients. The live [`BitbankPrivateApiClient`]
/// as well as simulated backtesting clients implement this interface so that
/// trading strategies can be written without caring about the underlying
/// transport.
#[async_trait]
pub trait BitbankTradingApi: Clone + Send + Sync + 'static {
    type Error: std::fmt::Debug + Send + Sync + 'static;

    async fn get_active_orders(
        &self,
        pair: Option<&str>,
        count: Option<&str>,
        from_id: Option<u64>,
        end_id: Option<u64>,
        since: Option<u64>,
        end: Option<u64>,
    ) -> Result<BitbankActiveOrdersResponse, Self::Error>;

    async fn get_assets(&self) -> Result<BitbankAssetsData, Self::Error>;

    async fn post_order(
        &self,
        pair: &str,
        amount: &str,
        price: Option<&str>,
        side: &str,
        r#type: &str,
        post_only: Option<bool>,
        trigger_price: Option<&str>,
    ) -> Result<BitbankCreateOrderResponse, Self::Error>;

    async fn post_cancel_orders(
        &self,
        pair: &str,
        order_ids: Vec<u64>,
    ) -> Result<BitbankCancelOrdersResponse, Self::Error>;
}
