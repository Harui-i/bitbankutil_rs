use std::collections::BTreeSet;

use crate::{
    bitbank_structs::{
        BitbankCancelOrdersResponse, BitbankCreateOrderResponse, BitbankGetOrderResponse,
    },
    trading_api::BitbankTradingApi,
};
use rust_decimal::Decimal;
use tokio::{task::JoinSet, time::Instant};

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone)]
pub struct SimplifiedOrder {
    pub pair: String,
    pub side: String,
    pub amount: Decimal,
    pub price: Decimal,
}

// Replace active orders
// `current_orders` : Vec of BitbankGetOrderResponse, represents current orders in the pair
// `pair` : &str represents the pair you want to replace orders.
pub fn place_wanna_orders<C>(
    mut wanna_place_orders: BTreeSet<SimplifiedOrder>,
    current_orders: Vec<BitbankGetOrderResponse>,
    pair: String,
    api_client: C,
) -> impl std::future::Future<Output = ()> + Send
where
    C: BitbankTradingApi,
{
    async move {
        let start = Instant::now();
        let mut should_cancelled_orderids = vec![];
        let mut js = JoinSet::new();

        for cur_order in current_orders {
            let current_sord = SimplifiedOrder {
                pair: cur_order.pair.clone(),
                side: cur_order.side.to_string(),
                amount: cur_order
                    .remaining_amount
                    .clone()
                    .unwrap()
                    .parse::<Decimal>()
                    .unwrap(),
                price: cur_order.price.clone().unwrap().parse::<Decimal>().unwrap(),
            };

            // this order shoulb be canceled
            if !wanna_place_orders.contains(&current_sord) && current_sord.pair == pair {
                log::debug!("this order is cancelled. {:?}", current_sord);
                should_cancelled_orderids.push(cur_order.order_id.as_u64().unwrap());
            }
            // this current order is in wanna_place_orders. (i.e. already placed order)
            else {
                wanna_place_orders.remove(&current_sord);
            }
        }

        if !should_cancelled_orderids.is_empty() {
            let cancel_order_response_result = api_client
                .post_cancel_orders(&pair.clone(), should_cancelled_orderids)
                .await;

            if let Err(err) = cancel_order_response_result {
                log::error!("post_cancel_orders failed in place_wanna_orders: {:?}", err);
                return;
            }
            let cancel_order_response = cancel_order_response_result.unwrap();

            log::debug!(
                "cancel current orders. response: {:?}",
                cancel_order_response
            );
        }

        // side, lot, price
        // place orders
        for sord in wanna_place_orders {
            let bbc2 = api_client.clone();
            let pair2 = pair.clone();
            js.spawn(async move {
                bbc2.post_order(
                    &pair2,
                    &sord.amount.to_string(),
                    Some(&sord.price.to_string()),
                    &sord.side,
                    "limit",
                    Some(true),
                    None,
                )
                .await
            });
        }

        while let Some(js_res) = js.join_next().await {
            match js_res {
                Ok(result) => match result {
                    Ok(response) => {
                        log::debug!("order result: {:?}", response);
                    }
                    Err(err) => {
                        log::error!("post_order failed in place_wanna_orders: {:?}", err);
                    }
                },
                Err(join_error) => {
                    log::error!("post_order task panicked: {:?}", join_error);
                }
            }
        }

        log::debug!("Replaced orders within {} ms.", start.elapsed().as_millis());
    }
}

