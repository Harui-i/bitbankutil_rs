use bitbankutil_rs::{
    bitbank_bot::{BotContext, BotEvent, BotRunner, BotStrategy},
    market_event::{MarketDepthSnapshot, MarketEvent, MarketTrade},
    order_domain::{BalanceSnapshot, DesiredLimitOrder, OrderId, OrderSide},
    paper_execution::{
        PaperClock, PaperEvent, PaperExecutionConfig, PaperExecutionEngine, PaperFeeSchedule,
        PaperTradingVenue,
    },
    replay::{CapturePayload, CaptureRecord, run_replay},
    trading_venue::{AccountSnapshot, TradingVenue},
};
use rust_decimal::Decimal;
use serde_json::json;
use std::sync::{Arc, Mutex, atomic::AtomicBool};

fn engine(base: i64, quote: i64, fee: Decimal) -> PaperExecutionEngine {
    let mut config = PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap();
    config.fee_schedule = PaperFeeSchedule::new(fee, Decimal::ZERO);
    PaperExecutionEngine::new(
        config,
        PaperClock::Replay { now_ms: 1000 },
        [("btc", base), ("jpy", quote)]
            .into_iter()
            .map(|(asset, n)| BalanceSnapshot {
                asset: asset.into(),
                free_amount: n.into(),
                locked_amount: 0.into(),
                onhand_amount: n.into(),
            })
            .collect(),
        None,
    )
    .map(|engine| seeded_engine(engine, 1000))
    .unwrap()
}
fn order(side: OrderSide, amount: Decimal) -> DesiredLimitOrder {
    DesiredLimitOrder::limit("btc_jpy".into(), side, amount, 100.into())
}
fn trade(id: i64, time: i64, amount: Decimal, side: OrderSide) -> MarketTrade {
    MarketTrade {
        transaction_id: id,
        executed_at: time,
        amount,
        side,
        price: if side == OrderSide::Buy {
            101.into()
        } else {
            99.into()
        },
    }
}
fn receive(e: &mut PaperExecutionEngine, trades: Vec<MarketTrade>) {
    assert!(
        e.apply_market_event(&MarketEvent::Transactions {
            pair: "btc_jpy".into(),
            transactions: trades
        })
        .is_empty()
    );
}
fn balance(e: &PaperExecutionEngine, asset: &str) -> BalanceSnapshot {
    e.balances().into_iter().find(|b| b.asset == asset).unwrap()
}
fn fills(events: &[PaperEvent]) -> Vec<(OrderId, Decimal, Decimal, i64)> {
    events
        .iter()
        .filter_map(|event| match event {
            PaperEvent::OrderFilled {
                order_id,
                amount,
                fee_amount_quote,
                trade,
                ..
            } => Some((*order_id, *amount, *fee_amount_quote, trade.executed_at)),
            _ => None,
        })
        .collect()
}

