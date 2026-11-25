use crate::book_units::{Price, Quantity};
use serde::Deserialize;
use serde_json::Number;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::depth::Depth;

pub mod websocket_struct;

/// Bitbank APIの標準レスポンス。
#[derive(serde::Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankApiResponse {
    /// 成功フラグ。
    pub success: Number,
    /// APIごとのレスポンス本体。
    pub data: serde_json::Value,
}

/// Tickerレスポンス（単一ペア）。
///
/// 仕様: <https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#ticker>
#[derive(serde::Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankTickerResponse {
    /// 売り注文の最安値。
    pub sell: Option<String>,
    /// 買い注文の最高値。
    pub buy: Option<String>,
    /// 過去24時間の最高値。
    pub high: String,
    /// 過去24時間の最安値。
    pub low: String,
    /// 24時間前の始値。
    pub open: String,
    /// 最終取引価格。
    pub last: String,
    /// 過去24時間の取引量。
    pub vol: String,
    /// Unixタイムスタンプ（ミリ秒）。
    pub timestamp: Number,
}

/// Tickersレスポンス（複数ペア）。
///
/// 仕様: <https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#tickers>
#[derive(serde::Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankTickersDatum {
    /// 通貨ペア。
    pub pair: String,
    /// 売り注文の最安値。
    pub sell: Option<String>,
    /// 買い注文の最高値。
    pub buy: Option<String>,
    /// 過去24時間の最高値。
    pub high: String,
    /// 過去24時間の最安値。
    pub low: String,
    /// 24時間前の始値。
    pub open: String,
    /// 最終取引価格。
    pub last: String,
    /// 過去24時間の取引量。
    pub vol: String,
    /// Unixタイムスタンプ（ミリ秒）。
    pub timestamp: Number,
}

/// Circuit Break情報。
///
/// 仕様: <https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#circuit-break-info>
#[derive(serde::Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankCircuitBreakInfo {
    /// enum: `NONE`, `CIRCUIT_BREAK`, `FULL_RANGE_CIRCUIT_BREAK`, `RESUMPTION`, `LISTING`.
    pub mode: String,
    /// 推定価格。モードが`NONE`の場合、または推定価格がない場合はNull。
    pub estimated_itayose_price: Option<String>,
    /// 推定数量。モードが`NONE`の場合はNull。
    pub estimated_itayose_amount: Option<String>,
    /// 寄付き価格範囲の上限。モードが`NONE`、`FULL_RANGE_CIRCUIT_BREAK`、`LISTING`の場合はNull。
    pub itayose_upper_price: Option<String>,
    /// 寄付き価格範囲の下限。モードが`NONE`、`FULL_RANGE_CIRCUIT_BREAK`、`LISTING`の場合はNull。
    pub itayose_lower_price: Option<String>,
    /// 上限トリガー価格。モードが`NONE`でない場合はNull。
    pub upper_trigger_price: Option<String>,
    /// 下限トリガー価格。モードが`NONE`でない場合はNull。
    pub lower_trigger_price: Option<String>,
    /// enum: `NORMAL`, `SELL_MAKER`, `BUY_MAKER`, `DYNAMIC`
    pub fee_type: String,
    /// 再開タイムスタンプ（ミリ秒）。モードが`NONE`の場合、または再開タイムスタンプがまだ未定の場合はNull。
    pub reopen_timestamp: Option<Number>,
    /// Unixタイムスタンプ（ミリ秒）。
    pub timestamp: Number,
}

/// ローソク足データ（OHLCV）。
///
/// 仕様: <https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#candlestick>
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankOhlcv {
    /// 始値。
    pub open: String,
    /// 高値。
    pub high: String,
    /// 安値。
    pub low: String,
    /// 終値。
    pub close: String,
    /// 出来高。
    pub volume: String,
    /// Unixタイムスタンプ（ミリ秒）。
    pub timestamp: i64,
}

/// ローソク足の区分とそのデータ本体。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankCandlestickEntry {
    /// ローソク足の種類。
    pub r#type: String,
    /// OHLCVの配列。
    pub ohlcv: Vec<BitbankOhlcv>,
}

/// ローソク足レスポンス。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankCandlestickResponse {
    /// ローソク足エントリの一覧。
    pub candlestick: Vec<BitbankCandlestickEntry>,
    /// これはドキュメントされてないが実際には含まれているフィールド。
    /// Issueを建てた: <https://github.com/bitbankinc/bitbank-api-docs/issues/142>
    pub timestamp: i64,
}

