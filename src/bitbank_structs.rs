use rust_decimal::prelude::*;
use serde::Deserialize;
use serde_json::Number;
use std::collections::BTreeMap;
use std::fmt;

use crate::depth::Depth;

pub mod websocket_struct;

#[derive(serde::Deserialize, Debug, Clone)]
pub struct BitbankApiResponse {
    pub success: Number,
    pub data: serde_json::Value,
}

// https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#ticker
#[derive(serde::Deserialize, Debug, Clone)]
pub struct BitbankTickerResponse {
    pub sell: Option<String>, // 売り注文の最安値
    pub buy: Option<String>,  // 買い注文の最高値
    pub high: String,         // 過去24時間の最高値
    pub low: String,          // 過去24時間の最安値
    pub open: String,         // 24時間前の始値
    pub last: String,         // 最終取引価格
    pub vol: String,          // 過去24時間の取引量
    pub timestamp: Number,    // Unixタイムスタンプ（ミリ秒）
}

// https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#circuit-break-info
#[derive(serde::Deserialize, Debug, Clone)]
pub struct BitbankCircuitBreakInfo {
    pub mode: String, // enum: `NONE`, `CIRCUIT_BREAK`, `FULL_RANGE_CIRCUIT_BREAK`, `RESUMPTION`, `LISTING`.
    pub estimated_itayose_price: Option<String>, // 推定価格。モードが`NONE`の場合、または推定価格がない場合はNull。
    pub estimated_itayose_amount: Option<String>, // 推定数量。モードが`NONE`の場合はNull。
    pub itayose_upper_price: Option<String>, // 寄付き価格範囲の上限。モードが`NONE`、`FULL_RANGE_CIRCUIT_BREAK`、`LISTING`の場合はNull。
    pub itayose_lower_price: Option<String>, // 寄付き価格範囲の下限。モードが`NONE`、`FULL_RANGE_CIRCUIT_BREAK`、`LISTING`の場合はNull。
    pub upper_trigger_price: Option<String>, // 上限トリガー価格。モードが`NONE`でない場合はNull。
    pub lower_trigger_price: Option<String>, // 下限トリガー価格。モードが`NONE`でない場合はNull。
    pub fee_type: String,                    // enum: `NORMAL`, `SELL_MAKER`, `BUY_MAKER`, `DYNAMIC`
    pub reopen_timestamp: Option<Number>, // 再開タイムスタンプ（ミリ秒）。モードが`NONE`の場合、または再開タイムスタンプがまだ未定の場合はNull。
    pub timestamp: Number,                // Unixタイムスタンプ（ミリ秒）
}

