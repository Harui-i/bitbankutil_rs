use rust_decimal::prelude::*;
use serde::Deserialize;
use serde_json::Number;
use std::collections::BTreeMap;
use std::fmt;

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
    /// "limit"、"market"、"stop"、"stop_limit"、"take_profit"、"stop_loss"。
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
    /// "limit"、"market"、"stop"、"stop_limit"、"take_profit"、"stop_loss"。
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
    /// "limit"、"market"、"stop"、"stop_limit"、"take_profit"、"stop_loss"。
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
    #[serde(with = "rust_decimal::serde::float")]
    pub amount: Decimal,
    /// 約定時刻（Unixタイムスタンプ、ミリ秒）。
    pub executed_at: i64,
    /// 約定価格。
    #[serde(with = "rust_decimal::serde::float")]
    pub price: Decimal,
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
    sequenceId: String,
}

/// 板情報（差分と全体の統合管理）。
#[derive(Debug, Clone)]
pub struct BitbankDepth {
    /// 差分バッファ（シーケンスID順）。
    diff_buffer: BTreeMap<String, BitbankDepthDiff>,
    /// 価格、数量。
    asks: BTreeMap<Decimal, f64>,
    /// 価格、数量。
    bids: BTreeMap<Decimal, f64>,

    /// 全体板を受信済みか。
    is_complete: bool,
    /// 最後に反映したタイムスタンプ。
    last_timestamp: i64,
}

impl Depth for BitbankDepth {
    fn asks(&self) -> &BTreeMap<Decimal, f64> {
        &self.asks
    }

    fn bids(&self) -> &BTreeMap<Decimal, f64> {
        &self.bids
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
            asks: BTreeMap::new(),
            bids: BTreeMap::new(),
            last_timestamp: 0,
            is_complete: false,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.is_complete
    }

    pub fn last_timestamp(&self) -> i64 {
        self.last_timestamp
    }

    pub fn insert_diff(&mut self, diff: BitbankDepthDiff) {
        for ask in &diff.a {
            let price = &ask[0].parse::<Decimal>().unwrap();
            let amount = ask[1].parse::<f64>().unwrap();

            if amount == f64::zero() {
                self.asks.remove(price);
            } else {
                self.asks.insert(*price, amount);
            }
        }

        for bid in &diff.b {
            let price = &bid[0].parse::<Decimal>().unwrap();
            let amount = bid[1].parse::<f64>().unwrap();

            if amount == f64::zero() {
                self.bids.remove(price);
            } else {
                self.bids.insert(*price, amount);
            }
        }

        if self.last_timestamp < diff.t {
            self.last_timestamp = diff.t;
        }
        self.diff_buffer.insert(diff.s.clone(), diff);
    }

    pub fn update_whole(&mut self, whole: BitbankDepthWhole) {
        let seq = whole.sequenceId.clone();

        // `diff_buffer`に残っている、シーケンスIDが`whole`のシーケンスID以下のdiff項目を削除する。
        let keys_to_remove: Vec<String> = self
            .diff_buffer
            .iter()
            .filter(|(key, _)| key < &&seq)
            .map(|(key, _)| key.clone())
            .collect();

        for key in keys_to_remove {
            self.diff_buffer.remove(&key);
        }

        self.asks.clear();
        self.bids.clear();

        for ask in whole.asks {
            let price = &ask[0].parse::<Decimal>().unwrap();
            let amount = ask[1].parse::<f64>().unwrap();

            assert_ne!(amount, f64::zero());
            self.asks.insert(*price, amount);
        }

        for bid in whole.bids {
            let price = &bid[0].parse::<Decimal>().unwrap();
            let amount = bid[1].parse::<f64>().unwrap();

            assert_ne!(amount, f64::zero());
            self.bids.insert(*price, amount);
        }

        if self.last_timestamp < whole.timestamp {
            self.last_timestamp = whole.timestamp;
        }

        self.process_diff_buffer();
        self.is_complete = true;
    }

    fn process_diff_buffer(&mut self) {
        for (_sequence_id, depth_diff) in self.diff_buffer.iter() {
            for ask in &depth_diff.a {
                let price = &ask[0].parse::<Decimal>().unwrap();
                let amount = ask[1].parse::<f64>().unwrap();

                if amount == f64::zero() {
                    self.asks.remove(price);
                } else {
                    self.asks.insert(*price, amount);
                }
            }

            for bid in &depth_diff.b {
                let price = &bid[0].parse::<Decimal>().unwrap();
                let amount = bid[1].parse::<f64>().unwrap();

                if amount == f64::zero() {
                    self.bids.remove(price);
                } else {
                    self.bids.insert(*price, amount);
                }
            }
        }
    }
}
