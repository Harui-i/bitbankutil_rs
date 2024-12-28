use rust_decimal::prelude::*;
use serde::Deserialize;
use serde_json::Number;
use std::collections::BTreeMap;
use std::fmt;

use crate::depth::Depth;

// https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#ticker
#[allow(dead_code)]
#[derive(serde::Deserialize, Debug, Clone)]
pub struct BitbankTickerResponse {
    pub sell: Option<String>, // the lowest price of sell orders
    pub buy: Option<String>,  // the highest price of buy orders
    pub high: String,         // the highest price in last 24 hours
    pub low: String,          // thw lowest price in last 24 hours
    pub open: String,         // the open price at 24 hours ago
    pub last: String,         // the latest price executed
    pub vol: String,          // trading volume in last 24 hours
    pub timestamp: Number,    // ticked at unix timestamp (milliseconds)
}

// https://github.com/bitbankinc/bitbank-api-docs/blob/master/public-api.md#ticker
#[allow(dead_code)]
#[derive(serde::Deserialize, Debug, Clone)]
pub struct BitbankTickersDatum {
    pub pair: String,         // pair enum
    pub sell: Option<String>, // the lowest price of sell orders
    pub buy: Option<String>,  // the highest price of buy orders
    pub high: String,         // the highest price in last 24 hours
    pub low: String,          // thw lowest price in last 24 hours
    pub open: String,         // the open price at 24 hours ago
    pub last: String,         // the latest price executed
    pub vol: String,          // trading volume in last 24 hours
    pub timestamp: Number,    // ticked at unix timestamp (milliseconds)
}

//https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#assets
#[allow(dead_code)]
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
    pub network_list: Option<serde_json::Value>, // undefined for jpy
    pub collateral_ratio: String,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankAssetsData {
    pub assets: Vec<BitbankAssetDatum>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankCreateOrderResponse {
    pub order_id: Number,
    pub pair: String,
    pub side: String,                     // "buy" or "sell"
    pub position_side: Option<String>,    // string or null.
    pub r#type: String, // "limit", "market", "stop", "stop_limit", "take_profit", "stop_loss"
    pub start_amount: Option<String>, // order qty when placed
    pub remaining_amount: Option<String>, // qty not executed
    pub executed_amount: String, // qty executed
    pub price: Option<String>, // order price
    pub post_only: Option<bool>, // post only or not
    pub user_cancelable: bool, // whether cancelable order or note
    pub average_price: String, // avg executed price
    pub ordered_at: Number, // ordered at unix timestamp (milliseconds)
    pub expire_at: Option<Number>, // expiration time in unix timestamp (milliseconds)
    pub trigger_price: Option<String>,
    pub status: String, // status enum: `INACTIVE`, `UNFILLED`, `PARTIALLY_FILLED`, `FULLY_FILLED`, `CANCELED_UNFILLED`, `CANCELED_PARTIALLY_FILLED`
}

// https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#fetch-order-information
#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankGetOrderResponse {
    pub order_id: Number,
    pub pair: String,
    pub side: String,                     // "buy" or "sell"
    pub position_side: Option<String>,    // string or null.
    pub r#type: String, // "limit", "market", "stop", "stop_limit", "take_profit", "stop_loss"
    pub start_amount: Option<String>, // order qty when placed
    pub remaining_amount: Option<String>, // qty not executed
    pub executed_amount: String, // qty executed
    pub price: Option<String>, // order price
    pub post_only: Option<bool>, // post only or not
    pub user_cancelable: bool, // whether cancelable order or note
    pub average_price: String, // avg executed price
    pub ordered_at: Number, // ordered at unix timestamp (milliseconds)
    pub expire_at: Option<Number>, // expiration time in unix timestamp (milliseconds)
    pub trigger_price: Option<String>,
    pub status: String, // status enum: `INACTIVE`, `UNFILLED`, `PARTIALLY_FILLED`, `FULLY_FILLED`, `CANCELED_UNFILLED`, `CANCELED_PARTIALLY_FILLED`
}