/// 登録済みの出金先。
#[derive(Deserialize, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankWithdrawalAccount {
    /// 出金申請に指定する出金先ID。
    pub uuid: String,
    /// 出金先のラベル（暗号資産）。
    pub label: Option<String>,
    /// 出金ネットワーク（暗号資産）。
    pub network: Option<String>,
    /// 出金先アドレス（暗号資産）。
    pub address: Option<String>,
    /// 出金先の銀行名（JPY）。
    pub bank_name: Option<String>,
    /// 出金先の支店名（JPY）。
    pub branch_name: Option<String>,
    /// 出金先の口座種別（JPY）。
    pub account_type: Option<String>,
    /// 出金先の口座番号（JPY）。
    pub account_number: Option<String>,
    /// 出金先の口座名義（JPY）。
    pub account_owner: Option<String>,
}

impl fmt::Debug for BitbankWithdrawalAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BitbankWithdrawalAccount")
            .finish_non_exhaustive()
    }
}

/// 出金先照会の結果。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankWithdrawalAccountsResponse {
    /// 指定アセットの出金先一覧。
    pub accounts: Vec<BitbankWithdrawalAccount>,
}

/// 出金先に付随する宛先タグまたはメモ。
#[derive(Deserialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum BitbankWithdrawalDestinationTag {
    /// 数値で返る宛先タグ。
    Number(Number),
    /// 文字列で返る宛先タグまたはメモ。
    Text(String),
}

/// 出金申請の結果、または出金履歴の1件。
#[derive(Deserialize, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankWithdrawal {
    /// 出金ID。
    pub uuid: String,
    /// アセット名。
    pub asset: String,
    /// 出金先ID。
    pub account_uuid: String,
    /// 出金数量の十進文字列。
    pub amount: String,
    /// 出金手数料の十進文字列。
    pub fee: String,
    /// 出金先のラベル（暗号資産）。
    pub label: Option<String>,
    /// 出金先アドレス（暗号資産）。
    pub address: Option<String>,
    /// 出金ネットワーク（暗号資産）。
    pub network: Option<String>,
    /// 宛先タグまたはメモ（指定のある暗号資産）。
    pub destination_tag: Option<BitbankWithdrawalDestinationTag>,
    /// 出金トランザクションID（暗号資産。未反映の場合はNone）。
    pub txid: Option<String>,
    /// 出金先の銀行名（JPY）。
    pub bank_name: Option<String>,
    /// 出金先の支店名（JPY）。
    pub branch_name: Option<String>,
    /// 出金先の口座種別（JPY）。
    pub account_type: Option<String>,
    /// 出金先の口座番号（JPY）。
    pub account_number: Option<String>,
    /// 出金先の口座名義（JPY）。
    pub account_owner: Option<String>,
    /// 出金のステータス。
    pub status: String,
    /// 申請時点のUnixタイムスタンプ（ミリ秒）。
    pub requested_at: i64,
}

impl fmt::Debug for BitbankWithdrawal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BitbankWithdrawal").finish_non_exhaustive()
    }
}

/// 出金履歴照会の結果。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankWithdrawalHistoryResponse {
    /// 条件に一致する出金履歴。
    pub withdrawals: Vec<BitbankWithdrawal>,
}

/// 資産情報のエントリ。
///
/// 仕様: <https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#assets>
#[derive(serde::Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankAssetDatum {
    /// 通貨コード。
    pub asset: String,
    /// 利用可能数量。
    pub free_amount: String,
    /// 数量の精度。
    pub amount_precision: Number,
    /// 保有数量。
    pub onhand_amount: String,
    /// ロック中数量。
    pub locked_amount: String,
    /// 出金中数量。
    pub withdrawing_amount: String,
    /// 出金手数料。
    pub withdrawal_fee: serde_json::Value,
    /// 入金停止フラグ。
    pub stop_deposit: bool,
    /// 出金停止フラグ。
    pub stop_withdrawal: bool,
    /// ネットワーク一覧。JPYでは未定義。
    pub network_list: Option<serde_json::Value>,
    /// 担保掛目。
    pub collateral_ratio: String,
}

/// 資産一覧レスポンスの本体。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankAssetsData {
    /// 資産情報の一覧。
    pub assets: Vec<BitbankAssetDatum>,
}

/// 入金履歴レスポンス。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankDepositHistoryResponse {
    /// 入金履歴の一覧。
    pub deposits: Vec<BitbankDepositHistoryDatum>,
}

