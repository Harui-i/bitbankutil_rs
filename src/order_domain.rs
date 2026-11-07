use crate::book_units::{Amount, Price, Quantity};
use std::{fmt, str::FromStr};

use crate::bitbank_structs::{BitbankAssetDatum, BitbankGetOrderResponse};

#[derive(
    PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy, Hash, serde::Serialize, serde::Deserialize,
)]
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

#[derive(
    PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum OrderType {
    Limit,
    Market,
    Stop,
    StopLimit,
    TakeProfit,
    StopLoss,
    Losscut,
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
            OrderType::Losscut => "losscut",
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
            "losscut" => Ok(OrderType::Losscut),
            _ => Err(ParseOrderError::UnknownType(value.to_owned())),
        }
    }
}

#[derive(
    PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct OrderId(pub u64);

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DesiredLimitOrder {
    pub pair: String,
    pub side: OrderSide,
    pub amount: Quantity,
    pub price: Price,
    pub post_only: Option<bool>,
}

impl DesiredLimitOrder {
    pub fn limit(pair: String, side: OrderSide, amount: Quantity, price: Price) -> Self {
        Self {
            pair,
            side,
            amount,
            price,
            post_only: Some(true),
        }
    }

    pub fn limit_price(&self) -> Price {
        self.price
    }

    pub fn matches_open_order(&self, open_order: &OpenOrder) -> bool {
        self.pair == open_order.pair
            && self.side == open_order.side
            && open_order.order_type == OrderType::Limit
            && self.amount == open_order.remaining_amount
            && Some(self.price) == open_order.price
            && post_only_matches(self.post_only, open_order.post_only)
    }
}

