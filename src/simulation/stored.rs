//! Owned, versioned disk representation of compiled events. Tracing spans are
//! diagnostics only and are recreated; no decoder or converter runs on restore.
use super::*;
use crate::market_event::{MarketCircuitBreakInfo, MarketTicker, StoredDepth};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
enum StoredEvent {
    Ticker {
        pair: String,
        ticker: MarketTicker,
    },
    Transactions {
        pair: String,
        transactions: Vec<MarketTrade>,
    },
    Depth {
        pair: String,
        depth: StoredDepth,
    },
    Circuit {
        pair: String,
        info: MarketCircuitBreakInfo,
    },
}
impl StoredEvent {
    fn encode(event: &MarketEvent, books: &mut BTreeMap<String, MarketDepthSnapshot>) -> Self {
        match event {
            MarketEvent::Ticker { pair, ticker } => Self::Ticker {
                pair: pair.clone(),
                ticker: ticker.clone(),
            },
            MarketEvent::Transactions { pair, transactions } => Self::Transactions {
                pair: pair.clone(),
                transactions: transactions.clone(),
            },
            MarketEvent::CircuitBreakInfo { pair, info } => Self::Circuit {
                pair: pair.clone(),
                info: info.clone(),
            },
            MarketEvent::DepthUpdated { pair, depth } => {
                let stored = depth.store(books.get(pair));
                books.insert(pair.clone(), depth.clone());
                Self::Depth {
                    pair: pair.clone(),
                    depth: stored,
                }
            }
        }
    }
    fn decode(
        self,
        books: &mut BTreeMap<String, MarketDepthSnapshot>,
    ) -> Result<MarketEvent, String> {
        Ok(match self {
            Self::Ticker { pair, ticker } => MarketEvent::Ticker { pair, ticker },
            Self::Transactions { pair, transactions } => {
                MarketEvent::Transactions { pair, transactions }
            }
            Self::Circuit { pair, info } => MarketEvent::CircuitBreakInfo { pair, info },
            Self::Depth { pair, depth } => {
                let depth = MarketDepthSnapshot::restore(depth, books.get(&pair))?;
                books.insert(pair.clone(), depth.clone());
                MarketEvent::DepthUpdated { pair, depth }
            }
        })
    }
}
#[derive(Serialize, Deserialize)]
struct StoredMarket {
    key: (i64, u64, u64),
    sequence: u64,
    id: Option<InputId>,
    received: i64,
    exchange: i64,
    event: Result<Option<StoredEvent>, String>,
}
#[derive(Serialize, Deserialize)]
enum StoredBotEvent<C> {
    Market(StoredEvent),
    Custom(C),
}
#[derive(Serialize, Deserialize)]
pub(crate) struct StoredEvents<C> {
    admissions: Vec<Option<PreparedAdmission>>,
    markets: Vec<StoredMarket>,
    deliveries: Vec<((i64, u64), Option<InputId>, StoredBotEvent<C>)>,
    issues: Vec<String>,
}
impl<C: Clone> PreparedEvents<C> {
    pub(crate) fn store(&self) -> Result<StoredEvents<C>, String> {
        let mut books = BTreeMap::new();
        let markets = self
            .markets
            .iter()
            .map(|m| {
                let MarketInput::Compiled(event) = &m.event.input else {
                    return Err("uncompiled disk block".into());
                };
                Ok(StoredMarket {
                    key: m.key,
                    sequence: m.event.input_sequence,
                    id: m.event.id.clone(),
                    received: m.event.received_at_ms,
                    exchange: m.event.exchange_at_ms,
                    event: event
                        .as_ref()
                        .map(|e| e.as_ref().map(|e| StoredEvent::encode(e, &mut books)))
                        .map_err(Clone::clone),
                })
            })
            .collect::<Result<_, String>>()?;
        // Delivery and exchange-order books have independent histories.
        books.clear();
        let deliveries = self
            .deliveries
            .iter()
            .map(|(key, d)| {
                (
                    *key,
                    d.id.clone(),
                    match &d.event {
                        BotEvent::Market(e) => {
                            StoredBotEvent::Market(StoredEvent::encode(e, &mut books))
                        }
                        BotEvent::Custom(c) => StoredBotEvent::Custom(c.clone()),
                    },
                )
            })
            .collect();
        Ok(StoredEvents {
            admissions: self.admissions.clone(),
            markets,
            deliveries,
            issues: self.issues.clone(),
        })
    }
}
impl<C> StoredEvents<C> {
    pub(crate) fn restore(self) -> Result<PreparedEvents<C>, String> {
        let mut books = BTreeMap::new();
        let markets = self
            .markets
            .into_iter()
            .map(|m| {
                let event = match m.event {
                    Ok(Some(e)) => Ok(Some(Arc::new(e.decode(&mut books)?))),
                    Ok(None) => Ok(None),
                    Err(e) => Err(e),
                };
                Ok(MarketEntry {
                    key: m.key,
                    event: InternalMarket {
                        source_span: tracing::Span::none(),
                        input_sequence: m.sequence,
                        id: m.id,
                        received_at_ms: m.received,
                        exchange_at_ms: m.exchange,
                        input: MarketInput::Compiled(event),
                    },
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        books.clear();
        let deliveries: Vec<_> = self
            .deliveries
            .into_iter()
            .map(|(key, id, event)| {
                Ok((
                    key,
                    Delivery {
                        source_span: tracing::Span::none(),
                        id,
                        event: match event {
                            StoredBotEvent::Market(e) => BotEvent::Market(e.decode(&mut books)?),
                            StoredBotEvent::Custom(c) => BotEvent::Custom(c),
                        },
                    },
                ))
            })
            .collect::<Result<_, String>>()?;
        // Reject corrupt indexes before they can reach admit_prepared().
        for a in self.admissions.iter().flatten() {
            for (range, len) in [
                (&a.markets, markets.len()),
                (&a.deliveries, deliveries.len()),
                (&a.issues, self.issues.len()),
            ] {
                if range.start > range.end || range.end > len {
                    return Err("invalid compiled admission range".into());
                }
            }
        }
        Ok(PreparedEvents {
            admissions: self.admissions,
            markets,
            deliveries,
            issues: self.issues,
        })
    }
}
