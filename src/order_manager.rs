use std::collections::BTreeSet;

use crate::{
    bitbank_private::BitbankPrivateApiClient,
    bitbank_structs::{
        BitbankCancelOrdersResponse, BitbankCreateOrderResponse, BitbankGetOrderResponse,
    },
    order_domain::{DesiredOrder, OpenOrder, OrderSide},
};
use rust_decimal::Decimal;
use tokio::{task::JoinSet, time::Instant};

pub type SimplifiedOrder = DesiredOrder;

// 有効な注文を置き換える
// `current_orders` : BitbankGetOrderResponseのVecで、ペア内の現在の注文を表す
// `pair` : &str は注文を置き換えたいペアを表す
pub async fn place_wanna_orders(
    mut wanna_place_orders: BTreeSet<SimplifiedOrder>,
    current_orders: Vec<BitbankGetOrderResponse>,
    pair: String,
    api_client: BitbankPrivateApiClient,
) {
    let start = Instant::now();
    let mut should_cancelled_orderids = vec![];
    let mut js = JoinSet::new();

    for cur_order in current_orders {
        let current_order = OpenOrder::try_from(&cur_order)
            .expect("failed to convert bitbank order response into OpenOrder");
        let current_sord = current_order.to_desired_order();
        let matched_wanna_order = wanna_place_orders
            .iter()
            .find(|wanna_order| wanna_order.matches_open_order(&current_order))
            .cloned();

        // この注文はキャンセルされるべき
        if matched_wanna_order.is_none() && current_sord.pair == pair {
            log::debug!("this order is cancelled. {:?}", current_sord);
            should_cancelled_orderids.push(current_order.order_id.0);
        }
        // この現在の注文はwanna_place_ordersにある（つまり、すでに発注済み）
        else if let Some(matched_wanna_order) = matched_wanna_order {
            wanna_place_orders.remove(&matched_wanna_order);
        }
    }

    if !should_cancelled_orderids.is_empty() {
        let cancel_order_response_result = api_client
            .post_cancel_orders(&pair.clone(), should_cancelled_orderids)
            .await;

        if let Err(err) = cancel_order_response_result {
            log::error!(
                "in place_wanna_orders, post_cancel_orders has returned error: {:?}",
                err
            );
            return;
        }
        let cancel_order_response = cancel_order_response_result.unwrap();

        log::debug!(
            "cancel current orders. response: {:?}",
            cancel_order_response
        );
    }

    // side、lot、price
    // 注文を発注する
    for sord in wanna_place_orders {
        let bbc2 = api_client.clone();
        let pair2 = pair.clone();
        js.spawn(async move {
            bbc2.post_order(
                &pair2,
                &sord.amount.to_string(),
                sord.price
                    .as_ref()
                    .map(|price| price.to_string())
                    .as_deref(),
                sord.side.as_str(),
                sord.order_type.as_str(),
                sord.post_only,
                None,
            )
            .await
        });
    }

    while let Some(js_res) = js.join_next().await {
        let bcor: Result<
            BitbankCreateOrderResponse,
            Option<crypto_botters::bitbank::BitbankHandleError>,
        > = js_res.unwrap();

        log::debug!("order result: {:?}", bcor);
    }

    log::debug!("Replaced orders within {} ms.", start.elapsed().as_millis());
}

