//! The bots' settings: kept in the journal, changed from the control page.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::strategy::{MAX_COVER, MIN_DURATION};

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// The bots, in order. A bot's place here is its key index, `m/…/0/n`:
/// append new bots, never reorder or remove one, or a bot would take
/// another's address. Its ID keys the journal; its name is only the default
/// display name, which the control page can change.
pub const ROSTER: [(&str, &str, Kind); 3] = [
    ("alice", "Alice", Kind::Clock),
    ("bob", "Bob", Kind::Minimax),
    ("charlie", "Charlie", Kind::Farmer),
];

pub fn ids() -> impl Iterator<Item = &'static str> {
    ROSTER.iter().map(|(id, _, _)| *id)
}

/// What a bot does.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Clock,
    Minimax,
    Farmer,
}

/// Each bot's settings, by ID.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Config(pub BTreeMap<String, Settings>);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "strategy", rename_all = "snake_case", deny_unknown_fields)]
pub enum Settings {
    Clock(Clock),
    Minimax(Minimax),
    Farmer(Farmer),
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

/// A service at a low commission that lives on what its delegators pay,
/// open while there are players its offer is for.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Farmer {
    pub enabled: bool,
    pub name: String,
    pub commission_bps: i64,
    pub burn_per_block: i64,
    /// The shortest contribution it makes; longer ones reach the end of
    /// the delegations to it.
    pub duration: i64,
    /// Extends its service once its end is this close.
    pub lookahead_blocks: i64,
    /// Opens once this many players other than the bots mint Independent
    /// below its break-even burn: those who would gain by delegating.
    pub min_players: i64,
    /// Stays open this many blocks after they leave.
    pub linger_blocks: i64,
}

impl Kind {
    fn defaults(self, name: &str, now: i64) -> Settings {
        let name = name.to_string();
        match self {
            Self::Clock => Settings::Clock(Clock {
                enabled: false,
                name,
                burn_per_block: 1,
                duration: MIN_DURATION,
                lookahead_blocks: 30,
            }),
            Self::Minimax => Settings::Minimax(Minimax {
                enabled: false,
                name,
                commission_bps: 500,
                budget_sats: 1_000_000_000,
                starts_at: now,
                ends_at: now + 7 * 86_400,
                lookahead_blocks: 600,
            }),
            Self::Farmer => Settings::Farmer(Farmer {
                enabled: false,
                name,
                commission_bps: 100,
                burn_per_block: 1,
                // Short, so that little runs on after it closes.
                duration: MIN_DURATION,
                lookahead_blocks: 60,
                min_players: 2,
                linger_blocks: 3_600,
            }),
        }
    }
}

impl Settings {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Clock(_) => Kind::Clock,
            Self::Minimax(_) => Kind::Minimax,
            Self::Farmer(_) => Kind::Farmer,
        }
    }

    pub fn enabled(&self) -> bool {
        match self {
            Self::Clock(clock) => clock.enabled,
            Self::Minimax(minimax) => minimax.enabled,
            Self::Farmer(farmer) => farmer.enabled,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Clock(clock) => &clock.name,
            Self::Minimax(minimax) => &minimax.name,
            Self::Farmer(farmer) => &farmer.name,
        }
    }

    /// A service's rate, registered while it is enabled.
    pub fn commission_bps(&self) -> Option<i64> {
        match self {
            Self::Clock(_) => None,
            Self::Minimax(minimax) => Some(minimax.commission_bps),
            Self::Farmer(farmer) => Some(farmer.commission_bps),
        }
    }

    /// Its contributions' mode.
    pub fn mode(&self) -> &'static str {
        match self {
            Self::Clock(_) => "independent",
            Self::Minimax(_) | Self::Farmer(_) => "service",
        }
    }

    fn validate(&self) -> Result<(), String> {
        let name = self.name();
        if name.trim() != name
            || !(1..=64).contains(&name.chars().count())
            || name.chars().any(char::is_control)
        {
            return Err(format!(
                "Name {name:?} must have 1–64 characters, no control characters and no surrounding spaces."
            ));
        }
        if self
            .commission_bps()
            .is_some_and(|bps| !(0..=10_000).contains(&bps))
        {
            return Err(format!(
                "{name}: commission must be 0 to 10,000 basis points."
            ));
        }
        match self {
            Self::Clock(clock) => {
                if !(1..=1_000_000_000).contains(&clock.burn_per_block)
                    || !(MIN_DURATION..=100_000).contains(&clock.duration)
                {
                    return Err(format!(
                        "{name}: burn must be 1 to 1,000,000,000 sat a block and duration {MIN_DURATION} to 100,000 blocks."
                    ));
                }
                // Over the lookahead, a gap could open and close unseen between two polls.
                if !(10..=10_000).contains(&clock.lookahead_blocks) {
                    return Err(format!("{name}: lookahead must be 10 to 10,000 blocks."));
                }
            }
            Self::Minimax(minimax) => {
                if !(0..=MAX_SAFE_INTEGER).contains(&minimax.budget_sats) {
                    return Err(format!("{name}: budget must be 0 to 2^53 - 1 sats."));
                }
                if !(0..=4_102_444_800).contains(&minimax.starts_at)
                    || minimax.ends_at <= minimax.starts_at
                    || minimax.ends_at > 4_102_444_800
                {
                    return Err(format!(
                        "{name}: the window must end after it starts, before 2100."
                    ));
                }
                if !(0..=10_000).contains(&minimax.lookahead_blocks) {
                    return Err(format!("{name}: lookahead must be 0 to 10,000 blocks."));
                }
            }
            Self::Farmer(farmer) => {
                if !(1..=1_000_000_000).contains(&farmer.burn_per_block)
                    || !(MIN_DURATION..=MAX_COVER).contains(&farmer.duration)
                {
                    return Err(format!(
                        "{name}: burn must be 1 to 1,000,000,000 sat a block and duration {MIN_DURATION} to {MAX_COVER} blocks."
                    ));
                }
                // A delegation starting now would burn ineligibly until the service resumes.
                if !(10..=10_000).contains(&farmer.lookahead_blocks) {
                    return Err(format!("{name}: lookahead must be 10 to 10,000 blocks."));
                }
                if !(1..=1_000).contains(&farmer.min_players)
                    || !(0..=100_000).contains(&farmer.linger_blocks)
                {
                    return Err(format!(
                        "{name}: players must be 1 to 1,000 and lingering 0 to 100,000 blocks."
                    ));
                }
            }
        }
        Ok(())
    }
}

