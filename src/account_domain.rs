//! 戦略から利用する注文状態・自分の約定履歴。数量・価格・手数料は Decimal、時刻は Unix ミリ秒。
//!
//! bitbank固有のレスポンスをそのまま戦略へ渡すと、戦略ごとに数値文字列の変換や
//! 注文ステータスの文字列比較が必要になる。ここで共通型へ変換し、実取引とPaperが
//! 同じ型を返すことで、戦略側はAPIの形式や取引環境の違いを意識せずに照会結果を扱える。
//! APIの形式が変わった場合も、共通型で表せる範囲なら変換部分で吸収できる。
//!
//! 発注・未約定注文の照合に使う型は order_domain に置き、完了・取消済み注文も含めた
//! 状態照会と、自分の約定履歴に使う型・取得条件・変換処理はこのモジュールにまとめる。
//! この分割は照会に関する定義を一か所で追えるようにするためのもので、別ファイルで
//! なければ機能が成立しないわけではない。
use crate::{
    bitbank_structs::{BitbankGetOrderResponse, BitbankTradeHistoryDatum},
    order_domain::{OrderId, OrderSide, OrderType, ParseOrderError},
};
use rust_decimal::Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    Inactive,
    Unfilled,
    PartiallyFilled,
    FullyFilled,
    CanceledUnfilled,
    CanceledPartiallyFilled,
    Rejected,
}

