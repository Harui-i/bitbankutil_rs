use bitbankutil_rs::{
    execution_recording::{ExecutionRecorder, JsonlSink},
    market_event::{MarketCircuitBreakInfo, MarketDepthSnapshot, MarketEvent, MarketTrade},
    order_domain::{BalanceSnapshot, DesiredLimitOrder, OrderId, OrderSide},
    paper_execution::*,
    trading_venue::{TradingVenue, TradingVenueError},
};
use rust_decimal::Decimal;
use std::{future::Future, task::Poll};

fn engine(delta: i64, recording: bool) -> PaperExecutionEngine {
    let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
    config.time_model = PaperTimeModel::FixedOffset {
        exchange_minus_local_ms: delta,
    };
    let e = PaperExecutionEngine::new(
        config,
        PaperClock::Replay {
            now_ms: 1000 - delta,
        },
        [("btc", 1000), ("jpy", 1_000_000)]
            .into_iter()
            .map(|(asset, n)| BalanceSnapshot {
                asset: asset.into(),
                free_amount: n.into(),
                locked_amount: 0.into(),
                onhand_amount: n.into(),
            })
            .collect(),
        recording.then(|| {
            ExecutionRecorder::new("queue", serde_json::json!({}), JsonlSink(std::io::sink()))
        }),
    )
    .unwrap();
    e
}
fn book(time: i64, bid: i64, bid_q: f64, ask: i64, ask_q: f64) -> MarketEvent {
    MarketEvent::DepthUpdated {
        pair: "btc_jpy".into(),
        depth: MarketDepthSnapshot::new(
            [(Decimal::from(110), 1.0), (Decimal::from(ask), ask_q)].into(),
            [(Decimal::from(90), 1.0), (Decimal::from(bid), bid_q)].into(),
            time,
        ),
    }
}
fn order(side: OrderSide, price: i64, amount: i64) -> DesiredLimitOrder {
    let mut o = DesiredLimitOrder::limit("btc_jpy".into(), side, amount.into(), price.into());
    o.post_only = Some(true);
    o
}
fn trade(
    e: &mut PaperExecutionEngine,
    delta: i64,
    time: i64,
    side: OrderSide,
    price: i64,
    amount: i64,
    id: i64,
) -> Vec<PaperEvent> {
    e.advance_replay_time(time + 50 - delta).unwrap();
    e.apply_market_event(&MarketEvent::Transactions {
        pair: "btc_jpy".into(),
        transactions: vec![MarketTrade {
            transaction_id: id,
            executed_at: time,
            side,
            price: price.into(),
            amount: amount.into(),
        }],
    });
    e.advance_trade_watermark(time).unwrap()
}
fn fills(events: Vec<PaperEvent>) -> Vec<(OrderId, Decimal)> {
    events
        .into_iter()
        .filter_map(|e| match e {
            PaperEvent::OrderFilled {
                order_id, amount, ..
            } => Some((order_id, amount)),
            _ => None,
        })
        .collect()
}

#[test]
fn equal_price_consumes_shared_external_queue_once_then_own_fifo() {
    for delta in [-250, 0, 250] {
        for recording in [false, true] {
            let mut e = engine(delta, recording);
            e.apply_market_event(&book(1000, 99, 10.0, 101, 10.0));
            e.place_order(order(OrderSide::Buy, 99, 3)).unwrap();
            assert!(fills(trade(&mut e, delta, 1100, OrderSide::Sell, 99, 6, 1)).is_empty());
            assert_eq!(
                fills(trade(&mut e, delta, 1200, OrderSide::Sell, 99, 5, 2)),
                [(OrderId(1), 1.into())]
            );
            assert_eq!(
                fills(trade(&mut e, delta, 1300, OrderSide::Sell, 99, 2, 3)),
                [(OrderId(1), 2.into())]
            );
            assert!(e.open_orders().is_empty());
            assert_eq!(
                e.balances()
                    .iter()
                    .find(|b| b.asset == "btc")
                    .unwrap()
                    .free_amount,
                1003.into()
            );
        }
    }
    let mut e = engine(0, false);
    e.apply_market_event(&book(1000, 99, 10.0, 101, 10.0));
    e.place_order(order(OrderSide::Buy, 99, 2)).unwrap();
    e.place_order(order(OrderSide::Buy, 99, 3)).unwrap();
    assert_eq!(
        fills(trade(&mut e, 0, 1100, OrderSide::Sell, 99, 14, 1)),
        [(OrderId(1), 2.into()), (OrderId(2), 2.into())]
    );
    assert_eq!(e.open_orders()[0].remaining_amount, 1.into());
}

