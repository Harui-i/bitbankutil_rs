use std::{
    collections::{HashMap, VecDeque},
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use async_trait::async_trait;
use log::{debug, warn};
use rust_decimal::prelude::*;
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};

use crate::{
    bitbank_bot::{BitbankEvent, BitbankInboundMessage, BotContext, BotStrategy},
    bitbank_structs::{
        BitbankActiveOrdersResponse, BitbankAssetDatum, BitbankAssetsData,
        BitbankCancelOrderResponse, BitbankCancelOrdersResponse, BitbankCircuitBreakInfo,
        BitbankCreateOrderResponse, BitbankDepth, BitbankDepthDiff, BitbankDepthWhole,
        BitbankGetOrderResponse, BitbankTickerResponse, BitbankTransactionDatum,
        BitbankTransactionsData,
    },
    depth::Depth,
    trading_api::BitbankTradingApi,
};

#[derive(thiserror::Error, Debug)]
pub enum BacktestError {
    #[error("io error while loading capture: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse json at line {line}: {source}")]
    Parse {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("missing payload for room {room_name}")]
    MissingPayload { room_name: String },
    #[error("unsupported room name {room_name}")]
    UnsupportedRoom { room_name: String },
    #[error("strategy emitted event but context receiver overflowed")]
    EventChannelClosed,
}

