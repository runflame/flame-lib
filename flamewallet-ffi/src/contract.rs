//! Reading a contract an indexer served.

use flamepayments::InputSpec;
use flamevm::{Scalar, Value, FLAME_FLAVOR};

use crate::error::FlameError;
use crate::transfer::Opening;

/// A published contract, as much of it as a wallet reads.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ContractInfo {
    /// The 32-byte contract id.
    pub id: Vec<u8>,
    /// The 32-byte compressed point it is locked with: what
    /// [`crate::Wallet::owns`] is asked about.
    pub predicate: Vec<u8>,
    pub value: ContractValue,
}

/// What a contract holds.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ContractValue {
    /// A cleartext token: the amount is public. A devnet genesis allocation
    /// is one. `is_native` says whether `flavor` is [`native_flavor`].
    Clear {
        qty: u64,
        flavor: Vec<u8>,
        is_native: bool,
    },
    /// A confidential token: the amount is readable only with its opening.
    Confidential,
    /// Anything that is not a bare token; no transfer spends it.
    Other,
}

/// The flavor of flames, the native token, in the 32 bytes
/// [`decode_contract`] and [`crate::Wallet::open_note`] give a flavor in.
#[uniffi::export]
pub fn native_flavor() -> Vec<u8> {
    FLAME_FLAVOR.to_bytes().to_vec()
}

/// Whether `flavor` is [`native_flavor`]: what `is_native` reports.
pub(crate) fn is_native(flavor: Scalar) -> bool {
    flavor == FLAME_FLAVOR
}

/// Decodes a contract from its published bytes.
#[uniffi::export]
pub fn decode_contract(bytes: Vec<u8>) -> Result<ContractInfo, FlameError> {
    let contract = decode(&bytes)?;
    let value = match contract.payload() {
        Value::ClearToken(token) => match token.qty().to_u64() {
            Some(qty) => ContractValue::Clear {
                qty,
                flavor: token.flv().to_bytes().to_vec(),
                is_native: is_native(token.flv()),
            },
            None => ContractValue::Other,
        },
        Value::Token(_) => ContractValue::Confidential,
        _ => ContractValue::Other,
    };
    Ok(ContractInfo {
        id: contract.id().to_vec(),
        predicate: contract.predicate.to_point().to_bytes().to_vec(),
        value,
    })
}

/// Whether `opening` opens the confidential contract in `bytes`.
///
/// This is how a recipient checks an opening delivered out of band before
/// counting the payment: the same id comparison
/// [`crate::Wallet::build_transfer`] makes, run without spending anything.
#[uniffi::export]
pub fn opening_matches(contract: Vec<u8>, opening: Opening) -> Result<bool, FlameError> {
    let contract = decode(&contract)?;
    let opening = opening.to_wallet()?;
    // The proof and key are never reached: the check is on the id alone.
    let placeholder = flamechain::utreexo::Proof::Transient;
    let key = curve25519_dalek::scalar::Scalar::ZERO;
    match InputSpec::confidential(&contract, &opening, placeholder, key) {
        Ok(_) => Ok(true),
        Err(flamepayments::BuilderError::OpeningMismatch) => Ok(false),
        Err(error) => Err(FlameError::transfer(error)),
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<flamevm::Contract, FlameError> {
    flamechain::codec::contract_from_bytes(bytes)
        .map_err(|error| FlameError::bytes("contract", error))
}
