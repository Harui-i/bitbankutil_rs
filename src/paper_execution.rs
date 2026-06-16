use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use rust_decimal::Decimal;

use crate::{
    market_event::{MarketEvent, MarketTrade},
    order_domain::{BalanceSnapshot, DesiredLimitOrder, OpenOrder, OrderId, OrderSide, OrderType},
    order_executor::{
        OrderExecutionError, OrderExecutor, OrderExecutorFuture, PlacedOrder, PlacementRequest,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperExecutionConfig {
    pub pair: String,
    pub fee_schedule: PaperFeeSchedule,
    pub next_order_id: OrderId,
}

impl PaperExecutionConfig {
    pub fn bitbank_spot_default(pair: impl Into<String>) -> Result<Self, PaperExecutionError> {
        let pair = pair.into();
        Ok(Self {
            fee_schedule: PaperFeeSchedule::bitbank_spot_default(&pair)?,
            pair,
            next_order_id: OrderId(1),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperFeeSchedule {
    pub maker_fee_rate_quote: Decimal,
    pub taker_fee_rate_quote: Decimal,
}

impl PaperFeeSchedule {
    pub fn new(maker_fee_rate_quote: Decimal, taker_fee_rate_quote: Decimal) -> Self {
        Self {
            maker_fee_rate_quote,
            taker_fee_rate_quote,
        }
    }

    pub fn bitbank_spot_default(pair: &str) -> Result<Self, PaperExecutionError> {
        if !pair.ends_with("_jpy") {
            return Err(PaperExecutionError::UnsupportedPair(pair.to_owned()));
        }

        if pair == "btc_jpy" {
            Ok(Self::new(Decimal::ZERO, Decimal::new(1, 3)))
        } else {
            Ok(Self::new(Decimal::new(-2, 4), Decimal::new(12, 4)))
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PaperEvent {
    OrderAccepted {
        order_id: OrderId,
        order: DesiredLimitOrder,
    },
    OrderRejected {
        order: DesiredLimitOrder,
        reason: PaperRejectReason,
    },
    OrderCancelled {
        order_id: OrderId,
        order: OpenOrder,
    },
    OrderFilled {
        order_id: OrderId,
        order: DesiredLimitOrder,
        price: Decimal,
        amount: Decimal,
        fee_amount_quote: Decimal,
        trade: MarketTrade,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaperRejectReason {
    PairMismatch {
        expected: String,
        actual: String,
    },
    NonPositiveOrder {
        amount: Decimal,
        price: Decimal,
    },
    InsufficientFunds {
        asset: String,
        required: Decimal,
        free: Decimal,
    },
    UnsupportedPair(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaperExecutionError {
    PairMismatch {
        expected: String,
        actual: String,
    },
    NonPositiveOrder {
        amount: Decimal,
        price: Decimal,
    },
    InsufficientFunds {
        asset: String,
        required: Decimal,
        free: Decimal,
    },
    MissingBalance(String),
    UnsupportedPair(String),
}

#[derive(Debug, Clone)]
pub struct PaperExecutionEngine {
    config: PaperExecutionConfig,
    base_asset: String,
    quote_asset: String,
    balances: BTreeMap<String, BalanceSnapshot>,
    open_orders: BTreeMap<OrderId, OpenOrder>,
    event_history: Vec<PaperEvent>,
}

impl PaperExecutionEngine {
    pub fn new(
        config: PaperExecutionConfig,
        balances: Vec<BalanceSnapshot>,
    ) -> Result<Self, PaperExecutionError> {
        let (base_asset, quote_asset) = parse_jpy_pair(&config.pair)?;
        let balances = balances
            .into_iter()
            .map(|balance| (balance.asset.clone(), balance))
            .collect::<BTreeMap<_, _>>();

        if !balances.contains_key(&base_asset) {
            return Err(PaperExecutionError::MissingBalance(base_asset));
        }
        if !balances.contains_key(&quote_asset) {
            return Err(PaperExecutionError::MissingBalance(quote_asset));
        }

        Ok(Self {
            config,
            base_asset,
            quote_asset,
            balances,
            open_orders: BTreeMap::new(),
            event_history: Vec::new(),
        })
    }

    pub fn place_order(
        &mut self,
        order: DesiredLimitOrder,
    ) -> Result<PlacedOrder, PaperExecutionError> {
        if order.pair != self.config.pair {
            let reason = PaperRejectReason::PairMismatch {
                expected: self.config.pair.clone(),
                actual: order.pair.clone(),
            };
            self.record_event(PaperEvent::OrderRejected {
                order,
                reason: reason.clone(),
            });
            return Err(PaperExecutionError::from(reason));
        }

        if order.amount <= Decimal::ZERO || order.price <= Decimal::ZERO {
            let reason = PaperRejectReason::NonPositiveOrder {
                amount: order.amount,
                price: order.price,
            };
            self.record_event(PaperEvent::OrderRejected {
                order,
                reason: reason.clone(),
            });
            return Err(PaperExecutionError::from(reason));
        }

        let lock_result = self.lock_funds_for_order(&order);
        if let Err(err) = lock_result {
            self.record_event(PaperEvent::OrderRejected {
                order,
                reason: err.clone(),
            });
            return Err(PaperExecutionError::from(err));
        }

        let order_id = self.config.next_order_id;
        self.config.next_order_id = OrderId(self.config.next_order_id.0 + 1);

        self.open_orders.insert(
            order_id,
            OpenOrder {
                order_id,
                pair: order.pair.clone(),
                side: order.side,
                order_type: OrderType::Limit,
                remaining_amount: order.amount,
                price: Some(order.price),
                post_only: order.post_only,
            },
        );
        self.record_event(PaperEvent::OrderAccepted {
            order_id,
            order: order.clone(),
        });

        Ok(PlacedOrder {
            order_id: Some(order_id),
        })
    }

    pub fn cancel_orders(&mut self, pair: &str, order_ids: Vec<OrderId>) {
        for order_id in order_ids {
            let Some(open_order) = self.open_orders.remove(&order_id) else {
                continue;
            };

            if open_order.pair != pair {
                self.open_orders.insert(order_id, open_order);
                continue;
            }

            self.unlock_funds_for_open_order(&open_order);
            self.record_event(PaperEvent::OrderCancelled {
                order_id,
                order: open_order,
            });
        }
    }

    pub fn apply_market_event(&mut self, event: &MarketEvent) -> Vec<PaperEvent> {
        let MarketEvent::Transactions { pair, transactions } = event else {
            return Vec::new();
        };
        if pair != &self.config.pair {
            return Vec::new();
        }

        let mut events = Vec::new();
        for trade in transactions {
            let fill_order_ids = self.fill_order_ids_for_trade(trade);
            for order_id in fill_order_ids {
                let Some(open_order) = self.open_orders.remove(&order_id) else {
                    continue;
                };

                let event = self.fill_open_order(open_order, trade.clone());
                self.record_event(event.clone());
                events.push(event);
            }
        }

        events
    }

    pub fn open_orders(&self) -> Vec<OpenOrder> {
        self.open_orders.values().cloned().collect()
    }

    pub fn balances(&self) -> Vec<BalanceSnapshot> {
        self.balances.values().cloned().collect()
    }

    pub fn drain_events(&mut self) -> Vec<PaperEvent> {
        std::mem::take(&mut self.event_history)
    }

    pub fn config(&self) -> &PaperExecutionConfig {
        &self.config
    }

    fn lock_funds_for_order(&mut self, order: &DesiredLimitOrder) -> Result<(), PaperRejectReason> {
        let (asset, required) = match order.side {
            OrderSide::Buy => {
                let notional = order.amount * order.price;
                let fee =
                    positive_quote_fee(notional, self.config.fee_schedule.maker_fee_rate_quote);
                (self.quote_asset.clone(), notional + fee)
            }
            OrderSide::Sell => (self.base_asset.clone(), order.amount),
        };

        let balance = self
            .balances
            .get_mut(&asset)
            .expect("paper engine balances were validated at construction");
        if balance.free_amount < required {
            return Err(PaperRejectReason::InsufficientFunds {
                asset,
                required,
                free: balance.free_amount,
            });
        }

        balance.free_amount -= required;
        balance.locked_amount += required;
        balance.onhand_amount = balance.free_amount + balance.locked_amount;
        Ok(())
    }

    fn unlock_funds_for_open_order(&mut self, order: &OpenOrder) {
        let asset = match order.side {
            OrderSide::Buy => self.quote_asset.clone(),
            OrderSide::Sell => self.base_asset.clone(),
        };
        let amount =
            locked_amount_for_open_order(order, self.config.fee_schedule.maker_fee_rate_quote);
        move_locked_to_free(&mut self.balances, &asset, amount);
    }

    fn fill_order_ids_for_trade(&self, trade: &MarketTrade) -> Vec<OrderId> {
        let mut fill_candidates = self
            .open_orders
            .values()
            .filter(|order| order_matches_trade(order, trade))
            .cloned()
            .collect::<Vec<_>>();

        fill_candidates.sort_by(|a, b| match a.side {
            OrderSide::Buy => b
                .price
                .cmp(&a.price)
                .then_with(|| a.order_id.cmp(&b.order_id)),
            OrderSide::Sell => a
                .price
                .cmp(&b.price)
                .then_with(|| a.order_id.cmp(&b.order_id)),
        });

        fill_candidates
            .into_iter()
            .map(|order| order.order_id)
            .collect()
    }

    fn fill_open_order(&mut self, open_order: OpenOrder, trade: MarketTrade) -> PaperEvent {
        let order = open_order
            .to_desired_limit_order()
            .expect("paper engine only stores limit orders with prices");
        let notional = order.amount * order.price;
        let fee_amount_quote = notional * self.config.fee_schedule.maker_fee_rate_quote;

        match order.side {
            OrderSide::Buy => {
                let locked_amount = notional
                    + positive_quote_fee(notional, self.config.fee_schedule.maker_fee_rate_quote);
                decrease_locked(&mut self.balances, &self.quote_asset, locked_amount);
                add_free(&mut self.balances, &self.base_asset, order.amount);
                if fee_amount_quote < Decimal::ZERO {
                    add_free(&mut self.balances, &self.quote_asset, -fee_amount_quote);
                }
            }
            OrderSide::Sell => {
                decrease_locked(&mut self.balances, &self.base_asset, order.amount);
                add_free(
                    &mut self.balances,
                    &self.quote_asset,
                    notional - fee_amount_quote,
                );
            }
        }

        PaperEvent::OrderFilled {
            order_id: open_order.order_id,
            order,
            price: open_order
                .price
                .expect("paper engine limit order must have price"),
            amount: open_order.remaining_amount,
            fee_amount_quote,
            trade,
        }
    }

    fn record_event(&mut self, event: PaperEvent) {
        self.event_history.push(event);
    }
}

#[derive(Debug, Clone)]
pub struct PaperOrderExecutor {
    engine: Arc<Mutex<PaperExecutionEngine>>,
}

impl PaperOrderExecutor {
    pub fn new(engine: PaperExecutionEngine) -> Self {
        Self {
            engine: Arc::new(Mutex::new(engine)),
        }
    }

    pub fn from_shared(engine: Arc<Mutex<PaperExecutionEngine>>) -> Self {
        Self { engine }
    }

    pub fn engine(&self) -> Arc<Mutex<PaperExecutionEngine>> {
        self.engine.clone()
    }
}

impl OrderExecutor for PaperOrderExecutor {
    fn place_order(&self, request: PlacementRequest) -> OrderExecutorFuture<'_, PlacedOrder> {
        Box::pin(async move {
            self.engine
                .lock()
                .expect("paper execution engine mutex poisoned")
                .place_order(request.order)
                .map_err(|err| OrderExecutionError::Other(format!("{err:?}")))
        })
    }

    fn cancel_orders<'a>(
        &'a self,
        pair: &'a str,
        order_ids: Vec<OrderId>,
    ) -> OrderExecutorFuture<'a, ()> {
        Box::pin(async move {
            self.engine
                .lock()
                .expect("paper execution engine mutex poisoned")
                .cancel_orders(pair, order_ids);
            Ok(())
        })
    }
}

fn parse_jpy_pair(pair: &str) -> Result<(String, String), PaperExecutionError> {
    let Some(base) = pair.strip_suffix("_jpy") else {
        return Err(PaperExecutionError::UnsupportedPair(pair.to_owned()));
    };
    if base.is_empty() || base.contains('_') {
        return Err(PaperExecutionError::UnsupportedPair(pair.to_owned()));
    }

    Ok((base.to_owned(), "jpy".to_owned()))
}

fn order_matches_trade(order: &OpenOrder, trade: &MarketTrade) -> bool {
    let Some(price) = order.price else {
        return false;
    };

    match (trade.side, order.side) {
        (OrderSide::Buy, OrderSide::Sell) => trade.price >= price,
        (OrderSide::Sell, OrderSide::Buy) => trade.price <= price,
        _ => false,
    }
}

fn locked_amount_for_open_order(order: &OpenOrder, maker_fee_rate_quote: Decimal) -> Decimal {
    match order.side {
        OrderSide::Buy => {
            let notional = order.remaining_amount
                * order
                    .price
                    .expect("paper engine limit order must have price");
            notional + positive_quote_fee(notional, maker_fee_rate_quote)
        }
        OrderSide::Sell => order.remaining_amount,
    }
}

fn positive_quote_fee(notional: Decimal, fee_rate: Decimal) -> Decimal {
    let fee = notional * fee_rate;
    if fee > Decimal::ZERO {
        fee
    } else {
        Decimal::ZERO
    }
}

fn move_locked_to_free(
    balances: &mut BTreeMap<String, BalanceSnapshot>,
    asset: &str,
    amount: Decimal,
) {
    decrease_locked(balances, asset, amount);
    add_free(balances, asset, amount);
}

fn decrease_locked(balances: &mut BTreeMap<String, BalanceSnapshot>, asset: &str, amount: Decimal) {
    let balance = balances
        .get_mut(asset)
        .expect("paper engine balance must exist");
    balance.locked_amount -= amount;
    balance.onhand_amount = balance.free_amount + balance.locked_amount;
}

fn add_free(balances: &mut BTreeMap<String, BalanceSnapshot>, asset: &str, amount: Decimal) {
    let balance = balances
        .get_mut(asset)
        .expect("paper engine balance must exist");
    balance.free_amount += amount;
    balance.onhand_amount = balance.free_amount + balance.locked_amount;
}

impl From<PaperRejectReason> for PaperExecutionError {
    fn from(value: PaperRejectReason) -> Self {
        match value {
            PaperRejectReason::PairMismatch { expected, actual } => {
                Self::PairMismatch { expected, actual }
            }
            PaperRejectReason::NonPositiveOrder { amount, price } => {
                Self::NonPositiveOrder { amount, price }
            }
            PaperRejectReason::InsufficientFunds {
                asset,
                required,
                free,
            } => Self::InsufficientFunds {
                asset,
                required,
                free,
            },
            PaperRejectReason::UnsupportedPair(pair) => Self::UnsupportedPair(pair),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balance(asset: &str, free_amount: Decimal) -> BalanceSnapshot {
        BalanceSnapshot {
            asset: asset.to_owned(),
            free_amount,
            locked_amount: Decimal::ZERO,
            onhand_amount: free_amount,
        }
    }

    fn engine_with_balances(btc_free: Decimal, jpy_free: Decimal) -> PaperExecutionEngine {
        PaperExecutionEngine::new(
            PaperExecutionConfig::bitbank_spot_default("btc_jpy").unwrap(),
            vec![balance("btc", btc_free), balance("jpy", jpy_free)],
        )
        .unwrap()
    }

    fn order(side: OrderSide, amount: Decimal, price: Decimal) -> DesiredLimitOrder {
        DesiredLimitOrder::limit("btc_jpy".to_owned(), side, amount, price)
    }

    fn trade(side: OrderSide, amount: Decimal, price: Decimal, transaction_id: i64) -> MarketTrade {
        MarketTrade {
            amount,
            executed_at: 1_710_000_000_000,
            price,
            side,
            transaction_id,
        }
    }

    fn transactions(trades: Vec<MarketTrade>) -> MarketEvent {
        MarketEvent::Transactions {
            pair: "btc_jpy".to_owned(),
            transactions: trades,
        }
    }

    fn balance_of(engine: &PaperExecutionEngine, asset: &str) -> BalanceSnapshot {
        engine
            .balances()
            .into_iter()
            .find(|balance| balance.asset == asset)
            .unwrap()
    }

    #[test]
    fn bitbank_default_fee_depends_on_pair() {
        assert_eq!(
            PaperFeeSchedule::bitbank_spot_default("btc_jpy").unwrap(),
            PaperFeeSchedule::new(Decimal::ZERO, Decimal::new(1, 3))
        );
        assert_eq!(
            PaperFeeSchedule::bitbank_spot_default("eth_jpy").unwrap(),
            PaperFeeSchedule::new(Decimal::new(-2, 4), Decimal::new(12, 4))
        );
    }

    #[test]
    fn place_buy_order_locks_quote_balance() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(1_000_000, 0));

        let placed = engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();

        assert_eq!(placed.order_id, Some(OrderId(1)));
        assert_eq!(
            balance_of(&engine, "jpy").free_amount,
            Decimal::new(500_000, 0)
        );
        assert_eq!(
            balance_of(&engine, "jpy").locked_amount,
            Decimal::new(500_000, 0)
        );
        assert_eq!(engine.open_orders().len(), 1);
        assert!(matches!(
            engine.drain_events().as_slice(),
            [PaperEvent::OrderAccepted {
                order_id: OrderId(1),
                ..
            }]
        ));
    }

    #[test]
    fn place_sell_order_locks_base_balance() {
        let mut engine = engine_with_balances(Decimal::new(2, 1), Decimal::ZERO);

        engine
            .place_order(order(
                OrderSide::Sell,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();

        assert_eq!(balance_of(&engine, "btc").free_amount, Decimal::new(1, 1));
        assert_eq!(balance_of(&engine, "btc").locked_amount, Decimal::new(1, 1));
    }

    #[test]
    fn insufficient_funds_rejects_order_and_records_event() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(100_000, 0));
        let order = order(
            OrderSide::Buy,
            Decimal::new(1, 1),
            Decimal::new(5_000_000, 0),
        );

        let result = engine.place_order(order.clone());

        assert!(matches!(
            result,
            Err(PaperExecutionError::InsufficientFunds { .. })
        ));
        assert!(engine.open_orders().is_empty());
        assert!(matches!(
            engine.drain_events().as_slice(),
            [PaperEvent::OrderRejected {
                order: rejected_order,
                reason: PaperRejectReason::InsufficientFunds { .. }
            }] if rejected_order == &order
        ));
    }

    #[test]
    fn non_positive_amount_or_price_rejects_order_without_changing_balances() {
        for invalid_order in [
            order(OrderSide::Buy, Decimal::ZERO, Decimal::new(5_000_000, 0)),
            order(OrderSide::Buy, Decimal::new(-1, 1), Decimal::new(5_000_000, 0)),
            order(OrderSide::Buy, Decimal::new(1, 1), Decimal::ZERO),
            order(OrderSide::Buy, Decimal::new(1, 1), Decimal::new(-5_000_000, 0)),
            order(OrderSide::Sell, Decimal::ZERO, Decimal::new(5_000_000, 0)),
            order(OrderSide::Sell, Decimal::new(-1, 1), Decimal::new(5_000_000, 0)),
            order(OrderSide::Sell, Decimal::new(1, 1), Decimal::ZERO),
            order(OrderSide::Sell, Decimal::new(1, 1), Decimal::new(-5_000_000, 0)),
        ] {
            let mut engine = engine_with_balances(Decimal::new(1, 0), Decimal::new(1_000_000, 0));

            let result = engine.place_order(invalid_order);

            assert!(result.is_err());
            assert!(engine.open_orders().is_empty());
            assert_eq!(balance_of(&engine, "btc"), balance("btc", Decimal::new(1, 0)));
            assert_eq!(
                balance_of(&engine, "jpy"),
                balance("jpy", Decimal::new(1_000_000, 0))
            );
            assert!(matches!(
                engine.drain_events().as_slice(),
                [PaperEvent::OrderRejected {
                    reason: PaperRejectReason::NonPositiveOrder { .. },
                    ..
                }]
            ));
        }
    }

    #[test]
    fn cancel_order_unlocks_balance_and_missing_id_succeeds() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(1_000_000, 0));
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        engine.cancel_orders("btc_jpy", vec![OrderId(1), OrderId(999)]);

        assert!(engine.open_orders().is_empty());
        assert_eq!(
            balance_of(&engine, "jpy").free_amount,
            Decimal::new(1_000_000, 0)
        );
        assert_eq!(balance_of(&engine, "jpy").locked_amount, Decimal::ZERO);
        assert!(matches!(
            engine.drain_events().as_slice(),
            [PaperEvent::OrderCancelled {
                order_id: OrderId(1),
                ..
            }]
        ));
    }

    #[test]
    fn buy_trade_fills_sell_order_and_updates_balances() {
        let mut engine = engine_with_balances(Decimal::new(1, 1), Decimal::ZERO);
        engine
            .place_order(order(
                OrderSide::Sell,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        let events = engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Buy,
            Decimal::new(1, 2),
            Decimal::new(5_100_000, 0),
            10,
        )]));

        assert!(engine.open_orders().is_empty());
        assert_eq!(balance_of(&engine, "btc").locked_amount, Decimal::ZERO);
        assert_eq!(
            balance_of(&engine, "jpy").free_amount,
            Decimal::new(500_000, 0)
        );
        assert!(matches!(
            events.as_slice(),
            [PaperEvent::OrderFilled {
                order_id: OrderId(1),
                price,
                amount,
                fee_amount_quote,
                ..
            }] if *price == Decimal::new(5_000_000, 0)
                && *amount == Decimal::new(1, 1)
                && *fee_amount_quote == Decimal::ZERO
        ));
    }

    #[test]
    fn sell_trade_fills_buy_order_and_updates_balances() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(1_000_000, 0));
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        let events = engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Sell,
            Decimal::new(1, 2),
            Decimal::new(4_900_000, 0),
            11,
        )]));

        assert!(engine.open_orders().is_empty());
        assert_eq!(balance_of(&engine, "btc").free_amount, Decimal::new(1, 1));
        assert_eq!(balance_of(&engine, "jpy").locked_amount, Decimal::ZERO);
        assert_eq!(
            balance_of(&engine, "jpy").free_amount,
            Decimal::new(500_000, 0)
        );
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn positive_maker_fee_for_buy_order_is_not_charged_twice() {
        let mut engine = PaperExecutionEngine::new(
            PaperExecutionConfig {
                pair: "btc_jpy".to_owned(),
                fee_schedule: PaperFeeSchedule::new(Decimal::new(1, 3), Decimal::ZERO),
                next_order_id: OrderId(1),
            },
            vec![
                balance("btc", Decimal::ZERO),
                balance("jpy", Decimal::new(1_000_000, 0)),
            ],
        )
        .unwrap();
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        let events = engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Sell,
            Decimal::new(1, 1),
            Decimal::new(5_000_000, 0),
            12,
        )]));

        assert_eq!(balance_of(&engine, "btc").free_amount, Decimal::new(1, 1));
        assert_eq!(balance_of(&engine, "jpy").locked_amount, Decimal::ZERO);
        assert_eq!(
            balance_of(&engine, "jpy").free_amount,
            Decimal::new(499_500, 0)
        );
        assert!(matches!(
            events.as_slice(),
            [PaperEvent::OrderFilled {
                fee_amount_quote, ..
            }] if *fee_amount_quote == Decimal::new(500, 0)
        ));
    }

    #[test]
    fn trade_does_not_fill_when_pair_side_or_price_do_not_match() {
        let mut engine = engine_with_balances(Decimal::new(1, 1), Decimal::new(1_000_000, 0));
        engine
            .place_order(order(
                OrderSide::Sell,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        assert!(engine
            .apply_market_event(&transactions(vec![trade(
                OrderSide::Sell,
                Decimal::new(1, 1),
                Decimal::new(5_100_000, 0),
                12,
            )]))
            .is_empty());
        assert!(engine
            .apply_market_event(&transactions(vec![trade(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(4_900_000, 0),
                13,
            )]))
            .is_empty());
        assert!(engine
            .apply_market_event(&MarketEvent::Transactions {
                pair: "eth_jpy".to_owned(),
                transactions: vec![trade(
                    OrderSide::Buy,
                    Decimal::new(1, 1),
                    Decimal::new(5_100_000, 0),
                    14,
                )],
            })
            .is_empty());
        assert_eq!(engine.open_orders().len(), 1);
    }

    #[test]
    fn maker_rebate_is_applied_to_quote_balance() {
        let mut engine = PaperExecutionEngine::new(
            PaperExecutionConfig::bitbank_spot_default("eth_jpy").unwrap(),
            vec![
                balance("eth", Decimal::new(1, 0)),
                balance("jpy", Decimal::ZERO),
            ],
        )
        .unwrap();
        engine
            .place_order(DesiredLimitOrder::limit(
                "eth_jpy".to_owned(),
                OrderSide::Sell,
                Decimal::new(1, 0),
                Decimal::new(500_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        let events = engine.apply_market_event(&MarketEvent::Transactions {
            pair: "eth_jpy".to_owned(),
            transactions: vec![trade(
                OrderSide::Buy,
                Decimal::new(1, 0),
                Decimal::new(500_000, 0),
                20,
            )],
        });

        assert_eq!(
            balance_of(&engine, "jpy").free_amount,
            Decimal::new(500_100, 0)
        );
        assert!(matches!(
            events.as_slice(),
            [PaperEvent::OrderFilled {
                fee_amount_quote, ..
            }] if *fee_amount_quote == Decimal::new(-100, 0)
        ));
    }

    #[test]
    fn fills_multiple_orders_by_price_then_order_id() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(2_000_000, 0));
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(4_900_000, 0),
            ))
            .unwrap();
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();
        engine.drain_events();

        let events = engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Sell,
            Decimal::new(1, 2),
            Decimal::new(4_800_000, 0),
            30,
        )]));

        let fill_ids = events
            .into_iter()
            .map(|event| match event {
                PaperEvent::OrderFilled { order_id, .. } => order_id,
                _ => panic!("unexpected paper event"),
            })
            .collect::<Vec<_>>();
        assert_eq!(fill_ids, vec![OrderId(2), OrderId(3), OrderId(1)]);
    }

    #[tokio::test]
    async fn paper_order_executor_places_and_cancels_through_order_executor_trait() {
        let executor = PaperOrderExecutor::new(engine_with_balances(
            Decimal::ZERO,
            Decimal::new(1_000_000, 0),
        ));

        let placed = executor
            .place_order(PlacementRequest::from(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            )))
            .await
            .unwrap();
        executor
            .cancel_orders("btc_jpy", vec![placed.order_id.unwrap()])
            .await
            .unwrap();

        let engine = executor.engine();
        let mut engine = engine.lock().unwrap();
        assert!(engine.open_orders().is_empty());
        assert!(matches!(
            engine.drain_events().as_slice(),
            [
                PaperEvent::OrderAccepted { .. },
                PaperEvent::OrderCancelled { .. }
            ]
        ));
    }

    #[test]
    fn unsupported_non_jpy_pair_is_rejected_at_construction() {
        let result = PaperExecutionConfig::bitbank_spot_default("xrp_btc");

        assert!(matches!(
            result,
            Err(PaperExecutionError::UnsupportedPair(pair)) if pair == "xrp_btc"
        ));
    }

    #[test]
    fn all_paper_events_can_be_drained() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(1_000_000, 0));
        engine
            .place_order(order(
                OrderSide::Buy,
                Decimal::new(1, 1),
                Decimal::new(5_000_000, 0),
            ))
            .unwrap();

        assert_eq!(engine.drain_events().len(), 1);
        assert!(engine.drain_events().is_empty());
    }

    #[test]
    fn order_candidate_set_does_not_depend_on_trade_amount() {
        let mut engine = engine_with_balances(Decimal::ZERO, Decimal::new(1_500_000, 0));
        for _ in 0..3 {
            engine
                .place_order(order(
                    OrderSide::Buy,
                    Decimal::new(1, 1),
                    Decimal::new(5_000_000, 0),
                ))
                .unwrap();
        }
        engine.drain_events();

        let events = engine.apply_market_event(&transactions(vec![trade(
            OrderSide::Sell,
            Decimal::new(1, 8),
            Decimal::new(5_000_000, 0),
            40,
        )]));

        assert_eq!(events.len(), 3);
        assert!(engine.open_orders().is_empty());
    }
}
