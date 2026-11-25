use crate::bitbank_structs::{
    BitbankMarginNotice, BitbankMarginPayables, BitbankMarginPosition, BitbankTradeHistoryDatum,
    BitbankWithdrawal, deserialize_string_or_number,
};
use serde::Deserialize;
use serde_json::Number;

/// Private Stream の通知。PubNub のメッセージ本文（`method` と `params`）に対応する。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub enum BitbankPrivateEvent {
    /// 残高の更新。
    AssetUpdate(Vec<BitbankAssetUpdate>),
    /// 新規注文。
    SpotOrderNew(Vec<BitbankSpotOrderUpdate>),
    /// 注文の更新。
    SpotOrder(Vec<BitbankSpotOrderUpdate>),
    /// 注文の無効化。
    SpotOrderInvalidation(BitbankSpotOrderInvalidation),
    /// 約定。
    SpotTrade(Vec<BitbankTradeHistoryDatum>),
    /// 販売所の新規注文。
    DealerOrderNew(Vec<BitbankDealerOrder>),
    /// 出金の更新。
    Withdrawal(Vec<BitbankWithdrawal>),
    /// 入金の更新。
    Deposit(Vec<BitbankDepositUpdate>),
    /// 信用取引の建玉の更新。
    MarginPositionUpdate(Vec<BitbankMarginPosition>),
    /// 信用取引の不足金の更新。
    MarginPayableUpdate(Vec<BitbankMarginPayables>),
    /// 追証、不足金、精算の通知。解消・完済時は各項目が `None` になる。
    MarginNoticeUpdate(Vec<BitbankMarginNotice>),
}

/// 資産の残高通知。
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankAssetUpdate {
    /// 通貨コード。
    pub asset: String,
    /// 数量の精度。
    #[serde(alias = "amountPrecision")]
    pub amount_precision: u64,
    /// 利用可能数量。
    #[serde(alias = "freeAmount")]
    pub free_amount: String,
    /// 注文等で拘束されている数量。
    #[serde(alias = "lockedAmount")]
    pub locked_amount: String,
    /// 保有数量。
    #[serde(alias = "onhandAmount")]
    pub onhand_amount: String,
    /// 出金中数量。
    #[serde(alias = "withdrawingAmount")]
    pub withdrawing_amount: String,
}

/// 取引所の新規注文・注文更新の通知。
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankSpotOrderUpdate {
    /// 注文ID。
    pub order_id: Number,
    /// 通貨ペア。
    pub pair: String,
    /// 売買方向（`buy`、`sell`）。
    pub side: String,
    /// 信用取引の建玉方向（`long`、`short`）。
    pub position_side: Option<String>,
    /// 注文種別。
    pub r#type: String,
    /// 発注時の数量。
    pub start_amount: Option<String>,
    /// 未約定数量。
    pub remaining_amount: Option<String>,
    /// 約定済み数量。
    pub executed_amount: String,
    /// 注文価格。
    pub price: Option<String>,
    /// 平均約定価格。
    pub average_price: String,
    /// トリガー価格。
    pub trigger_price: Option<String>,
    /// 指値注文の Post Only 指定。
    pub post_only: Option<bool>,
    /// ユーザーがキャンセルできる注文かどうか。
    pub user_cancelable: bool,
    /// この通知で注文が発動したかどうか。配信されない場合は `None`。
    pub is_just_triggered: Option<bool>,
    /// 注文ステータス。
    pub status: String,
    /// 発注時刻（Unixミリ秒）。
    pub ordered_at: Number,
    /// 約定時刻（Unixミリ秒）。
    pub executed_at: Option<Number>,
    /// 取消時刻（Unixミリ秒）。
    pub canceled_at: Option<Number>,
    /// 有効期限（Unixミリ秒）。
    pub expire_at: Option<Number>,
    /// 発動時刻（Unixミリ秒）。
    pub triggered_at: Option<Number>,
}

/// 無効になった注文の通知。
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankSpotOrderInvalidation {
    /// 無効になった注文のID一覧。
    pub order_id: Vec<Number>,
}

/// 販売所の新規注文通知。
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankDealerOrder {
    /// 注文ID。JSON数値で届いた場合も文字列で保持する。
    #[serde(deserialize_with = "deserialize_string_or_number")]
    pub order_id: String,
    /// 通貨コード。
    pub asset: String,
    /// 売買方向（`buy`、`sell`）。
    pub side: String,
    /// 注文価格。
    pub price: String,
    /// 注文数量。
    pub amount: String,
    /// 発注時刻（Unixミリ秒）。
    pub ordered_at: Number,
}

/// 入金の通知。
#[derive(Clone, Deserialize)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankDepositUpdate {
    /// 入金ID。
    pub uuid: String,
    /// 通貨コード。
    pub asset: String,
    /// 入金額。JSON数値で届いた場合も精度を保って文字列に変換する。
    #[serde(deserialize_with = "deserialize_string_or_number")]
    pub amount: String,
    /// 暗号資産のネットワーク。
    pub network: Option<String>,
    /// 暗号資産の入金先アドレス。
    pub address: Option<String>,
    /// 暗号資産のトランザクションID。
    pub txid: Option<String>,
    /// 入金検知時刻（Unixミリ秒）。
    pub found_at: u64,
    /// 入金確認時刻（Unixミリ秒）。確認前は `None`。
    pub confirmed_at: Option<u64>,
    /// 入金ステータス（`FOUND`、`CONFIRMED`、`DONE`）。
    pub status: String,
}

impl std::fmt::Debug for BitbankDepositUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitbankDepositUpdate")
            .finish_non_exhaustive()
    }
}