fn post_only_matches(desired: Option<bool>, existing: Option<bool>) -> bool {
    existing.is_none() || desired == existing
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OpenOrder {
    pub order_id: OrderId,
    pub pair: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub remaining_amount: Quantity,
    pub price: Option<Price>,
    pub post_only: Option<bool>,
}

impl OpenOrder {
    pub fn to_desired_limit_order(&self) -> Option<DesiredLimitOrder> {
        if self.order_type != OrderType::Limit {
            return None;
        }

        Some(DesiredLimitOrder {
            pair: self.pair.clone(),
            side: self.side,
            amount: self.remaining_amount,
            price: self.price?,
            post_only: self.post_only,
        })
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
            .parse()
            .map_err(|error| ParseOrderError::InvalidNumeric {
                field: "remaining_amount".to_owned(),
                error,
            })?;
        let price = value
            .price
            .as_deref()
            .map(|price| {
                price
                    .parse()
                    .map_err(|error| ParseOrderError::InvalidNumeric {
                        field: "price".to_owned(),
                        error,
                    })
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

#[derive(PartialEq, Eq, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BalanceSnapshot {
    pub asset: String,
    pub free_amount: Amount,
    pub locked_amount: Amount,
    pub onhand_amount: Amount,
}

impl TryFrom<&BitbankAssetDatum> for BalanceSnapshot {
    type Error = ParseOrderError;

    fn try_from(value: &BitbankAssetDatum) -> Result<Self, Self::Error> {
        Ok(Self {
            asset: value.asset.clone(),
            free_amount: parse_numeric_field("free_amount", &value.free_amount)?,
            locked_amount: parse_numeric_field("locked_amount", &value.locked_amount)?,
            onhand_amount: parse_numeric_field("onhand_amount", &value.onhand_amount)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum ParseOrderError {
    UnknownSide(String),
    UnknownType(String),
    InvalidOrderId,
    MissingRemainingAmount,
    InvalidNumeric {
        field: String,
        error: crate::book_units::NumericError,
    },
}

fn parse_numeric_field<T: std::str::FromStr<Err = crate::book_units::NumericError>>(
    field: &str,
    value: &str,
) -> Result<T, ParseOrderError> {
    value
        .parse()
        .map_err(|error| ParseOrderError::InvalidNumeric {
            field: field.to_owned(),
            error,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book_units::*;
    use serde_json::json;

    #[test]
    fn balance_precision_and_numeric_errors_are_preserved() {
        let value = crate::bitbank_structs::BitbankAssetDatum {
            asset: "jpy".into(),
            free_amount: "0.000000000000000000000001".into(),
            locked_amount: "0".into(),
            onhand_amount: "0.000000000000000000000001".into(),
            amount_precision: 24.into(),
            withdrawing_amount: "0".into(),
            withdrawal_fee: serde_json::json!("0"),
            stop_deposit: false,
            stop_withdrawal: false,
            network_list: None,
            collateral_ratio: "1".into(),
        };
        assert_eq!(
            BalanceSnapshot::try_from(&value).unwrap().free_amount,
            Amount::from_units(1)
        );
        assert_eq!(
            parse_numeric_field::<Price>("price", "0.000000001"),
            Err(ParseOrderError::InvalidNumeric {
                field: "price".into(),
                error: NumericError::Precision
            })
        );
    }

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
        assert_eq!(order.remaining_amount, literal("0.1"));
        assert_eq!(order.price, Some(literal("5000000")));
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
    fn accepts_bitbank_losscut_order_type() {
        let response: BitbankGetOrderResponse = serde_json::from_value(json!({
            "order_id": 12345,
            "pair": "btc_jpy",
            "side": "sell",
            "position_side": null,
            "type": "losscut",
            "start_amount": "0.2",
            "remaining_amount": "0.1",
            "executed_amount": "0.1",
            "price": null,
            "post_only": null,
            "user_cancelable": true,
            "average_price": "0",
            "ordered_at": 1710000000000_u64,
            "expire_at": null,
            "trigger_price": null,
            "status": "UNFILLED"
        }))
        .unwrap();

        let order = OpenOrder::try_from(&response).unwrap();

        assert_eq!(order.order_type, OrderType::Losscut);
        assert_eq!(order.order_type.as_str(), "losscut");
    }

    #[test]
    fn converts_open_order_to_desired_limit_order_without_order_id() {
        let open_order = OpenOrder {
            order_id: OrderId(12345),
            pair: "btc_jpy".to_owned(),
            side: OrderSide::Sell,
            order_type: OrderType::Limit,
            remaining_amount: literal("0.25"),
            price: Some(literal("4900000")),
            post_only: Some(true),
        };

        assert_eq!(
            open_order.to_desired_limit_order().unwrap(),
            DesiredLimitOrder::limit(
                "btc_jpy".to_owned(),
                OrderSide::Sell,
                literal("0.25"),
                literal("4900000"),
            )
        );
    }

    #[test]
    fn desired_limit_order_matches_open_order_with_missing_post_only() {
        let desired_order = DesiredLimitOrder::limit(
            "btc_jpy".to_owned(),
            OrderSide::Sell,
            literal("0.25"),
            literal("4900000"),
        );
        let open_order = OpenOrder {
            order_id: OrderId(12345),
            pair: "btc_jpy".to_owned(),
            side: OrderSide::Sell,
            order_type: OrderType::Limit,
            remaining_amount: literal("0.25"),
            price: Some(literal("4900000")),
            post_only: None,
        };

        assert!(desired_order.matches_open_order(&open_order));
    }

    #[test]
    fn desired_limit_order_does_not_match_open_order_with_different_post_only() {
        let desired_order = DesiredLimitOrder::limit(
            "btc_jpy".to_owned(),
            OrderSide::Sell,
            literal("0.25"),
            literal("4900000"),
        );
        let open_order = OpenOrder {
            order_id: OrderId(12345),
            pair: "btc_jpy".to_owned(),
            side: OrderSide::Sell,
            order_type: OrderType::Limit,
            remaining_amount: literal("0.25"),
            price: Some(literal("4900000")),
            post_only: Some(false),
        };

        assert!(!desired_order.matches_open_order(&open_order));
    }
}
