//! Opening the note that follows an output: how a recipient learns what it
//! was paid, and how a sender reads back its own change.

use flamepayments::{Account, NoteError, ViewingKey};

use crate::contract;
use crate::error::FlameError;
use crate::keys::KeyPath;
use crate::transfer::Opening;

/// What an opened note gives its recipient.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ReceivedNote {
    /// What a [`crate::TransferInput`] spends the output with.
    pub opening: Opening,
    /// The memo, as the sender wrote it.
    pub memo: Vec<u8>,
}

/// Why a note did not open: the outcomes of `docs/payments.md`
/// "Receiving", which a wallet treats differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NoteFailure {
    /// No note followed the output. The output is ours but unreadable: keep
    /// and list it, and leave it out of the balance.
    Missing,
    /// The note is malformed. Treated as [`NoteFailure::Missing`].
    Malformed,
    /// A note version this library does not know; a later one may open it.
    UnknownVersion,
    /// The note does not decrypt under this address. Treated as
    /// [`NoteFailure::Missing`].
    Undecryptable,
    /// The note decrypts but misstates its output: the sender erred or lied.
    /// Keep and flag it; never count it.
    OpeningMismatch,
    /// The contract is a cleartext token, or not a token: there is no note
    /// to open, and [`crate::decode_contract`] reads its amount.
    NotConfidential,
}

impl From<NoteError> for NoteFailure {
    fn from(error: NoteError) -> NoteFailure {
        match error {
            NoteError::Missing => NoteFailure::Missing,
            NoteError::Malformed => NoteFailure::Malformed,
            NoteError::UnknownVersion(_) => NoteFailure::UnknownVersion,
            NoteError::Undecryptable => NoteFailure::Undecryptable,
            NoteError::OpeningMismatch => NoteFailure::OpeningMismatch,
            NoteError::NotConfidential => NoteFailure::NotConfidential,
        }
    }
}

pub(crate) fn open<K: ViewingKey>(
    account: &Account<K>,
    contract: &[u8],
    note: Option<&[u8]>,
    path: KeyPath,
) -> Result<ReceivedNote, FlameError> {
    let published = contract::decode(contract)?;
    let KeyPath { branch, index } = path;
    let address = account.address_at(branch, index)?;
    // `open_note` trusts the caller to have matched the predicate; a wrong
    // path would otherwise surface as an undecryptable note.
    if published.predicate.to_point() != address.spending_key().compress() {
        return Err(FlameError::InvalidKeyPath {
            reason: "the contract is not locked to the key at this path".into(),
        });
    }
    let view_key = zeroize::Zeroizing::new(account.viewing_key_at(branch, index)?);
    let received =
        flamepayments::open_note(&published, note, &address, &view_key).map_err(|error| {
            FlameError::Note {
                failure: error.into(),
                reason: error.to_string(),
            }
        })?;
    Ok(ReceivedNote {
        opening: Opening::from_wallet(&received.opening),
        memo: received.memo,
    })
}