#[test]
fn cancellation_retains_pre_cancel_fills_without_funding_replacement_twice() {
    for side in [OrderSide::Buy, OrderSide::Sell] {
        for fee in [Decimal::new(1, 2), Decimal::new(-1, 2)] {
            let mut e = engine(1, 101, fee);
            e.place_order(order(side, 1.into())).unwrap();
            e.advance_replay_time(1200).unwrap();
            e.cancel_orders("btc_jpy", vec![OrderId(1), OrderId(1)]);
            assert!(e.open_orders().is_empty());
            assert_eq!(e.settlement_status().pending_cancellations, 1);
            // 取消受付だけでは同じ資金を再利用できない。
            assert!(e.place_order(order(side, 1.into())).is_err());
            let opposite = if side == OrderSide::Buy {
                OrderSide::Sell
            } else {
                OrderSide::Buy
            };
            let t = trade(1, 1100, Decimal::new(4, 1), opposite);
            receive(&mut e, vec![t.clone(), t.clone()]);
            receive(
                &mut e,
                vec![
                    trade(2, 1200, 1.into(), opposite),
                    trade(3, 1201, 1.into(), opposite),
                ],
            );
            let events = e.advance_trade_watermark(1201).unwrap();
            assert_eq!(
                fills(&events),
                [(
                    OrderId(1),
                    Decimal::new(4, 1),
                    fee * Decimal::from(40),
                    1100
                )]
            );
            assert!(
                matches!(events.last(), Some(PaperEvent::OrderCancelled { order, .. }) if order.remaining_amount == Decimal::new(6, 1))
            );
            assert_eq!(balance(&e, "btc").locked_amount, Decimal::ZERO);
            assert_eq!(balance(&e, "jpy").locked_amount, Decimal::ZERO);
            let expected_base = if side == OrderSide::Buy {
                Decimal::new(14, 1)
            } else {
                Decimal::new(6, 1)
            };
            let expected_quote = Decimal::from(101)
                + if side == OrderSide::Buy {
                    Decimal::from(-40)
                } else {
                    Decimal::from(40)
                }
                - fee * Decimal::from(40);
            assert_eq!(balance(&e, "btc").free_amount, expected_base);
            assert_eq!(balance(&e, "jpy").free_amount, expected_quote);
            e.advance_replay_time(1250).unwrap();
            e.place_order(order(side, Decimal::new(5, 1))).unwrap();
            let before = e.balances();
            receive(&mut e, vec![t]); // 境界より古い既知の重複は正常に無視。
            e.cancel_orders("btc_jpy", vec![OrderId(1)]);
            assert!(e.advance_trade_watermark(1201).unwrap().is_empty());
            assert_eq!(before, e.balances());
            assert!(
                e.balances()
                    .iter()
                    .all(|b| b.free_amount >= Decimal::ZERO && b.locked_amount >= Decimal::ZERO)
            );
        }
    }
}

