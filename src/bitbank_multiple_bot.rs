//! Backwards-compatible helpers for multi-pair bots.
//!
//! The original `MultiBotTrait` exposed in this module forced callers to thread
//! state manually through callback returns. The new actor-based runtime in
//! [`crate::bitbank_bot`] keeps strategy state on `self` and uses message
//! passing, which allows composing multiple information sources with minimal
//! boilerplate. This module now re-exports the relevant building blocks while
//! keeping ergonomics focused on the multi-pair Bitbank use case.

pub use crate::bitbank_bot::{
    BitbankBotBuilder as MultiBotBuilder, BitbankBotRuntime as MultiBotRuntime,
    BitbankEvent as MultiBotEvent, BotContext as MultiBotContext, BotHandle as MultiBotHandle,
    BotStrategy as MultiBotStrategyBase,
};

/// Strategies that consume Bitbank multi-pair events.
pub trait MultiBotStrategy: MultiBotStrategyBase<Event = MultiBotEvent> {}

impl<T> MultiBotStrategy for T where T: MultiBotStrategyBase<Event = MultiBotEvent> {}