/// 入金履歴の1件。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankDepositHistoryDatum {
    /// 入金UUID。
    pub uuid: String,
    /// 入金アドレス。レスポンスに含まれない場合は `None`。
    pub address: Option<String>,
    /// 資産コード。
    pub asset: String,
    /// 入金ネットワーク。レスポンスに含まれない場合は `None`。
    pub network: Option<String>,
    /// 入金額。APIがJSON数値を返す場合も文字列に変換する。
    #[serde(deserialize_with = "deserialize_deposit_amount")]
    pub amount: String,
    /// 暗号資産のトランザクションID。
    pub txid: Option<String>,
    /// 入金ステータス（`FOUND`、`CONFIRMED`、`DONE`）。
    pub status: String,
    /// 入金区分（`NORMAL`、`MANUAL`、`STAKING`）。
    pub category: String,
    /// 入金検知時刻のUnixタイムスタンプ（ミリ秒）。
    pub found_at: u64,
    /// 入金確認時刻のUnixタイムスタンプ（ミリ秒）。確認前は `None`。
    pub confirmed_at: Option<u64>,
}

fn deserialize_deposit_amount<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Amount {
        Text(String),
        Number(Number),
    }

    match Amount::deserialize(deserializer)? {
        Amount::Text(text) => Ok(text),
        Amount::Number(number) => Ok(number.to_string()),
    }
}

/// 未反映入金レスポンス。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankUnconfirmedDepositsResponse {
    /// 送付元情報の登録が必要な入金の一覧。
    pub deposits: Vec<BitbankUnconfirmedDepositDatum>,
}

/// 未反映入金の1件。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankUnconfirmedDepositDatum {
    /// 未反映入金のUUID。
    pub uuid: String,
    /// 資産コード。
    pub asset: String,
    /// 入金額。
    pub amount: String,
    /// 入金ネットワーク。
    pub network: String,
    /// トランザクションID。
    pub txid: String,
    /// 作成時刻のUnixタイムスタンプ（ミリ秒）。
    pub created_at: u64,
}

/// 入金の送付元候補一覧レスポンス。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankDepositOriginatorsResponse {
    /// 送付元候補の一覧。
    pub originators: Vec<BitbankDepositOriginatorDatum>,
}

/// 未反映入金に登録できる送付元候補。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankDepositOriginatorDatum {
    /// 送付元UUID。
    pub uuid: String,
    /// 送付元のラベル。
    pub label: String,
    /// 送付元の区分（`WALLET` または `ELSE`）。
    pub deposit_type: String,
    /// 入金目的。
    pub deposit_purpose: Option<String>,
    /// 送付元の審査ステータス。
    pub originator_status: String,
    /// 送付元の種別（`OWN`、`PERSON`、`COMPANY`）。
    pub originator_type: String,
    /// 送付人の姓。
    pub originator_last_name: Option<String>,
    /// 送付人の名。
    pub originator_first_name: Option<String>,
    /// 送付元の国。
    pub originator_country: Option<String>,
    /// 送付元の都道府県、州、地域。
    pub originator_prefecture: Option<String>,
    /// 送付元の市区町村。
    pub originator_city: Option<String>,
    /// 送付元の住所。
    pub originator_address: Option<String>,
    /// 送付元の建物名。
    pub originator_building: Option<String>,
    /// 送付元の法人名。
    pub originator_company_name: Option<String>,
    /// 送付元の法人種別。
    pub originator_company_type: Option<String>,
    /// 法人種別を法人名の前後どちらに付けるか。
    pub originator_company_type_position: Option<String>,
    /// 法人の実質的支配者の一覧。
    pub originator_substantial_controllers: Option<Vec<BitbankDepositOriginatorController>>,
}

/// 法人の送付元の実質的支配者。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankDepositOriginatorController {
    /// 実質的支配者のUUID。
    pub uuid: String,
    /// 実質的支配者の氏名。
    pub name: String,
    /// 実質的支配者の国。
    pub country: String,
    /// 実質的支配者の都道府県、州、地域。
    pub prefecture: Option<String>,
}

/// 未反映入金と、登録する送付元の組み合わせ。
#[derive(serde::Serialize, Debug, Clone)]
pub struct BitbankDepositConfirmation<'a> {
    /// 未反映入金のUUID。
    pub uuid: &'a str,
    /// 送付元候補のUUID。
    pub originator_uuid: &'a str,
}

/// 約定履歴のエントリ。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankTradeHistoryDatum {
    /// 取引ID。
    pub trade_id: Number,
    /// ペア。
    pub pair: String,
    /// 注文ID。
    pub order_id: Number,
    /// "buy" または "sell"。
    pub side: String,
    /// "long" または "short"。
    pub position_side: Option<String>,
    /// "limit"、"market"、"stop"、"stop_limit"、"take_profit"、"stop_loss" のいずれか。
    pub r#type: String,
    /// 数量。
    pub amount: String,
    /// 注文価格。
    pub price: String,
    /// maker または taker。
    pub maker_taker: String,
    /// 基軸資産の手数料額。
    pub fee_amount_base: String,
    /// クオート資産の手数料額。
    pub fee_amount_quote: String,
    /// 後で取得されるクオート手数料発生額。現物取引の場合、この値はfee_amount_quoteと同じである。
    pub fee_occurred_amount_quote: String,
    /// 実現損益。
    pub profit_loss: Option<String>,
    /// 金利。
    pub interest: Option<String>,
    /// 注文約定時のUnixタイムスタンプ（ミリ秒）。
    pub executed_at: Number,
}