#[test]
fn sell_queue_price_priority_and_price_through_keep_public_volume_cap() {
    let mut e = engine(0, false);
    e.apply_market_event(&book(1000, 99, 10.0, 101, 10.0));
    e.place_order(order(OrderSide::Sell, 101, 2)).unwrap();
    e.place_order(order(OrderSide::Sell, 100, 3)).unwrap();
    // Through the better-priced order; total self volume is still only four.
    assert_eq!(
        fills(trade(&mut e, 0, 1100, OrderSide::Buy, 102, 4, 1)),
        [(OrderId(2), 3.into()), (OrderId(1), 1.into())]
    );
    assert_eq!(e.open_orders()[0].remaining_amount, 1.into());
    assert_eq!(
        fills(trade(&mut e, 0, 1200, OrderSide::Buy, 101, 1, 2)),
        [(OrderId(1), 1.into())]
    );
}

#[test]
fn depth_decrease_and_trade_do_not_double_consume_and_additions_stay_behind() {
    let mut e = engine(0, false);
    e.apply_market_event(&book(1000, 99, 10.0, 101, 10.0));
    e.place_order(order(OrderSide::Buy, 99, 3)).unwrap();
    e.advance_replay_time(1050).unwrap();
    e.apply_market_event(&book(1050, 99, 4.0, 101, 10.0));
    assert!(fills(trade(&mut e, 0, 1060, OrderSide::Sell, 99, 6, 1)).is_empty());
    // Cancellation did not improve rank. Replenishment joins after the existing own order.
    e.advance_replay_time(1120).unwrap();
    e.apply_market_event(&book(1120, 99, 10.0, 101, 10.0));
    e.place_order(order(OrderSide::Buy, 99, 2)).unwrap();
    assert_eq!(
        fills(trade(&mut e, 0, 1200, OrderSide::Sell, 99, 7, 2)),
        [(OrderId(1), 3.into())]
    );
    assert!(fills(trade(&mut e, 0, 1300, OrderSide::Sell, 99, 6, 3)).is_empty());
    assert_eq!(
        fills(trade(&mut e, 0, 1400, OrderSide::Sell, 99, 2, 4)),
        [(OrderId(2), 2.into())]
    );
}

#[test]
fn late_pre_activation_trade_does_not_advance_new_order_queue() {
    let mut e = engine(0, false);
    e.apply_market_event(&book(1000, 99, 10.0, 101, 10.0));
    e.advance_replay_time(1200).unwrap();
    e.place_order(order(OrderSide::Buy, 99, 3)).unwrap();
    assert!(fills(trade(&mut e, 0, 1150, OrderSide::Sell, 99, 10, 1)).is_empty());
    assert!(fills(trade(&mut e, 0, 1300, OrderSide::Sell, 99, 10, 2)).is_empty());
    assert_eq!(
        fills(trade(&mut e, 0, 1400, OrderSide::Sell, 99, 1, 3)),
        [(OrderId(1), 1.into())]
    );
}

