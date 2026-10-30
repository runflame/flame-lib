//! The coin list: the coins the DEX knows by their tag alone.
//!
//! Anyone can deploy a coin under any tag with their own key, so a tag
//! names a coin only through a list someone vouches for. This one is
//! `coins.json` beside this crate, changed by pull request; the client is
//! built with it and the web page reads it from GitHub. Every entry's ids
//! and flavors are derived again from its tag and issuer before it is
//! used, here and in this crate's tests, so a listing can be wrong only
//! about which coin deserves its tag.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::client::Result;
use crate::Coin;

/// The list this client was built with.
pub const BUILTIN: &str = include_str!("../coins.json");

#[derive(Serialize, Deserialize)]
pub struct List {
    pub name: String,
    /// The network the coins live on.
    pub network: String,
    pub coins: Vec<Listing>,
}

/// One listed coin. `symbol` and `issuer` name it; the rest follows from
/// them and is checked.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Listing {
    pub symbol: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The issuer's key, hex.
    pub issuer: String,
    pub actor: String,
    pub flavor: String,
    pub pool: String,
    pub lp_flavor: String,
}

impl List {
    /// The list at `path`, or the built-in one; refused unless it checks.
    pub fn load(path: Option<&Path>) -> Result<List> {
        let (text, from) = match path {
            Some(p) => (
                std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
                p.display().to_string(),
            ),
            None => (BUILTIN.to_string(), "the built-in coin list".to_string()),
        };
        let list: List = serde_json::from_str(&text).map_err(|e| format!("{from}: {e}"))?;
        list.check().map_err(|e| format!("{from}: {e}"))?;
        Ok(list)
    }

    /// Every symbol is listed once, and every entry is what its symbol and
    /// issuer derive.
    pub fn check(&self) -> Result<()> {
        for (i, l) in self.coins.iter().enumerate() {
            if self.coins[..i].iter().any(|o| o.symbol == l.symbol) {
                return Err(format!("{} is listed twice", l.symbol).into());
            }
            let derived = Coin::new(&l.symbol, crate::key(&l.issuer)?)?.listing(&l.name)?;
            let derived = Listing {
                description: l.description.clone(),
                ..derived
            };
            if &derived != l {
                return Err(format!(
                    "{} does not match its symbol and issuer: it should read {}",
                    l.symbol,
                    serde_json::to_string_pretty(&derived)?
                )
                .into());
            }
        }
        Ok(())
    }

    pub fn find(&self, symbol: &str) -> Option<&Listing> {
        self.coins.iter().find(|l| l.symbol == symbol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_list_checks() {
        List::load(None).unwrap();
    }

    #[test]
    fn a_wrong_entry_is_refused() {
        let coin = Coin::new("EMBER", [7; 32]).unwrap();
        let mut list = List {
            name: "test".into(),
            network: "devnet".into(),
            coins: vec![coin.listing("Ember").unwrap()],
        };
        list.check().unwrap();
        list.coins[0].pool = list.coins[0].actor.clone();
        assert!(list.check().is_err());
        list.coins[0] = coin.listing("Ember").unwrap();
        list.coins.push(coin.listing("Ember again").unwrap());
        assert!(list.check().is_err());
    }
}
