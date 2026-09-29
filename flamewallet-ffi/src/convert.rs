//! Byte vectors back into the fixed-size things they claim to be.
//!
//! Every byte field of an exported record arrives as a `Vec<u8>` of any
//! length, so each of these checks the length first and the encoding second,
//! and names the field it refuses.

use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar as DalekScalar;
use flamevm::{Predicate, Scalar};

use crate::error::FlameError;

pub(crate) fn array32(what: &str, bytes: &[u8]) -> Result<[u8; 32], FlameError> {
    bytes
        .try_into()
        .map_err(|_| FlameError::bytes(what, format!("{} bytes, expected 32", bytes.len())))
}

/// A predicate as a compressed point that decompresses: a payment locked to
/// anything else could never be spent, so it is refused before it is built.
pub(crate) fn predicate(bytes: &[u8]) -> Result<Predicate, FlameError> {
    let point = CompressedRistretto(array32("predicate", bytes)?);
    if point.decompress().is_none() {
        return Err(FlameError::bytes("predicate", "not a Ristretto point"));
    }
    Ok(Predicate::opaque(point))
}

/// A canonical flavor scalar. `None` is the native flavor.
pub(crate) fn flavor(bytes: Option<&[u8]>) -> Result<Scalar, FlameError> {
    match bytes {
        None => Ok(flamevm::FLAME_FLAVOR),
        Some(bytes) => Scalar::from_bytes(array32("flavor", bytes)?)
            .ok_or_else(|| FlameError::bytes("flavor", "not a canonical scalar")),
    }
}

/// A canonical blinding factor.
pub(crate) fn blinding(bytes: &[u8]) -> Result<DalekScalar, FlameError> {
    Option::from(DalekScalar::from_canonical_bytes(array32(
        "blinding", bytes,
    )?))
    .ok_or_else(|| FlameError::bytes("blinding", "not a canonical scalar"))
}
