//! Bytes in, bytes out: the two codecs this node speaks, in one place.
//!
//! A contract and an effect log travel as a `CellEnvelope`. A Utreexo proof
//! does not — upstream keeps `readerwriter`'s encoding for that one
//! subsystem, so this module is the only place in `flamed` that names either
//! codec.

use flamechain::utreexo::Proof;
use flamevm::{CellDecode, CellEncode, CellEnvelope, CellError, Contract, TxLog};
use readerwriter::{Decodable, Encodable, ReadError, Reader};

/// The largest envelope this node will decode a contract from.
///
/// A genesis allocation's envelope is a couple of hundred bytes, so this is
/// two orders of magnitude of headroom, and it is a bound rather than a
/// guess only because a decoder with no bound is a denial of service.
pub const MAX_CONTRACT_BYTES: usize = 64 * 1024;

/// A contract as the chain publishes it.
pub fn contract_bytes(contract: &Contract) -> Result<Vec<u8>, CellError> {
    Ok(contract.to_envelope()?.encode())
}

/// A contract, back from those bytes.
pub fn contract_from_bytes(bytes: &[u8]) -> Result<Contract, CellError> {
    // Four gas per byte is upstream's own rule for decoding an envelope.
    let mut gas = (bytes.len() as u64).saturating_mul(4);
    let mut envelope = CellEnvelope::decode(bytes, MAX_CONTRACT_BYTES, &mut gas)?;
    // `CellEnvelope::decode` refuses an envelope whose bag lacks its own
    // root, so this lookup cannot fail. It is a lookup rather than an
    // `expect` because a panic on wire bytes is never the right failure.
    let root = envelope
        .cells()
        .get(&envelope.root())
        .ok_or(CellError::MissingCell(envelope.root()))?;
    let contract = Contract::from_cell(&root, &mut envelope)?;
    // An envelope may carry cells its root never references, and the id is
    // the root's hash alone, so padding decodes to the same contract and
    // passes every check made against the id. Two operators comparing
    // genesis.json files byte for byte is how they confirm they are on one
    // network, so the bytes have to be the only ones this contract has.
    // `ExternalTx::from_bytes_bounded` guards itself the same way.
    if contract_bytes(&contract)? != bytes {
        return Err(CellError::InvalidFormat);
    }
    Ok(contract)
}

/// An effect log, as the index archives it.
pub fn log_bytes(log: &TxLog) -> Result<Vec<u8>, CellError> {
    Ok(log.to_envelope()?.encode())
}

/// A Utreexo proof as the RPC hands it out.
pub fn proof_bytes(proof: &Proof) -> Vec<u8> {
    proof.encode_to_vec()
}

/// A Utreexo proof, back from those bytes.
///
/// The node itself never calls this on anything a stranger sent: the proofs
/// it verifies ride inside a `BlockTx`, which `BlockTx::from_bytes_bounded`
/// checks. This exists for the tests and for a wallet holding a proof it was
/// given. It goes through `read_all`, which refuses trailing bytes, so it
/// stays strict if that ever changes.
pub fn proof_from_bytes(bytes: &[u8]) -> Result<Proof, ReadError> {
    Reader::read_all(&mut { bytes }, <Proof as Decodable>::decode)
}
