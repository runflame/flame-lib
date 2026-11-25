mod burn;
mod consts;
mod endorsement;
mod endorsement_validation;
mod minter_identity;
mod minter_p2wsh;
pub mod minter_witness_script;
mod records;
mod records_watcher;

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
pub use records::{BtcBlockRecords, BtcRecordsError, BtcRecordsEvent};
pub use records_watcher::{BtcRecordsWatchError, BtcRecordsWatcher};

/// Reads a predicate: 32 bytes that are a valid Ristretto point.
fn read_predicate(reader: &mut impl Reader) -> Result<Predicate, ReadError> {
    let point = CompressedRistretto(reader.read_u8x32()?);
    point.decompress().ok_or(ReadError::InvalidFormat)?;
    Ok(Predicate::opaque(point))
}