#[test]
fn cancellation_reentry_loses_rank_and_duplicate_trade_is_ignored() {
    let mut e = engine(0, false);
    e.apply_market_event(&book(1000, 99, 1.0, 101, 1.0));
    e.place_order(order(OrderSide::Buy, 99, 3)).unwrap();
    e.place_order(order(OrderSide::Buy, 99, 3)).unwrap();
    assert_eq!(
        fills(trade(&mut e, 0, 1100, OrderSide::Sell, 99, 2, 1)),
        [(OrderId(1), 1.into())]
    );
    e.advance_replay_time(1200).unwrap();
    e.cancel_orders("btc_jpy", vec![OrderId(1)]);
    e.place_order(order(OrderSide::Buy, 99, 2)).unwrap();
    assert_eq!(
        fills(trade(&mut e, 0, 1250, OrderSide::Sell, 99, 4, 2)),
        [(OrderId(2), 3.into()), (OrderId(3), 1.into())]
    );
    assert!(fills(trade(&mut e, 0, 1250, OrderSide::Sell, 99, 4, 2)).is_empty());
}

#[test]
fn post_only_crossing_rejects_entire_order_without_locking_funds() {
    for (side, price) in [
        (OrderSide::Buy, 101),
        (OrderSide::Buy, 102),
        (OrderSide::Sell, 99),
        (OrderSide::Sell, 98),
    ] {
        let mut e = engine(0, false);
        e.apply_market_event(&book(1000, 99, 0.1, 101, 0.1));
        let before = e.balances();
        assert_eq!(
            e.place_order(order(side, price, 10)),
            Err(PaperExecutionError::PostOnlyWouldTakeLiquidity)
        );
        assert_eq!(before, e.balances());
        assert!(e.open_orders().is_empty());
        assert!(e.settlement_status().model_issues.is_empty());
    }
}

#[test]
fn unsupported_or_missing_evidence_is_persistent_evaluation_uncertainty() {
    let mut e = engine(0, false);
    assert!(matches!(
        e.place_order(order(OrderSide::Buy, 99, 1)),
        Err(PaperExecutionError::ExecutionModelUnavailable(
            PaperModelUnavailable::MissingBook
        ))
    ));
    e.apply_market_event(&book(1000, 99, 1.0, 101, 1.0));
    e.place_order(order(OrderSide::Buy, 99, 1)).unwrap();
    e.advance_trade_watermark(2000).unwrap();
    assert!(!e.settlement_status().model_issues.is_empty());
    assert!(e.settlement_status().unsettled);
    for (side, price, reason) in [
        (
            OrderSide::Buy,
            89,
            PaperModelUnavailable::PriceOutsideVisibleBook,
        ),
        (
            OrderSide::Sell,
            111,
            PaperModelUnavailable::PriceOutsideVisibleBook,
        ),
        (OrderSide::Buy, 101, PaperModelUnavailable::UnsupportedTaker),
    ] {
        let mut e = engine(0, false);
        e.apply_market_event(&book(1000, 99, 1.0, 101, 1.0));
        let mut o = order(side, price, 1);
        o.post_only = Some(false);
        assert_eq!(
            e.place_order(o),
            Err(PaperExecutionError::ExecutionModelUnavailable(reason))
        );
    }
}

fn mode(mode: &str) -> MarketEvent {
    MarketEvent::CircuitBreakInfo {
        pair: "btc_jpy".into(),
        info: MarketCircuitBreakInfo {
            mode: mode.into(),
            estimated_itayose_price: None,
            estimated_itayose_amount: None,
            itayose_upper_price: None,
            itayose_lower_price: None,
            upper_trigger_price: None,
            lower_trigger_price: None,
            fee_type: "NORMAL".into(),
            reopen_timestamp: None,
            timestamp: 1000.into(),
        },
    }
}

#[test]
fn unknown_mode_can_require_observation_and_observed_non_normal_overrides_assumption() {
    let mut c = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
    c.assume_normal_mode = false;
    let mut e = PaperExecutionEngine::new(
        c,
        PaperClock::Replay { now_ms: 1000 },
        engine(0, false).balances(),
        None,
    )
    .unwrap();
    e.apply_market_event(&book(1000, 99, 1.0, 101, 1.0));
    assert!(matches!(
        e.place_order(order(OrderSide::Buy, 99, 1)),
        Err(PaperExecutionError::ExecutionModelUnavailable(
            PaperModelUnavailable::UnsupportedMarketMode
        ))
    ));
    e.apply_market_event(&mode("NONE"));
    e.place_order(order(OrderSide::Buy, 99, 1)).unwrap();
    e.apply_market_event(&mode("CIRCUIT_BREAK"));
    assert!(matches!(
        e.place_order(order(OrderSide::Buy, 99, 1)),
        Err(PaperExecutionError::ExecutionModelUnavailable(
            PaperModelUnavailable::UnsupportedMarketMode
        ))
    ));
}

