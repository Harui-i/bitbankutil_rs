use crate::bitbank_structs::{
    BitbankCircuitBreakInfo, BitbankDepth, BitbankTickerResponse, BitbankTransactionDatum,
    DepthDelta, SharedDepthLevels,
};
use crate::book_units::{Price, Quantity};
use crate::depth::Depth;
use crate::order_domain::{OrderSide, ParseOrderError};
use serde_json::Number;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

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

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MarketTrade {
    pub amount: Quantity,
    pub executed_at: i64,
    pub price: Price,
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

#[derive(Debug, Clone)]
pub struct MarketDepthSnapshot {
    asks: SharedDepthLevels,
    bids: SharedDepthLevels,
    revision: Arc<()>,
    changes: Option<DepthDelta>,
    last_timestamp: i64,
    is_complete: bool,
}

impl MarketDepthSnapshot {
    /// 全板をBTreeMapへ実体化せず、指定sideを価格昇順に走査する。
    pub fn levels(&self, side: OrderSide) -> impl Iterator<Item = (&Price, &Quantity)> {
        match side {
            OrderSide::Buy => &self.bids,
            OrderSide::Sell => &self.asks,
        }
        .iter()
    }
    pub fn lowest_price(&self, side: OrderSide) -> Option<Price> {
        match side {
            OrderSide::Buy => &self.bids,
            OrderSide::Sell => &self.asks,
        }
        .first_key_value()
        .map(|(p, _)| *p)
    }
    pub fn highest_price(&self, side: OrderSide) -> Option<Price> {
        match side {
            OrderSide::Buy => &self.bids,
            OrderSide::Sell => &self.asks,
        }
        .last_key_value()
        .map(|(p, _)| *p)
    }
    pub fn quantity_at(&self, side: OrderSide, price: &Price) -> Option<Quantity> {
        match side {
            OrderSide::Buy => &self.bids,
            OrderSide::Sell => &self.asks,
        }
        .get(price)
        .copied()
    }
    pub fn new(
        asks: crate::book_units::Levels,
        bids: crate::book_units::Levels,
        last_timestamp: i64,
    ) -> Self {
        Self {
            asks: Arc::new(asks.into()),
            bids: Arc::new(bids.into()),
            revision: Arc::new(()),
            changes: None,
            last_timestamp,
            is_complete: true,
        }
    }

    pub fn empty() -> Self {
        Self {
            asks: Arc::new(crate::depth_levels::DepthLevels::default()),
            bids: Arc::new(crate::depth_levels::DepthLevels::default()),
            revision: Arc::new(()),
            changes: None,
            last_timestamp: 0,
            is_complete: false,
        }
    }

    /// 直前の板から継続した更新なら変更価格だけを返す。
    /// 要素は(side, 10^-8単位の価格, 10^-8単位の数量)。数量0は削除。
    /// 接続切替・更新の取りこぼしではNoneを返すため全体を再同期する。
    /// 同じ接続の全体板についても、直前の復元板との差分を返す。
    pub fn changes_since(&self, previous: &Self) -> Option<&[(OrderSide, Price, Quantity)]> {
        if Arc::ptr_eq(&self.revision, &previous.revision) {
            return Some(&[]);
        }
        self.changes.as_ref().and_then(|delta| {
            Arc::ptr_eq(&delta.previous, &previous.revision).then_some(delta.levels.as_ref())
        })
    }

    pub fn last_timestamp(&self) -> i64 {
        self.last_timestamp
    }

    pub fn is_complete(&self) -> bool {
        self.is_complete
    }
}

impl PartialEq for MarketDepthSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.asks == other.asks
            && self.bids == other.bids
            && self.last_timestamp == other.last_timestamp
            && self.is_complete == other.is_complete
    }
}

impl Depth for MarketDepthSnapshot {
    fn asks(&self) -> &BTreeMap<Price, Quantity> {
        self.asks.materialize()
    }
    fn bids(&self) -> &BTreeMap<Price, Quantity> {
        self.bids.materialize()
    }
    fn best_ask(&self) -> Option<(&Price, &Quantity)> {
        self.asks.first_key_value()
    }
    fn best_bid(&self) -> Option<(&Price, &Quantity)> {
        self.bids.last_key_value()
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
            asks: depth.asks.clone(),
            bids: depth.bids.clone(),
            revision: depth.revision.clone(),
            changes: depth.changes.clone(),
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
    InvalidDepth(String),
    InvalidTradeSide(ParseOrderError),
}

impl From<ParseOrderError> for MarketEventConversionError {
    fn from(err: ParseOrderError) -> Self {
        Self::InvalidTradeSide(err)
    }
}