impl Config {
    /// Every bot off, with its default settings; the minimax bot's budget
    /// spread over the week from `now`.
    pub fn new(now: i64) -> Self {
        Self(
            ROSTER
                .iter()
                .map(|(id, name, kind)| (id.to_string(), kind.defaults(name, now)))
                .collect(),
        )
    }

    /// Settings from an older roster keep what still fits and take defaults
    /// for new bots.
    pub fn adopt(mut self, now: i64) -> Self {
        let defaults = Self::new(now);
        self.0.retain(|id, settings| {
            defaults
                .0
                .get(id)
                .is_some_and(|default| default.kind() == settings.kind())
        });
        for (id, default) in defaults.0 {
            self.0.entry(id).or_insert(default);
        }
        self
    }

    pub fn bot(&self, id: &str) -> &Settings {
        &self.0[id]
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.0.len() != ROSTER.len() {
            return Err(format!(
                "Settings must cover exactly the bots {:?}.",
                ids().collect::<Vec<_>>()
            ));
        }
        for (id, _, kind) in ROSTER {
            let settings = self
                .0
                .get(id)
                .ok_or_else(|| format!("Settings for {id} are missing."))?;
            if settings.kind() != kind {
                return Err(format!("{id} is a {kind:?} bot."));
            }
            settings.validate()?;
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
        assert_eq!(config.bot("alice").name(), "Alice");
        let invalid: [fn(&mut Config); 18] = [
            |c| set_clock(c, |s| s.name = " Clock".into()),
            |c| set_minimax(c, |s| s.name = String::new()),
            |c| set_minimax(c, |s| s.name = "a\nb".into()),
            |c| set_minimax(c, |s| s.name = "x".repeat(65)),
            |c| set_clock(c, |s| s.burn_per_block = 0),
            |c| set_clock(c, |s| s.duration = 143),
            |c| set_clock(c, |s| s.lookahead_blocks = 9),
            |c| set_minimax(c, |s| s.commission_bps = 10_001),
            |c| set_minimax(c, |s| s.budget_sats = -1),
            |c| set_minimax(c, |s| s.ends_at = s.starts_at),
            |c| set_minimax(c, |s| s.lookahead_blocks = 10_001),
            |c| set_farmer(c, |s| s.commission_bps = -1),
            |c| set_farmer(c, |s| s.duration = MAX_COVER + 1),
            |c| set_farmer(c, |s| s.lookahead_blocks = 9),
            |c| set_farmer(c, |s| s.min_players = 0),
            |c| set_farmer(c, |s| s.linger_blocks = -1),
            |c| {
                c.0.remove("charlie");
            },
            |c| {
                let alice = c.0["alice"].clone();
                c.0.insert("charlie".into(), alice);
            },
        ];
        for change in invalid {
            let mut config = config.clone();
            change(&mut config);
            assert!(config.validate().is_err(), "{config:?}");
        }
        let mut unicode = config.clone();
        set_minimax(&mut unicode, |s| s.name = "ж".repeat(64));
        assert_eq!(unicode.validate(), Ok(()));
    }

    #[test]
    fn settings_are_tagged_by_strategy_and_older_rosters_are_adopted() {
        let config = Config::new(0);
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["charlie"]["strategy"], "farmer");
        assert_eq!(
            serde_json::from_value::<Config>(json.clone()).unwrap(),
            config
        );
        let mut extra = json.clone();
        extra["alice"]["budget_sats"] = 1.into();
        assert!(serde_json::from_value::<Config>(extra).is_err());

        // A saved roster without Charlie, and with a bot since removed.
        let mut old = json;
        old.as_object_mut().unwrap().remove("charlie");
        old["dave"] = old["alice"].clone();
        old["bob"]["name"] = "Robert".into();
        let adopted = serde_json::from_value::<Config>(old).unwrap().adopt(0);
        assert_eq!(adopted.validate(), Ok(()));
        assert_eq!(adopted.bot("bob").name(), "Robert");
        assert_eq!(adopted.bot("charlie"), config.bot("charlie"));
    }

    fn set_clock(config: &mut Config, change: impl FnOnce(&mut Clock)) {
        match config.0.get_mut("alice") {
            Some(Settings::Clock(clock)) => change(clock),
            _ => unreachable!(),
        }
    }

    fn set_minimax(config: &mut Config, change: impl FnOnce(&mut Minimax)) {
        match config.0.get_mut("bob") {
            Some(Settings::Minimax(minimax)) => change(minimax),
            _ => unreachable!(),
        }
    }

    fn set_farmer(config: &mut Config, change: impl FnOnce(&mut Farmer)) {
        match config.0.get_mut("charlie") {
            Some(Settings::Farmer(farmer)) => change(farmer),
            _ => unreachable!(),
        }
    }
}