/*   Receive the currently placed orders (`current_orders`) and the desired order state (`wanna_place_orders`), and perform new orders or order cancellations.
If possible (if there is enough funds to place new orders and then cancel orders), order cancellations and new orders are processed in parallel.
wanna_place_orders

`btc_free_amount`: The amount of the cryptocurrency of the trading pair that is not used for orders.
`btc_locked_amount`: The amount of the cryptocurrency of the trading pair that is used for orders.

`jpy_free_amount`: The amount of Japanese yen that is not used for orders.
`jpy_btc_locked_amount`: The amount of Japanese yen that is used for orders in the trading pair.
*/
pub fn place_wanna_orders_concurrent<C>(
    mut wanna_place_orders: Vec<SimplifiedOrder>,
    current_orders: Vec<BitbankGetOrderResponse>,
    btc_free_amount: Decimal,
    jpy_free_amount: Decimal,
    pair: String,
    api_client: C,
) -> impl std::future::Future<Output = ()> + Send
where
    C: BitbankTradingApi,
{
    async move {
        let start = Instant::now();
        let mut should_cancelled_orderids = vec![];

        for cur_order in current_orders {
            let current_sord = SimplifiedOrder {
                pair: cur_order.pair.clone(),
                side: cur_order.side.to_string(),
                amount: cur_order
                    .remaining_amount
                    .clone()
                    .unwrap()
                    .parse::<Decimal>()
                    .unwrap(),
                price: cur_order.price.clone().unwrap().parse::<Decimal>().unwrap(),
            };

            // this order shoulb be canceled
            if !wanna_place_orders.contains(&current_sord) && current_sord.pair == pair {
                log::debug!("this order will be cancelled. {:?}", current_sord);
                should_cancelled_orderids.push(cur_order.order_id.as_u64().unwrap());
            }
            // this current order is in wanna_place_orders. (i.e. already placed order)
            // wanna_place_orders.contains(¤t_sord) || current_sord.pair != pair
            else {
                // Remove current_sord from wanna_place_orders (It takes O(wanna_place_orders.len()), but wanna_place_orders.len() should be small enough, so it's OK)
                // Since we want to delete only one, we will judge it straightforwardly.
                log::debug!("this order already exists: {:?}", current_sord);
                for (i, wanna_sord) in wanna_place_orders.iter().enumerate() {
                    if current_sord == *wanna_sord {
                        wanna_place_orders.remove(i);
                        break;
                    }
                }
            }
        }

        let mut next_btc_free_amount = btc_free_amount;
        let mut next_jpy_free_amount = jpy_free_amount;

        let mut first_posted_orders: BTreeSet<SimplifiedOrder> = BTreeSet::new();
        let mut second_posted_orders: BTreeSet<SimplifiedOrder> = BTreeSet::new();

        // Assume that the order of wanna_place_orders is the priority of the orders you want to place.
        for sord in wanna_place_orders {
            if sord.side == "buy" {
                let consumed_jpy = sord.amount * sord.price;

                if next_jpy_free_amount >= consumed_jpy {
                    log::debug!("{:?} posted firstly.", sord);
                    first_posted_orders.insert(sord);
                    next_jpy_free_amount -= consumed_jpy;
                } else {
                    log::debug!("{:?} posted secondly.", sord);
                    second_posted_orders.insert(sord);
                }
            } else if sord.side == "sell" {
                let consumed_btc = sord.amount;

                if next_btc_free_amount >= consumed_btc {
                    log::debug!("{:?} posted firstly.", sord);
                    first_posted_orders.insert(sord);
                    next_btc_free_amount -= consumed_btc;
                } else {
                    log::debug!("{:?} posted secondly.", sord);
                    second_posted_orders.insert(sord);
                }
            } else {
                panic!(
                    "unexpected side in place_wanna_orders_concurrent: {}",
                    sord.side
                );
            }
        }

        enum FirstJoinSetResponse<E> {
            CancelResponse(Result<BitbankCancelOrdersResponse, E>),
            PostResponse(Result<BitbankCreateOrderResponse, E>),
        }

        // JoinSet that places orders in first_posted_orders and cancels orders in should_cancelled_orderids.
        let mut first_js = JoinSet::new();

        // have to cancell some orders
        if !should_cancelled_orderids.is_empty() {
            let bbc2 = api_client.clone();
            let pair2 = pair.clone();

            first_js.spawn(async move {
                FirstJoinSetResponse::CancelResponse(
                    bbc2.post_cancel_orders(&pair2.clone(), should_cancelled_orderids)
                        .await,
                )
            });
        }

        for sord in first_posted_orders {
            let bbc2 = api_client.clone();
            let pair2 = pair.clone();

            first_js.spawn(async move {
                FirstJoinSetResponse::PostResponse(
                    bbc2.post_order(
                        &pair2,
                        &sord.amount.to_string(),
                        Some(&sord.price.to_string()),
                        &sord.side,
                        "limit",
                        Some(true),
                        None,
                    )
                    .await,
                )
            });
        }

        while let Some(first_js_res) = first_js.join_next().await {
            match first_js_res {
                Ok(FirstJoinSetResponse::CancelResponse(response)) => match response {
                    Ok(ok) => {
                        log::debug!("cancel order response in first_joinset: {:?}", ok);
                    }
                    Err(err) => {
                        log::error!(
                            "post_cancel_orders failed in first_joinset processing: {:?}",
                            err
                        );
                    }
                },
                Ok(FirstJoinSetResponse::PostResponse(response)) => match response {
                    Ok(ok) => {
                        log::debug!("create order response in first_joinset: {:?}", ok);
                    }
                    Err(err) => {
                        log::error!("post_order failed in first_joinset processing: {:?}", err);
                    }
                },
                Err(join_error) => {
                    log::error!("first_js task panicked: {:?}", join_error);
                }
            }
        }

        if !second_posted_orders.is_empty() {
            let mut second_js = JoinSet::new();

            for sord in second_posted_orders {
                let bbc2 = api_client.clone();
                let pair2 = pair.clone();
                second_js.spawn(async move {
                    bbc2.post_order(
                        &pair2,
                        &sord.amount.to_string(),
                        Some(&sord.price.to_string()),
                        &sord.side,
                        "limit",
                        Some(true),
                        None,
                    )
                    .await
                });
            }

            while let Some(second_js_res) = second_js.join_next().await {
                match second_js_res {
                    Ok(Ok(response)) => {
                        log::debug!("post order response in second_js: {:?}", response);
                    }
                    Ok(Err(err)) => {
                        log::error!("post_order failed in second_js processing: {:?}", err);
                    }
                    Err(join_error) => {
                        log::error!("second_js task panicked: {:?}", join_error);
                    }
                }
            }
        }

        log::debug!(
            "Replaced orders(concurrently) within {} ms.",
            start.elapsed().as_millis()
        );
    }
}
