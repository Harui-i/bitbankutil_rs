pub mod bitbank_bot;
pub mod bitbank_private;
pub mod bitbank_public;
pub mod bitbank_structs;
pub mod order_manager;
pub mod response_handler;
pub mod websocket_handler;

pub mod depth {
    use core::fmt;
    use rust_decimal::prelude::*;
    use std::collections::BTreeMap;
    pub trait Depth {
        fn asks(&self) -> &BTreeMap<Decimal, f64>;
        fn bids(&self) -> &BTreeMap<Decimal, f64>;

        fn best_ask(&self) -> Option<(&Decimal, &f64)> {
            self.asks().iter().next()
        }

        fn best_bid(&self) -> Option<(&Decimal, &f64)> {
            self.bids().iter().next_back()
        }

        fn kth_best_ask(&self, k: usize) -> Option<(&Decimal, &f64)> {
            self.asks().iter().nth(k)
        }

        fn kth_best_bid(&self, k: usize) -> Option<(&Decimal, &f64)> {
            self.bids().iter().nth_back(k)
        }

        // 最小価格pを返します。ここで、Sum_{bestask <= price <= p} (amount) >= rです。
        // 直感的に考えると、サイズrの成行買い注文を実行したときの最高価格です。
        fn r_depth_ask_price(&self, r: f64) -> Option<&Decimal> {
            let mut sum = f64::zero();
            for (price, amount) in self.asks().iter() {
                sum += amount;
                if sum >= r {
                    return Some(price);
                }
            }

            None
        }

        // 最大価格pを返します。ここで、Sum_{p <= price <= bestbid} (amount) >= rです。
        // 直感的に考えると、サイズrの成行売り注文を実行したときの最低価格です。
        fn r_depth_bid_price(&self, r: f64) -> Option<&Decimal> {
            let mut sum = f64::zero();
            for (price, amount) in self.bids().iter().rev() {
                sum += amount;
                if sum >= r {
                    return Some(price);
                }
            }

            None
        }

        // 最小価格pを返します。ここで、Sum_{bestask <= price <= p} (amount * price) >= sです。
        // 直感的に考えると、サイズs（ドル建て）の成行買い注文を実行したときの最高価格です。
        fn s_depth_ask_price(&self, s: f64) -> Option<&Decimal> {
            let mut sum = f64::zero();
            for (price, amount) in self.asks().iter() {
                sum += price.to_f64().unwrap() * amount.clone();
                if sum >= s {
                    return Some(price);
                }
            }

            None
        }

        // 最大価格pを返します。ここで、Sum_{p <= price <= bestbid} (amount * price) >= sです。
        // 直感的に考えると、サイズs（ドル建て）の成行売り注文を実行したときの最低価格です。
        fn s_depth_bid_price(&self, s: f64) -> Option<&Decimal> {
            let mut sum = f64::zero();
            for (price, amount) in self.bids().iter().rev() {
                sum += price.to_f64().unwrap() * amount.clone();
                if sum >= s {
                    return Some(price);
                }
            }

            None
        }

        // log(r-depth ask price) - log(best ask price)を返します。
        fn r_depth_ask_logdiff(&self, r: f64) -> Option<f64> {
            let ask_price = self.r_depth_ask_price(r)?;
            let best_ask_price = self.best_ask()?.0;

            let ask_price_f64 = ask_price.to_f64().unwrap();
            let best_ask_price_f64 = best_ask_price.to_f64().unwrap();

            Some(ask_price_f64.ln() - best_ask_price_f64.ln())
        }

        // log(r-depth bid price) - log(best bid price)を返します。
        fn r_depth_bid_logdiff(&self, r: f64) -> Option<f64> {
            let bid_price = self.r_depth_bid_price(r)?;
            let best_bid_price = self.best_bid()?.0;

            let bid_price_f64 = bid_price.to_f64().unwrap();
            let best_bid_price_f64 = best_bid_price.to_f64().unwrap();

            Some(bid_price_f64.ln() - best_bid_price_f64.ln())
        }

        // log(s-depth ask price) - log(best ask price)を返します。
        fn s_depth_ask_logdiff(&self, s: f64) -> Option<f64> {
            let ask_price = self.s_depth_ask_price(s)?;
            let best_ask_price = self.best_ask()?.0;

            let ask_price = ask_price.to_f64().unwrap();
            let best_ask_price = best_ask_price.to_f64().unwrap();

            Some(ask_price.ln() - best_ask_price.ln())
        }

        // log(s-depth bid price) - log(best bid price)を返します。
        fn s_depth_bid_logdiff(&self, s: f64) -> Option<f64> {
            let bid_price = self.s_depth_bid_price(s)?;
            let best_bid_price = self.best_bid()?.0;

            let bid_price = bid_price.to_f64().unwrap();
            let best_bid_price = best_bid_price.to_f64().unwrap();

            Some(bid_price.ln() - best_bid_price.ln())
        }