#[test]
fn invalid_and_out_of_order_books_never_become_zero_queue_success() {
    let mut e = engine(0, false);
    e.apply_market_event(&book(1000, 99, 10.0, 101, 1.0));
    e.apply_market_event(&book(900, 98, 1.0, 99, 1.0));
    assert_eq!(e.settlement_status().book.unwrap().exchange_at_ms, 1000);
    e.apply_market_event(&book(1050, 99, f64::NAN, 101, 1.0));
    assert!(e.place_order(order(OrderSide::Buy, 99, 1)).is_err());
    assert!(e.settlement_status().book.is_none());
    assert!(!e.settlement_status().model_issues.is_empty());
    e.apply_market_event(&MarketEvent::DepthUpdated {
        pair: "btc_jpy".into(),
        depth: MarketDepthSnapshot::empty(),
    });
    assert!(e.place_order(order(OrderSide::Buy, 99, 1)).is_err());
}

#[test]
fn incomplete_snapshot_invalidates_existing_book_even_without_exchange_timestamp() {
    let mut e = engine(0, false);
    e.apply_market_event(&book(1000, 99, 10.0, 101, 1.0));
    e.apply_market_event(&MarketEvent::DepthUpdated {
        pair: "btc_jpy".into(),
        depth: MarketDepthSnapshot::empty(),
    });
    assert!(e.settlement_status().book.is_none());
    assert!(matches!(
        e.place_order(order(OrderSide::Buy, 99, 1)),
        Err(PaperExecutionError::ExecutionModelUnavailable(
            PaperModelUnavailable::MissingBook
        ))
    ));
    assert!(!e.settlement_status().model_issues.is_empty());
}