/// 約定履歴レスポンス。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankTradeHistoryResponse {
    /// 約定履歴の一覧。
    pub trades: Vec<BitbankTradeHistoryDatum>,
}

/// 発注レスポンス。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankCreateOrderResponse {
    /// 注文ID。
    pub order_id: Number,
    /// 通貨ペア。
    pub pair: String,
    /// "buy" または "sell"。
    pub side: String,
    /// 文字列またはnull。
    pub position_side: Option<String>,
    /// "limit"、"market"、"stop"、"stop_limit"、"take_profit"、"stop_loss"、"losscut"。
    pub r#type: String,
    /// 発注時の注文数量。
    pub start_amount: Option<String>,
    /// 未約定の数量。
    pub remaining_amount: Option<String>,
    /// 約定済み数量。
    pub executed_amount: String,
    /// 注文価格。
    pub price: Option<String>,
    /// ポストオンリーかどうか。
    pub post_only: Option<bool>,
    /// キャンセル可能な注文かどうか。
    pub user_cancelable: bool,
    /// 平均約定価格。
    pub average_price: String,
    /// 発注時のUnixタイムスタンプ（ミリ秒）。
    pub ordered_at: Number,
    /// 有効期限のUnixタイムスタンプ（ミリ秒）。
    pub expire_at: Option<Number>,
    /// トリガー価格。
    pub trigger_price: Option<String>,
    /// ステータス: `INACTIVE`、`UNFILLED`、`PARTIALLY_FILLED`、`FULLY_FILLED`、`CANCELED_UNFILLED`、`CANCELED_PARTIALLY_FILLED`。
    pub status: String,
}

/// 注文情報取得レスポンス。
///
/// 仕様: <https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-order-information>
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankGetOrderResponse {
    /// 注文ID。
    pub order_id: Number,
    /// 通貨ペア。
    pub pair: String,
    /// "buy" または "sell"。
    pub side: String,
    /// 文字列またはnull。
    pub position_side: Option<String>,
    /// "limit"、"market"、"stop"、"stop_limit"、"take_profit"、"stop_loss"、"losscut"。
    pub r#type: String,
    /// 発注時の注文数量。
    pub start_amount: Option<String>,
    /// 未約定の数量。
    pub remaining_amount: Option<String>,
    /// 約定済み数量。
    pub executed_amount: String,
    /// 注文価格。
    pub price: Option<String>,
    /// ポストオンリーかどうか。
    pub post_only: Option<bool>,
    /// キャンセル可能な注文かどうか。
    pub user_cancelable: bool,
    /// 平均約定価格。
    pub average_price: String,
    /// 発注時のUnixタイムスタンプ（ミリ秒）。
    pub ordered_at: Number,
    /// 有効期限のUnixタイムスタンプ（ミリ秒）。
    pub expire_at: Option<Number>,
    /// トリガー価格。
    pub trigger_price: Option<String>,
    /// ステータス: `INACTIVE`、`UNFILLED`、`PARTIALLY_FILLED`、`FULLY_FILLED`、`CANCELED_UNFILLED`、`CANCELED_PARTIALLY_FILLED`。
    pub status: String,
}

/// 注文キャンセルレスポンス。
///
/// 仕様: <https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#cancel-order>
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankCancelOrderResponse {
    /// 注文ID。
    pub order_id: Number,
    /// 通貨ペア。
    pub pair: String,
    /// "buy" または "sell"。
    pub side: String,
    /// 文字列またはnull。
    pub position_side: Option<String>,
    /// "limit"、"market"、"stop"、"stop_limit"、"take_profit"、"stop_loss"、"losscut"。
    pub r#type: String,
    /// 発注時の注文数量。
    pub start_amount: Option<String>,
    /// 未約定の数量。
    pub remaining_amount: Option<String>,
    /// 約定済み数量。
    pub executed_amount: String,
    /// 注文価格（typeが "limit" または "stop_limit" の場合のみ存在）。
    pub price: Option<String>,
    /// ポストオンリーかどうか（typeが "limit" の場合のみ存在）。
    pub post_only: Option<bool>,
    /// キャンセル可能な注文かどうか。
    pub user_cancelable: bool,
    /// 平均約定価格。
    pub average_price: String,
    /// 発注時のUnixタイムスタンプ（ミリ秒）。
    pub ordered_at: Number,
    /// 有効期限のUnixタイムスタンプ（ミリ秒）。
    pub expire_at: Option<Number>,
    /// キャンセル時のUnixタイムスタンプ（ミリ秒）。
    pub canceled_at: Option<Number>,
    /// トリガー時のUnixタイムスタンプ（ミリ秒）（typeが "stop" または "stop_limit" の場合のみ存在）。
    pub triggered_at: Option<Number>,
    /// トリガー価格（typeが "stop" または "stop_limit" の場合のみ存在）。
    pub trigger_price: Option<String>,
    /// ステータス: `INACTIVE`、`UNFILLED`、`PARTIALLY_FILLED`、`FULLY_FILLED`、`CANCELED_UNFILLED`、`CANCELED_PARTIALLY_FILLED`。
    pub status: String,
}

