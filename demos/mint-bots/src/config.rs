//! The bots' settings: kept in the journal, changed from the control page.

use serde::{Deserialize, Serialize};

use crate::strategy::MIN_DURATION;

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub clock: Clock,
    pub minimax: Minimax,
}

/// Fills blocks nobody else mints in, so their reward is not lost.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Clock {
    pub enabled: bool,
    pub name: String,
    pub burn_per_block: i64,
    pub duration: i64,
    /// Contributes once the next idle block is this close.
    pub lookahead_blocks: i64,
}

/// Spends a finite budget over a window of time, each part where it buys
/// the most FLM, as a service others can delegate to.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Minimax {
    pub enabled: bool,
    pub name: String,
    pub commission_bps: i64,
    pub budget_sats: i64,
    /// Unix seconds.
    pub starts_at: i64,
    pub ends_at: i64,
    /// How far ahead it may schedule a contribution.
    pub lookahead_blocks: i64,
}

impl Config {
    /// Both bots off; the minimax bot's budget spread over the week from `now`.
    pub fn new(now: i64) -> Self {
        Self {
            clock: Clock {
                enabled: false,
                name: "Clock bot".into(),
                burn_per_block: 1,
                duration: MIN_DURATION,
                lookahead_blocks: 30,
            },
            minimax: Minimax {
                enabled: false,
                name: "Minimax bot".into(),
                commission_bps: 500,
                budget_sats: 1_000_000_000,
                starts_at: now,
                ends_at: now + 7 * 86_400,
                lookahead_blocks: 600,
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let name = |name: &str| {
            let trimmed = name.trim();
            if trimmed != name
                || !(1..=64).contains(&name.chars().count())
                || name.chars().any(char::is_control)
            {
                return Err(format!(
                    "Name {name:?} must have 1–64 characters, no control characters and no surrounding spaces."
                ));
            }
            Ok(())
        };
        name(&self.clock.name)?;
        name(&self.minimax.name)?;
        let clock = &self.clock;
        if !(1..=1_000_000_000).contains(&clock.burn_per_block)
            || !(MIN_DURATION..=100_000).contains(&clock.duration)
        {
            return Err(format!(
                "Clock bot: burn must be 1 to 1,000,000,000 sat a block and duration {MIN_DURATION} to 100,000 blocks."
            ));
        }
        // Over the lookahead, a gap could open and close unseen between two polls.
        if !(10..=10_000).contains(&clock.lookahead_blocks) {
            return Err("Clock bot: lookahead must be 10 to 10,000 blocks.".into());
        }
        let minimax = &self.minimax;
        if !(0..=10_000).contains(&minimax.commission_bps) {
            return Err("Minimax bot: commission must be 0 to 10,000 basis points.".into());
        }
        if !(0..=MAX_SAFE_INTEGER).contains(&minimax.budget_sats) {
            return Err("Minimax bot: budget must be 0 to 2^53 - 1 sats.".into());
        }
        if !(0..=4_102_444_800).contains(&minimax.starts_at)
            || minimax.ends_at <= minimax.starts_at
            || minimax.ends_at > 4_102_444_800
        {
            return Err("Minimax bot: the window must end after it starts, before 2100.".into());
        }
        if !(0..=10_000).contains(&minimax.lookahead_blocks) {
            return Err("Minimax bot: lookahead must be 0 to 10,000 blocks.".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_each_limit_is_enforced() {
        let config = Config::new(1_800_000_000);
        assert_eq!(config.validate(), Ok(()));
        let invalid = [
            |c: &mut Config| c.clock.name = " Clock".into(),
            |c: &mut Config| c.minimax.name = String::new(),
            |c: &mut Config| c.minimax.name = "a\nb".into(),
            |c: &mut Config| c.minimax.name = "x".repeat(65),
            |c: &mut Config| c.clock.burn_per_block = 0,
            |c: &mut Config| c.clock.duration = 143,
            |c: &mut Config| c.clock.lookahead_blocks = 9,
            |c: &mut Config| c.minimax.commission_bps = 10_001,
            |c: &mut Config| c.minimax.budget_sats = -1,
            |c: &mut Config| c.minimax.ends_at = c.minimax.starts_at,
            |c: &mut Config| c.minimax.lookahead_blocks = 10_001,
        ];
        for change in invalid {
            let mut config = config.clone();
            change(&mut config);
            assert!(config.validate().is_err(), "{config:?}");
        }
        let mut unicode = config.clone();
        unicode.minimax.name = "ж".repeat(64);
        assert_eq!(unicode.validate(), Ok(()));
    }
}
