//! Bytes in, bytes out: the two codecs this node speaks, in one place.
//!
//! A contract and an effect log travel as a `CellEnvelope`; a Utreexo proof
//! in `readerwriter`'s encoding. The contract and proof codecs are
//! `flamechain`'s, shared with the wallet that reads what this node serves;
//! the effect log is the node's alone.

pub use flamechain::codec::{
    contract_bytes, contract_from_bytes, proof_bytes, proof_from_bytes, MAX_CONTRACT_BYTES,
};
use flamevm::{CellEncode, CellError, TxLog};

/// An effect log, as the index archives it.
pub fn log_bytes(log: &TxLog) -> Result<Vec<u8>, CellError> {
    Ok(log.to_envelope()?.encode())
}