/// 複数注文キャンセルレスポンス。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankCancelOrdersResponse {
    /// キャンセル結果の一覧。
    pub orders: Vec<BitbankCancelOrderResponse>,
}

/// アクティブ注文レスポンス。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankActiveOrdersResponse {
    /// アクティブ注文の一覧。
    pub orders: Vec<BitbankGetOrderResponse>,
}

/// WebSocket用のチャンネル・トークン情報。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankChannelAndTokenResponse {
    /// PubNubチャンネル名。
    pub pubnub_channel: String,
    /// PubNubトークン。
    pub pubnub_token: String,
}

/// 現物取引のステータス情報。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankSpotStatus {
    /// 通貨ペア。
    pub pair: String,
    /// ステータス。
    pub status: String,
    /// 最小発注量。
    pub min_amount: String,
}

/// 現物取引ステータス一覧レスポンス。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankSpotStatusResponse {
    /// ステータス一覧。
    pub statuses: Vec<BitbankSpotStatus>,
}

/// 取引ペアの設定。
///
/// 手数料率や数量などの十進値は API の文字列形式で保持する。
///
/// 仕様: <https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#get-all-pairs-info>
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankSpotPair {
    /// 通貨ペア名。
    pub name: String,
    /// 基軸通貨。
    pub base_asset: String,
    /// 決済通貨。
    pub quote_asset: String,
    /// 基軸通貨建てのメイカー手数料率。
    pub maker_fee_rate_base: String,
    /// 基軸通貨建てのテイカー手数料率。
    pub taker_fee_rate_base: String,
    /// 決済通貨建てのメイカー手数料率。
    pub maker_fee_rate_quote: String,
    /// 決済通貨建てのテイカー手数料率。
    pub taker_fee_rate_quote: String,
    /// 信用新規注文の決済通貨建てメイカー手数料率。
    pub margin_open_maker_fee_rate_quote: Option<String>,
    /// 信用新規注文の決済通貨建てテイカー手数料率。
    pub margin_open_taker_fee_rate_quote: Option<String>,
    /// 信用返済注文の決済通貨建てメイカー手数料率。
    pub margin_close_maker_fee_rate_quote: Option<String>,
    /// 信用返済注文の決済通貨建てテイカー手数料率。
    pub margin_close_taker_fee_rate_quote: Option<String>,
    /// ロング建玉の日次金利。
    pub margin_long_interest: Option<String>,
    /// ショート建玉の日次金利。
    pub margin_short_interest: Option<String>,
    /// 個人の現在のリスク想定比率。
    pub margin_current_individual_ratio: Option<String>,
    /// 個人の現在のリスク想定比率の適用終了時刻（UNIX ミリ秒）。
    pub margin_current_individual_until: Option<i64>,
    /// 法人の現在のリスク想定比率。
    pub margin_current_company_ratio: Option<String>,
    /// 法人の現在のリスク想定比率の適用終了時刻（UNIX ミリ秒）。
    pub margin_current_company_until: Option<i64>,
    /// 個人の次回のリスク想定比率。
    pub margin_next_individual_ratio: Option<String>,
    /// 個人の次回のリスク想定比率の適用終了時刻（UNIX ミリ秒）。
    pub margin_next_individual_until: Option<i64>,
    /// 法人の次回のリスク想定比率。
    pub margin_next_company_ratio: Option<String>,
    /// 法人の次回のリスク想定比率の適用終了時刻（UNIX ミリ秒）。
    pub margin_next_company_until: Option<i64>,
    /// 最小発注数量。
    pub unit_amount: String,
    /// 指値注文の最大数量。
    pub limit_max_amount: String,
    /// 成行注文の最大数量。
    pub market_max_amount: String,
    /// 成行注文の許容率。
    pub market_allowance_rate: String,
    /// サーキットブレーカー中の成行注文の許容率。実 API が返す追加フィールド。
    pub market_in_cb_allowance_rate: Option<String>,
    /// 価格の小数桁数。
    pub price_digits: u32,
    /// 数量の小数桁数。
    pub amount_digits: u32,
    /// ペアが有効か。
    pub is_enabled: bool,
    /// 注文受付を停止しているか。
    pub stop_order: bool,
    /// 注文受付と取消を停止しているか。
    pub stop_order_and_cancel: bool,
    /// 成行注文を停止しているか。
    pub stop_market_order: bool,
    /// 逆指値成行注文を停止しているか。
    pub stop_stop_order: bool,
    /// 逆指値指値注文を停止しているか。
    pub stop_stop_limit_order: bool,
    /// 新規ロング注文を停止しているか。
    pub stop_margin_long_order: bool,
    /// 新規ショート注文を停止しているか。
    pub stop_margin_short_order: bool,
    /// 買い注文を停止しているか。
    pub stop_buy_order: bool,
    /// 売り注文を停止しているか。
    pub stop_sell_order: bool,
}

