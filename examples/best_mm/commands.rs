//! async版と同じ判断・発注順を使う、応答待ちを明示した戦略。
use super::*;
use bitbankutil_rs::event_strategy::{
    Command, EventStrategy, Request, ResponseValue, StrategyEvent,
};

pub(super) struct CommandBot {
    bot: MyBot,
    waiting: usize,
    second: Vec<DesiredLimitOrder>,
    next_id: u64,
}
impl CommandBot {
    pub(super) fn new(bot: MyBot) -> Self {
        Self {
            bot,
            waiting: 0,
            second: Vec::new(),
            next_id: 0,
        }
    }
    fn request(&mut self, command: Command, out: &mut Vec<Request>) -> Result<(), String> {
        self.next_id = self.next_id.checked_add(1).ok_or("command id overflow")?;
        out.push(Request {
            id: self.next_id,
            command,
        });
        self.waiting += 1;
        Ok(())
    }
}
impl EventStrategy for CommandBot {
    type CustomEvent = std::convert::Infallible;
    fn on_event(
        &mut self,
        event: StrategyEvent<Self::CustomEvent>,
        now: i64,
        out: &mut Vec<Request>,
    ) -> Result<bool, String> {
        match event {
            StrategyEvent::Input(event) => {
                let evaluate = match event {
                    BotEvent::Market(MarketEvent::DepthUpdated { depth, .. }) => {
                        self.bot.depth = depth;
                        self.bot.depth.is_complete()
                    }
                    BotEvent::Market(MarketEvent::Transactions { .. }) => true,
                    _ => false,
                };
                if evaluate
                    && self.bot.depth.is_complete()
                    && now as u128 - self.bot.last_updated >= self.bot.bot_config.refresh_cycle
                {
                    self.bot.last_updated = now as u128;
                    self.request(
                        Command::Account {
                            pair: self.bot.bot_config.pair.clone(),
                        },
                        out,
                    )?;
                }
            }
            StrategyEvent::Response(response) => {
                self.waiting = self.waiting.checked_sub(1).ok_or("unexpected response")?;
                if let ResponseValue::Account(Ok(account)) = response.value {
                    if let Some((desired, orders, base_free, jpy_free)) = self
                        .bot
                        .orders_for_account(account)
                        .map_err(|e| e.to_string())?
                    {
                        let plan =
                            bitbankutil_rs::order_manager::plan_concurrent_orders_from_open_orders(
                                desired,
                                orders,
                                base_free,
                                jpy_free,
                                &self.bot.bot_config.pair,
                            )
                            .map_err(|e| e.to_string())?;
                        self.second = plan.second_placements.into_iter().collect();
                        if !plan.cancels.is_empty() {
                            self.request(
                                Command::Cancel {
                                    pair: self.bot.bot_config.pair.clone(),
                                    orders: plan.cancels,
                                },
                                out,
                            )?;
                        }
                        for order in plan.first_placements {
                            self.request(Command::Place(order), out)?;
                        }
                    }
                }
                if self.waiting == 0 {
                    for order in std::mem::take(&mut self.second) {
                        self.request(Command::Place(order), out)?;
                    }
                }
            }
        }
        Ok(self.waiting > 0)
    }
}
