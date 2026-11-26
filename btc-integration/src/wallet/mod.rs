use std::future::Future;

use bitcoin::{OutPoint, ScriptBuf, Transaction, TxOut, Txid};
use thiserror::Error;

use crate::identity::IdentityKeyError;

pub mod impls;
mod minter_wallet;

pub use minter_wallet::MinterWallet;

pub trait BtcWallet: Send + Sync {
    fn send(&self, output: TxOut) -> impl Future<Output = Result<Txid, WalletError>> + Send;
}

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

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum WalletError {
    #[error("there is no confirmed output to spend")]
    NoConfirmedOutputs,
    #[error("refused: {0}")]
    Refused(String),
    #[error("the Bitcoin node did not answer: {0}")]
    Unreachable(String),
    #[error("the watch-only wallet built a transaction that was not asked for")]
    UnexpectedTransaction,
    #[error("the key is not the one in the witness script of the minter")]
    WrongKey,
    #[error("the key of the minter could not sign: {0}")]
    Key(#[from] IdentityKeyError),
}
