//! The one error every exported call returns.
//!
//! Each variant names what the caller handed in wrong, and carries the
//! underlying error as text: the Rust error types do not cross the boundary,
//! and a binding needs a variant to branch on and a message to log, not a
//! chain of sources.

/// Why a call was refused.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FlameError {
    /// The phrase is not a valid BIP-39 mnemonic in any supported language,
    /// or the word count is not one BIP-39 defines.
    #[error("invalid mnemonic: {reason}")]
    InvalidMnemonic { reason: String },

    /// A seed is not 64 bytes, or flamekd refused to derive from it.
    #[error("invalid seed: {reason}")]
    InvalidSeed { reason: String },

    /// A view or receiving key does not parse for the wallet's network, or
    /// flamekd refused to derive from the key a wallet holds.
    #[error("invalid key: {reason}")]
    InvalidKey { reason: String },

    /// The wallet's key does not allow the call: a view wallet cannot
    /// spend, and a receive wallet can neither spend nor open notes.
    /// `wallet` is this wallet's kind, `needs` the least kind that could.
    #[error("a {wallet:?} wallet cannot do this; it needs a {needs:?} wallet")]
    NotPermitted {
        wallet: crate::WalletKind,
        needs: crate::WalletKind,
    },

    /// An address does not parse for the wallet's network.
    #[error("invalid address: {reason}")]
    InvalidAddress { reason: String },

    /// A branch or index is hardened; both must be normal.
    #[error("invalid key path: {reason}")]
    InvalidKeyPath { reason: String },

    /// Bytes do not decode into what they claim to be. `what` names the
    /// field: `contract`, `proof`, `predicate`, `flavor`, `blinding`.
    #[error("invalid {what}: {reason}")]
    InvalidBytes { what: String, reason: String },

    /// Input `input` is not locked to the key at the path it names, so its
    /// signature could never verify.
    #[error("input {input} is not locked to the key at its path")]
    KeyMismatch { input: u32 },

    /// A note did not open; `failure` says how, and so what the wallet does
    /// with the output.
    #[error("note not opened: {reason}")]
    Note {
        failure: crate::NoteFailure,
        reason: String,
    },

    /// The transfer could not be built or signed.
    #[error("transfer refused: {reason}")]
    Transfer { reason: String },
}

impl FlameError {
    pub(crate) fn bytes(what: &str, reason: impl ToString) -> FlameError {
        FlameError::InvalidBytes {
            what: what.to_owned(),
            reason: reason.to_string(),
        }
    }

    pub(crate) fn transfer(reason: impl ToString) -> FlameError {
        FlameError::Transfer {
            reason: reason.to_string(),
        }
    }

    /// As the `From` conversion, but a flamekd refusal blames the seed: for
    /// the constructors that derive the account from one.
    pub(crate) fn from_seed_derivation(error: flamepayments::KeyError) -> FlameError {
        match error {
            flamepayments::KeyError::Kd(_) => FlameError::InvalidSeed {
                reason: error.to_string(),
            },
            other => other.into(),
        }
    }
}

/// A flamekd refusal blames the key the wallet holds, whichever kind: only
/// the seed constructors know there was a seed, and they use
/// [`FlameError::from_seed_derivation`].
impl From<flamepayments::KeyError> for FlameError {
    fn from(error: flamepayments::KeyError) -> FlameError {
        match error {
            flamepayments::KeyError::HardenedIndex(_) => FlameError::InvalidKeyPath {
                reason: error.to_string(),
            },
            flamepayments::KeyError::Kd(_) => FlameError::InvalidKey {
                reason: error.to_string(),
            },
        }
    }
}
