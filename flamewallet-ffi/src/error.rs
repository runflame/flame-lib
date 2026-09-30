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
}

impl From<flamepayments::KeyError> for FlameError {
    fn from(error: flamepayments::KeyError) -> FlameError {
        match error {
            flamepayments::KeyError::HardenedIndex(_) => FlameError::InvalidKeyPath {
                reason: error.to_string(),
            },
            flamepayments::KeyError::Kd(_) => FlameError::InvalidSeed {
                reason: error.to_string(),
            },
        }
    }
}
