use std::collections::BTreeSet;

use crate::{
    bitbank_private::BitbankPrivateApiClient,
    bitbank_structs::{
        BitbankCancelOrdersResponse, BitbankCreateOrderResponse, BitbankGetOrderResponse,
    },
    order_domain::{DesiredLimitOrder, OpenOrder, OrderId, OrderSide, OrderType},
};
use rust_decimal::Decimal;
use tokio::{task::JoinSet, time::Instant};

#[derive(Debug, PartialEq, Eq)]
pub struct OrderPlan {
    pub cancels: Vec<OrderId>,
    pub placements: Vec<DesiredLimitOrder>,
}

#[derive(Debug, PartialEq, Eq)]
struct ConcurrentOrderPlan {
    cancels: Vec<OrderId>,
    first_placements: BTreeSet<DesiredLimitOrder>,
    second_placements: BTreeSet<DesiredLimitOrder>,
}

pub fn plan_orders(
    mut wanna_place_orders: Vec<DesiredLimitOrder>,
    current_orders: Vec<OpenOrder>,
    pair: &str,
) -> OrderPlan {
    let mut cancels = vec![];

    for cur_order in current_orders {
        let matched_wanna_order_index = wanna_place_orders
            .iter()
            .position(|wanna_order| wanna_order.matches_open_order(&cur_order));

        // この注文はキャンセルされるべき
        if matched_wanna_order_index.is_none() && cur_order.pair == pair {
            log::debug!("this order will be cancelled. {:?}", cur_order);
            cancels.push(cur_order.order_id);
        }
        // この現在の注文はwanna_place_ordersにある（つまり、すでに発注済み）
        else if let Some(matched_wanna_order_index) = matched_wanna_order_index {
            // 1つだけ削除したいので、最初に一致した希望注文を削除する。
            log::debug!("this order already exists: {:?}", cur_order);
            wanna_place_orders.remove(matched_wanna_order_index);
        }
    }

    OrderPlan {
        cancels,
        placements: wanna_place_orders,
    }
}

fn plan_concurrent_orders_from_open_orders(
    wanna_place_orders: Vec<DesiredLimitOrder>,
    current_orders: Vec<OpenOrder>,
    btc_free_amount: Decimal,
    jpy_free_amount: Decimal,
    pair: &str,
) -> ConcurrentOrderPlan {
    let OrderPlan {
        cancels,
        placements,
    } = plan_orders(wanna_place_orders, current_orders, pair);

    let mut next_btc_free_amount = btc_free_amount;
    let mut next_jpy_free_amount = jpy_free_amount;

    let mut first_placements: BTreeSet<DesiredLimitOrder> = BTreeSet::new();
    let mut second_placements: BTreeSet<DesiredLimitOrder> = BTreeSet::new();

    // placementsの順序が発注したい注文の優先順位であると仮定する。
    for sord in placements {
        if sord.side == OrderSide::Buy {
            let consumed_jpy = sord.amount * sord.limit_price();

            if next_jpy_free_amount >= consumed_jpy {
                log::debug!("{:?} posted firstly.", sord);
                first_placements.insert(sord);
                next_jpy_free_amount -= consumed_jpy;
            } else {
                log::debug!("{:?} posted secondly.", sord);
                second_placements.insert(sord);
            }
        } else if sord.side == OrderSide::Sell {
            let consumed_btc = sord.amount;

            if next_btc_free_amount >= consumed_btc {
                log::debug!("{:?} posted firstly.", sord);
                first_placements.insert(sord);
                next_btc_free_amount -= consumed_btc;
            } else {
                log::debug!("{:?} posted secondly.", sord);
                second_placements.insert(sord);
            }
        } else {
            panic!(
                "unexpected side in place_wanna_orders_concurrent: {}",
                sord.side
            );
        }
    }

    ConcurrentOrderPlan {
        cancels,
        first_placements,
        second_placements,
    }
}

fn open_orders_from_bitbank_responses(
    current_orders: Vec<BitbankGetOrderResponse>,
) -> Vec<OpenOrder> {
    current_orders
        .iter()
        .map(|cur_order| {
            OpenOrder::try_from(cur_order)
                .expect("failed to convert bitbank order response into OpenOrder")
        })
        .collect()
}

