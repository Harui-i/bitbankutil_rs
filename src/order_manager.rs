use crate::book_units::Amount;
use std::collections::BTreeSet;

use crate::{
    execution_recording::{current_context, scope},
    order_domain::{DesiredLimitOrder, OpenOrder, OrderId, OrderSide},
    trading_venue::{PlacedOrder, PlacementRequest, TradingVenue, TradingVenueError},
};
use tokio::time::Instant;

/// 操作を同じタスクで並行pollし、再生上の要求登録順を起床順に依存させない。
struct OperationSet<T> {
    tasks: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>>,
}
impl<T> OperationSet<T> {
    fn new() -> Self {
        Self { tasks: Vec::new() }
    }
    fn spawn(&mut self, future: impl std::future::Future<Output = T> + Send + 'static) {
        self.tasks.push(Box::pin(future));
    }
    async fn join_next(&mut self) -> Option<Result<T, std::convert::Infallible>> {
        std::future::poll_fn(|cx| {
            if self.tasks.is_empty() {
                return std::task::Poll::Ready(None);
            }
            for i in 0..self.tasks.len() {
                if let std::task::Poll::Ready(result) = self.tasks[i].as_mut().poll(cx) {
                    drop(self.tasks.remove(i));
                    return std::task::Poll::Ready(Some(Ok(result)));
                }
            }
            std::task::Poll::Pending
        })
        .await
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct OrderPlan {
    pub cancels: Vec<OrderId>,
    pub placements: Vec<DesiredLimitOrder>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ConcurrentOrderPlan {
    pub cancels: Vec<OrderId>,
    pub first_placements: BTreeSet<DesiredLimitOrder>,
    pub second_placements: BTreeSet<DesiredLimitOrder>,
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

pub fn plan_concurrent_orders_from_open_orders(
    wanna_place_orders: Vec<DesiredLimitOrder>,
    current_orders: Vec<OpenOrder>,
    btc_free_amount: Amount,
    jpy_free_amount: Amount,
    pair: &str,
) -> Result<ConcurrentOrderPlan, crate::book_units::NumericError> {
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
            let consumed_jpy = sord.limit_price().notional(sord.amount)?;

            if next_jpy_free_amount >= consumed_jpy {
                log::debug!("{:?} posted firstly.", sord);
                first_placements.insert(sord);
                next_jpy_free_amount = next_jpy_free_amount.checked_sub(consumed_jpy)?;
            } else {
                log::debug!("{:?} posted secondly.", sord);
                second_placements.insert(sord);
            }
        } else if sord.side == OrderSide::Sell {
            let consumed_btc = sord.amount.to_amount();

            if next_btc_free_amount >= consumed_btc {
                log::debug!("{:?} posted firstly.", sord);
                first_placements.insert(sord);
                next_btc_free_amount = next_btc_free_amount.checked_sub(consumed_btc)?;
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

    Ok(ConcurrentOrderPlan {
        cancels,
        first_placements,
        second_placements,
    })
}

pub async fn place_wanna_orders(
    wanna_place_orders: BTreeSet<DesiredLimitOrder>,
    current_orders: Vec<OpenOrder>,
    pair: String,
    venue: impl TradingVenue,
) {
    let start = Instant::now();
    let mut js = OperationSet::new();
    let OrderPlan {
        cancels,
        placements,
    } = plan_orders(
        wanna_place_orders.into_iter().collect(),
        current_orders,
        &pair,
    );

    if !cancels.is_empty() {
        let cancel_order_response_result = venue.cancel_orders(&pair, cancels).await;

        if let Err(err) = cancel_order_response_result {
            log::error!(
                "in place_wanna_orders, cancel_orders has returned error: {:?}",
                err
            );
            return;
        }

        log::debug!("cancel current orders.");
    }

    // side、lot、price
    // 注文を発注する
    for sord in placements {
        let venue2 = venue.clone();
        js.spawn(scope(current_context(), async move {
            venue2.place_order(PlacementRequest::from(sord)).await
        }));
    }

    while let Some(js_res) = js.join_next().await {
        let bcor = js_res.unwrap();
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
    current_orders: Vec<OpenOrder>,
    btc_free_amount: Amount,
    jpy_free_amount: Amount,
    pair: String,
    venue: impl TradingVenue,
) -> Result<(), crate::book_units::NumericError> {
    let start = Instant::now();
    let ConcurrentOrderPlan {
        cancels,
        first_placements,
        second_placements,
    } = plan_concurrent_orders_from_open_orders(
        wanna_place_orders,
        current_orders,
        btc_free_amount,
        jpy_free_amount,
        &pair,
    )?;

    enum FirstJoinSetResponse {
        CancelResponse(Result<(), TradingVenueError>),
        PostResponse(Result<PlacedOrder, TradingVenueError>),
    }

    // first_placementsの注文を発注し、cancelsの注文をキャンセルするJoinSet
    let mut first_js = OperationSet::new();

    // いくつかの注文をキャンセルする必要がある
    if !cancels.is_empty() {
        let venue2 = venue.clone();
        let pair2 = pair.clone();

        first_js.spawn(scope(current_context(), async move {
            FirstJoinSetResponse::CancelResponse(venue2.cancel_orders(&pair2, cancels).await)
        }));
    }

    for sord in first_placements {
        let venue2 = venue.clone();

        first_js.spawn(scope(current_context(), async move {
            FirstJoinSetResponse::PostResponse(
                venue2.place_order(PlacementRequest::from(sord)).await,
            )
        }));
    }

    while let Some(first_js_res) = first_js.join_next().await {
        let fjsr = first_js_res.unwrap();

        match fjsr {
            FirstJoinSetResponse::CancelResponse(cancel_response) => {
                log::debug!(
                    "cancel order response in first_joinset: {:?}",
                    cancel_response
                );
            }
            FirstJoinSetResponse::PostResponse(create_order_response) => {
                log::debug!(
                    "create order response in first_joinset: {:?}",
                    create_order_response
                );
            }
        }
    }

    if !second_placements.is_empty() {
        let mut second_js = OperationSet::new();

        for sord in second_placements {
            let venue2 = venue.clone();
            second_js.spawn(scope(current_context(), async move {
                venue2.place_order(PlacementRequest::from(sord)).await
            }));
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book_units::*;
    use crate::order_domain::OrderType;
    use crate::trading_venue::{AccountSnapshot, TradingVenueFuture};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum VenueCall {
        Place(DesiredLimitOrder),
        Cancel {
            pair: String,
            order_ids: Vec<OrderId>,
        },
    }

    #[derive(Clone, Default)]
    struct FakeTradingVenue {
        calls: Arc<Mutex<Vec<VenueCall>>>,
    }

    impl FakeTradingVenue {
        fn calls(&self) -> Vec<VenueCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl TradingVenue for FakeTradingVenue {
        fn account_snapshot<'a>(
            &'a self,
            _pair: &'a str,
        ) -> TradingVenueFuture<'a, AccountSnapshot> {
            Box::pin(async {
                Ok(AccountSnapshot {
                    query: None,
                    open_orders: vec![],
                    balances: vec![],
                })
            })
        }

        fn place_order(&self, request: PlacementRequest) -> TradingVenueFuture<'_, PlacedOrder> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap()
                    .push(VenueCall::Place(request.order));

                Ok(PlacedOrder { order_id: None })
            })
        }

        fn cancel_orders<'a>(
            &'a self,
            pair: &'a str,
            order_ids: Vec<OrderId>,
        ) -> TradingVenueFuture<'a, ()> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(VenueCall::Cancel {
                    pair: pair.to_owned(),
                    order_ids,
                });

                Ok(())
            })
        }
    }

    fn desired_order(
        pair: &str,
        side: OrderSide,
        amount: Quantity,
        price: Price,
    ) -> DesiredLimitOrder {
        DesiredLimitOrder::limit(pair.to_owned(), side, amount, price)
    }

    fn open_order(
        order_id: u64,
        pair: &str,
        side: OrderSide,
        amount: Quantity,
        price: Price,
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
            literal("0.1"),
            literal("5000000"),
        );
        let current_orders = vec![open_order(
            10,
            "btc_jpy",
            OrderSide::Buy,
            literal("0.1"),
            literal("5000000"),
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
            literal("0.2"),
            literal("5100000"),
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
            literal("0.1"),
            literal("5000000"),
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
            literal("1"),
            literal("400000"),
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
            literal("0.1"),
            literal("5000000"),
        );
        let new_wanted = desired_order(
            "btc_jpy",
            OrderSide::Sell,
            literal("0.2"),
            literal("5100000"),
        );
        let current_orders = vec![
            open_order(
                10,
                "btc_jpy",
                OrderSide::Buy,
                literal("0.1"),
                literal("5000000"),
                Some(true),
            ),
            open_order(
                11,
                "btc_jpy",
                OrderSide::Sell,
                literal("0.3"),
                literal("5200000"),
                Some(true),
            ),
            open_order(
                12,
                "eth_jpy",
                OrderSide::Sell,
                literal("1"),
                literal("400000"),
                Some(true),
            ),
        ];

        let plan = plan_concurrent_orders_from_open_orders(
            vec![existing_wanted, new_wanted.clone()],
            current_orders,
            literal("1"),
            literal("1000000"),
            "btc_jpy",
        )
        .unwrap();

        assert_eq!(plan.cancels, vec![OrderId(11)]);
        assert_eq!(plan.first_placements, set_of(vec![new_wanted]));
        assert!(plan.second_placements.is_empty());
    }

    #[test]
    fn plan_concurrent_orders_splits_new_orders_by_available_funds_in_priority_order() {
        let first_buy = desired_order(
            "btc_jpy",
            OrderSide::Buy,
            literal("0.1"),
            literal("1000000"),
        );
        let second_buy = desired_order(
            "btc_jpy",
            OrderSide::Buy,
            literal("0.2"),
            literal("1000000"),
        );
        let first_sell = desired_order(
            "btc_jpy",
            OrderSide::Sell,
            literal("0.3"),
            literal("1200000"),
        );
        let second_sell = desired_order(
            "btc_jpy",
            OrderSide::Sell,
            literal("0.3"),
            literal("1300000"),
        );

        let plan = plan_concurrent_orders_from_open_orders(
            vec![
                first_buy.clone(),
                second_buy.clone(),
                first_sell.clone(),
                second_sell.clone(),
            ],
            vec![],
            literal("0.5"),
            literal("100000"),
            "btc_jpy",
        )
        .unwrap();

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
            literal("0.25"),
            literal("4900000"),
        );
        let current_orders = vec![open_order(
            123,
            "btc_jpy",
            OrderSide::Sell,
            literal("0.25"),
            literal("4900000"),
            None,
        )];

        let plan = plan_concurrent_orders_from_open_orders(
            vec![desired],
            current_orders,
            literal("0"),
            literal("0"),
            "btc_jpy",
        )
        .unwrap();

        assert!(plan.cancels.is_empty());
        assert!(plan.first_placements.is_empty());
        assert!(plan.second_placements.is_empty());
    }

    #[tokio::test]
    async fn place_wanna_orders_executes_order_plan_through_venue() {
        let desired = desired_order(
            "btc_jpy",
            OrderSide::Buy,
            literal("0.1"),
            literal("5000000"),
        );
        let current_order = open_order(
            10,
            "btc_jpy",
            OrderSide::Sell,
            literal("0.2"),
            literal("5100000"),
            Some(true),
        );
        let venue = FakeTradingVenue::default();

        place_wanna_orders(
            set_of(vec![desired.clone()]),
            vec![current_order],
            "btc_jpy".to_owned(),
            venue.clone(),
        )
        .await;

        assert_eq!(
            venue.calls(),
            vec![
                VenueCall::Cancel {
                    pair: "btc_jpy".to_owned(),
                    order_ids: vec![OrderId(10)],
                },
                VenueCall::Place(desired),
            ]
        );
    }

    #[tokio::test]
    async fn place_wanna_orders_concurrent_executes_split_plan_through_venue() {
        let first_buy = desired_order(
            "btc_jpy",
            OrderSide::Buy,
            literal("0.1"),
            literal("1000000"),
        );
        let second_buy = desired_order(
            "btc_jpy",
            OrderSide::Buy,
            literal("0.2"),
            literal("1000000"),
        );
        let current_order = open_order(
            20,
            "btc_jpy",
            OrderSide::Sell,
            literal("0.3"),
            literal("1200000"),
            Some(true),
        );
        let venue = FakeTradingVenue::default();

        place_wanna_orders_concurrent(
            vec![first_buy.clone(), second_buy.clone()],
            vec![current_order],
            literal("0"),
            literal("100000"),
            "btc_jpy".to_owned(),
            venue.clone(),
        )
        .await
        .unwrap();

        let calls = venue.calls();
        assert!(calls.contains(&VenueCall::Cancel {
            pair: "btc_jpy".to_owned(),
            order_ids: vec![OrderId(20)],
        }));
        assert!(calls.contains(&VenueCall::Place(first_buy)));
        assert!(calls.contains(&VenueCall::Place(second_buy)));
        assert_eq!(calls.len(), 3);
    }
}
