#![allow(dead_code)]
//! 低水準kernel回帰テスト用の読み取り専用診断snapshot。
use bitbankutil_rs::{bitbank_bot::*, paper_execution::*};
use std::sync::{Arc, Mutex};
pub trait TestDiagnostics {
    fn engine(&self) -> Arc<Mutex<PaperDiagnosticSnapshot>>;
}
struct Noop;
impl BotStrategy<PaperTradingVenue> for Noop {
    type CustomEvent = ();
    type Error = std::convert::Infallible;
    async fn handle_event(
        &mut self,
        _: BotEvent<()>,
        _: &BotContext<(), PaperTradingVenue>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}
impl TestDiagnostics for PaperTradingVenue {
    fn engine(&self) -> Arc<Mutex<PaperDiagnosticSnapshot>> {
        Arc::new(Mutex::new(
            BotRunner::new(Noop, self.clone())
                .paper_diagnostics()
                .unwrap(),
        ))
    }
}
#[allow(dead_code)]
pub fn configure<V: bitbankutil_rs::trading_venue::TradingVenue>(venue: &V, lookahead_ms: i64) {
    if let Some(venue) = (venue as &dyn std::any::Any).downcast_ref::<PaperTradingVenue>() {
        venue
            .clone()
            .with_simulation_config(bitbankutil_rs::simulation::SimulationConfig { lookahead_ms })
            .unwrap();
    }
}
