//! Watch-only wallets that never run dry.
//!
//! A minter's node wallet watches the minter's script, lists its confirmed coins and funds
//! endorsements from them. On the overlay, whenever a wallet lists its coins and a watched
//! script has fewer than [`COIN_FLOOR`] confirmed unspent ones, new coins are minted: made-up
//! outputs that count as confirmed since the start block. They are logged like everything
//! else, so blocks that spend them always show the same prevouts.

use std::collections::HashMap;

use corepc_client::bitcoin::{Amount, OutPoint, ScriptBuf, Transaction, TxOut, Txid, hashes::Hash};

use super::{Record, State, StateError, encode};

/// Confirmed unspent coins a watched script always has. A minter endorsing every live slot
/// at once needs 12.
pub const COIN_FLOOR: usize = 20;
/// Value of a minted coin; an endorsement costs about 200 sats in fees.
pub const COIN_SATS: u64 = 100_000;
/// Fee rate of `fundrawtransaction`, in sat/vB.
const FEE_RATE: u64 = 1;
/// Change below this goes to the fee, as Core does for P2WSH at its dust relay fee.
const DUST_SATS: u64 = 330;
/// Weight of an unsigned input: outpoint, empty script length and sequence.
const UNSIGNED_INPUT_WEIGHT: u64 = 41 * 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coin {
    pub outpoint: OutPoint,
    pub output: TxOut,
    pub confirmations: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Funded {
    pub transaction: Transaction,
    pub fee: Amount,
    pub change_position: Option<usize>,
}

#[derive(Debug, thiserror::Error)]
pub enum FundError {
    #[error("Insufficient funds")]
    Insufficient,
    #[error("input {0} is not a coin the overlay knows")]
    UnknownInput(OutPoint),
    #[error("input {0} needs a weight in input_weights")]
    MissingWeight(OutPoint),
}

pub(super) fn minted_outpoint(index: usize) -> OutPoint {
    let mut data = b"flame-overlay-mint".to_vec();
    data.extend((index as u64).to_le_bytes());
    OutPoint::new(Txid::hash(&data), 0)
}

impl State {
    pub fn wallet_names(&self) -> Vec<String> {
        self.wallets.keys().cloned().collect()
    }

    pub fn has_wallet(&self, name: &str) -> bool {
        self.wallets.contains_key(name)
    }

    pub fn create_wallet(&mut self, name: &str) -> Result<(), StateError> {
        if self.has_wallet(name) {
            return Err(StateError::Invalid(format!("wallet {name} already exists")));
        }
        self.commit(Record::CreateWallet { name: name.into() })
    }

    pub fn watch(&mut self, wallet: &str, script: &ScriptBuf) -> Result<(), StateError> {
        if !self.has_wallet(wallet) {
            return Err(StateError::Invalid(format!("unknown wallet {wallet}")));
        }
        self.commit(Record::Watch {
            wallet: wallet.into(),
            script: script.to_hex_string(),
        })
    }

    /// The wallet's confirmed unspent coins, topped up to [`COIN_FLOOR`] per watched script.
    pub fn unspent(&mut self, wallet: &str) -> Result<Vec<Coin>, StateError> {
        let scripts = self
            .wallets
            .get(wallet)
            .cloned()
            .ok_or_else(|| StateError::Invalid(format!("unknown wallet {wallet}")))?;
        for script in &scripts {
            let held = self
                .confirmed_unspent()
                .iter()
                .filter(|coin| coin.output.script_pubkey == *script)
                .count();
            for _ in held..COIN_FLOOR {
                self.commit(Record::Mint {
                    script: script.to_hex_string(),
                    sats: COIN_SATS,
                })?;
            }
        }
        Ok(self
            .confirmed_unspent()
            .into_iter()
            .filter(|coin| scripts.contains(&coin.output.script_pubkey))
            .collect())
    }

    /// Coins created by minting or in the canonical chain, spent neither there nor in the queue.
    fn confirmed_unspent(&self) -> Vec<Coin> {
        let Some(start) = self.start_height else {
            return Vec::new();
        };
        let tip = start + self.chain.len().saturating_sub(1) as u64;
        let mut created: Vec<(OutPoint, u64)> = self
            .minted
            .iter()
            .map(|outpoint| (*outpoint, start))
            .collect();
        let mut spent = std::collections::HashSet::new();
        for hash in &self.chain {
            let block = &self.blocks[hash];
            for entry in &block.injected {
                let txid = entry.transaction.compute_txid();
                created.extend(
                    (0..entry.transaction.output.len())
                        .map(|vout| (OutPoint::new(txid, vout as u32), block.header.height)),
                );
                spent.extend(super::inputs(&entry.transaction));
            }
        }
        spent.extend(self.queue.iter().flat_map(super::inputs));
        created
            .into_iter()
            .filter(|(outpoint, _)| !spent.contains(outpoint))
            .map(|(outpoint, height)| Coin {
                outpoint,
                output: self.outputs[&outpoint].clone(),
                confirmations: tip - height + 1,
            })
            .collect()
    }

    /// Adds a fee and change to `change`, like `fundrawtransaction` with `add_inputs: false`.
    /// `input_weights` gives each input's full signed weight.
    pub fn fund(
        &self,
        mut transaction: Transaction,
        change: ScriptBuf,
        input_weights: &HashMap<OutPoint, u64>,
    ) -> Result<Funded, FundError> {
        // Segwit marker and flag.
        let mut weight = transaction.weight().to_wu() + 2;
        let mut input_sats = 0;
        for input in &transaction.input {
            let outpoint = input.previous_output;
            let output = self
                .outputs
                .get(&outpoint)
                .ok_or(FundError::UnknownInput(outpoint))?;
            let signed = input_weights
                .get(&outpoint)
                .ok_or(FundError::MissingWeight(outpoint))?;
            input_sats += output.value.to_sat();
            weight = weight - UNSIGNED_INPUT_WEIGHT + signed;
        }
        let output_sats: u64 = transaction.output.iter().map(|o| o.value.to_sat()).sum();
        let change_output = TxOut {
            value: Amount::ZERO,
            script_pubkey: change,
        };
        let change_weight = encode::serialize(&change_output).len() as u64 * 4;
        let fee = |weight: u64| weight.div_ceil(4) * FEE_RATE;

        let available = input_sats
            .checked_sub(output_sats)
            .ok_or(FundError::Insufficient)?;
        let change_sats = available.saturating_sub(fee(weight + change_weight));
        if change_sats >= DUST_SATS {
            transaction.output.push(TxOut {
                value: Amount::from_sat(change_sats),
                ..change_output
            });
            return Ok(Funded {
                change_position: Some(transaction.output.len() - 1),
                fee: Amount::from_sat(available - change_sats),
                transaction,
            });
        }
        if available < fee(weight) {
            return Err(FundError::Insufficient);
        }
        // Change too small to keep: it all goes to the fee.
        Ok(Funded {
            transaction,
            fee: Amount::from_sat(available),
            change_position: None,
        })
    }
}
