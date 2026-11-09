pub mod bitbank_bot;
pub mod bitbank_private;
pub mod bitbank_public;
pub mod bitbank_structs;
#[doc = include_str!("../docs/numeric-types.md")]
pub mod book_units;
mod depth_levels;
mod error_diagnostics;
pub mod market_event;
pub mod order_domain;
pub mod order_manager;
pub mod paper_execution;
pub mod response_handler;
pub mod telemetry;
pub mod trading_venue;
pub mod websocket_handler;

pub mod depth {
    use crate::book_units::{Price, Quantity};
    use core::fmt;
    use std::collections::BTreeMap;
    pub trait Depth {
        fn asks(&self) -> &BTreeMap<Price, Quantity>;
        fn bids(&self) -> &BTreeMap<Price, Quantity>;

        fn best_ask(&self) -> Option<(&Price, &Quantity)> {
            self.asks().iter().next()
        }

        fn best_bid(&self) -> Option<(&Price, &Quantity)> {
            self.bids().iter().next_back()
        }

        fn kth_best_ask(&self, k: usize) -> Option<(&Price, &Quantity)> {
            self.asks().iter().nth(k)
        }

        fn kth_best_bid(&self, k: usize) -> Option<(&Price, &Quantity)> {
            self.bids().iter().nth_back(k)
        }

        // 最小価格pを返す。ここで、Sum_{bestask <= price <= p} (amount) >= rである。
        // 直感的に考えると、サイズrの成行買い注文を実行したときの最高価格である。
        fn r_depth_ask_price(&self, r: f64) -> Option<&Price> {
            let mut sum = 0.0;
            for (price, amount) in self.asks().iter() {
                sum += amount.to_f64();
                if sum >= r {
                    return Some(price);
                }
            }

            None
        }

        // 最大価格pを返す。ここで、Sum_{p <= price <= bestbid} (amount) >= rである。
        // 直感的に考えると、サイズrの成行売り注文を実行したときの最低価格である。
        fn r_depth_bid_price(&self, r: f64) -> Option<&Price> {
            let mut sum = 0.0;
            for (price, amount) in self.bids().iter().rev() {
                sum += amount.to_f64();
                if sum >= r {
                    return Some(price);
                }
            }

            None
        }

        // 最小価格pを返す。ここで、Sum_{bestask <= price <= p} (amount * price) >= sである。
        // 直感的に考えると、サイズs（ドル建て）の成行買い注文を実行したときの最高価格である。
        fn s_depth_ask_price(&self, s: f64) -> Option<&Price> {
            let mut sum = 0.0;
            for (price, amount) in self.asks().iter() {
                sum += price.to_f64() * amount.to_f64();
                if sum >= s {
                    return Some(price);
                }
            }

            None
        }

        // 最大価格pを返す。ここで、Sum_{p <= price <= bestbid} (amount * price) >= sである。
        // 直感的に考えると、サイズs（ドル建て）の成行売り注文を実行したときの最低価格である。
        fn s_depth_bid_price(&self, s: f64) -> Option<&Price> {
            let mut sum = 0.0;
            for (price, amount) in self.bids().iter().rev() {
                sum += price.to_f64() * amount.to_f64();
                if sum >= s {
                    return Some(price);
                }
            }

            None
        }

        // log(r-depth ask price) - log(best ask price)を返す。
        fn r_depth_ask_logdiff(&self, r: f64) -> Option<f64> {
            let ask_price = self.r_depth_ask_price(r)?;
            let best_ask_price = self.best_ask()?.0;

            let ask_price_f64 = ask_price.to_f64();
            let best_ask_price_f64 = best_ask_price.to_f64();

            Some(ask_price_f64.ln() - best_ask_price_f64.ln())
        }

        // log(r-depth bid price) - log(best bid price)を返す。
        fn r_depth_bid_logdiff(&self, r: f64) -> Option<f64> {
            let bid_price = self.r_depth_bid_price(r)?;
            let best_bid_price = self.best_bid()?.0;

            let bid_price_f64 = bid_price.to_f64();
            let best_bid_price_f64 = best_bid_price.to_f64();

            Some(bid_price_f64.ln() - best_bid_price_f64.ln())
        }

