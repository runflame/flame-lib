//! Flame wallet: accounts derived from a flamekd seed, and transfers built
//! on the VM's private-witness path.
//!
//! An [`Account`] turns a 64-byte seed into addresses, predicates and
//! spending keys along the standard derivation path. The [`builder`] module
//! assembles a transfer from those pieces: a confidential input travels as a
//! private witness (`String::contract` over a token with open commitments),
//! never as script literals, so a spend publishes nothing about the amount it
//! spends. See `flamewallet.md`.

pub mod builder;
pub mod keys;

pub use builder::{
    block_tx, build_transfer, sign, BuilderError, InputSpec, Opening, OutputSpec, MAX_OUTPUTS,
};
pub use keys::{Account, KeyError};

#[cfg(test)]
mod tests;