impl std::str::FromStr for OrderStatus {
    type Err = ParseOrderError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "INACTIVE" => Ok(Self::Inactive),
            "UNFILLED" => Ok(Self::Unfilled),
            "PARTIALLY_FILLED" => Ok(Self::PartiallyFilled),
            "FULLY_FILLED" => Ok(Self::FullyFilled),
            "CANCELED_UNFILLED" => Ok(Self::CanceledUnfilled),
            "CANCELED_PARTIALLY_FILLED" => Ok(Self::CanceledPartiallyFilled),
            "REJECTED" => Ok(Self::Rejected),
            _ => Err(ParseOrderError::UnknownStatus(value.into())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderSnapshot {
    pub order_id: OrderId,
    pub pair: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub position_side: Option<String>,
    pub start_amount: Option<Decimal>,
    pub remaining_amount: Option<Decimal>,
    pub executed_amount: Decimal,
    pub price: Option<Decimal>,
    pub average_price: Decimal,
    pub post_only: Option<bool>,
    pub user_cancelable: bool,
    pub status: OrderStatus,
    /// Paperでは発注・取消の実時刻を持たないため None。
    pub ordered_at: Option<i64>,
    pub expire_at: Option<i64>,
    pub canceled_at: Option<i64>,
    pub triggered_at: Option<i64>,
    pub trigger_price: Option<Decimal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidityRole {
    Maker,
    Taker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnTrade {
    /// Paperでは公開約定IDとは別に、自分の約定ごとに採番する。
    pub trade_id: u64,
    pub order_id: OrderId,
    pub pair: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub position_side: Option<String>,
    pub amount: Decimal,
    pub price: Decimal,
    pub liquidity: LiquidityRole,
    /// 負の手数料はリベート。
    pub fee_amount_base: Decimal,
    pub fee_amount_quote: Decimal,
    pub fee_occurred_amount_quote: Decimal,
    pub profit_loss: Option<Decimal>,
    pub interest: Option<Decimal>,
    pub executed_at: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HistoryOrder {
    Asc,
    #[default]
    Desc,
}

impl HistoryOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

/// 1回分の履歴取得条件。全件の自動ページングは行わない。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TradeHistoryQuery {
    pub pair: Option<String>,
    /// 1..=1000。None も共通インターフェースでは1000件として扱う。
    pub count: Option<u16>,
    /// 指定時は pair も必須。
    pub order_id: Option<OrderId>,
    pub since: Option<i64>,
    pub end: Option<i64>,
    pub order: HistoryOrder,
}

impl TradeHistoryQuery {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.pair.as_ref().is_some_and(|p| p.is_empty()) {
            return Err("pair must not be empty");
        }
        if self.order_id.is_some() && self.pair.is_none() {
            return Err("order_id requires pair");
        }
        // 下位クライアントの既存の i64 引数への変換を保証する。
        if self.order_id.is_some_and(|id| id.0 > i64::MAX as u64) {
            return Err("order_id exceeds i64 range");
        }
        if self.count.is_some_and(|n| n == 0 || n > 1000) {
            return Err("count must be in 1..=1000");
        }
        if self.since.is_some_and(|t| t < 0) || self.end.is_some_and(|t| t < 0) {
            return Err("timestamps must be nonnegative");
        }
        if let (Some(since), Some(end)) = (self.since, self.end) {
            if since > end {
                return Err("since must not exceed end");
            }
        }
        Ok(())
    }

    pub(crate) fn select(&self, trades: &[OwnTrade]) -> Vec<OwnTrade> {
        let mut result: Vec<_> = trades
            .iter()
            .filter(|t| {
                self.pair.as_ref().is_none_or(|pair| pair == &t.pair)
                    && self.order_id.is_none_or(|id| id == t.order_id)
                    && self.since.is_none_or(|since| t.executed_at >= since)
                    && self.end.is_none_or(|end| t.executed_at <= end)
            })
            .cloned()
            .collect();
        result.sort_by_key(|t| (t.executed_at, t.trade_id));
        if self.order == HistoryOrder::Desc {
            result.reverse();
        }
        result.truncate(usize::from(self.count.unwrap_or(1000)));
        result
    }
}

fn decimal(field: &str, value: &str) -> Result<Decimal, ParseOrderError> {
    value
        .parse()
        .map_err(|_| ParseOrderError::InvalidDecimal(field.into()))
}
fn optional_decimal(
    field: &str,
    value: &Option<String>,
) -> Result<Option<Decimal>, ParseOrderError> {
    value.as_deref().map(|s| decimal(field, s)).transpose()
}
fn timestamp(field: &str, value: &serde_json::Number) -> Result<i64, ParseOrderError> {
    value
        .as_i64()
        .filter(|t| *t >= 0)
        .ok_or_else(|| ParseOrderError::InvalidInteger(field.into()))
}
fn optional_timestamp(
    field: &str,
    value: &Option<serde_json::Number>,
) -> Result<Option<i64>, ParseOrderError> {
    value.as_ref().map(|n| timestamp(field, n)).transpose()
}

impl TryFrom<&BitbankGetOrderResponse> for OrderSnapshot {
    type Error = ParseOrderError;
    fn try_from(v: &BitbankGetOrderResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            order_id: OrderId(v.order_id.as_u64().ok_or(ParseOrderError::InvalidOrderId)?),
            pair: v.pair.clone(),
            side: v.side.parse()?,
            order_type: v.r#type.parse()?,
            position_side: v.position_side.clone(),
            start_amount: optional_decimal("start_amount", &v.start_amount)?,
            remaining_amount: optional_decimal("remaining_amount", &v.remaining_amount)?,
            executed_amount: decimal("executed_amount", &v.executed_amount)?,
            price: optional_decimal("price", &v.price)?,
            average_price: decimal("average_price", &v.average_price)?,
            post_only: v.post_only,
            user_cancelable: v.user_cancelable,
            status: v.status.parse()?,
            ordered_at: Some(timestamp("ordered_at", &v.ordered_at)?),
            expire_at: optional_timestamp("expire_at", &v.expire_at)?,
            canceled_at: optional_timestamp("canceled_at", &v.canceled_at)?,
            triggered_at: optional_timestamp("triggered_at", &v.triggered_at)?,
            trigger_price: optional_decimal("trigger_price", &v.trigger_price)?,
        })
    }
}

impl TryFrom<&BitbankTradeHistoryDatum> for OwnTrade {
    type Error = ParseOrderError;
    fn try_from(v: &BitbankTradeHistoryDatum) -> Result<Self, Self::Error> {
        Ok(Self {
            trade_id: v
                .trade_id
                .as_u64()
                .ok_or_else(|| ParseOrderError::InvalidInteger("trade_id".into()))?,
            order_id: OrderId(v.order_id.as_u64().ok_or(ParseOrderError::InvalidOrderId)?),
            pair: v.pair.clone(),
            side: v.side.parse()?,
            order_type: v.r#type.parse()?,
            position_side: v.position_side.clone(),
            amount: decimal("amount", &v.amount)?,
            price: decimal("price", &v.price)?,
            liquidity: match v.maker_taker.as_str() {
                "maker" => LiquidityRole::Maker,
                "taker" => LiquidityRole::Taker,
                other => return Err(ParseOrderError::UnknownLiquidityRole(other.into())),
            },
            fee_amount_base: decimal("fee_amount_base", &v.fee_amount_base)?,
            fee_amount_quote: decimal("fee_amount_quote", &v.fee_amount_quote)?,
            fee_occurred_amount_quote: decimal(
                "fee_occurred_amount_quote",
                &v.fee_occurred_amount_quote,
            )?,
            profit_loss: optional_decimal("profit_loss", &v.profit_loss)?,
            interest: optional_decimal("interest", &v.interest)?,
            executed_at: timestamp("executed_at", &v.executed_at)?,
        })
    }
}