// 有効な注文を置き換える
// `current_orders` : BitbankGetOrderResponseのVecで、ペア内の現在の注文を表す
// `pair` : &str は注文を置き換えたいペアを表す
pub async fn place_wanna_orders(
    wanna_place_orders: BTreeSet<DesiredLimitOrder>,
    current_orders: Vec<BitbankGetOrderResponse>,
    pair: String,
    api_client: BitbankPrivateApiClient,
) {
    let start = Instant::now();
    let mut js = JoinSet::new();
    let OrderPlan {
        cancels,
        placements,
    } = plan_orders(
        wanna_place_orders.into_iter().collect(),
        open_orders_from_bitbank_responses(current_orders),
        &pair,
    );

    if !cancels.is_empty() {
        let cancel_order_response_result = api_client
            .post_cancel_orders(
                &pair.clone(),
                cancels.into_iter().map(|order_id| order_id.0).collect(),
            )
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
    for sord in placements {
        let bbc2 = api_client.clone();
        let pair2 = pair.clone();
        js.spawn(async move {
            bbc2.post_order(
                &pair2,
                &sord.amount.to_string(),
                Some(&sord.price.to_string()),
                sord.side.as_str(),
                OrderType::Limit.as_str(),
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
    wanna_place_orders: Vec<DesiredLimitOrder>,
    current_orders: Vec<BitbankGetOrderResponse>,
    btc_free_amount: Decimal,
    jpy_free_amount: Decimal,
    pair: String,
    api_client: BitbankPrivateApiClient,
) {
    let start = Instant::now();
    let ConcurrentOrderPlan {
        cancels,
        first_placements,
        second_placements,
    } = plan_concurrent_orders_from_open_orders(
        wanna_place_orders,
        open_orders_from_bitbank_responses(current_orders),
        btc_free_amount,
        jpy_free_amount,
        &pair,
    );

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

    // first_placementsの注文を発注し、cancelsの注文をキャンセルするJoinSet
    let mut first_js = JoinSet::new();

    // いくつかの注文をキャンセルする必要がある
    if !cancels.is_empty() {
        let bbc2 = api_client.clone();
        let pair2 = pair.clone();

        first_js.spawn(async move {
            FirstJoinSetResponse::CancelResponse(
                bbc2.post_cancel_orders(
                    &pair2.clone(),
                    cancels.into_iter().map(|order_id| order_id.0).collect(),
                )
                .await,
            )
        });
    }

    for sord in first_placements {
        let bbc2 = api_client.clone();
        let pair2 = pair.clone();

        first_js.spawn(async move {
            FirstJoinSetResponse::PostResponse(
                bbc2.post_order(
                    &pair2,
                    &sord.amount.to_string(),
                    Some(&sord.price.to_string()),
                    sord.side.as_str(),
                    OrderType::Limit.as_str(),
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

    if !second_placements.is_empty() {
        let mut second_js = JoinSet::new();

        for sord in second_placements {
            let bbc2 = api_client.clone();
            let pair2 = pair.clone();
            second_js.spawn(async move {
                bbc2.post_order(
                    &pair2,
                    &sord.amount.to_string(),
                    Some(&sord.price.to_string()),
                    sord.side.as_str(),
                    OrderType::Limit.as_str(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn desired_order(
        pair: &str,
        side: OrderSide,
        amount: Decimal,
        price: Decimal,
    ) -> DesiredLimitOrder {
        DesiredLimitOrder::limit(pair.to_owned(), side, amount, price)
    }

    fn open_order(
        order_id: u64,
        pair: &str,
        side: OrderSide,
        amount: Decimal,
        price: Decimal,
        post_only: Option<bool>,
    ) -> OpenOrder {
        OpenOrder {
            order_id: OrderId(order_id),
            pair: pair.to_owned(),
            side,
            order_type: OrderType::Limit,
            remaining_amount: amount,
            price: Some(price),
            post_only,
        }
    }

    fn set_of(orders: Vec<DesiredLimitOrder>) -> BTreeSet<DesiredLimitOrder> {
        orders.into_iter().collect()
    }

    #[test]
    fn plan_orders_does_nothing_when_current_orders_match_desired_orders() {
        let desired = desired_order(
            "btc_jpy",
            OrderSide::Buy,
            Decimal::new(1, 1),
            Decimal::new(5_000_000, 0),
        );
        let current_orders = vec![open_order(
            10,
            "btc_jpy",
            OrderSide::Buy,
            Decimal::new(1, 1),
            Decimal::new(5_000_000, 0),
            Some(true),
        )];

        let plan = plan_orders(vec![desired], current_orders, "btc_jpy");

        assert!(plan.cancels.is_empty());
        assert!(plan.placements.is_empty());
    }

    #[test]
    fn plan_orders_cancels_existing_order_missing_from_desired_orders() {
        let current_orders = vec![open_order(
            10,
            "btc_jpy",
            OrderSide::Sell,
            Decimal::new(2, 1),
            Decimal::new(5_100_000, 0),
            Some(true),
        )];

        let plan = plan_orders(vec![], current_orders, "btc_jpy");

        assert_eq!(plan.cancels, vec![OrderId(10)]);
        assert!(plan.placements.is_empty());
    }

    #[test]
    fn plan_orders_places_desired_order_missing_from_current_orders() {
        let desired = desired_order(
            "btc_jpy",
            OrderSide::Buy,
            Decimal::new(1, 1),
            Decimal::new(5_000_000, 0),
        );

        let plan = plan_orders(vec![desired.clone()], vec![], "btc_jpy");

        assert!(plan.cancels.is_empty());
        assert_eq!(plan.placements, vec![desired]);
    }

    #[test]
    fn plan_orders_ignores_unwanted_current_orders_for_other_pairs() {
        let current_orders = vec![open_order(
            12,
            "eth_jpy",
            OrderSide::Sell,
            Decimal::new(1, 0),
            Decimal::new(400_000, 0),
            Some(true),
        )];

        let plan = plan_orders(vec![], current_orders, "btc_jpy");

        assert!(plan.cancels.is_empty());
        assert!(plan.placements.is_empty());
    }

    #[test]
    fn plan_concurrent_orders_removes_existing_order_and_cancels_unwanted_same_pair_order() {
        let existing_wanted = desired_order(
            "btc_jpy",
            OrderSide::Buy,
            Decimal::new(1, 1),
            Decimal::new(5_000_000, 0),
        );
        let new_wanted = desired_order(
            "btc_jpy",
            OrderSide::Sell,
            Decimal::new(2, 1),
            Decimal::new(5_100_000, 0),
        );
        let current_orders = vec![
            open_order(
                10,
                "btc_jpy",
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
                Some(true),
            ),
            open_order(
                11,
                "btc_jpy",
                OrderSide::Sell,
                Decimal::new(3, 1),
                Decimal::new(5_200_000, 0),
                Some(true),
            ),
            open_order(
                12,
                "eth_jpy",
                OrderSide::Sell,
                Decimal::new(1, 0),
                Decimal::new(400_000, 0),
                Some(true),
            ),
        ];

        let plan = plan_concurrent_orders_from_open_orders(
            vec![existing_wanted, new_wanted.clone()],
            current_orders,
            Decimal::new(1, 0),
            Decimal::new(1_000_000, 0),
            "btc_jpy",
        );

        assert_eq!(plan.cancels, vec![OrderId(11)]);
        assert_eq!(plan.first_placements, set_of(vec![new_wanted]));
        assert!(plan.second_placements.is_empty());
    }

    #[test]
    fn plan_concurrent_orders_splits_new_orders_by_available_funds_in_priority_order() {
        let first_buy = desired_order(
            "btc_jpy",
            OrderSide::Buy,
            Decimal::new(1, 1),
            Decimal::new(1_000_000, 0),
        );
        let second_buy = desired_order(
            "btc_jpy",
            OrderSide::Buy,
            Decimal::new(2, 1),
            Decimal::new(1_000_000, 0),
        );
        let first_sell = desired_order(
            "btc_jpy",
            OrderSide::Sell,
            Decimal::new(3, 1),
            Decimal::new(1_200_000, 0),
        );
        let second_sell = desired_order(
            "btc_jpy",
            OrderSide::Sell,
            Decimal::new(3, 1),
            Decimal::new(1_300_000, 0),
        );

        let plan = plan_concurrent_orders_from_open_orders(
            vec![
                first_buy.clone(),
                second_buy.clone(),
                first_sell.clone(),
                second_sell.clone(),
            ],
            vec![],
            Decimal::new(5, 1),
            Decimal::new(100_000, 0),
            "btc_jpy",
        );

        assert!(plan.cancels.is_empty());
        assert_eq!(plan.first_placements, set_of(vec![first_buy, first_sell]));
        assert_eq!(
            plan.second_placements,
            set_of(vec![second_buy, second_sell])
        );
    }

    #[test]
    fn plan_concurrent_orders_treats_missing_post_only_as_matching_existing_order() {
        let desired = desired_order(
            "btc_jpy",
            OrderSide::Sell,
            Decimal::new(25, 2),
            Decimal::new(4_900_000, 0),
        );
        let current_orders = vec![open_order(
            123,
            "btc_jpy",
            OrderSide::Sell,
            Decimal::new(25, 2),
            Decimal::new(4_900_000, 0),
            None,
        )];

        let plan = plan_concurrent_orders_from_open_orders(
            vec![desired],
            current_orders,
            Decimal::ZERO,
            Decimal::ZERO,
            "btc_jpy",
        );

        assert!(plan.cancels.is_empty());
        assert!(plan.first_placements.is_empty());
        assert!(plan.second_placements.is_empty());
    }
}