#[test]
fn delayed_and_out_of_order_trades_have_identical_fills_fees_and_markouts() {
    let input = [
        trade(10, 1000, 1.into(), OrderSide::Buy), // 開始同msは対象外
        trade(11, 1100, Decimal::new(4, 1), OrderSide::Buy),
        trade(12, 1150, Decimal::new(2, 1), OrderSide::Buy),
        trade(13, 1200, 1.into(), OrderSide::Buy), // 取消同msは対象外
        trade(14, 1250, 1.into(), OrderSide::Buy),
    ]; // B開始同msも対象外
    let mut results = Vec::new();
    for schedule in [
        vec![vec![0, 1], vec![2, 3, 4]],
        vec![vec![], vec![4, 2, 0, 3, 1, 1]],
    ] {
        let mut e = engine(2, 0, Decimal::new(1, 2));
        e.place_order(order(OrderSide::Sell, 1.into())).unwrap();
        e.advance_replay_time(1190).unwrap();
        receive(
            &mut e,
            schedule[0].iter().map(|i| input[*i].clone()).collect(),
        );
        e.advance_replay_time(1200).unwrap();
        e.cancel_orders("btc_jpy", vec![OrderId(1)]);
        e.advance_replay_time(1250).unwrap();
        e.place_order(order(OrderSide::Sell, 1.into())).unwrap();
        e.advance_replay_time(1400).unwrap();
        receive(
            &mut e,
            schedule[1].iter().map(|i| input[*i].clone()).collect(),
        );
        let events = e.advance_trade_watermark(1400).unwrap();
        let filled = fills(&events);
        assert_eq!(filled.len(), 2);
        assert!(filled.iter().all(|f| f.0 == OrderId(1)));
        assert_eq!(e.open_orders()[0].order_id, OrderId(2));
        assert_eq!(e.open_orders()[0].remaining_amount, Decimal::ONE);
        assert!(
            matches!(events.last(), Some(PaperEvent::OrderCancelled { order, .. }) if order.remaining_amount == Decimal::new(4, 1))
        );
        let qty: Decimal = filled.iter().map(|f| f.1).sum();
        let notional = qty * Decimal::from(100);
        let fees: Decimal = filled.iter().map(|f| f.2).sum();
        // 同一期間・同一開始残高で、約定代金・数量・手数料・評価損益と約定後の値動きを比較。
        let marks: Vec<_> = [98, 103, 105]
            .into_iter()
            .map(|price| qty * (Decimal::from(100) - Decimal::from(price)) - fees)
            .collect();
        let pnl = balance(&e, "jpy").onhand_amount
            + balance(&e, "btc").onhand_amount * Decimal::from(105)
            - Decimal::from(210);
        results.push((
            filled,
            notional,
            qty,
            fees,
            marks,
            pnl,
            e.balances(),
            e.open_orders(),
        ));
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn full_fill_reports_zero_cancel_remainder_and_reuses_only_unspent_funds() {
    let mut e = engine(1, 0, Decimal::ZERO);
    e.place_order(order(OrderSide::Sell, 1.into())).unwrap();
    e.advance_replay_time(1200).unwrap();
    e.cancel_orders("btc_jpy", vec![OrderId(1)]);
    receive(&mut e, vec![trade(1, 1100, 1.into(), OrderSide::Buy)]);
    let events = e.advance_trade_watermark(1200).unwrap();
    assert!(
        matches!(events.last(), Some(PaperEvent::OrderCancelled { order, .. }) if order.remaining_amount == Decimal::ZERO)
    );
    assert!(e.place_order(order(OrderSide::Sell, 1.into())).is_err());
    assert_eq!(balance(&e, "btc").onhand_amount, Decimal::ZERO);
    assert_eq!(balance(&e, "jpy").free_amount, Decimal::from(100));
}

#[test]
fn partial_fill_known_before_cancel_and_late_fill_share_original_interval() {
    let mut e = engine(1, 0, Decimal::ZERO);
    e.place_order(order(OrderSide::Sell, 1.into())).unwrap();
    receive(
        &mut e,
        vec![trade(1, 1050, Decimal::new(2, 1), OrderSide::Buy)],
    );
    e.advance_trade_watermark(1050).unwrap();
    e.advance_replay_time(1200).unwrap();
    e.cancel_orders("btc_jpy", vec![OrderId(1)]);
    receive(
        &mut e,
        vec![trade(2, 1100, Decimal::new(3, 1), OrderSide::Buy)],
    );
    let events = e.advance_trade_watermark(1200).unwrap();
    assert_eq!(fills(&events)[0].1, Decimal::new(3, 1));
    assert!(
        matches!(events.last(), Some(PaperEvent::OrderCancelled { order, .. }) if order.remaining_amount == Decimal::new(5, 1))
    );
    assert_eq!(balance(&e, "btc").free_amount, Decimal::new(5, 1));
    assert_eq!(balance(&e, "jpy").free_amount, Decimal::from(50));
}

#[test]
fn unknown_tail_book_time_gap_and_watermark_violation_are_not_final_profit() {
    let mut e = engine(1, 0, Decimal::ZERO);
    e.place_order(order(OrderSide::Sell, 1.into())).unwrap();
    e.advance_replay_time(1200).unwrap();
    e.cancel_orders("btc_jpy", vec![OrderId(1)]);
    e.apply_market_event(&MarketEvent::DepthUpdated {
        pair: "btc_jpy".into(),
        depth: MarketDepthSnapshot::new(
            std::collections::BTreeMap::from([(1_000_000.into(), 1.0)]),
            std::collections::BTreeMap::from([(1.into(), 1.0)]),
            9000,
        ),
    });
    assert_eq!(e.settlement_status().pending_cancellations, 1);
    assert!(e.settlement_status().unsettled);
    assert_eq!(balance(&e, "btc").locked_amount, Decimal::ONE);
    e.mark_incomplete("transport_closed; missing transactions");
    assert!(e.advance_trade_watermark(9000).is_err());
    assert!(e.place_order(order(OrderSide::Sell, 1.into())).is_err());
    assert_eq!(balance(&e, "btc").locked_amount, Decimal::ONE);

    let mut e = engine(1, 0, Decimal::ZERO);
    e.advance_trade_watermark(1100).unwrap();
    assert!(e.advance_trade_watermark(1099).is_err());
    receive(&mut e, vec![trade(2, 1050, 1.into(), OrderSide::Buy)]);
    assert!(e.settlement_status().unsettled);
    assert_eq!(e.settlement_status().issues.len(), 1);
    assert!(e.advance_trade_watermark(1200).is_err());

    let mut e = engine(1, 0, Decimal::ZERO);
    receive(&mut e, vec![trade(1, 1100, 1.into(), OrderSide::Buy)]);
    receive(&mut e, vec![trade(1, 1101, 1.into(), OrderSide::Buy)]);
    assert!(e.advance_trade_watermark(1200).is_err());
}

#[test]
fn equal_millisecond_trades_use_transaction_id_order() {
    let mut e = engine(1, 0, Decimal::ZERO);
    e.place_order(order(OrderSide::Sell, 1.into())).unwrap();
    receive(
        &mut e,
        vec![
            trade(20, 1100, 1.into(), OrderSide::Buy),
            trade(10, 1100, 1.into(), OrderSide::Buy),
        ],
    );
    let events = e.advance_trade_watermark(1100).unwrap();
    assert!(
        matches!(&events[0], PaperEvent::OrderFilled { trade, .. } if trade.transaction_id == 10)
    );
}

#[derive(Clone, Debug)]
enum Action {
    Place,
    Cancel,
    Settle,
}
struct Strategy {
    observations: Arc<Mutex<Vec<(i64, AccountSnapshot)>>>,
}
impl BotStrategy<PaperTradingVenue> for Strategy {
    type CustomEvent = Action;
    type Error = String;
    async fn handle_event(
        &mut self,
        event: BotEvent<Action>,
        ctx: &BotContext<Action, PaperTradingVenue>,
    ) -> Result<(), String> {
        match event {
            BotEvent::Custom(Action::Place) => {
                ctx.venue()
                    .place_order(order(OrderSide::Sell, 1.into()).into())
                    .await
                    .map_err(|e| format!("{e:?}"))?;
            }
            BotEvent::Custom(Action::Cancel) => {
                ctx.venue()
                    .cancel_orders("btc_jpy", vec![OrderId(1)])
                    .await
                    .map_err(|e| format!("{e:?}"))?;
            }
            BotEvent::Custom(Action::Settle) => {
                ctx.venue()
                    .advance_trade_watermark(1400)
                    .map_err(|e| format!("{e:?}"))?;
            }
            BotEvent::Market(MarketEvent::Transactions { transactions, .. }) => {
                assert_eq!(ctx.now_ms(), 1400);
                assert_eq!(transactions[0].executed_at, 1100);
                // 未確定結果や未来の入力を戦略へ先読みさせない。
                assert_eq!(
                    ctx.venue()
                        .account_snapshot("btc_jpy")
                        .await
                        .unwrap()
                        .balances[1]
                        .free_amount,
                    Decimal::ZERO
                );
            }
            _ => {}
        }
        let account = ctx
            .venue()
            .account_snapshot("btc_jpy")
            .await
            .map_err(|e| format!("{e:?}"))?;
        self.observations
            .lock()
            .unwrap()
            .push((ctx.now_ms(), account));
        Ok(())
    }
}
async fn strategy_path(replay: bool) -> (Vec<(i64, AccountSnapshot)>, Vec<PaperEvent>) {
    let venue = PaperTradingVenue::new(engine(2, 0, Decimal::new(1, 2)));
    // 最初の接続時刻より前から時計を開始する。
    let observations = Arc::new(Mutex::new(Vec::new()));
    let mut runner = BotRunner::new(
        Strategy {
            observations: observations.clone(),
        },
        venue.clone(),
    );
    if !replay {
        for (time, event) in [
            (1000, BotEvent::Custom(Action::Place)),
            (1200, BotEvent::Custom(Action::Cancel)),
            (1250, BotEvent::Custom(Action::Place)),
            (
                1400,
                BotEvent::Market(MarketEvent::Transactions {
                    pair: "btc_jpy".into(),
                    transactions: vec![trade(1, 1100, 1.into(), OrderSide::Buy)],
                }),
            ),
            (1500, BotEvent::Custom(Action::Settle)),
        ] {
            runner.process(time, None, event).await.unwrap();
        }
    } else {
        let mut input = Vec::new();
        let mut seq = [0, 0];
        let mut push = |source: usize, time: u64, payload: serde_json::Value| {
            seq[source] += 1;
            let mut raw = json!({"format_version":1,"exchange":if source==0 {"bitbank"} else {"bybit"},"stream":if source==0 {"public"} else {"linear"},"session_id":format!("s{source}"),"connection_id":"one","sequence":seq[source],"received_at_micros":time*1000});
            raw.as_object_mut()
                .unwrap()
                .extend(payload.as_object().unwrap().clone());
            input.push(Ok((
                source,
                serde_json::from_value::<CaptureRecord>(raw).unwrap(),
            )));
        };
        for source in 0..2 {
            push(
                source,
                1000,
                json!({"kind":"connected","endpoint":"synthetic","subscriptions":[]}),
            );
        }
        for (time, name) in [(1000, "place"), (1200, "cancel"), (1250, "place")] {
            push(1, time, json!({"kind":"message","message":name}));
        }
        push(
            0,
            1400,
            json!({"kind":"message","message":["message",{"room_name":"transactions_btc_jpy","message":{"data":{"transactions":[{"transaction_id":1,"side":"buy","price":"101","amount":"1","executed_at":1100}]}}}]}),
        );
        push(1, 1500, json!({"kind":"message","message":"settle"}));
        let report = run_replay(
            &mut runner,
            input,
            false,
            &AtomicBool::new(false),
            |record| {
                Ok(match &record.payload {
                    CapturePayload::Message { message } => Some(match message.as_str().unwrap() {
                        "place" => Action::Place,
                        "cancel" => Action::Cancel,
                        _ => Action::Settle,
                    }),
                    _ => None,
                })
            },
        )
        .await;
        assert!(report.error.is_none(), "{:?}", report.error);
        assert!(report.incomplete); // 1400より先の末尾はまだ未確定。
    }
    let events = venue.engine().lock().unwrap().drain_events();
    let observations = observations.lock().unwrap().clone();
    assert_eq!(
        observations.iter().map(|v| v.0).collect::<Vec<_>>(),
        [1000, 1200, 1250, 1400, 1500]
    );
    assert_eq!(
        fills(&events),
        [(OrderId(1), Decimal::ONE, Decimal::ONE, 1100)]
    );
    assert_eq!(
        observations.last().unwrap().1.open_orders[0].order_id,
        OrderId(2)
    );
    (observations, events)
}
#[tokio::test]
async fn paper_and_backtest_strategy_paths_preserve_receive_order_and_do_not_peek() {
    let expected = strategy_path(false).await;
    assert_eq!(expected, strategy_path(true).await);
    assert_eq!(expected, strategy_path(true).await);
}

#[test]
fn explicit_delay_assumption_settles_only_safe_prefix_and_flags_excess_delay() {
    let mut e = engine(1, 0, Decimal::ZERO)
        .with_max_trade_delay_ms(300)
        .unwrap();
    e.place_order(order(OrderSide::Sell, 1.into())).unwrap();
    e.advance_replay_time(1200).unwrap();
    e.cancel_orders("btc_jpy", vec![OrderId(1)]);
    e.advance_replay_time(1400).unwrap();
    // 遅延上限と同じ300msの取引も、先に取り込んでから境界1099まで確定。
    let events = e.apply_market_event(&MarketEvent::Transactions {
        pair: "btc_jpy".into(),
        transactions: vec![trade(1, 1100, 1.into(), OrderSide::Buy)],
    });
    assert!(events.is_empty());
    assert_eq!(e.settlement_status().watermark_ms, Some(1099));
    e.advance_replay_time(1501).unwrap();
    let events = e.apply_market_event(&MarketEvent::Transactions {
        pair: "btc_jpy".into(),
        transactions: vec![],
    });
    assert_eq!(
        fills(&events),
        [(OrderId(1), Decimal::ONE, Decimal::ZERO, 1100)]
    );
    assert_eq!(e.settlement_status().pending_cancellations, 0);
    assert!(e.settlement_status().unsettled); // 有限待機では末尾を証明できない。
    e.apply_market_event(&MarketEvent::Transactions {
        pair: "btc_jpy".into(),
        transactions: vec![trade(2, 1200, 1.into(), OrderSide::Buy)],
    });
    assert!(!e.settlement_status().issues.is_empty());
    assert!(e.advance_trade_watermark(1501).is_err());
}

struct ObserveBalances(Arc<Mutex<Vec<Decimal>>>);
impl BotStrategy<PaperTradingVenue> for ObserveBalances {
    type CustomEvent = ();
    type Error = String;
    async fn handle_event(
        &mut self,
        _: BotEvent<()>,
        ctx: &BotContext<(), PaperTradingVenue>,
    ) -> Result<(), String> {
        let account = ctx
            .venue()
            .account_snapshot("btc_jpy")
            .await
            .map_err(|e| format!("{e:?}"))?;
        self.0.lock().unwrap().push(
            account
                .balances
                .iter()
                .find(|b| b.asset == "jpy")
                .unwrap()
                .free_amount,
        );
        Ok(())
    }
}
#[tokio::test]
async fn configured_settlement_updates_account_before_strategy_and_records_unconfirmed_shutdown() {
    use bitbankutil_rs::{
        bitbank_bot::BitbankBotBuilder,
        execution_recording::{ExecutionRecorder, JsonlSink, read_records},
    };
    let path = std::env::temp_dir().join(format!(
        "paper-shutdown-{}-{}.jsonl",
        std::process::id(),
        bitbankutil_rs::execution_recording::timestamp_ms()
    ));
    let recorder = ExecutionRecorder::new("shutdown", json!({}), JsonlSink::open(&path).unwrap());
    let initial = engine(1, 0, Decimal::ZERO);
    let mut e = PaperExecutionEngine::new(
        initial.config().clone(),
        PaperClock::Replay { now_ms: 1000 },
        initial.balances(),
        Some(recorder.clone()),
    )
    .map(|engine| seeded_engine(engine, 1000))
    .unwrap()
    .with_max_trade_delay_ms(300)
    .unwrap();
    e.place_order(order(OrderSide::Sell, 1.into())).unwrap();
    e.advance_replay_time(1200).unwrap();
    e.cancel_orders("btc_jpy", vec![OrderId(1)]);
    let venue = PaperTradingVenue::new(e);
    let observations = Arc::new(Mutex::new(Vec::new()));
    let mut runner = BotRunner::new(ObserveBalances(observations.clone()), venue.clone());
    runner
        .process(
            1400,
            None,
            BotEvent::Market(MarketEvent::Transactions {
                pair: "btc_jpy".into(),
                transactions: vec![trade(1, 1100, 1.into(), OrderSide::Buy)],
            }),
        )
        .await
        .unwrap();
    runner
        .process(
            1501,
            None,
            BotEvent::Market(MarketEvent::Transactions {
                pair: "btc_jpy".into(),
                transactions: vec![],
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        *observations.lock().unwrap(),
        [Decimal::ZERO, Decimal::from(100)]
    );
    let runtime = BitbankBotBuilder::new(ObserveBalances(observations), venue).spawn();
    runtime.shutdown().await.unwrap();
    recorder.finish().unwrap();
    let records =
        read_records(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).unwrap();
    let status = records
        .iter()
        .rfind(|r| r.kind == "paper_settlement")
        .unwrap();
    assert_eq!(status.data["unsettled"], true);
    assert_eq!(status.data["watermark_ms"], 1200);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cancellation_cannot_rewrite_an_already_sealed_millisecond() {
    let mut e = engine(1, 0, Decimal::ZERO);
    e.place_order(order(OrderSide::Sell, 1.into())).unwrap();
    e.advance_replay_time(1100).unwrap();
    receive(
        &mut e,
        vec![trade(1, 1100, Decimal::new(4, 1), OrderSide::Buy)],
    );
    e.advance_trade_watermark(1100).unwrap();
    e.cancel_orders("btc_jpy", vec![OrderId(1)]);
    assert!(e.drain_events().iter().any(|event| matches!(
        event,
        PaperEvent::OrderCancelPending {
            effective_at_ms: 1101,
            ..
        }
    )));
    assert_eq!(balance(&e, "btc").locked_amount, Decimal::new(6, 1));
    receive(&mut e, vec![trade(2, 1101, 1.into(), OrderSide::Buy)]);
    let events = e.advance_trade_watermark(1101).unwrap();
    assert!(fills(&events).is_empty());
    assert!(
        matches!(events.last(), Some(PaperEvent::OrderCancelled { order, .. }) if order.remaining_amount == Decimal::new(6, 1))
    );
    assert_eq!(balance(&e, "jpy").free_amount, Decimal::from(40));
    assert_eq!(balance(&e, "btc").free_amount, Decimal::new(6, 1));
}

#[tokio::test]
async fn paper_runtime_delivers_market_after_known_disconnect_without_releasing_funds() {
    use bitbankutil_rs::bitbank_bot::BitbankBotBuilder;
    let mut e = engine(1, 0, Decimal::ZERO);
    e.place_order(order(OrderSide::Sell, Decimal::ONE)).unwrap();
    e.advance_replay_time(1200).unwrap();
    e.cancel_orders("btc_jpy", vec![OrderId(1)]);
    e.mark_incomplete("transport_closed");
    let before = e.balances();
    let venue = PaperTradingVenue::new(e);
    let observations = Arc::new(Mutex::new(Vec::new()));
    let runtime =
        BitbankBotBuilder::new(ObserveBalances(observations.clone()), venue.clone()).spawn();
    let sender = runtime.event_sender();
    sender
        .send(BotEvent::Market(MarketEvent::DepthUpdated {
            pair: "btc_jpy".into(),
            depth: MarketDepthSnapshot::new(
                std::collections::BTreeMap::from([(1_000_000.into(), 1.0)]),
                std::collections::BTreeMap::from([(1.into(), 1.0)]),
                1300,
            ),
        }))
        .await
        .unwrap();
    sender
        .send(BotEvent::Market(MarketEvent::Transactions {
            pair: "btc_jpy".into(),
            transactions: vec![trade(1, 1100, Decimal::ONE, OrderSide::Buy)],
        }))
        .await
        .unwrap();
    runtime.shutdown().await.unwrap();
    assert_eq!(
        *observations.lock().unwrap(),
        [Decimal::ZERO, Decimal::ZERO]
    );
    assert_eq!(
        venue.account_snapshot("btc_jpy").await.unwrap().balances,
        before
    );
    assert!(
        venue
            .place_order(order(OrderSide::Sell, Decimal::ONE).into())
            .await
            .is_err()
    );
    assert!(venue.advance_trade_watermark(1400).is_err());
    let shared = venue.engine();
    let mut e = shared.lock().unwrap();
    let status = e.settlement_status();
    assert!(status.unsettled);
    assert_eq!(status.issues, ["transport_closed"]);
    assert_eq!(status.pending_cancellations, 1);
    assert!(!e.drain_events().iter().any(|event| matches!(
        event,
        PaperEvent::OrderFilled { .. } | PaperEvent::OrderCancelled { .. }
    )));
}

// 時刻・資金の回帰fixture。注文価格をspread内に置き、先行数量を0にする。
fn seeded_engine(mut engine: PaperExecutionEngine, exchange_ms: i64) -> PaperExecutionEngine {
    engine.apply_market_event(&bitbankutil_rs::market_event::MarketEvent::DepthUpdated {
        pair: "btc_jpy".into(),
        depth: bitbankutil_rs::market_event::MarketDepthSnapshot::new(
            std::collections::BTreeMap::from([(1_000_000.into(), 1.0)]),
            std::collections::BTreeMap::from([(1.into(), 1.0)]),
            exchange_ms,
        ),
    });
    engine
}
