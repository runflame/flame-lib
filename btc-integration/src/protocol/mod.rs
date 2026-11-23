mod burn;
mod consts;
mod endorsement;
mod endorsement_validation;
mod minter_identity;
mod minter_p2wsh;
pub mod minter_witness_script;
mod records;

use bitcoin::script::PushBytesBuf;
use curve25519_dalek::ristretto::CompressedRistretto;
use flamevm::Predicate;
use readerwriter::{ReadError, Reader};

pub use burn::{Burn, BurnData};
pub use endorsement::{
    MintingEndorsementAuth, MintingEndorsementData, MintingEndorsementOutput,
    UncheckedMintingEndorsement,
};
pub use endorsement_validation::{
    AuthenticatedMintingEndorsement, MintingEndorsementProcessingError,
    MintingEndorsementValidationError, MintingEndorsementValidator,
    validate_transaction_endorsements,
};
pub use minter_identity::{MinterIdentity, MinterIdentityError};
pub use minter_p2wsh::MinterP2wsh;
pub use records::{BitcoinRecords, RecordsError};

/// The bytes a script pushes as one item.
fn push_bytes(bytes: Vec<u8>) -> PushBytesBuf {
    PushBytesBuf::try_from(bytes).expect("a payload of the protocol is far below the push limit")
}

/// Reads a predicate: 32 bytes that are a valid Ristretto point.
fn read_predicate(reader: &mut impl Reader) -> Result<Predicate, ReadError> {
    let point = CompressedRistretto(reader.read_u8x32()?);
    point.decompress().ok_or(ReadError::InvalidFormat)?;
    Ok(Predicate::opaque(point))
}