#[tokio::test]
async fn post_only_is_checked_at_effective_time_and_rejection_arrives_after_response_delay() {
    let mut e = engine(0, true);
    e.apply_market_event(&book(1000, 99, 10.0, 101, 10.0));
    let venue = PaperTradingVenue::with_latency(
        e,
        PaperLatencyConfig {
            placement: PaperOperationLatency {
                entry_ms: 100,
                response_ms: 20,
            },
            ..Default::default()
        },
    )
    .unwrap();
    venue.set_replay_driver(true);
    let before = venue.engine().lock().unwrap().balances();
    let mut request = Box::pin(venue.place_order(order(OrderSide::Buy, 100, 3).into()));
    std::future::poll_fn(|cx| {
        assert!(request.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    venue.advance_replay_time(1050).unwrap();
    venue
        .on_market_event(&book(1050, 98, 1.0, 99, 0.1))
        .unwrap();
    venue.process_internal_events(1100).unwrap();
    assert_eq!(venue.engine().lock().unwrap().balances(), before);
    venue.process_internal_events(1120).unwrap();
    assert!(matches!(
        request.await,
        Err(TradingVenueError::Paper(
            PaperExecutionError::PostOnlyWouldTakeLiquidity
        ))
    ));
    assert!(venue.known_account_snapshot().open_orders.is_empty());
}

#[test]
fn opposing_own_orders_are_not_silently_treated_as_maker_liquidity() {
    let mut e = engine(0, false);
    e.apply_market_event(&book(1000, 99, 1.0, 101, 1.0));
    e.place_order(order(OrderSide::Sell, 100, 2)).unwrap();
    let balances = e.balances();
    assert_eq!(
        e.place_order(order(OrderSide::Buy, 100, 2)),
        Err(PaperExecutionError::PostOnlyWouldTakeLiquidity)
    );
    assert_eq!(e.balances(), balances);
    e.cancel_orders("btc_jpy", vec![OrderId(1)]);
    e.place_order(order(OrderSide::Buy, 100, 2)).unwrap();
}

#[tokio::test]
async fn realtime_and_replay_share_positive_queue_and_uncertainty_metadata() {
    for delta in [-100, 100] {
        let mut c = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
        c.time_model = PaperTimeModel::FixedOffset {
            exchange_minus_local_ms: delta,
        };
        let mut e =
            PaperExecutionEngine::new(c, PaperClock::Realtime, engine(0, false).balances(), None)
                .unwrap();
        let now = bitbankutil_rs::execution_recording::timestamp_ms() as i64 + delta;
        e.apply_market_event(&book(now, 99, 10.0, 101, 10.0));
        let v = PaperTradingVenue::new(e);
        v.place_order(order(OrderSide::Buy, 99, 3).into())
            .await
            .unwrap();
        let accepted = v
            .engine()
            .lock()
            .unwrap()
            .drain_events()
            .into_iter()
            .find_map(|e| match e {
                PaperEvent::OrderAccepted {
                    effective_after_ms, ..
                } => Some(effective_after_ms),
                _ => None,
            })
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        for (id, amount) in [(1, 10), (2, 1)] {
            let time = accepted + id;
            v.on_market_event(&MarketEvent::Transactions {
                pair: "btc_jpy".into(),
                transactions: vec![MarketTrade {
                    transaction_id: id,
                    executed_at: time,
                    side: OrderSide::Sell,
                    price: 99.into(),
                    amount: amount.into(),
                }],
            })
            .unwrap();
            v.advance_trade_watermark(time).unwrap();
            assert_eq!(
                v.known_account_snapshot().open_orders[0].remaining_amount,
                Decimal::from(if id == 1 { 3 } else { 2 })
            );
        }
        // Once a missing-book decision occurred, query metadata explains the permanent uncertainty.
        let v = PaperTradingVenue::new(engine(0, false));
        assert!(
            v.place_order(order(OrderSide::Buy, 99, 1).into())
                .await
                .is_err()
        );
        let snapshot = v.account_snapshot("btc_jpy").await.unwrap();
        let query = snapshot.query.unwrap();
        assert!(query.orders.unsettled);
        assert!(!query.orders.issues.is_empty());
    }
}

#[test]
fn same_ms_trade_does_not_advance_queue_and_missing_pair_book_does_not_admit() {
    let mut e = engine(0, false);
    let MarketEvent::DepthUpdated { depth, .. } = book(1000, 99, 10.0, 101, 10.0) else {
        unreachable!()
    };
    e.apply_market_event(&MarketEvent::DepthUpdated {
        pair: "eth_jpy".into(),
        depth,
    });
    assert!(e.place_order(order(OrderSide::Buy, 99, 1)).is_err());
    e.apply_market_event(&book(1000, 99, 10.0, 101, 10.0));
    e.place_order(order(OrderSide::Buy, 99, 1)).unwrap();
    assert!(fills(trade(&mut e, 0, 1000, OrderSide::Sell, 99, 100, 1)).is_empty());
    assert!(fills(trade(&mut e, 0, 1100, OrderSide::Sell, 99, 10, 2)).is_empty());
    assert_eq!(
        fills(trade(&mut e, 0, 1200, OrderSide::Sell, 99, 1, 3)),
        [(OrderId(1), 1.into())]
    );
}

#[test]
fn external_quantity_behind_last_active_order_is_consumed_before_future_order() {
    fn path(delayed: bool, depth_decrement: bool) -> Vec<(OrderId, Decimal)> {
        let mut e = engine(0, false);
        e.apply_market_event(&book(1000, 99, 10.0, 101, 1.0));
        e.place_order(order(OrderSide::Buy, 100, 1)).unwrap();
        e.advance_replay_time(1050).unwrap();
        e.apply_market_event(&book(1050, 100, 5.0, 101, 1.0));
        let event = MarketEvent::Transactions {
            pair: "btc_jpy".into(),
            transactions: vec![MarketTrade {
                transaction_id: 1,
                executed_at: 1100,
                side: OrderSide::Sell,
                price: 100.into(),
                amount: 6.into(),
            }],
        };
        if !delayed {
            e.advance_replay_time(1110).unwrap();
            e.apply_market_event(&event);
            assert_eq!(
                fills(e.advance_trade_watermark(1100).unwrap()),
                [(OrderId(1), 1.into())]
            );
        }
        e.advance_replay_time(1150).unwrap();
        if depth_decrement {
            e.apply_market_event(&book(1150, 99, 10.0, 101, 1.0));
        }
        e.advance_replay_time(1200).unwrap();
        e.place_order(order(OrderSide::Buy, 100, 1)).unwrap();
        if delayed {
            e.advance_replay_time(1250).unwrap();
            e.apply_market_event(&event);
            assert_eq!(
                fills(e.advance_trade_watermark(1100).unwrap()),
                [(OrderId(1), 1.into())]
            );
        }
        fills(trade(&mut e, 0, 1300, OrderSide::Sell, 100, 1, 2))
    }
    let expected = vec![(OrderId(2), Decimal::ONE)];
    for depth_decrement in [false, true] {
        assert_eq!(path(false, depth_decrement), expected);
        assert_eq!(path(true, depth_decrement), expected);
    }
}

#[test]
fn idle_level_consumes_external_trades_and_preserves_fees_across_watermark_timing() {
    fn path(delayed: bool) -> (Vec<(OrderId, Decimal)>, Vec<BalanceSnapshot>, Decimal) {
        let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
        config.fee_schedule = PaperFeeSchedule::new(Decimal::new(1, 3), Decimal::ZERO);
        let mut e = PaperExecutionEngine::new(
            config,
            PaperClock::Replay { now_ms: 1000 },
            engine(0, false).balances(),
            None,
        )
        .unwrap();
        e.apply_market_event(&book(1000, 99, 10.0, 101, 1.0));
        e.place_order(order(OrderSide::Buy, 100, 1)).unwrap();
        e.advance_replay_time(1050).unwrap();
        e.apply_market_event(&book(1050, 100, 5.0, 101, 1.0));
        let events =
            [(1100, 1, 1), (1150, 5, 2)].map(|(time, amount, id)| MarketEvent::Transactions {
                pair: "btc_jpy".into(),
                transactions: vec![MarketTrade {
                    transaction_id: id,
                    executed_at: time,
                    side: OrderSide::Sell,
                    price: 100.into(),
                    amount: amount.into(),
                }],
            });
        if !delayed {
            for (event, time) in events.iter().zip([1100, 1150]) {
                e.advance_replay_time(time + 10).unwrap();
                e.apply_market_event(event);
                e.advance_trade_watermark(time).unwrap();
                assert!(e.open_orders().is_empty());
            }
        }
        e.advance_replay_time(1200).unwrap();
        e.place_order(order(OrderSide::Buy, 100, 1)).unwrap();
        if delayed {
            e.advance_replay_time(1250).unwrap();
            for event in &events {
                e.apply_market_event(event);
            }
            e.advance_trade_watermark(1150).unwrap();
        }
        let final_fills = fills(trade(&mut e, 0, 1300, OrderSide::Sell, 100, 1, 3));
        let fees = e
            .drain_events()
            .into_iter()
            .filter_map(|e| match e {
                PaperEvent::OrderFilled {
                    fee_amount_quote, ..
                } => Some(fee_amount_quote),
                _ => None,
            })
            .sum();
        (final_fills, e.balances(), fees)
    }
    let immediate = path(false);
    let delayed = path(true);
    assert_eq!(immediate, delayed);
    assert_eq!(immediate.0, [(OrderId(2), Decimal::ONE)]);
    assert_eq!(immediate.2, Decimal::new(2, 1));
}