#[derive(Clone, Debug)]
pub struct RateLimitConfig {
    /// Minimum interval between two consecutive requests of the same category.
    pub min_interval: Duration,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            min_interval: Duration::from_millis(100),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LatencyConfig {
    /// Simulated per-request latency (round trip).
    pub request_latency: Duration,
}

impl Default for LatencyConfig {
    fn default() -> Self {
        Self {
            request_latency: Duration::from_millis(0),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BacktestConfig {
    pub pair: String,
    pub data_path: PathBuf,
    pub initial_base: Decimal,
    pub initial_quote: Decimal,
    pub maker_fee_rate: Decimal,
    pub taker_fee_rate: Decimal,
    pub rate_limit: RateLimitConfig,
    pub latency: LatencyConfig,
    /// Higher values accelerate the backtest (100.0 = 100x faster than wall-clock).
    pub speed_multiplier: f64,
}

impl BacktestConfig {
    pub fn builder(
        pair: impl Into<String>,
        data_path: impl Into<PathBuf>,
    ) -> BacktestConfigBuilder {
        BacktestConfigBuilder {
            pair: pair.into(),
            data_path: data_path.into(),
            initial_base: Decimal::ZERO,
            initial_quote: Decimal::ZERO,
            maker_fee_rate: Decimal::ZERO,
            taker_fee_rate: Decimal::ZERO,
            rate_limit: RateLimitConfig::default(),
            latency: LatencyConfig::default(),
            speed_multiplier: 500.0,
        }
    }
}

pub struct BacktestConfigBuilder {
    pair: String,
    data_path: PathBuf,
    initial_base: Decimal,
    initial_quote: Decimal,
    maker_fee_rate: Decimal,
    taker_fee_rate: Decimal,
    rate_limit: RateLimitConfig,
    latency: LatencyConfig,
    speed_multiplier: f64,
}

impl BacktestConfigBuilder {
    pub fn initial_base(mut self, amount: Decimal) -> Self {
        self.initial_base = amount;
        self
    }

    pub fn initial_quote(mut self, amount: Decimal) -> Self {
        self.initial_quote = amount;
        self
    }

    pub fn maker_fee_rate(mut self, rate: Decimal) -> Self {
        self.maker_fee_rate = rate;
        self
    }

    pub fn taker_fee_rate(mut self, rate: Decimal) -> Self {
        self.taker_fee_rate = rate;
        self
    }

    pub fn rate_limit(mut self, config: RateLimitConfig) -> Self {
        self.rate_limit = config;
        self
    }

    pub fn latency(mut self, config: LatencyConfig) -> Self {
        self.latency = config;
        self
    }

    pub fn speed_multiplier(mut self, speed: f64) -> Self {
        self.speed_multiplier = speed.max(1.0);
        self
    }

    pub fn build(self) -> BacktestConfig {
        BacktestConfig {
            pair: self.pair,
            data_path: self.data_path,
            initial_base: self.initial_base,
            initial_quote: self.initial_quote,
            maker_fee_rate: self.maker_fee_rate,
            taker_fee_rate: self.taker_fee_rate,
            rate_limit: self.rate_limit,
            latency: self.latency,
            speed_multiplier: self.speed_multiplier,
        }
    }
}

#[derive(Clone)]
pub struct SimulatedBitbankApi {
    inner: std::sync::Arc<SimulatedInner>,
}

#[derive(Clone)]
struct SimulatedInner {
    state: std::sync::Arc<Mutex<SimulatedState>>,
    rate_limit: RateLimiter,
    latency: LatencyConfig,
    speed_multiplier: f64,
}

#[derive(Clone)]
struct RateLimiter {
    min_interval: Duration,
    last_call: std::sync::Arc<Mutex<HashMap<ApiCategory, tokio::time::Instant>>>,
}

#[derive(Hash, Eq, PartialEq, Clone, Copy, Debug)]
enum ApiCategory {
    GetAssets,
    GetActiveOrders,
    PostOrder,
    CancelOrders,
}

#[derive(thiserror::Error, Debug)]
pub enum BacktestApiError {
    #[error("insufficient funds for {side} order (needed {needed}, available {available})")]
    InsufficientFunds {
        side: &'static str,
        needed: Decimal,
        available: Decimal,
    },
    #[error("unknown order id {order_id}")]
    UnknownOrder { order_id: u64 },
    #[error("price not provided for limit order")]
    MissingPrice,
    #[error("failed to parse decimal: {0}")]
    ParseDecimal(#[from] rust_decimal::Error),
}

#[async_trait]
impl BitbankTradingApi for SimulatedBitbankApi {
    type Error = BacktestApiError;

    async fn get_active_orders(
        &self,
        pair: Option<&str>,
        _count: Option<&str>,
        _from_id: Option<u64>,
        _end_id: Option<u64>,
        _since: Option<u64>,
        _end: Option<u64>,
    ) -> Result<BitbankActiveOrdersResponse, Self::Error> {
        self.inner
            .throttle(ApiCategory::GetActiveOrders, self.inner.speed_multiplier)
            .await;
        self.inner.simulate_latency().await;
        let mut guard = self.inner.state.lock().await;
        let requested_pair = pair
            .map(|p| p.to_string())
            .unwrap_or_else(|| guard.pair.clone());
        guard.build_active_orders_response(&requested_pair)
    }

    async fn get_assets(&self) -> Result<BitbankAssetsData, Self::Error> {
        self.inner
            .throttle(ApiCategory::GetAssets, self.inner.speed_multiplier)
            .await;
        self.inner.simulate_latency().await;
        let mut guard = self.inner.state.lock().await;
        Ok(guard.build_assets_snapshot())
    }

    async fn post_order(
        &self,
        pair: &str,
        amount: &str,
        price: Option<&str>,
        side: &str,
        _type: &str,
        post_only: Option<bool>,
        _trigger_price: Option<&str>,
    ) -> Result<BitbankCreateOrderResponse, Self::Error> {
        self.inner
            .throttle(ApiCategory::PostOrder, self.inner.speed_multiplier)
            .await;
        self.inner.simulate_latency().await;
        let mut guard = self.inner.state.lock().await;
        guard.place_order(pair, amount, price, side, post_only.unwrap_or(false))
    }

    async fn post_cancel_orders(
        &self,
        pair: &str,
        order_ids: Vec<u64>,
    ) -> Result<BitbankCancelOrdersResponse, Self::Error> {
        self.inner
            .throttle(ApiCategory::CancelOrders, self.inner.speed_multiplier)
            .await;
        self.inner.simulate_latency().await;
        let mut guard = self.inner.state.lock().await;
        guard.cancel_orders(pair, &order_ids)
    }
}

impl SimulatedBitbankApi {
    fn new(state: std::sync::Arc<Mutex<SimulatedState>>, config: &BacktestConfig) -> Self {
        let inner = SimulatedInner {
            state,
            rate_limit: RateLimiter::new(config.rate_limit.clone()),
            latency: config.latency.clone(),
            speed_multiplier: config.speed_multiplier,
        };

        Self {
            inner: std::sync::Arc::new(inner),
        }
    }
}

impl RateLimiter {
    fn new(config: RateLimitConfig) -> Self {
        Self {
            min_interval: config.min_interval,
            last_call: std::sync::Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl SimulatedInner {
    async fn throttle(&self, category: ApiCategory, speed: f64) {
        if self.rate_limit.min_interval.is_zero() {
            return;
        }
        let scaled = scale_duration(self.rate_limit.min_interval, speed);
        let mut guard = self.rate_limit.last_call.lock().await;
        if let Some(last) = guard.get_mut(&category) {
            let elapsed = last.elapsed();
            if elapsed < scaled {
                tokio::time::sleep(scaled - elapsed).await;
            }
            *last = tokio::time::Instant::now();
        } else {
            guard.insert(category, tokio::time::Instant::now());
        }
    }

    async fn simulate_latency(&self) {
        if self.latency.request_latency.is_zero() {
            return;
        }
        let scaled = scale_duration(self.latency.request_latency, self.speed_multiplier);
        tokio::time::sleep(scaled).await;
    }
}

#[derive(Clone, Debug)]
struct SimulatedOrder {
    id: u64,
    side: OrderSide,
    price: Decimal,
    amount: Decimal,
    remaining: Decimal,
}

#[derive(Clone, Copy, Debug)]
enum OrderSide {
    Buy,
    Sell,
}

#[derive(Clone, Debug)]
struct BacktestMetrics {
    total_orders: usize,
    trade_count: usize,
    total_fees: Decimal,
    initial_equity: Option<Decimal>,
    last_equity: Option<Decimal>,
    equity_peak: Option<Decimal>,
    max_drawdown: Decimal,
}

impl BacktestMetrics {
    fn new() -> Self {
        Self {
            total_orders: 0,
            trade_count: 0,
            total_fees: Decimal::ZERO,
            initial_equity: None,
            last_equity: None,
            equity_peak: None,
            max_drawdown: Decimal::ZERO,
        }
    }

    fn register_equity(&mut self, equity: Decimal) {
        if self.initial_equity.is_none() {
            self.initial_equity = Some(equity);
            self.equity_peak = Some(equity);
        }

        if self.equity_peak.map_or(true, |peak| equity > peak) {
            self.equity_peak = Some(equity);
        }

        if let Some(peak) = self.equity_peak {
            let drawdown = peak - equity;
            if drawdown > self.max_drawdown {
                self.max_drawdown = drawdown;
            }
        }

        self.last_equity = Some(equity);
    }
}

struct SimulatedState {
    pair: String,
    maker_fee_rate: Decimal,
    taker_fee_rate: Decimal,
    base_free: Decimal,
    base_locked: Decimal,
    quote_free: Decimal,
    quote_locked: Decimal,
    next_order_id: u64,
    open_orders: HashMap<u64, SimulatedOrder>,
    metrics: BacktestMetrics,
    last_mid: Option<Decimal>,
}

impl SimulatedState {
    fn new(config: &BacktestConfig) -> Self {
        Self {
            pair: config.pair.clone(),
            maker_fee_rate: config.maker_fee_rate,
            taker_fee_rate: config.taker_fee_rate,
            base_free: config.initial_base,
            base_locked: Decimal::ZERO,
            quote_free: config.initial_quote,
            quote_locked: Decimal::ZERO,
            next_order_id: 1,
            open_orders: HashMap::new(),
            metrics: BacktestMetrics::new(),
            last_mid: None,
        }
    }

    fn place_order(
        &mut self,
        pair: &str,
        amount: &str,
        price: Option<&str>,
        side: &str,
        post_only: bool,
    ) -> Result<BitbankCreateOrderResponse, BacktestApiError> {
        debug!("placing order in backtest: side={side}, amount={amount}, price={price:?}");
        if pair != self.pair {
            warn!(
                "placing order for pair {pair} while simulator configured for {}",
                self.pair
            );
        }

        let amount = Decimal::from_str(amount)?;
        let price = price
            .map(|p| Decimal::from_str(p))
            .transpose()?
            .ok_or(BacktestApiError::MissingPrice)?;

        let (side_enum, needed, available_bucket) = if side == "buy" {
            (
                OrderSide::Buy,
                price * amount,
                self.quote_free, // read copy
            )
        } else {
            (
                OrderSide::Sell,
                amount,
                self.base_free, // read copy
            )
        };

        if available_bucket < needed {
            return Err(BacktestApiError::InsufficientFunds {
                side: if matches!(side_enum, OrderSide::Buy) {
                    "buy"
                } else {
                    "sell"
                },
                needed,
                available: available_bucket,
            });
        }

        match side_enum {
            OrderSide::Buy => {
                self.quote_free -= needed;
                self.quote_locked += needed;
            }
            OrderSide::Sell => {
                self.base_free -= needed;
                self.base_locked += needed;
            }
        }

        let order = SimulatedOrder {
            id: self.next_order_id,
            side: side_enum,
            price,
            amount,
            remaining: amount,
        };
        self.next_order_id += 1;
        self.metrics.total_orders += 1;
        self.open_orders.insert(order.id, order.clone());

        Ok(BitbankCreateOrderResponse {
            order_id: serde_json::Number::from(order.id),
            pair: pair.to_string(),
            side: side.to_string(),
            position_side: None,
            r#type: "limit".to_string(),
            start_amount: Some(amount.to_string()),
            remaining_amount: Some(amount.to_string()),
            executed_amount: "0".to_string(),
            price: Some(price.to_string()),
            post_only: Some(post_only),
            user_cancelable: true,
            average_price: price.to_string(),
            ordered_at: serde_json::Number::from(0u64),
            expire_at: None,
            trigger_price: None,
            status: "UNFILLED".to_string(),
        })
    }

    fn cancel_orders(
        &mut self,
        pair: &str,
        order_ids: &[u64],
    ) -> Result<BitbankCancelOrdersResponse, BacktestApiError> {
        if pair != self.pair {
            warn!(
                "cancel orders called for pair {pair}, simulator configured for {}",
                self.pair
            );
        }
        let mut responses = Vec::new();

        for &order_id in order_ids {
            if let Some(order) = self.open_orders.remove(&order_id) {
                match order.side {
                    OrderSide::Buy => {
                        let locked = order.price * order.remaining;
                        self.quote_locked -= locked;
                        self.quote_free += locked;
                    }
                    OrderSide::Sell => {
                        self.base_locked -= order.remaining;
                        self.base_free += order.remaining;
                    }
                }

                responses.push(BitbankCancelOrderResponse {
                    order_id: serde_json::Number::from(order_id),
                    pair: self.pair.clone(),
                    side: match order.side {
                        OrderSide::Buy => "buy".to_string(),
                        OrderSide::Sell => "sell".to_string(),
                    },
                    position_side: None,
                    r#type: "limit".to_string(),
                    start_amount: Some(order.amount.to_string()),
                    remaining_amount: Some(order.remaining.to_string()),
                    executed_amount: "0".to_string(),
                    price: Some(order.price.to_string()),
                    post_only: Some(true),
                    user_cancelable: true,
                    average_price: order.price.to_string(),
                    ordered_at: serde_json::Number::from(0u64),
                    expire_at: None,
                    canceled_at: Some(serde_json::Number::from(0u64)),
                    triggered_at: None,
                    trigger_price: None,
                    status: "CANCELED_UNFILLED".to_string(),
                });
            } else {
                return Err(BacktestApiError::UnknownOrder { order_id });
            }
        }

        Ok(BitbankCancelOrdersResponse { orders: responses })
    }

    fn build_active_orders_response(
        &mut self,
        pair: &str,
    ) -> Result<BitbankActiveOrdersResponse, BacktestApiError> {
        if pair != self.pair {
            warn!(
                "get_active_orders requested for pair {pair} while simulator configured for {}",
                self.pair
            );
        }

        let orders = self
            .open_orders
            .values()
            .map(|order| BitbankGetOrderResponse {
                order_id: serde_json::Number::from(order.id),
                pair: self.pair.clone(),
                side: match order.side {
                    OrderSide::Buy => "buy".to_string(),
                    OrderSide::Sell => "sell".to_string(),
                },
                position_side: None,
                r#type: "limit".to_string(),
                start_amount: Some(order.amount.to_string()),
                remaining_amount: Some(order.remaining.to_string()),
                executed_amount: (order.amount - order.remaining).to_string(),
                price: Some(order.price.to_string()),
                post_only: Some(true),
                user_cancelable: true,
                average_price: order.price.to_string(),
                ordered_at: serde_json::Number::from(0u64),
                expire_at: None,
                trigger_price: None,
                status: "UNFILLED".to_string(),
            })
            .collect();

        Ok(BitbankActiveOrdersResponse { orders })
    }

    fn build_assets_snapshot(&mut self) -> BitbankAssetsData {
        let mut assets = Vec::new();
        let base_asset = self.pair.split('_').next().unwrap_or("base");

        assets.push(BitbankAssetDatum {
            asset: base_asset.to_string(),
            free_amount: self.base_free.to_string(),
            amount_precision: serde_json::Number::from(8),
            onhand_amount: (self.base_free + self.base_locked).to_string(),
            locked_amount: self.base_locked.to_string(),
            withdrawing_amount: "0".to_string(),
            withdrawal_fee: Value::Null,
            stop_deposit: false,
            stop_withdrawal: false,
            network_list: None,
            collateral_ratio: "1".to_string(),
        });

        assets.push(BitbankAssetDatum {
            asset: "jpy".to_string(),
            free_amount: self.quote_free.to_string(),
            amount_precision: serde_json::Number::from(0),
            onhand_amount: (self.quote_free + self.quote_locked).to_string(),
            locked_amount: self.quote_locked.to_string(),
            withdrawing_amount: "0".to_string(),
            withdrawal_fee: Value::Null,
            stop_deposit: false,
            stop_withdrawal: false,
            network_list: None,
            collateral_ratio: "1".to_string(),
        });

        BitbankAssetsData { assets }
    }

    fn on_depth(&mut self, depth: &BitbankDepth) {
        if !depth.is_complete() {
            return;
        }

        let best_ask = depth.best_ask().map(|(price, _)| price.clone());
        let best_bid = depth.best_bid().map(|(price, _)| price.clone());

        if let (Some(ask), Some(bid)) = (best_ask.clone(), best_bid.clone()) {
            let mid = (ask + bid) / Decimal::from(2);
            self.last_mid = Some(mid);
            let equity = self.current_equity(mid);
            self.metrics.register_equity(equity);
        }

        let mut filled_orders = Vec::new();

        for order in self.open_orders.values() {
            match order.side {
                OrderSide::Buy => {
                    if let Some(ref ask) = best_ask {
                        if *ask <= order.price {
                            filled_orders.push((order.id, order.price));
                        }
                    }
                }
                OrderSide::Sell => {
                    if let Some(ref bid) = best_bid {
                        if *bid >= order.price {
                            filled_orders.push((order.id, order.price));
                        }
                    }
                }
            }
        }

        for (order_id, fill_price) in filled_orders {
            self.fill_order(order_id, fill_price, true);
        }
    }

    fn on_transactions(&mut self, transactions: &[BitbankTransactionDatum]) {
        if transactions.is_empty() {
            return;
        }

        let last_trade_price = transactions.last().map(|t| t.price);
        if let Some(mid_price) = last_trade_price {
            if self.last_mid.is_none() {
                self.last_mid = Some(mid_price);
            }
            let equity = self.current_equity(mid_price);
            self.metrics.register_equity(equity);
        }
    }

    fn current_equity(&self, mid_price: Decimal) -> Decimal {
        let base_total = self.base_free + self.base_locked;
        let quote_total = self.quote_free + self.quote_locked;
        quote_total + base_total * mid_price
    }

    fn fill_order(&mut self, order_id: u64, fill_price: Decimal, maker: bool) {
        if let Some(order) = self.open_orders.remove(&order_id) {
            let qty = order.remaining;
            match order.side {
                OrderSide::Buy => {
                    let locked = order.price * qty;
                    self.quote_locked -= locked;
                    if self.quote_locked < Decimal::ZERO {
                        self.quote_locked = Decimal::ZERO;
                    }
                    let fee_rate = if maker {
                        self.maker_fee_rate
                    } else {
                        self.taker_fee_rate
                    };
                    let fee = fill_price * qty * fee_rate;
                    self.metrics.total_fees += fee;
                    self.base_free += qty;
                    if self.quote_free >= fee {
                        self.quote_free -= fee;
                    } else {
                        self.quote_free = Decimal::ZERO;
                    }
                }
                OrderSide::Sell => {
                    self.base_locked -= qty;
                    let proceeds = fill_price * qty;
                    if self.base_locked < Decimal::ZERO {
                        self.base_locked = Decimal::ZERO;
                    }
                    let fee_rate = if maker {
                        self.maker_fee_rate
                    } else {
                        self.taker_fee_rate
                    };
                    let fee = proceeds * fee_rate;
                    self.metrics.total_fees += fee;
                    self.quote_free += proceeds;
                    if self.quote_free >= fee {
                        self.quote_free -= fee;
                    } else {
                        self.quote_free = Decimal::ZERO;
                    }
                }
            }
            self.metrics.trade_count += 1;
            if let Some(mid) = self.last_mid {
                let equity = self.current_equity(mid);
                self.metrics.register_equity(equity);
            }
        }
    }
}

struct BacktestRecord {
    timestamp_micros: u64,
    message: BitbankInboundMessage,
}

fn scale_duration(base: Duration, speed: f64) -> Duration {
    if speed <= 0.0 {
        return base;
    }
    if base.is_zero() {
        return base;
    }
    let nanos = (base.as_nanos() as f64 / speed).max(0.0);
    let nanos = nanos.round() as u128;
    if nanos == 0 {
        Duration::from_nanos(0)
    } else if nanos > u64::MAX as u128 {
        Duration::from_nanos(u64::MAX)
    } else {
        Duration::from_nanos(nanos as u64)
    }
}

fn parse_capture_line(
    value: Value,
    pair_suffix: &str,
) -> Result<Option<BacktestRecord>, BacktestError> {
    let room_name = value
        .get("room_name")
        .and_then(Value::as_str)
        .ok_or_else(|| BacktestError::MissingPayload {
            room_name: "<missing>".to_string(),
        })?;

    if !room_name.ends_with(pair_suffix) {
        return Ok(None);
    }

    let timestamp_micros = value
        .get("received_at_micros")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let data_key = format!("data_{room_name}");
    let payload = value
        .get(&data_key)
        .ok_or_else(|| BacktestError::MissingPayload {
            room_name: room_name.to_string(),
        })?
        .clone();

    let message = if room_name.starts_with("depth_whole_") {
        let depth: BitbankDepthWhole =
            serde_json::from_value(payload).map_err(|source| BacktestError::Parse {
                line: timestamp_micros as usize,
                source,
            })?;
        BitbankInboundMessage::DepthWhole(depth)
    } else if room_name.starts_with("depth_diff_") {
        let diff: BitbankDepthDiff =
            serde_json::from_value(payload).map_err(|source| BacktestError::Parse {
                line: timestamp_micros as usize,
                source,
            })?;
        BitbankInboundMessage::DepthDiff(diff)
    } else if room_name.starts_with("transactions_") {
        let txs: BitbankTransactionsData =
            serde_json::from_value(payload).map_err(|source| BacktestError::Parse {
                line: timestamp_micros as usize,
                source,
            })?;
        BitbankInboundMessage::Transactions(txs.transactions)
    } else if room_name.starts_with("ticker_") {
        let ticker: BitbankTickerResponse =
            serde_json::from_value(payload).map_err(|source| BacktestError::Parse {
                line: timestamp_micros as usize,
                source,
            })?;
        BitbankInboundMessage::Ticker(ticker)
    } else if room_name.starts_with("circuit_break_info_") {
        let info: BitbankCircuitBreakInfo =
            serde_json::from_value(payload).map_err(|source| BacktestError::Parse {
                line: timestamp_micros as usize,
                source,
            })?;
        BitbankInboundMessage::CircuitBreakInfo(info)
    } else {
        return Err(BacktestError::UnsupportedRoom {
            room_name: room_name.to_string(),
        });
    };

    Ok(Some(BacktestRecord {
        timestamp_micros,
        message,
    }))
}

fn load_records(path: &Path, pair: &str) -> Result<Vec<BacktestRecord>, BacktestError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let json_value: Value =
            serde_json::from_str(&line).map_err(|source| BacktestError::Parse {
                line: idx + 1,
                source,
            })?;
        if let Some(record) = parse_capture_line(json_value, pair)? {
            records.push(record);
        }
    }

    records.sort_by_key(|record| record.timestamp_micros);
    Ok(records)
}

#[derive(Debug, Clone)]
pub struct BacktestReport {
    pub total_orders: usize,
    pub filled_trades: usize,
    pub total_fees: Decimal,
    pub final_pnl: Decimal,
    pub max_drawdown: Decimal,
    pub ending_base: Decimal,
    pub ending_quote: Decimal,
}

pub struct BacktestEngine {
    pair: String,
    records: Vec<BacktestRecord>,
    api: SimulatedBitbankApi,
    state: std::sync::Arc<Mutex<SimulatedState>>,
}

impl BacktestEngine {
    pub fn new(config: BacktestConfig) -> Result<Self, BacktestError> {
        let pair_suffix = config.pair.clone();
        let records = load_records(&config.data_path, &pair_suffix)?;
        let state = std::sync::Arc::new(Mutex::new(SimulatedState::new(&config)));
        let api = SimulatedBitbankApi::new(state.clone(), &config);

        Ok(Self {
            pair: config.pair,
            records,
            api,
            state,
        })
    }

    pub fn api_client(&self) -> SimulatedBitbankApi {
        self.api.clone()
    }

    pub async fn run<S>(&self, mut strategy: S) -> Result<BacktestReport, BacktestError>
    where
        S: BotStrategy<Event = BitbankEvent>,
    {
        let (ctx_tx, mut ctx_rx) = mpsc::channel::<BitbankEvent>(128);
        let ctx = BotContext::new(ctx_tx);
        let mut depth = BitbankDepth::new();
        let mut pending_events = VecDeque::new();

        for record in &self.records {
            let maybe_event =
                Self::process_inbound(&self.pair, &mut depth, &record.message, &self.state).await;

            if let Some(event) = maybe_event {
                pending_events.push_back(event);
            }

            while let Some(event) = pending_events.pop_front() {
                strategy.handle_event(event.clone(), &ctx).await;
                while let Ok(extra_event) = ctx_rx.try_recv() {
                    pending_events.push_back(extra_event);
                }
            }
        }

        while let Ok(extra_event) = ctx_rx.try_recv() {
            pending_events.push_back(extra_event);
        }

        while let Some(event) = pending_events.pop_front() {
            strategy.handle_event(event.clone(), &ctx).await;
        }

        let guard = self.state.lock().await;
        let metrics = guard.metrics.clone();
        let final_mid = guard
            .last_mid
            .unwrap_or_else(|| Decimal::from_str("1").unwrap());
        let final_equity = guard.current_equity(final_mid);
        let initial_equity = metrics.initial_equity.unwrap_or(final_equity);
        let final_pnl = final_equity - initial_equity;

        Ok(BacktestReport {
            total_orders: metrics.total_orders,
            filled_trades: metrics.trade_count,
            total_fees: metrics.total_fees,
            final_pnl,
            max_drawdown: metrics.max_drawdown,
            ending_base: guard.base_free + guard.base_locked,
            ending_quote: guard.quote_free + guard.quote_locked,
        })
    }

    async fn process_inbound(
        pair: &str,
        depth: &mut BitbankDepth,
        message: &BitbankInboundMessage,
        state: &std::sync::Arc<Mutex<SimulatedState>>,
    ) -> Option<BitbankEvent> {
        match message {
            BitbankInboundMessage::Ticker(ticker) => Some(BitbankEvent::Ticker {
                pair: pair.to_string(),
                ticker: ticker.clone(),
            }),
            BitbankInboundMessage::Transactions(transactions) => {
                {
                    let mut guard = state.lock().await;
                    guard.on_transactions(transactions);
                }
                Some(BitbankEvent::Transactions {
                    pair: pair.to_string(),
                    transactions: transactions.clone(),
                })
            }
            BitbankInboundMessage::DepthWhole(whole) => {
                depth.update_whole(whole.clone());
                if depth.is_complete() {
                    {
                        let mut guard = state.lock().await;
                        guard.on_depth(depth);
                    }
                    Some(BitbankEvent::DepthUpdated {
                        pair: pair.to_string(),
                        depth: depth.clone(),
                    })
                } else {
                    None
                }
            }
            BitbankInboundMessage::DepthDiff(diff) => {
                depth.insert_diff(diff.clone());
                if depth.is_complete() {
                    {
                        let mut guard = state.lock().await;
                        guard.on_depth(depth);
                    }
                    Some(BitbankEvent::DepthUpdated {
                        pair: pair.to_string(),
                        depth: depth.clone(),
                    })
                } else {
                    None
                }
            }
            BitbankInboundMessage::CircuitBreakInfo(info) => Some(BitbankEvent::CircuitBreakInfo {
                pair: pair.to_string(),
                info: info.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_duration_respects_speed() {
        let base = Duration::from_millis(1000);
        let scaled = scale_duration(base, 100.0);
        assert!(scaled <= Duration::from_millis(20));
    }
}