        // log(s-depth ask price) - log(best ask price)を返す。
        fn s_depth_ask_logdiff(&self, s: f64) -> Option<f64> {
            let ask_price = self.s_depth_ask_price(s)?;
            let best_ask_price = self.best_ask()?.0;

            let ask_price = ask_price.to_f64();
            let best_ask_price = best_ask_price.to_f64();

            Some(ask_price.ln() - best_ask_price.ln())
        }

        // log(s-depth bid price) - log(best bid price)を返す。
        fn s_depth_bid_logdiff(&self, s: f64) -> Option<f64> {
            let bid_price = self.s_depth_bid_price(s)?;
            let best_bid_price = self.best_bid()?.0;

            let bid_price = bid_price.to_f64();
            let best_bid_price = best_bid_price.to_f64();

            Some(bid_price.ln() - best_bid_price.ln())
        }

        fn bidask_spread(&self) -> Option<Price> {
            if self.best_ask().is_some() && self.best_bid().is_some() {
                self.best_ask()
                    .unwrap()
                    .0
                    .checked_sub(*self.best_bid().unwrap().0)
                    .ok()
            } else {
                None
            }
        }

        fn bidask_imbalance(&self) -> Option<f64> {
            if self.best_ask().is_some() && self.best_bid().is_some() {
                let ask = self.best_ask().unwrap().1;
                let bid = self.best_bid().unwrap().1;
                Some(bid.to_f64() - ask.to_f64())
            } else {
                None
            }
        }

        // 上位kレベルのデプスデータをフォーマットする。kがnoneの場合、20レベルをフォーマットする。
        fn format_depth(&self, k: Option<usize>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let k2 = k.unwrap_or(20);

            for (price, amount) in self.asks().iter().take(k2).rev() {
                writeln!(f, "{}\t{:.4}", price, amount)?;
            }

            writeln!(f, "asks")?;
            write!(f, "mid ")?;

            if self.best_ask().is_some() && self.best_bid().is_some() {
                write!(
                    f,
                    "spread: {:.4}",
                    self.best_ask()
                        .unwrap()
                        .0
                        .checked_sub(*self.best_bid().unwrap().0)
                        .map_err(|_| fmt::Error)?
                )?;
            }

            if self.kth_best_ask(2).is_some() && self.kth_best_bid(2).is_some() {
                write!(
                    f,
                    ", second-best spread: {:.4}",
                    self.kth_best_ask(2)
                        .unwrap()
                        .0
                        .checked_sub(*self.kth_best_bid(2).unwrap().0)
                        .map_err(|_| fmt::Error)?
                )?;
            }

            writeln!(f)?;

            writeln!(f, "bids")?;
            for (price, amount) in self.bids().iter().rev().take(k2) {
                writeln!(f, "{}\t{:.4}", price, amount)?;
            }

            Ok(())
        }
    }
}

pub mod bybit {
    use crate::book_units::{Price, Quantity};
    use serde::Deserialize;
    use serde_json::Number;

    use std::collections::BTreeMap;
    use std::fmt;

    use crate::depth::Depth;

    #[allow(dead_code, non_snake_case)]
    #[derive(Deserialize, Debug)]
    pub struct BybitTransactionDatum {
        pub BT: bool,    // ブロック取引かどうか
        pub S: String,   // サイド
        pub T: i64,      // タイムスタンプ
        pub i: String,   // 取引ID
        pub p: Price,    // 価格
        pub s: String,   // シンボル
        pub v: Quantity, // 取引サイズ
    }

    #[allow(dead_code, non_snake_case)]
    #[derive(Deserialize, Debug)]
    pub struct BybitTradeWebSocketMessage {
        pub data: Vec<BybitTransactionDatum>,
        pub topic: String,
        pub ts: i64,
        r#type: String,
    }

    #[derive(Deserialize, Debug)]
    #[allow(non_snake_case, dead_code)]
    pub struct BybitOrderbookWebSocketMessage {
        pub topic: String,  // トピック、例："publicTrade.BTCUSDT"
        pub r#type: String, // データ型 `snapshot`、`delta`
        pub ts: i64,        // タイムスタンプ
        pub data: BybitOrderbookData,
        cts: Number, // このオーダーブックデータが生成されたときのマッチングエンジンからのタイムスタンプ。これは公開取引チャネルのTと相関させることができる。
    }