/// 取引ペアの設定一覧レスポンス。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankSpotPairsResponse {
    /// 全ペアの設定。
    pub pairs: Vec<BitbankSpotPair>,
}

#[allow(non_snake_case)]
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankTransactionsData {
    /// 約定一覧。
    pub transactions: Vec<BitbankTransactionDatum>,
}

/// 約定のエントリ。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankTransactionDatum {
    /// 約定数量。
    pub amount: Quantity,
    /// 約定時刻（Unixタイムスタンプ、ミリ秒）。
    pub executed_at: i64,
    /// 約定価格。
    pub price: Price,
    /// "buy" または "sell"。
    pub side: String,
    /// 約定ID。
    pub transaction_id: i64,
}

/// 板差分（WebSocket）。
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankDepthDiff {
    /// 売り注文、数量。
    pub a: Vec<Vec<String>>,
    /// 買い注文、数量。
    pub b: Vec<Vec<String>>,
    /// オプション。売り注文の最高値を超える売り注文の数量。数量に変更がない場合は、メッセージに含まれません。
    pub ao: Option<String>,
    /// オプション。買い注文の最安値を下回る買い注文の数量。数量に変更がない場合は、メッセージに含まれません。
    pub bu: Option<String>,
    /// オプション。買い注文の最安値を下回る売り注文の数量。数量に変更がない場合は、メッセージに含まれません。
    pub au: Option<String>,
    /// オプション。売り注文の最高値を超える買い注文の数量。数量に変更がない場合は、メッセージに含まれません。
    pub bo: Option<String>,
    /// オプション。成行売り注文の数量。数量に変更がない場合は、メッセージに含まれません。
    pub am: Option<String>,
    /// オプション。成行買い注文の数量。数量に変更がない場合は、メッセージに含まれません。
    pub bm: Option<String>,

    /// Unixタイムスタンプ（ミリ秒）。
    pub t: i64,
    /// シーケンスID。昇順だが、必ずしも連続しているとは限りない。
    pub s: String,
}

#[allow(non_snake_case)]
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(feature = "strict-validation", serde(deny_unknown_fields))]
pub struct BitbankDepthWhole {
    /// 売り板（価格、数量）。
    asks: Vec<Vec<String>>,
    /// 買い板（価格、数量）。
    bids: Vec<Vec<String>>,
    #[allow(dead_code)]
    /// asks_highest値より価格が高いasksの合計。
    ///
    /// サーキットブレーカーなしでは、best-bidから200件のオファーがwebsocket経由で送信される。
    /// そのため、asks_overは残りのオファーの合計である。
    asks_over: String,
    #[allow(dead_code)]
    /// bids_lowest値より価格が低いbidsの合計。
    bids_under: String,

    /// これら4つの値は非CBモードでは0である。
    #[allow(dead_code)]
    /// bids_lowestより価格が低いasksの合計。（つまり低価格）
    asks_under: String,
    #[allow(dead_code)]
    /// asks_highestより価格が高いbidsの合計。（つまり高価格）
    bids_over: String,
    #[allow(dead_code)]
    /// 成行売り注文の数量。通常、NORMALモードでは "0" である。
    ask_market: String,
    #[allow(dead_code)]
    /// 成行買い注文の数量。通常、NORMALモードでは "0" である。
    bid_market: String,

    /// Unixタイムスタンプ（ミリ秒）。
    pub timestamp: i64,
    /// シーケンスID。
    pub(crate) sequenceId: String,
}

pub(crate) type SharedDepthLevels = Arc<crate::depth_levels::DepthLevels>;

