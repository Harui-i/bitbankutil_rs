use crate::bitbank_structs::{
    BitbankCircuitBreakInfo, BitbankDepth, BitbankTickerResponse, BitbankTransactionDatum,
};
use crate::depth::Depth;
use crate::order_domain::{OrderSide, ParseOrderError};
use rust_decimal::Decimal;
use serde_json::Number;
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct MarketTicker {
    pub sell: Option<String>,
    pub buy: Option<String>,
    pub high: String,
    pub low: String,
    pub open: String,
    pub last: String,
    pub vol: String,
    pub timestamp: Number,
}

impl From<BitbankTickerResponse> for MarketTicker {
    fn from(ticker: BitbankTickerResponse) -> Self {
        Self {
            sell: ticker.sell,
            buy: ticker.buy,
            high: ticker.high,
            low: ticker.low,
            open: ticker.open,
            last: ticker.last,
            vol: ticker.vol,
            timestamp: ticker.timestamp,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarketTrade {
    pub amount: Decimal,
    pub executed_at: i64,
    pub price: Decimal,
    pub side: OrderSide,
    pub transaction_id: i64,
}

impl TryFrom<BitbankTransactionDatum> for MarketTrade {
    type Error = MarketEventConversionError;

    fn try_from(trade: BitbankTransactionDatum) -> Result<Self, Self::Error> {
        Ok(Self {
            amount: trade.amount,
            executed_at: trade.executed_at,
            price: trade.price,
            side: trade.side.parse()?,
            transaction_id: trade.transaction_id,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarketDepthSnapshot {
    asks: BTreeMap<Decimal, f64>,
    bids: BTreeMap<Decimal, f64>,
    last_timestamp: i64,
    is_complete: bool,
}

impl MarketDepthSnapshot {
    pub fn new(
        asks: BTreeMap<Decimal, f64>,
        bids: BTreeMap<Decimal, f64>,
        last_timestamp: i64,
    ) -> Self {
        Self {
            asks,
            bids,
            last_timestamp,
            is_complete: true,
        }
    }

    pub fn empty() -> Self {
        Self {
            asks: BTreeMap::new(),
            bids: BTreeMap::new(),
            last_timestamp: 0,
            is_complete: false,
        }
    }

    pub fn last_timestamp(&self) -> i64 {
        self.last_timestamp
    }

    pub fn is_complete(&self) -> bool {
        self.is_complete
    }
}

impl Depth for MarketDepthSnapshot {
    fn asks(&self) -> &BTreeMap<Decimal, f64> {
        &self.asks
    }

    fn bids(&self) -> &BTreeMap<Decimal, f64> {
        &self.bids
    }
}

impl fmt::Display for MarketDepthSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f)?;
        self.format_depth(Some(20), f)
    }
}

impl From<&BitbankDepth> for MarketDepthSnapshot {
    fn from(depth: &BitbankDepth) -> Self {
        Self {
            asks: depth.asks().clone(),
            bids: depth.bids().clone(),
            last_timestamp: depth.last_timestamp(),
            is_complete: depth.is_complete(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarketCircuitBreakInfo {
    pub mode: String,
    pub estimated_itayose_price: Option<String>,
    pub estimated_itayose_amount: Option<String>,
    pub itayose_upper_price: Option<String>,
    pub itayose_lower_price: Option<String>,
    pub upper_trigger_price: Option<String>,
    pub lower_trigger_price: Option<String>,
    pub fee_type: String,
    pub reopen_timestamp: Option<Number>,
    pub timestamp: Number,
}

impl From<BitbankCircuitBreakInfo> for MarketCircuitBreakInfo {
    fn from(info: BitbankCircuitBreakInfo) -> Self {
        Self {
            mode: info.mode,
            estimated_itayose_price: info.estimated_itayose_price,
            estimated_itayose_amount: info.estimated_itayose_amount,
            itayose_upper_price: info.itayose_upper_price,
            itayose_lower_price: info.itayose_lower_price,
            upper_trigger_price: info.upper_trigger_price,
            lower_trigger_price: info.lower_trigger_price,
            fee_type: info.fee_type,
            reopen_timestamp: info.reopen_timestamp,
            timestamp: info.timestamp,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarketEvent {
    Ticker {
        pair: String,
        ticker: MarketTicker,
    },
    Transactions {
        pair: String,
        transactions: Vec<MarketTrade>,
    },
    DepthUpdated {
        pair: String,
        depth: MarketDepthSnapshot,
    },
    CircuitBreakInfo {
        pair: String,
        info: MarketCircuitBreakInfo,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarketEventConversionError {
    InvalidTradeSide(ParseOrderError),
}

impl From<ParseOrderError> for MarketEventConversionError {
    fn from(err: ParseOrderError) -> Self {
        Self::InvalidTradeSide(err)
    }
}
