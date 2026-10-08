//! Wallet-side Flame payments: accounts derived from a flamekd seed,
//! transfers built on the VM's private-witness path, and the encrypted notes
//! those transfers carry.
//!
//! A [`SpendAccount`] turns a 64-byte seed into addresses, predicates and
//! spending keys along the standard derivation path; a [`ViewAccount`]
//! does the same from a view key without the spending keys, and a
//! [`ReceiveAccount`] from a receiving key without the viewing keys either.
//! The [`builder`] module assembles a transfer from those pieces: a
//! confidential input travels as a private witness (`String::contract` over
//! a token with open commitments), never as script literals, so a spend
//! publishes nothing about the amount it spends. The [`note`] module seals the note every output carries and opens
//! a received one, as `docs/payments.md` specifies. See `flamepayments.md`.

pub mod builder;
pub mod keys;
pub mod note;

pub use builder::{
    block_tx, build_transfer, sign, BuilderError, InputSpec, Opening, OutputSpec, MAX_OUTPUTS,
};
pub use keys::{
    Account, AccountKey, KeyError, ReceiveAccount, SpendAccount, ViewAccount, ViewingKey,
};
pub use note::{open_note, outputs_with_notes, NoteError, ReceivedNote, MEMO_MAX, NOTE_VERSION};

#[cfg(test)]
mod tests;