#[derive(Debug, Clone)]
pub(crate) struct DepthDelta {
    pub previous: Arc<()>,
    pub levels: Arc<[(crate::order_domain::OrderSide, Price, Quantity)]>,
}

/// 板情報（差分と全体の統合管理）。
#[derive(Debug, Clone)]
pub struct BitbankDepth {
    /// 差分バッファ（シーケンスID順）。
    diff_buffer: BTreeMap<u64, BitbankDepthDiff>,
    last_whole: Option<u64>,
    last_diff: Option<u64>,
    /// 価格、数量。
    pub(crate) asks: SharedDepthLevels,
    /// 価格、数量。
    pub(crate) bids: SharedDepthLevels,

    /// 全体板を受信済みか。
    is_complete: bool,
    /// 最後に反映したタイムスタンプ。
    last_timestamp: i64,
    pub(crate) revision: Arc<()>,
    pub(crate) changes: Option<DepthDelta>,
}

impl Depth for BitbankDepth {
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

impl fmt::Display for BitbankDepth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        assert!(self.is_complete);
        writeln!(f)?;
        self.format_depth(Some(20), f)
    }
}

impl Default for BitbankDepth {
    fn default() -> Self {
        Self::new()
    }
}

impl BitbankDepth {
    pub fn new() -> Self {
        BitbankDepth {
            diff_buffer: BTreeMap::new(),
            last_whole: None,
            last_diff: None,
            asks: Arc::new(crate::depth_levels::DepthLevels::default()),
            bids: Arc::new(crate::depth_levels::DepthLevels::default()),
            last_timestamp: 0,
            is_complete: false,
            revision: Arc::new(()),
            changes: None,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.is_complete
    }

    pub fn last_timestamp(&self) -> i64 {
        self.last_timestamp
    }
    pub(crate) fn whole_sequence(&self) -> Option<u64> {
        self.last_whole
    }

    pub fn insert_diff(&mut self, diff: BitbankDepthDiff) -> Result<(), DepthValidationError> {
        let seq = diff
            .s
            .parse::<u64>()
            .map_err(|_| DepthValidationError::new("invalid_sequence", "s", None, None))?;
        let asks = Self::parse_levels(&diff.a, false, "sell")?;
        let bids = Self::parse_levels(&diff.b, false, "buy")?;
        if self.last_whole.is_some_and(|s| seq <= s) || self.diff_buffer.contains_key(&seq) {
            return Ok(());
        }
        if !self.last_diff.is_some_and(|s| seq < s) {
            let previous = self.revision.clone();
            let mut changes = Vec::with_capacity(diff.a.len() + diff.b.len());
            for (side, levels) in [
                (crate::order_domain::OrderSide::Sell, &asks),
                (crate::order_domain::OrderSide::Buy, &bids),
            ] {
                changes.extend(levels.iter().map(|&(price, amount)| (side, price, amount)));
            }
            // 同一価格の最終値だけをqueueへ渡す。差分の適用順と一致させる。
            changes.sort_by_key(|&(side, price, _)| (side, price));
            changes.dedup_by(|later, earlier| {
                if (later.0, later.1) == (earlier.0, earlier.1) {
                    earlier.2 = later.2;
                    true
                } else {
                    false
                }
            });
            if !asks.is_empty() {
                Self::apply_levels(Arc::make_mut(&mut self.asks), &asks);
            }
            if !bids.is_empty() {
                Self::apply_levels(Arc::make_mut(&mut self.bids), &bids);
            }
            self.revision = Arc::new(());
            self.changes = Some(DepthDelta {
                previous,
                levels: changes.into(),
            });
            self.last_timestamp = self.last_timestamp.max(diff.t);
            self.last_diff = Some(seq);
        }
        self.diff_buffer.insert(seq, diff);
        Ok(())
    }
    pub fn update_whole(&mut self, whole: BitbankDepthWhole) -> Result<(), DepthValidationError> {
        let seq = whole
            .sequenceId
            .parse::<u64>()
            .map_err(|_| DepthValidationError::new("invalid_sequence", "sequenceId", None, None))?;
        let asks = Self::parse_levels(&whole.asks, true, "sell")?;
        let bids = Self::parse_levels(&whole.bids, true, "buy")?;
        if self.last_whole.is_some_and(|s| seq <= s) {
            return Ok(());
        }
        let previous = self
            .is_complete
            .then(|| (self.revision.clone(), self.asks.clone(), self.bids.clone()));
        self.diff_buffer.retain(|s, _| *s > seq);
        self.asks = Arc::new(crate::depth_levels::DepthLevels::default());
        self.bids = Arc::new(crate::depth_levels::DepthLevels::default());
        Self::apply_levels(Arc::make_mut(&mut self.asks), &asks);
        Self::apply_levels(Arc::make_mut(&mut self.bids), &bids);
        self.revision = Arc::new(());
        self.changes = None;
        self.last_timestamp = self.last_timestamp.max(whole.timestamp);
        self.last_whole = Some(seq);
        self.last_diff = Some(self.last_diff.unwrap_or(seq).max(seq));
        self.process_diff_buffer();
        self.is_complete = true;
        // 全体配信でも、同じ接続の直前板との差は一度計算すれば反復再生で共有できる。
        // 消えた価格の0更新も含め、queue側の全価格再走査を避ける。
        if let Some((previous, old_asks, old_bids)) = previous {
            let mut changes = Vec::new();
            for (side, old, new) in [
                (crate::order_domain::OrderSide::Sell, old_asks, &self.asks),
                (crate::order_domain::OrderSide::Buy, old_bids, &self.bids),
            ] {
                let mut old = old.iter().peekable();
                let mut new = new.iter().peekable();
                while old.peek().is_some() || new.peek().is_some() {
                    match (old.peek(), new.peek()) {
                        (Some((op, oq)), Some((np, nq))) if op == np => {
                            if oq != nq {
                                changes.push((side, **np, **nq));
                            }
                            old.next();
                            new.next();
                        }
                        (Some((op, _)), Some((np, _))) if op < np => {
                            changes.push((side, **op, Quantity::ZERO));
                            old.next();
                        }
                        (Some((op, _)), None) => {
                            changes.push((side, **op, Quantity::ZERO));
                            old.next();
                        }
                        (_, Some((np, nq))) => {
                            changes.push((side, **np, **nq));
                            new.next();
                        }
                        (None, None) => break,
                    }
                }
            }
            self.changes = Some(DepthDelta {
                previous,
                levels: changes.into(),
            });
        }
        Ok(())
    }
    fn parse_levels(
        levels: &[Vec<String>],
        whole: bool,
        side: &'static str,
    ) -> Result<Vec<(Price, Quantity)>, DepthValidationError> {
        let mut parsed = Vec::with_capacity(levels.len());
        for (index, level) in levels.iter().enumerate() {
            let failure =
                |reason, field| DepthValidationError::new(reason, field, Some(side), Some(index));
            if level.len() != 2 {
                return Err(failure("invalid_level_shape", "level"));
            }
            let price = level[0]
                .parse::<Price>()
                .map_err(|e| failure(crate::error_diagnostics::numeric(&e), "price"))?;
            let amount = level[1]
                .parse::<Quantity>()
                .map_err(|e| failure(crate::error_diagnostics::numeric(&e), "amount"))?;
            if price <= Price::ZERO {
                return Err(failure("non_positive_price", "price"));
            }
            if amount < Quantity::ZERO || (whole && amount == Quantity::ZERO) {
                return Err(failure(
                    if whole {
                        "non_positive_amount"
                    } else {
                        "negative_amount"
                    },
                    "amount",
                ));
            }
            parsed.push((price, amount));
        }
        Ok(parsed)
    }
    fn apply_levels(book: &mut crate::depth_levels::DepthLevels, levels: &[(Price, Quantity)]) {
        if levels.is_empty() {
            return;
        }
        for &(price, amount) in levels {
            if amount == Quantity::ZERO {
                book.remove(&price);
            } else {
                book.insert(price, amount);
            }
        }
    }

    fn process_diff_buffer(&mut self) {
        for diff in self.diff_buffer.values() {
            Self::apply_levels(
                Arc::make_mut(&mut self.asks),
                &Self::parse_levels(&diff.a, false, "sell").expect("validated buffered asks"),
            );
            Self::apply_levels(
                Arc::make_mut(&mut self.bids),
                &Self::parse_levels(&diff.b, false, "buy").expect("validated buffered bids"),
            );
            self.last_timestamp = self.last_timestamp.max(diff.t);
        }
    }
}

/// 板の検証失敗。外部の値やエラー全文を含めず、安全な位置と理由を保持する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthValidationError {
    pub reason_code: &'static str,
    pub field: &'static str,
    pub side: Option<&'static str>,
    pub level_index: Option<usize>,
}
impl DepthValidationError {
    fn new(
        reason_code: &'static str,
        field: &'static str,
        side: Option<&'static str>,
        level_index: Option<usize>,
    ) -> Self {
        Self {
            reason_code,
            field,
            side,
            level_index,
        }
    }
}
impl std::fmt::Display for DepthValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: field={}, side={:?}, level_index={:?}",
            self.reason_code, self.field, self.side, self.level_index
        )
    }
}
impl std::error::Error for DepthValidationError {}
