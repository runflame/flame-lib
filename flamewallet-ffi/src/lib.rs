//! A flat facade over `flamepayments` for Swift, Kotlin and Node.
//!
//! Everything that crosses this boundary is bytes, strings, integers and
//! plain records; no curve type, contract or proof does. The one exception
//! is [`Wallet`], an opaque handle that holds the account, so a spending key
//! is derived, used and dropped inside Rust and no caller ever holds one.
//!
//! The crate does no I/O. Contracts and membership proofs come in as the
//! bytes an indexer serves (`flamechain::codec`), and a signed transfer
//! goes out as `BlockTx::to_bytes`; fetching the one and publishing the
//! other is the application's job.

uniffi::setup_scaffolding!();

mod contract;
mod convert;
mod error;
mod keys;
mod note;
mod transfer;

#[cfg(test)]
mod tests;

pub use contract::{decode_contract, opening_matches, ContractInfo, ContractValue};
pub use error::FlameError;
pub use keys::{
    address_to_predicate, generate_mnemonic, mnemonic_to_seed, validate_mnemonic, IssuedAddress,
    KeyPath, Network, Wallet,
};
pub use note::{NoteFailure, ReceivedNote};
pub use transfer::{
    CreatedOutput, Opening, Transfer, TransferInput, TransferOutput, TransferRequest,
};

/// Branch `0` of the standard path: addresses handed out to payers.
pub const RECEIVING: u32 = flamekd::util::RECEIVING;

/// Branch `1` of the standard path: the wallet's own change.
pub const CHANGE: u32 = flamekd::util::CHANGE;