/* 現在発注済みの注文（`current_orders`）と希望する注文状態（`wanna_place_orders`）を受け取り、新規注文または注文のキャンセルを実行する。
可能な場合（新規注文を発注してから注文をキャンセルするのに十分な資金がある場合）、注文のキャンセルと新規注文は並行して処理される。
wanna_place_orders

`btc_free_amount`：注文に使用されていない取引ペアの暗号通貨の量。
`btc_locked_amount`：注文に使用されている取引ペアの暗号通貨の量。

`jpy_free_amount`：注文に使用されていない日本円の量。
`jpy_btc_locked_amount`：取引ペアの注文に使用されている日本円の量。
*/
pub async fn place_wanna_orders_concurrent(
    mut wanna_place_orders: Vec<SimplifiedOrder>,
    current_orders: Vec<BitbankGetOrderResponse>,
    btc_free_amount: Decimal,
    jpy_free_amount: Decimal,
    pair: String,
    api_client: BitbankPrivateApiClient,
) {
    let start = Instant::now();
    let mut should_cancelled_orderids = vec![];

    for cur_order in current_orders {
        let current_order = OpenOrder::try_from(&cur_order)
            .expect("failed to convert bitbank order response into OpenOrder");
        let current_sord = current_order.to_desired_order();
        let matched_wanna_order_index = wanna_place_orders
            .iter()
            .position(|wanna_order| wanna_order.matches_open_order(&current_order));

        // この注文はキャンセルされるべき
        if matched_wanna_order_index.is_none() && current_sord.pair == pair {
            log::debug!("this order will be cancelled. {:?}", current_sord);
            should_cancelled_orderids.push(current_order.order_id.0);
        }
        // この現在の注文はwanna_place_ordersにある（つまり、すでに発注済み）
        else if let Some(matched_wanna_order_index) = matched_wanna_order_index {
            // 1つだけ削除したいので、最初に一致した希望注文を削除する。
            log::debug!("this order already exists: {:?}", current_sord);
            wanna_place_orders.remove(matched_wanna_order_index);
        }
    }

    let mut next_btc_free_amount = btc_free_amount;
    let mut next_jpy_free_amount = jpy_free_amount;

    let mut first_posted_orders: BTreeSet<SimplifiedOrder> = BTreeSet::new();
    let mut second_posted_orders: BTreeSet<SimplifiedOrder> = BTreeSet::new();

    // wanna_place_ordersの順序が発注したい注文の優先順位であると仮定する。
    for sord in wanna_place_orders {
        if sord.side == OrderSide::Buy {
            let consumed_jpy = sord.amount * sord.limit_price();

            if next_jpy_free_amount >= consumed_jpy {
                log::debug!("{:?} posted firstly.", sord);
                first_posted_orders.insert(sord);
                next_jpy_free_amount -= consumed_jpy;
            } else {
                log::debug!("{:?} posted secondly.", sord);
                second_posted_orders.insert(sord);
            }
        } else if sord.side == OrderSide::Sell {
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

    enum FirstJoinSetResponse {
        CancelResponse(
            Result<
                BitbankCancelOrdersResponse,
                Option<crypto_botters::bitbank::BitbankHandleError>,
            >,
        ),
        PostResponse(
            Result<BitbankCreateOrderResponse, Option<crypto_botters::bitbank::BitbankHandleError>>,
        ),
    }

    // first_posted_ordersの注文を発注し、should_cancelled_orderidsの注文をキャンセルするJoinSet
    let mut first_js = JoinSet::new();

    // いくつかの注文をキャンセルする必要がある
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
                    sord.price
                        .as_ref()
                        .map(|price| price.to_string())
                        .as_deref(),
                    sord.side.as_str(),
                    sord.order_type.as_str(),
                    sord.post_only,
                    None,
                )
                .await,
            )
        });
    }

    while let Some(first_js_res) = first_js.join_next().await {
        let fjsr = first_js_res.unwrap();

        match fjsr {
            FirstJoinSetResponse::CancelResponse(bitbank_cancel_orders_response) => {
                log::debug!(
                    "cancel order response in first_joinset: {:?}",
                    bitbank_cancel_orders_response
                );
            }
            FirstJoinSetResponse::PostResponse(bitbank_create_order_response) => {
                log::debug!(
                    "create order response in first_joinset: {:?}",
                    bitbank_create_order_response
                );
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
                    sord.price
                        .as_ref()
                        .map(|price| price.to_string())
                        .as_deref(),
                    sord.side.as_str(),
                    sord.order_type.as_str(),
                    sord.post_only,
                    None,
                )
                .await
            });
        }

        while let Some(second_js_res) = second_js.join_next().await {
            let bcor = second_js_res.unwrap();
            log::debug!("post order response in second_js: {:?}", bcor);
        }
    }

    log::debug!(
        "Replaced orders(concurrently) within {} ms.",
        start.elapsed().as_millis()
    );
}
