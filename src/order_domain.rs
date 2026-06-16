use std::{fmt, str::FromStr};

use rust_decimal::Decimal;

use crate::bitbank_structs::{BitbankAssetDatum, BitbankGetOrderResponse};

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy, Hash)]
pub enum OrderSide {
    Buy,
    Sell,
}

impl OrderSide {
    pub fn as_str(self) -> &'static str {
        match self {
            OrderSide::Buy => "buy",
            OrderSide::Sell => "sell",
        }
    }
}

impl fmt::Display for OrderSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for OrderSide {
    type Err = ParseOrderError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "buy" => Ok(OrderSide::Buy),
            "sell" => Ok(OrderSide::Sell),
            _ => Err(ParseOrderError::UnknownSide(value.to_owned())),
        }
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy, Hash)]
pub enum OrderType {
    Limit,
    Market,
    Stop,
    StopLimit,
    TakeProfit,
    StopLoss,
}

impl OrderType {
    pub fn as_str(self) -> &'static str {
        match self {
            OrderType::Limit => "limit",
            OrderType::Market => "market",
            OrderType::Stop => "stop",
            OrderType::StopLimit => "stop_limit",
            OrderType::TakeProfit => "take_profit",
            OrderType::StopLoss => "stop_loss",
        }
    }
}

impl fmt::Display for OrderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for OrderType {
    type Err = ParseOrderError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "limit" => Ok(OrderType::Limit),
            "market" => Ok(OrderType::Market),
            "stop" => Ok(OrderType::Stop),
            "stop_limit" => Ok(OrderType::StopLimit),
            "take_profit" => Ok(OrderType::TakeProfit),
            "stop_loss" => Ok(OrderType::StopLoss),
            _ => Err(ParseOrderError::UnknownType(value.to_owned())),
        }
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy, Hash)]
pub struct OrderId(pub u64);

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone)]
pub struct DesiredOrder {
    pub pair: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub amount: Decimal,
    pub price: Option<Decimal>,
    pub post_only: Option<bool>,
}

impl DesiredOrder {
    pub fn limit(pair: String, side: OrderSide, amount: Decimal, price: Decimal) -> Self {
        Self {
            pair,
            side,
            order_type: OrderType::Limit,
            amount,
            price: Some(price),
            post_only: Some(true),
        }
    }

    pub fn limit_price(&self) -> Decimal {
        self.price
            .expect("limit desired order must have a price in order_manager")
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone)]
pub struct OpenOrder {
    pub order_id: OrderId,
    pub pair: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub remaining_amount: Decimal,
    pub price: Option<Decimal>,
    pub post_only: Option<bool>,
}

impl OpenOrder {
    pub fn to_desired_order(&self) -> DesiredOrder {
        DesiredOrder {
            pair: self.pair.clone(),
            side: self.side,
            order_type: self.order_type,
            amount: self.remaining_amount,
            price: self.price,
            post_only: self.post_only,
        }
    }
}

impl TryFrom<&BitbankGetOrderResponse> for OpenOrder {
    type Error = ParseOrderError;

    fn try_from(value: &BitbankGetOrderResponse) -> Result<Self, Self::Error> {
        let order_id = value
            .order_id
            .as_u64()
            .ok_or(ParseOrderError::InvalidOrderId)?;
        let remaining_amount = value
            .remaining_amount
            .as_deref()
            .ok_or(ParseOrderError::MissingRemainingAmount)?
            .parse::<Decimal>()
            .map_err(|_| ParseOrderError::InvalidDecimal("remaining_amount".to_owned()))?;
        let price = value
            .price
            .as_deref()
            .map(|price| {
                price
                    .parse::<Decimal>()
                    .map_err(|_| ParseOrderError::InvalidDecimal("price".to_owned()))
            })
            .transpose()?;

        Ok(OpenOrder {
            order_id: OrderId(order_id),
            pair: value.pair.clone(),
            side: value.side.parse()?,
            order_type: value.r#type.parse()?,
            remaining_amount,
            price,
            post_only: value.post_only,
        })
    }
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub struct BalanceSnapshot {
    pub asset: String,
    pub free_amount: Decimal,
    pub locked_amount: Decimal,
    pub onhand_amount: Decimal,
}

impl TryFrom<&BitbankAssetDatum> for BalanceSnapshot {
    type Error = ParseOrderError;

    fn try_from(value: &BitbankAssetDatum) -> Result<Self, Self::Error> {
        Ok(Self {
            asset: value.asset.clone(),
            free_amount: parse_decimal_field("free_amount", &value.free_amount)?,
            locked_amount: parse_decimal_field("locked_amount", &value.locked_amount)?,
            onhand_amount: parse_decimal_field("onhand_amount", &value.onhand_amount)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOrderError {
    UnknownSide(String),
    UnknownType(String),
    InvalidOrderId,
    MissingRemainingAmount,
    InvalidDecimal(String),
}

fn parse_decimal_field(field: &str, value: &str) -> Result<Decimal, ParseOrderError> {
    value
        .parse::<Decimal>()
        .map_err(|_| ParseOrderError::InvalidDecimal(field.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_bitbank_order_response_to_open_order() {
        let response: BitbankGetOrderResponse = serde_json::from_value(json!({
            "order_id": 12345,
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
            "status": "UNFILLED"
        }))
        .unwrap();

        let order = OpenOrder::try_from(&response).unwrap();

        assert_eq!(order.order_id, OrderId(12345));
        assert_eq!(order.pair, "btc_jpy");
        assert_eq!(order.side, OrderSide::Buy);
        assert_eq!(order.order_type, OrderType::Limit);
        assert_eq!(order.remaining_amount, Decimal::new(1, 1));
        assert_eq!(order.price, Some(Decimal::new(5_000_000, 0)));
        assert_eq!(order.post_only, Some(true));
    }

    #[test]
    fn rejects_unknown_bitbank_side() {
        let response: BitbankGetOrderResponse = serde_json::from_value(json!({
            "order_id": 12345,
            "pair": "btc_jpy",
            "side": "hold",
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
            "status": "UNFILLED"
        }))
        .unwrap();

        assert_eq!(
            OpenOrder::try_from(&response).unwrap_err(),
            ParseOrderError::UnknownSide("hold".to_owned())
        );
    }

    #[test]
    fn converts_open_order_to_desired_order_without_order_id() {
        let open_order = OpenOrder {
            order_id: OrderId(12345),
            pair: "btc_jpy".to_owned(),
            side: OrderSide::Sell,
            order_type: OrderType::Limit,
            remaining_amount: Decimal::new(25, 2),
            price: Some(Decimal::new(4_900_000, 0)),
            post_only: Some(true),
        };

        assert_eq!(
            open_order.to_desired_order(),
            DesiredOrder::limit(
                "btc_jpy".to_owned(),
                OrderSide::Sell,
                Decimal::new(25, 2),
                Decimal::new(4_900_000, 0),
            )
        );
    }
}