//https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#assets
#[derive(serde::Deserialize, Debug, Clone)]
pub struct BitbankAssetDatum {
    pub asset: String,
    pub free_amount: String,
    pub amount_precision: Number,
    pub onhand_amount: String,
    pub locked_amount: String,
    pub withdrawing_amount: String,
    pub withdrawal_fee: serde_json::Value,
    pub stop_deposit: bool,
    pub stop_withdrawal: bool,
    pub network_list: Option<serde_json::Value>, // JPYでは未定義
    pub collateral_ratio: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BitbankAssetsData {
    pub assets: Vec<BitbankAssetDatum>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BitbankTradeHistoryDatum {
    pub trade_id: Number,                  // 取引ID
    pub pair: String,                      // ペア
    pub order_id: Number,                  // 注文ID
    pub side: String,                      // "buy" または "sell"
    pub position_side: Option<String>,     // "long" または "short"
    pub r#type: String, // "limit"、"market"、"stop"、"stop_limit"、"take_profit"、"stop_loss" のいずれか
    pub amount: String, // 数量
    pub price: String,  // 注文価格
    pub maker_taker: String, // maker または taker
    pub fee_amount_base: String, // 基軸資産の手数料額
    pub fee_amount_quote: String, // クオート資産の手数料額
    pub fee_occurred_amount_quote: String, // 後で取得されるクオート手数料発生額。現物取引の場合、この値はfee_amount_quoteと同じである
    pub profit_loss: Option<String>,       // 実現損益
    pub interest: Option<String>,          // 金利
    pub executed_at: Number,               // 注文約定時のUnixタイムスタンプ（ミリ秒）
}

#[derive(Deserialize, Debug, Clone)]
pub struct BitbankTradeHistoryResponse {
    pub trades: Vec<BitbankTradeHistoryDatum>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BitbankCreateOrderResponse {
    pub order_id: Number,
    pub pair: String,
    pub side: String,                     // "buy" または "sell"
    pub position_side: Option<String>,    // 文字列またはnull
    pub r#type: String, // "limit"、"market"、"stop"、"stop_limit"、"take_profit"、"stop_loss"
    pub start_amount: Option<String>, // 発注時の注文数量
    pub remaining_amount: Option<String>, // 未約定の数量
    pub executed_amount: String, // 約定済み数量
    pub price: Option<String>, // 注文価格
    pub post_only: Option<bool>, // ポストオンリーかどうか
    pub user_cancelable: bool, // キャンセル可能な注文かどうか
    pub average_price: String, // 平均約定価格
    pub ordered_at: Number, // 発注時のUnixタイムスタンプ（ミリ秒）
    pub expire_at: Option<Number>, // 有効期限のUnixタイムスタンプ（ミリ秒）
    pub trigger_price: Option<String>,
    pub status: String, // ステータス: `INACTIVE`、`UNFILLED`、`PARTIALLY_FILLED`、`FULLY_FILLED`、`CANCELED_UNFILLED`、`CANCELED_PARTIALLY_FILLED`
}

// https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-order-information
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankGetOrderResponse {
    pub order_id: Number,
    pub pair: String,
    pub side: String,                     // "buy" または "sell"
    pub position_side: Option<String>,    // 文字列またはnull
    pub r#type: String, // "limit"、"market"、"stop"、"stop_limit"、"take_profit"、"stop_loss"
    pub start_amount: Option<String>, // 発注時の注文数量
    pub remaining_amount: Option<String>, // 未約定の数量
    pub executed_amount: String, // 約定済み数量
    pub price: Option<String>, // 注文価格
    pub post_only: Option<bool>, // ポストオンリーかどうか
    pub user_cancelable: bool, // キャンセル可能な注文かどうか
    pub average_price: String, // 平均約定価格
    pub ordered_at: Number, // 発注時のUnixタイムスタンプ（ミリ秒）
    pub expire_at: Option<Number>, // 有効期限のUnixタイムスタンプ（ミリ秒）
    pub trigger_price: Option<String>,
    pub status: String, // ステータス: `INACTIVE`、`UNFILLED`、`PARTIALLY_FILLED`、`FULLY_FILLED`、`CANCELED_UNFILLED`、`CANCELED_PARTIALLY_FILLED`
}

// https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#cancel-order
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankCancelOrderResponse {
    pub order_id: Number,
    pub pair: String,
    pub side: String,                     // "buy" または "sell"
    pub position_side: Option<String>,    // 文字列またはnull
    pub r#type: String, // "limit"、"market"、"stop"、"stop_limit"、"take_profit"、"stop_loss"
    pub start_amount: Option<String>, // 発注時の注文数量
    pub remaining_amount: Option<String>, // 未約定の数量
    pub executed_amount: String, // 約定済み数量
    pub price: Option<String>, // 注文価格（typeが "limit" または "stop_limit" の場合のみ存在）
    pub post_only: Option<bool>, // ポストオンリーかどうか（typeが "limit" の場合のみ存在）
    pub user_cancelable: bool, // キャンセル可能な注文かどうか
    pub average_price: String, // 平均約定価格
    pub ordered_at: Number, // 発注時のUnixタイムスタンプ（ミリ秒）
    pub expire_at: Option<Number>, // 有効期限のUnixタイムスタンプ（ミリ秒）
    pub canceled_at: Option<Number>, // キャンセル時のUnixタイムスタンプ（ミリ秒）
    pub triggered_at: Option<Number>, // トリガー時のUnixタイムスタンプ（ミリ秒）（typeが "stop" または "stop_limit" の場合のみ存在）
    pub trigger_price: Option<String>, // トリガー価格（typeが "stop" または "stop_limit" の場合のみ存在）
    pub status: String, // ステータス: `INACTIVE`、`UNFILLED`、`PARTIALLY_FILLED`、`FULLY_FILLED`、`CANCELED_UNFILLED`、`CANCELED_PARTIALLY_FILLED`
}

#[derive(Deserialize, Debug, Clone)]
pub struct BitbankCancelOrdersResponse {
    pub orders: Vec<BitbankCancelOrderResponse>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BitbankActiveOrdersResponse {
    pub orders: Vec<BitbankGetOrderResponse>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BitbankChannelAndTokenResponse {
    pub pubnub_channel: String,
    pub pubnub_token: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BitbankSpotStatus {
    pub pair: String,
    pub status: String,
    pub min_amount: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BitbankSpotStatusResponse {
    pub statuses: Vec<BitbankSpotStatus>,
}

#[allow(non_snake_case)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankTransactionsData {
    pub transactions: Vec<BitbankTransactionDatum>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BitbankTransactionDatum {
    #[serde(with = "rust_decimal::serde::float")]
    pub amount: Decimal,
    pub executed_at: i64,
    #[serde(with = "rust_decimal::serde::float")]
    pub price: Decimal,
    pub side: String, // "buy" または "sell"
    pub transaction_id: i64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BitbankDepthDiff {
    pub a: Vec<Vec<String>>, // 売り注文、数量
    pub b: Vec<Vec<String>>, // 買い注文、数量
    pub ao: Option<String>, // オプション。売り注文の最高値を超える売り注文の数量。数量に変更がない場合は、メッセージに含まれません。
    pub bu: Option<String>, // オプション。買い注文の最安値を下回る買い注文の数量。数量に変更がない場合は、メッセージに含まれません。
    pub au: Option<String>, // オプション。買い注文の最安値を下回る売り注文の数量。数量に変更がない場合は、メッセージに含まれません。
    pub bo: Option<String>, // オプション。売り注文の最高値を超える買い注文の数量。数量に変更がない場合は、メッセージに含まれません。
    pub am: Option<String>, // オプション。成行売り注文の数量。数量に変更がない場合は、メッセージに含まれません。
    pub bm: Option<String>, // オプション。成行買い注文の数量。数量に変更がない場合は、メッセージに含まれません。

    pub t: i64,    // Unixタイムスタンプ（ミリ秒）
    pub s: String, // シーケンスID。昇順だが、必ずしも連続しているとは限りない
}

#[allow(non_snake_case)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankDepthWhole {
    asks: Vec<Vec<String>>,
    bids: Vec<Vec<String>>,
    #[allow(dead_code)]
    asks_over: String, // asks_highest値より価格が高いasksの合計。
    // サーキットブレーカーなしでは、best-bidから200件のオファーがwebsocket経由で送信される。
    // そのため、asks_overは残りのオファーの合計である。
    #[allow(dead_code)]
    bids_under: String, // bids_lowest値より価格が低いbidsの合計。

    // これら4つの値は非CBモードでは0である。
    #[allow(dead_code)]
    asks_under: String, // bids_lowestより価格が低いasksの合計。（つまり低価格）
    #[allow(dead_code)]
    bids_over: String, // asks_highestより価格が高いbidsの合計。（つまり高価格）
    #[allow(dead_code)]
    ask_market: String, // 成行売り注文の数量。通常、NORMALモードでは "0" である。
    #[allow(dead_code)]
    bid_market: String, // 成行買い注文の数量。通常、NORMALモードでは "0" である。

    pub timestamp: i64,
    sequenceId: String,
}

#[derive(Debug, Clone)]
pub struct BitbankDepth {
    diff_buffer: BTreeMap<String, BitbankDepthDiff>,
    asks: BTreeMap<Decimal, f64>, // 価格、数量
    bids: BTreeMap<Decimal, f64>,

    is_complete: bool,
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
        write!(f, "\n")?;
        self.format_depth(Some(20), f)
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
                self.asks.insert(price.clone(), amount);
            }
        }

        for bid in &diff.b {
            let price = &bid[0].parse::<Decimal>().unwrap();
            let amount = bid[1].parse::<f64>().unwrap();

            if amount == f64::zero() {
                self.bids.remove(price);
            } else {
                self.bids.insert(price.clone(), amount);
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
            self.asks.insert(price.clone(), amount);
        }

        for bid in whole.bids {
            let price = &bid[0].parse::<Decimal>().unwrap();
            let amount = bid[1].parse::<f64>().unwrap();

            assert_ne!(amount, f64::zero());
            self.bids.insert(price.clone(), amount);
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
                    self.asks.insert(price.clone(), amount);
                }
            }

            for bid in &depth_diff.b {
                let price = &bid[0].parse::<Decimal>().unwrap();
                let amount = bid[1].parse::<f64>().unwrap();

                if amount == f64::zero() {
                    self.bids.remove(price);
                } else {
                    self.bids.insert(price.clone(), amount);
                }
            }
        }
    }
}
