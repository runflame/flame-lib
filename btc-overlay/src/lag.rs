//! Per-client lag: a client can see the chain as it was some seconds ago.
//!
//! Clients are told apart by their RPC username. A client with a lag sees each tip change
//! only once the change is that old: the tip, heights, confirmations and `waitfornewblock`
//! all follow its delayed view, and blocks it has not seen yet are not found. Block contents
//! are the same for every client; only when they appear differs. Transactions sent by a
//! client are never delayed.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    path::Path,
    str::FromStr,
    time::{Duration, Instant},
};

use corepc_client::bitcoin::{
    BlockHash,
    hashes::{Hash, sha256},
};
use serde::{Deserialize, Deserializer};

use crate::overlay::Tip;

/// The longest lag a client can have; older tip changes are visible to everyone.
pub const MAX_LAG: Duration = Duration::from_secs(24 * 60 * 60);

/// A fixed delay, or a range the delay of each tip change is picked from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lag {
    min_secs: u64,
    max_secs: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum LagError {
    #[error("invalid lag {0:?}: expected seconds like 30, or a range like 10..120")]
    Invalid(String),
    #[error("lag {0} is above the maximum of {max} seconds", max = MAX_LAG.as_secs())]
    TooLong(Lag),
    #[error("cannot read the lag config: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid lag config: {0}")]
    Config(#[from] serde_json::Error),
}

impl Lag {
    pub fn fixed(secs: u64) -> Result<Self, LagError> {
        Self::range(secs, secs)
    }

    pub fn range(min_secs: u64, max_secs: u64) -> Result<Self, LagError> {
        if min_secs > max_secs {
            return Err(LagError::Invalid(format!("{min_secs}..{max_secs}")));
        }
        let lag = Self { min_secs, max_secs };
        if Duration::from_secs(max_secs) > MAX_LAG {
            return Err(LagError::TooLong(lag));
        }
        Ok(lag)
    }

    /// The delay of the tip change to `tip`. Within a range it is picked by the client name
    /// and the tip's hash, so it is the same every time it is asked for.
    pub fn delay(&self, client: &str, tip: &BlockHash) -> Duration {
        let spread = self.max_secs - self.min_secs;
        if spread == 0 {
            return Duration::from_secs(self.min_secs);
        }
        let mut data = client.as_bytes().to_vec();
        data.push(0);
        data.extend(tip.as_byte_array());
        let hash = sha256::Hash::hash(&data);
        let pick = u64::from_le_bytes(hash.as_byte_array()[..8].try_into().unwrap());
        Duration::from_secs(self.min_secs + pick % (spread + 1))
    }
}

impl fmt::Display for Lag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.min_secs == self.max_secs {
            write!(f, "{}", self.min_secs)
        } else {
            write!(f, "{}..{}", self.min_secs, self.max_secs)
        }
    }
}

impl FromStr for Lag {
    type Err = LagError;

    fn from_str(text: &str) -> Result<Self, LagError> {
        let invalid = || LagError::Invalid(text.into());
        let secs = |part: &str| part.trim().parse::<u64>().map_err(|_| invalid());
        match text.split_once("..") {
            Some((min, max)) => Self::range(secs(min)?, secs(max)?),
            None => Self::fixed(secs(text)?),
        }
    }
}

impl<'de> Deserialize<'de> for Lag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Given {
            Secs(u64),
            Text(String),
        }
        match Given::deserialize(deserializer)? {
            Given::Secs(secs) => Self::fixed(secs),
            Given::Text(text) => text.parse(),
        }
        .map_err(serde::de::Error::custom)
    }
}

/// Reads a JSON object of client names and lags: `{"node2": 30, "node3": "10..120"}`.
pub fn load(path: &Path) -> Result<HashMap<String, Lag>, LagError> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

struct Change {
    at: Instant,
    tip: Tip,
    /// Blocks first connected by this change.
    new_blocks: Vec<BlockHash>,
}

/// The tip changes of the last [`MAX_LAG`], from which each client's view is made.
pub(crate) struct Timeline {
    /// The tip before the first change kept; everyone sees it.
    base: Option<Tip>,
    changes: VecDeque<Change>,
}

/// The chain as one client sees it.
pub struct View {
    pub tip: Option<Tip>,
    /// Blocks connected by changes the client has not seen yet.
    hidden: HashSet<BlockHash>,
    /// When the client sees the next change.
    pub next: Option<Instant>,
}

impl View {
    pub fn shows(&self, hash: &BlockHash) -> bool {
        !self.hidden.contains(hash)
    }
}

impl Timeline {
    pub(crate) fn new(tip: Option<Tip>) -> Self {
        Self {
            base: tip,
            changes: VecDeque::new(),
        }
    }

    pub(crate) fn record(&mut self, at: Instant, tip: Tip, new_blocks: Vec<BlockHash>) {
        // The first blocks served are history every client has.
        if self.base.is_none() && self.changes.is_empty() {
            self.base = Some(tip);
            return;
        }
        self.changes.push_back(Change {
            at,
            tip,
            new_blocks,
        });
        while let Some(change) = self.changes.front() {
            if change.at + MAX_LAG > at {
                break;
            }
            self.base = Some(change.tip);
            self.changes.pop_front();
        }
    }