// https://github.com/bitbankinc/bitbank-api-docs/blob/master/rest-api.md#cancel-order
#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankCancelOrderResponse {
    pub order_id: Number,
    pub pair: String,
    pub side: String,                     // "buy" or "sell"
    pub position_side: Option<String>,    // string or null.
    pub r#type: String, // "limit", "market", "stop", "stop_limit", "take_profit", "stop_loss"
    pub start_amount: Option<String>, // order qty when placed
    pub remaining_amount: Option<String>, // qty not executed
    pub executed_amount: String, // qty executed
    pub price: Option<String>, // order price (present only if type = "limit" or "stop_limit")
    pub post_only: Option<bool>, // whether post only or not (present only if type = "limit")
    pub user_cancelable: bool, // whether cancelable order or note
    pub average_price: String, // avg executed price
    pub ordered_at: Number, // ordered at unix timestamp (milliseconds)
    pub expire_at: Option<Number>, // expiration time in unix timestamp (milliseconds)
    pub canceled_at: Option<Number>, // canceled at unix timestamp (milliseconds)
    pub triggered_at: Option<Number>, // triggered at unix timestamp (milliseconds) (present only if type = "stop" or "stop_limit")
    pub trigger_price: Option<String>, // trigger price (present only if type = "stop" or "stop_limit" )
    pub status: String, // status enum: `INACTIVE`, `UNFILLED`, `PARTIALLY_FILLED`, `FULLY_FILLED`, `CANCELED_UNFILLED`, `CANCELED_PARTIALLY_FILLED`
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankCancelOrdersResponse {
    pub orders: Vec<BitbankCancelOrderResponse>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankActiveOrdersResponse {
    pub orders: Vec<BitbankGetOrderResponse>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankSpotStatus {
    pub pair: String,
    pub status: String,
    pub min_amount: String,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankSpotStatusResponse {
    pub statuses: Vec<BitbankSpotStatus>,
}

#[allow(dead_code, non_snake_case)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankWebSocketMessage {
    pub message: serde_json::Value,
    pub room_name: String,
}

#[allow(dead_code, non_snake_case)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankTransactionMessage {
    pub data: BitbankTransactionsData,
}

#[allow(dead_code, non_snake_case)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankTransactionsData {
    pub transactions: Vec<BitbankTransactionDatum>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankTransactionDatum {
    #[serde(with = "rust_decimal::serde::float")]
    pub amount: Decimal,
    pub executed_at: i64,
    #[serde(with = "rust_decimal::serde::float")]
    pub price: Decimal,
    pub side: String, // "buy" or "sell"
    pub transaction_id: i64,
}

#[allow(dead_code, non_snake_case)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankDepthDiffMessage {
    pub data: BitbankDepthDiff,
}

#[allow(dead_code, non_snake_case)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankDepthWholeMessage {
    pub data: BitbankDepthWhole,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankDepthDiff {
    pub a: Vec<Vec<String>>, // ask, amount
    pub b: Vec<Vec<String>>, // bid, amount
    pub ao: Option<String>, // optional. The quantity of asks over the highest price of asks orders. If there is no change in quantity, it will not be included in the message.
    pub bu: Option<String>, // optional. The quantity of bids under the lowest price of bids orders. If there is no change in quantity, it will not be included in the message.
    pub au: Option<String>, // optional. The quantity of asks under the lowest price of bids orders. If there is no change in quantity, it will not be included in the message.
    pub bo: Option<String>, // optional. The quantity of bids over the highest price of asks orders. If there is no change in quantity, it will not be included in the message.
    pub am: Option<String>, // optional. The quantity of market sell orders. If there is no change in quantity, it will not be included in the message.
    pub bm: Option<String>, // optional. The quantity of market buy orders. If there is no change in quantity, it will not be included in the message.

    pub t: i64,    // unixtime in milliseconds
    pub s: String, // sequence id. increasing-order but not necessarily consecutive
}

#[allow(dead_code)]
#[allow(non_snake_case)]
#[derive(Deserialize, Debug, Clone)]
pub struct BitbankDepthWhole {
    asks: Vec<Vec<String>>,
    bids: Vec<Vec<String>>,
    asks_over: String, // asks sum s.t. its price is higher than asks_highest value.
    // Without Circut Breaker, 200 offers from best-bid are sent via websocket.
    // So, asks_over is the sum of the rest of the offers.
    bids_under: String, // bids sum s.t. its price is lower than bids_lowest value.

    // these four values are 0 in non-CB mode.
    asks_under: String, // asks sum s.t. its price is lower than bids_lowest. (so low price)
    bids_over: String,  // bids sum s.t. its price is higher than asks_highest. (so high price)
    ask_market: String, // the quantity of market sell orders. Usually "0" in NORMAL mode.
    bid_market: String, // the quantity of market buy orders. Usually "0" in NORMAL mode.

    pub timestamp: i64,
    sequenceId: String,
}

#[derive(Clone)]
pub struct BitbankDepth {
    diff_buffer: BTreeMap<String, BitbankDepthDiff>,
    asks: BTreeMap<Decimal, f64>, // price, amount
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

        // delete diff items remaining in `diff_buffer` whose sequence id is less than or equal to `whole`'s sequence id.
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
