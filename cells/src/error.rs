use crate::CellID;

/// Failure while constructing, decoding, resolving, or traversing Cells.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CellError {
    #[error("Cell payload has {actual} bytes; maximum is {max}")]
    PayloadTooLarge { actual: usize, max: usize },

    #[error("Cell has {actual} references; maximum is {max}")]
    TooManyReferences { actual: usize, max: usize },

    #[error("Cell payload capacity exceeded")]
    PayloadCapacity,

    #[error("Cell reference capacity exceeded")]
    ReferenceCapacity,

    #[error("insufficient payload bytes")]
    InsufficientBytes,

    #[error("insufficient Cell references")]
    InsufficientReferences,

    #[error("trailing payload bytes")]
    TrailingBytes,

    #[error("trailing Cell references")]
    TrailingReferences,

    #[error("invalid Cell encoding")]
    InvalidFormat,

    #[error("invalid Cell hash level")]
    InvalidLevel,

    #[error("Cell depth exceeds 65535")]
    DepthOverflow,

    #[error("pruned Cell has no accessible application data")]
    PrunedCell,

    #[error("Cell reference {0:?} needs hash/depth metadata before encoding")]
    MissingCellMetadata(CellID),

    #[error("resolved Cell commitment mismatch for {0:?}")]
    CellCommitmentMismatch(CellID),

    #[error("missing Cell {0:?}")]
    MissingCell(CellID),

    #[error("resolved Cell ID mismatch: expected {expected:?}, got {actual:?}")]
    CellHashMismatch { expected: CellID, actual: CellID },

    #[error("configured limit exceeded")]
    LimitExceeded,

    #[error("resource budget exhausted")]
    ResourceExhausted,

    #[error("Bag of Cells count does not fit u32")]
    CellCountOverflow,

    #[error("cycle through Cell {0:?}")]
    Cycle(CellID),

    #[error("Trie key has {actual} bytes; maximum is {max}")]
    TrieKeyTooLong { actual: usize, max: usize },

    #[error("Trie key has {actual} bytes; expected {expected}")]
    TrieKeyLengthMismatch { expected: usize, actual: usize },

    #[error("malformed Trie")]
    MalformedTrie,
}