    #[derive(Deserialize, Debug)]
    #[allow(non_snake_case, dead_code)]
    pub struct BybitOrderbookData {
        pub s: String,           // シンボル
        pub b: Vec<Vec<String>>, // 買い注文
        pub a: Vec<Vec<String>>, // 売り注文
        pub u: i64,              // 更新ID。シーケンスである。時々、"u"=1 を受信しが、
        // これはサービスの再起動によるスナップショットデータである。そのため、ローカルのオーダーブックを上書きしてください。
        pub seq: i64, // クロスシーケンス。
                      // このフィールドを使用して、異なるレベルのオーダーブックデータを比較できる。seqが小さいほど、データが早く生成されたことを意味する。
    }

    pub struct BybitDepth {
        asks: BTreeMap<Price, Quantity>,
        bids: BTreeMap<Price, Quantity>,
    }

    impl Depth for BybitDepth {
        fn asks(&self) -> &BTreeMap<Price, Quantity> {
            &self.asks
        }

        fn bids(&self) -> &BTreeMap<Price, Quantity> {
            &self.bids
        }
    }

    impl fmt::Display for BybitDepth {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            writeln!(f)?;
            self.format_depth(Some(20), f)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn bybit_updates_are_exact_and_invalid_batches_are_atomic() {
            let mut book = BybitDepth::new();
            let data = |a: serde_json::Value, b: serde_json::Value| {
                serde_json::from_value::<BybitOrderbookData>(
                    serde_json::json!({"s":"XRPUSDT","a":a,"b":b,"u":1,"seq":1}),
                )
                .unwrap()
            };
            book.update(data(
                serde_json::json!([["101", "90000000000.00000001"]]),
                serde_json::json!([["100", "1"]]),
            ))
            .unwrap();
            assert_eq!(
                book.asks()[&Price::from(101)].units(),
                9_000_000_000_000_000_001
            );
            let before = book.asks().clone();
            assert!(
                book.update(data(
                    serde_json::json!([["101", "0"]]),
                    serde_json::json!([["100", "0.000000001"]])
                ))
                .is_err()
            );
            assert_eq!(book.asks(), &before);
            book.update(data(
                serde_json::json!([["101", "0"]]),
                serde_json::json!([]),
            ))
            .unwrap();
            assert!(book.asks().is_empty());
        }
    }

    impl Default for BybitDepth {
        fn default() -> Self {
            Self::new()
        }
    }

    impl BybitDepth {
        pub fn new() -> Self {
            Self {
                asks: BTreeMap::new(),
                bids: BTreeMap::new(),
            }
        }

        pub fn update(
            &mut self,
            data: BybitOrderbookData,
        ) -> Result<(), crate::book_units::NumericError> {
            fn parse(
                levels: Vec<Vec<String>>,
            ) -> Result<Vec<(Price, Quantity)>, crate::book_units::NumericError> {
                levels
                    .into_iter()
                    .map(|level| {
                        if level.len() != 2 {
                            return Err(crate::book_units::NumericError::Invalid);
                        }
                        let price: Price = level[0].parse()?;
                        let quantity: Quantity = level[1].parse()?;
                        if price <= Price::ZERO || quantity < Quantity::ZERO {
                            return Err(crate::book_units::NumericError::Invalid);
                        }
                        Ok((price, quantity))
                    })
                    .collect()
            }
            let asks = parse(data.a)?;
            let bids = parse(data.b)?;
            for (map, levels) in [(&mut self.asks, asks), (&mut self.bids, bids)] {
                for (price, quantity) in levels {
                    if quantity.is_zero() {
                        map.remove(&price);
                    } else {
                        map.insert(price, quantity);
                    }
                }
            }
            Ok(())
        }
    }
}

pub mod execution_recording;

mod recording_http;

pub mod replay;

pub mod clock_observation;

pub mod simulation;

pub mod event_strategy;
