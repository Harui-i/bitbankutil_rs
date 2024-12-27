pub mod bitbank_structs;
pub mod bitbank_bot;
pub mod bitbank_private;
pub mod bitbank_public;

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

        // return minimum price p, s.t. Sigma_{price <= p} (volume) >= r
        // To think intuitively, it is the highest price when you execute a market buy order of size r.
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

        // return maximum price p, s.t. Sigma_{p <= price} (volume) >= r
        // To think intuitively, it is the lowest price when you execute a market sell order of size r.
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

        // return log(r-depth ask price) - log(best ask price))
        fn r_depth_ask_logdiff(&self, r: f64) -> Option<f64> {
            let ask_price = self.r_depth_ask_price(r)?;
            let best_ask_price = self.best_ask()?.0;

            let ask_price_f64 = ask_price.to_f64().unwrap();
            let best_ask_price_f64 = best_ask_price.to_f64().unwrap();

            Some(ask_price_f64.ln() - best_ask_price_f64.ln())
        }

        fn r_depth_bid_logdiff(&self, r: f64) -> Option<f64> {
            let bid_price = self.r_depth_bid_price(r)?;
            let best_bid_price = self.best_bid()?.0;

            let bid_price_f64 = bid_price.to_f64().unwrap();
            let best_bid_price_f64 = best_bid_price.to_f64().unwrap();

            Some(bid_price_f64.ln() - best_bid_price_f64.ln())
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

        // format the depth data of top k levels. if k is none, then format 20 levels.
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
        pub BT: bool,  // Whether it is a block trade or not
        pub S: String, // Side
        pub T: i64,    // timestamp
        pub i: String, // trade id
        #[serde(with = "rust_decimal::serde::float")]
        pub p: Decimal, //
        pub s: String, // symbol
        #[serde(with = "rust_decimal::serde::float")]
        pub v: Decimal, // Trade size
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
        topic: String,  // topic, like "publicTrade.BTCUSDT"
        r#type: String, // Data type. snapshot
        pub ts: i64,    // timestamp
        pub data: serde_json::Value,
        cts: Number, // The timestamp from the match engine when this orderbook data is produced. It can be correlated with T from public trade channel
    }

    #[derive(Deserialize, Debug)]
    #[allow(non_snake_case, dead_code)]
    pub struct BybitOrderbookData {
        s: String,           // symbol
        b: Vec<Vec<String>>, // bids
        a: Vec<Vec<String>>, // asks
        u: i64,              // Update ID. Is a sequence. Occasionally, you'll receive "u"=1,
        // which is a snapshot data due to the restart of the service. So please overwrite your local orderbook
        seq: i64, // Cross sequenc.
                  //You can use this field to compare different levels orderbook data, and for the smaller seq, then it means the data is generated earlier.
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