        fn bidask_spread(&self) -> Option<Decimal> {
            if self.best_ask().is_some() && self.best_bid().is_some() {
                Some(self.best_ask().unwrap().0 - self.best_bid().unwrap().0)
            } else {
                None
            }
        }

        fn bidask_imbalance(&self) -> Option<f64> {
            if self.best_ask().is_some() && self.best_bid().is_some() {
                let ask = self.best_ask().unwrap().1;
                let bid = self.best_bid().unwrap().1;
                Some(bid - ask)
            } else {
                None
            }
        }

        // 上位kレベルのデプスデータをフォーマットします。kがnoneの場合、20レベルをフォーマットします。
        fn format_depth(&self, k: Option<usize>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let k2 = k.unwrap_or(20);

            for (price, amount) in self.asks().iter().take(k2).rev() {
                write!(f, "{}\t{:.4}\n", price, amount)?;
            }

            write!(f, "asks\n")?;
            write!(f, "mid ")?;

            if self.best_ask().is_some() && self.best_bid().is_some() {
                write!(
                    f,
                    "spread: {:.4}",
                    self.best_ask().unwrap().0 - self.best_bid().unwrap().0
                )?;
            }

            if self.kth_best_ask(2).is_some() && self.kth_best_bid(2).is_some() {
                write!(
                    f,
                    ", second-best spread: {:.4}",
                    self.kth_best_ask(2).unwrap().0 - self.kth_best_bid(2).unwrap().0
                )?;
            }

            write!(f, "\n")?;

            write!(f, "bids\n")?;
            for (price, amount) in self.bids().iter().rev().take(k2) {
                write!(f, "{}\t{:.4}\n", price, amount)?;
            }

            Ok(())
        }
    }
}

pub mod bybit {
    use rust_decimal::prelude::*;
    use serde::Deserialize;
    use serde_json::Number;

    use std::collections::BTreeMap;
    use std::fmt;

    use crate::depth::Depth;

    #[allow(dead_code, non_snake_case)]
    #[derive(Deserialize, Debug)]
    pub struct BybitTransactionDatum {
        pub BT: bool,  // ブロック取引かどうか
        pub S: String, // サイド
        pub T: i64,    // タイムスタンプ
        pub i: String, // 取引ID
        #[serde(with = "rust_decimal::serde::float")]
        pub p: Decimal, // 価格
        pub s: String, // シンボル
        #[serde(with = "rust_decimal::serde::float")]
        pub v: Decimal, // 取引サイズ
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
        cts: Number, // このオーダーブックデータが生成されたときのマッチングエンジンからのタイムスタンプ。これは公開取引チャネルのTと相関させることができます。
    }

    #[derive(Deserialize, Debug)]
    #[allow(non_snake_case, dead_code)]
    pub struct BybitOrderbookData {
        pub s: String,           // シンボル
        pub b: Vec<Vec<String>>, // 買い注文
        pub a: Vec<Vec<String>>, // 売り注文
        pub u: i64,              // 更新ID。シーケンスです。時々、"u"=1 を受信しますが、
        // これはサービスの再起動によるスナップショットデータです。そのため、ローカルのオーダーブックを上書きしてください。
        pub seq: i64, // クロスシーケンス。
                      // このフィールドを使用して、異なるレベルのオーダーブックデータを比較できます。seqが小さいほど、データが早く生成されたことを意味します。
    }

    pub struct BybitDepth {
        asks: BTreeMap<Decimal, f64>,
        bids: BTreeMap<Decimal, f64>,
    }

    impl Depth for BybitDepth {
        fn asks(&self) -> &BTreeMap<Decimal, f64> {
            &self.asks
        }

        fn bids(&self) -> &BTreeMap<Decimal, f64> {
            &self.bids
        }
    }

    impl fmt::Display for BybitDepth {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "\n")?;
            self.format_depth(Some(20), f)
        }
    }

    impl BybitDepth {
        pub fn new() -> Self {
            Self {
                asks: BTreeMap::new(),
                bids: BTreeMap::new(),
            }
        }

        pub fn update(&mut self, data: BybitOrderbookData) {
            for ask in data.a.iter() {
                let price = &ask[0].parse::<Decimal>().unwrap();
                let size = ask[1].parse::<f64>().unwrap();

                if size.is_zero() {
                    self.asks.remove(price);
                } else {
                    self.asks.insert(price.clone(), size);
                }
            }

            for bid in data.b.iter() {
                let price = &bid[0].parse::<Decimal>().unwrap();
                let size = bid[1].parse::<f64>().unwrap();

                if size.is_zero() {
                    self.bids.remove(price);
                } else {
                    self.bids.insert(price.clone(), size);
                }
            }
        }
    }
}
