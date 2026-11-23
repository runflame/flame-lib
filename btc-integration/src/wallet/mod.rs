//! The Bitcoin wallets: who pays for a transaction and sends it.
//!
//! A [`BtcWallet`] makes a transaction with the output it is given, pays for
//! it and sends it. A burn and an endorsement are such outputs.
//!
//! - [`impls::RpcWallet`] is an ordinary wallet of Bitcoin Core, which holds
//!   its keys. It pays for a burn and for the first output of a minter.
//! - [`MinterWallet`] is the wallet of a minter. It pays for the endorsements
//!   from the outputs of the minter script.
//!
//! The wallet of a minter is made of two halves, the way Bitcoin Core works
//! with a key it does not hold. Bitcoin Core knows the outputs of the script
//! and has no key: this half is a [`WatchOnlyWallet`]. The other half is the
//! [`IdentityKey`](crate::identity::IdentityKey), which signs.

use std::future::Future;

use bitcoin::{OutPoint, ScriptBuf, Transaction, TxOut, Txid};
use thiserror::Error;

use crate::identity::IdentityKeyError;

pub mod impls;
mod minter_wallet;

pub use minter_wallet::MinterWallet;

/// A wallet that pays.
pub trait BtcWallet: Send + Sync {
    /// Makes a transaction with `output`, pays for it with the coins of the
    /// wallet and sends it to the network. The change returns to the wallet.
    fn send(&self, output: TxOut) -> impl Future<Output = Result<Txid, WalletError>> + Send;
}

/// A wallet that knows the outputs of a script and has no key for them.
pub trait WatchOnlyWallet: Send + Sync {
    /// Adds inputs that spend confirmed outputs of `script_pubkey`, and an
    /// output that returns the change to it. Signs nothing: the caller signs
    /// the inputs, each of which weighs `input_weight` once signed.
    ///
    /// The inputs are not reserved. The caller publishes the transaction
    /// before it funds the next one.
    fn fund_from_script(
        &self,
        transaction: &Transaction,
        script_pubkey: &ScriptBuf,
        input_weight: u64,
    ) -> impl Future<Output = Result<BtcFundedTransaction, WalletError>> + Send;

    /// Sends a signed transaction to the network.
    fn publish(
        &self,
        transaction: &Transaction,
    ) -> impl Future<Output = Result<Txid, WalletError>> + Send;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtcFundingInput {
    pub outpoint: OutPoint,
    pub prevout: TxOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtcFundedTransaction {
    pub transaction: Transaction,
    /// Previous outputs aligned with `transaction.input`.
    pub inputs: Vec<BtcFundingInput>,
}

/// Why a wallet did not send a transaction.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum WalletError {
    /// There is no confirmed output to spend. For a minter this is the usual
    /// wait: its change is not in a block yet.
    #[error("there is no confirmed output to spend")]
    NoConfirmedOutputs,
    /// Bitcoin or the wallet answered with a refusal: the outputs do not pay
    /// for the transaction, or the transaction is not taken.
    #[error("refused: {0}")]
    Refused(String),
    /// The node did not answer, or its answer cannot be read.
    #[error("the Bitcoin node did not answer: {0}")]
    Unreachable(String),
    /// The watch-only wallet built a transaction that was not asked for.
    #[error("the watch-only wallet built a transaction that was not asked for")]
    UnexpectedTransaction,
    /// The key is not the one in the witness script of the minter.
    #[error("the key is not the one in the witness script of the minter")]
    WrongKey,
    /// The key of the minter could not sign.
    #[error("the key of the minter could not sign: {0}")]
    Key(#[from] IdentityKeyError),
}