    /// The view of a client with `lag` at `now`, or the live one without a lag. A client sees
    /// changes in order: one still delayed holds back the later ones, as a node cannot see a
    /// block before its parent.
    pub(crate) fn view(&self, client: &str, lag: Option<&Lag>, now: Instant) -> View {
        let mut view = View {
            tip: self.base,
            hidden: HashSet::new(),
            next: None,
        };
        let mut changes = self.changes.iter();
        for change in changes.by_ref() {
            let visible_at =
                change.at + lag.map_or(Duration::ZERO, |lag| lag.delay(client, &change.tip.hash));
            if visible_at > now {
                view.next = Some(visible_at);
                view.hidden.extend(&change.new_blocks);
                break;
            }
            view.tip = Some(change.tip);
        }
        for change in changes {
            view.hidden.extend(&change.new_blocks);
        }
        view
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tip(byte: u8, height: u64) -> Tip {
        Tip {
            hash: BlockHash::from_byte_array([byte; 32]),
            height,
        }
    }

    #[test]
    fn parses_fixed_lags_and_ranges() {
        assert_eq!("30".parse::<Lag>().unwrap(), Lag::fixed(30).unwrap());
        assert_eq!(
            "10..120".parse::<Lag>().unwrap(),
            Lag::range(10, 120).unwrap()
        );
        assert_eq!(Lag::range(10, 120).unwrap().to_string(), "10..120");
        assert!("120..10".parse::<Lag>().is_err());
        assert!("soon".parse::<Lag>().is_err());
        assert!(matches!(
            Lag::fixed(MAX_LAG.as_secs() + 1),
            Err(LagError::TooLong(_))
        ));
        let config: HashMap<String, Lag> =
            serde_json::from_str(r#"{"node2": 30, "node3": "10..120"}"#).unwrap();
        assert_eq!(config["node2"], Lag::fixed(30).unwrap());
        assert_eq!(config["node3"], Lag::range(10, 120).unwrap());
    }

    #[test]
    fn a_range_picks_a_stable_delay_per_client_and_tip() {
        let lag = Lag::range(10, 120).unwrap();
        let delays: Vec<_> = (0..50)
            .map(|byte| lag.delay("node3", &tip(byte, 0).hash))
            .collect();
        assert!(
            delays
                .iter()
                .all(|delay| (10..=120).contains(&delay.as_secs()))
        );
        assert!(
            delays.iter().any(|delay| *delay != delays[0]),
            "the delay varies"
        );
        assert_eq!(lag.delay("node3", &tip(7, 0).hash), delays[7]);
        assert!(
            (0..50).any(|byte| lag.delay("node4", &tip(byte, 0).hash) != delays[byte as usize])
        );
    }

    #[test]
    fn a_lagging_client_sees_changes_in_order_once_they_are_old_enough() {
        let start = Instant::now();
        let secs = |secs| start + Duration::from_secs(secs);
        let mut timeline = Timeline::new(None);
        timeline.record(start, tip(1, 1), vec![tip(1, 1).hash]);
        timeline.record(secs(10), tip(2, 2), vec![tip(2, 2).hash]);
        timeline.record(secs(15), tip(3, 3), vec![tip(3, 3).hash]);
        let lag = Lag::fixed(10).unwrap();

        // The first blocks are history: everyone sees them at once.
        let view = timeline.view("slow", Some(&lag), secs(19));
        assert_eq!(view.tip, Some(tip(1, 1)));
        assert!(view.shows(&tip(1, 1).hash));
        assert!(!view.shows(&tip(2, 2).hash) && !view.shows(&tip(3, 3).hash));
        assert_eq!(view.next, Some(secs(20)));

        let view = timeline.view("slow", Some(&lag), secs(20));
        assert_eq!(view.tip, Some(tip(2, 2)));
        assert_eq!(view.next, Some(secs(25)));
        assert_eq!(
            timeline.view("slow", Some(&lag), secs(25)).tip,
            Some(tip(3, 3))
        );
        assert_eq!(timeline.view("slow", Some(&lag), secs(25)).next, None);

        let live = timeline.view("fast", None, secs(15));
        assert_eq!(live.tip, Some(tip(3, 3)));
        assert!(live.shows(&tip(3, 3).hash));
    }

    #[test]
    fn a_delayed_change_holds_back_later_ones() {
        let start = Instant::now();
        let mut timeline = Timeline::new(Some(tip(0, 0)));
        timeline.record(start, tip(1, 1), vec![tip(1, 1).hash]);
        timeline.record(
            start + Duration::from_secs(1),
            tip(2, 2),
            vec![tip(2, 2).hash],
        );
        let lag = Lag::range(0, 100).unwrap();
        // A client whose first change is delayed past the time its second one is due.
        let (client, first, second) = (0..100)
            .map(|index| format!("node{index}"))
            .map(|client| {
                let first = start + lag.delay(&client, &tip(1, 1).hash);
                let second = start + Duration::from_secs(1) + lag.delay(&client, &tip(2, 2).hash);
                (client, first, second)
            })
            .find(|(_, first, second)| first > second)
            .unwrap();
        let view = timeline.view(&client, Some(&lag), second);
        assert_eq!(view.tip, Some(tip(0, 0)));
        assert_eq!(view.next, Some(first));
        assert_eq!(
            timeline.view(&client, Some(&lag), first).tip,
            Some(tip(2, 2))
        );
    }

    #[test]
    fn changes_older_than_the_longest_lag_are_folded_into_the_base() {
        let start = Instant::now();
        let mut timeline = Timeline::new(Some(tip(0, 0)));
        timeline.record(start, tip(1, 1), vec![tip(1, 1).hash]);
        timeline.record(start + MAX_LAG, tip(2, 2), vec![tip(2, 2).hash]);
        assert_eq!(timeline.changes.len(), 1);
        assert_eq!(timeline.base, Some(tip(1, 1)));
    }
}
