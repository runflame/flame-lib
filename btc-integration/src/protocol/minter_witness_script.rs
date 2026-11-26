//! Encoding and validation of Minter witness scripts.
//!
//! All scripts start with `<FLMV || predicate || validator_pubkey> OP_DROP`. The canonical
//! authorization suffix is `<compressed-public-key> OP_CHECKSIG`: a single key.

use bitcoin::opcodes::all::{OP_CHECKSIG, OP_DROP, OP_PUSHBYTES_33, OP_PUSHBYTES_68};
use bitcoin::{Script, ScriptBuf, secp256k1::PublicKey};
use ed25519_dalek::VerifyingKey;
use flamevm::Predicate;
use readerwriter::{ReadError, Reader};
use thiserror::Error;

use super::read_predicate;

const MINTER_AUTH_MAGIC: [u8; 4] = *b"FLMV";
const AUTH_PREFIX_LEN: usize = 1 + MINTER_AUTH_MAGIC.len() + 32 + 32 + 1;
const CANONICAL_SCRIPT_LEN: usize = AUTH_PREFIX_LEN + 1 + 33 + 1;
// 41 non-witness bytes and a two-item witness: signature (including sighash) and script.
pub const INPUT_WEIGHT: u64 = 41 * 4 + 1 + (1 + 73) + (1 + CANONICAL_SCRIPT_LEN as u64);
const _: () = assert!(INPUT_WEIGHT == 345);

#[derive(Clone, Debug)]
pub struct ParsedMinterWitnessScript<'a> {
    pub flame_predicate: Predicate,
    pub validator_pubkey: VerifyingKey,
    /// Opaque authorization suffix
    pub authorization: &'a Script,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MinterWitnessScriptError {
    #[error("the Minter witness script has an invalid FLMV prefix")]
    InvalidAuthenticationPrefix,
}

pub fn build(
    predicate: &Predicate,
    validator_pubkey: &VerifyingKey,
    public_key: &PublicKey,
) -> ScriptBuf {
    let mut authorization = vec![OP_PUSHBYTES_33.to_u8()];
    authorization.extend_from_slice(&public_key.serialize());
    authorization.push(OP_CHECKSIG.to_u8());
    build_with_authorization(
        predicate,
        validator_pubkey,
        Script::from_bytes(&authorization),
    )
}

pub fn build_with_authorization(
    predicate: &Predicate,
    validator_pubkey: &VerifyingKey,
    authorization: &Script,
) -> ScriptBuf {
    let mut script = vec![OP_PUSHBYTES_68.to_u8()];
    script.extend_from_slice(&MINTER_AUTH_MAGIC);
    script.extend_from_slice(predicate.to_point().as_bytes());
    script.extend_from_slice(validator_pubkey.as_bytes());
    script.push(OP_DROP.to_u8());
    script.extend_from_slice(authorization.as_bytes());
    ScriptBuf::from_bytes(script)
}

/// Validate the FLMV prefix and leave the authorization suffix uninterpreted.
pub fn parse(script: &Script) -> Result<ParsedMinterWitnessScript<'_>, MinterWitnessScriptError> {
    let mut remaining = script.as_bytes();
    let (flame_predicate, validator_pubkey) = read_prefix(&mut remaining)
        .map_err(|_| MinterWitnessScriptError::InvalidAuthenticationPrefix)?;
    Ok(ParsedMinterWitnessScript {
        flame_predicate,
        validator_pubkey,
        authorization: Script::from_bytes(remaining),
    })
}

pub fn single_key_public_key(authorization: &Script) -> Option<PublicKey> {
    let bytes = authorization.as_bytes();
    if bytes.len() != 35 || bytes[0] != OP_PUSHBYTES_33.to_u8() || bytes[34] != OP_CHECKSIG.to_u8()
    {
        return None;
    }
    PublicKey::from_slice(&bytes[1..34]).ok()
}

fn read_prefix(reader: &mut impl Reader) -> Result<(Predicate, VerifyingKey), ReadError> {
    if reader.read_u8()? != OP_PUSHBYTES_68.to_u8()
        || reader.read_bytes(MINTER_AUTH_MAGIC.len())? != MINTER_AUTH_MAGIC
    {
        return Err(ReadError::InvalidFormat);
    }
    let predicate = read_predicate(reader)?;
    let validator_pubkey =
        VerifyingKey::from_bytes(&reader.read_u8x32()?).map_err(|_| ReadError::InvalidFormat)?;
    if reader.read_u8()? != OP_DROP.to_u8() {
        return Err(ReadError::InvalidFormat);
    }
    Ok((predicate, validator_pubkey))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::secp256k1::{Secp256k1, SecretKey};

    fn validator_pubkey() -> VerifyingKey {
        ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key()
    }

    fn canonical_script() -> ScriptBuf {
        let key = SecretKey::from_slice(&[0x41; 32]).unwrap();
        build(
            &Predicate::opaque(Predicate::unspendable_key()),
            &validator_pubkey(),
            &key.public_key(&Secp256k1::new()),
        )
    }

    #[test]
    fn canonical_script_round_trips() {
        let script = canonical_script();
        let parsed = parse(&script).unwrap();
        assert_eq!(
            build_with_authorization(
                &parsed.flame_predicate,
                &parsed.validator_pubkey,
                parsed.authorization,
            ),
            script
        );
        let public_key = single_key_public_key(parsed.authorization).unwrap();
        assert_eq!(
            build(
                &parsed.flame_predicate,
                &parsed.validator_pubkey,
                &public_key
            ),
            script
        );
        assert_eq!(parsed.validator_pubkey, validator_pubkey());
        let mut expected = vec![0x44];
        expected.extend_from_slice(b"FLMV");
        expected.extend_from_slice(Predicate::unspendable_key().as_bytes());
        expected.extend_from_slice(validator_pubkey().as_bytes());
        expected.extend_from_slice(&[0x75, 0x21]);
        expected.extend_from_slice(&public_key.serialize());
        expected.push(0xac);
        assert_eq!(script.as_bytes(), expected);
    }

    #[test]
    fn only_truncated_prefixes_are_rejected() {
        let script = canonical_script();
        for len in 0..script.len() {
            assert_eq!(
                parse(Script::from_bytes(&script.as_bytes()[..len])).is_ok(),
                len >= AUTH_PREFIX_LEN,
            );
        }
    }

    #[test]
    fn malformed_prefix_is_rejected_but_suffix_is_opaque() {
        let script = canonical_script();
        for index in [0, 1, AUTH_PREFIX_LEN - 1] {
            let mut bytes = script.clone().into_bytes();
            bytes[index] = 0;
            assert!(parse(Script::from_bytes(&bytes)).is_err());
        }
        let mut bytes = script.clone().into_bytes();
        bytes[AUTH_PREFIX_LEN + 1..CANONICAL_SCRIPT_LEN - 1].fill(0);
        let parsed = parse(Script::from_bytes(&bytes)).unwrap();
        assert!(single_key_public_key(parsed.authorization).is_none());
        let mut bytes = script.into_bytes();
        bytes.push(0x51);
        assert!(parse(Script::from_bytes(&bytes)).is_ok());
    }

    #[test]
    fn rejects_invalid_validator_pubkey() {
        let mut bytes = canonical_script().into_bytes();
        let key_offset = 1 + MINTER_AUTH_MAGIC.len() + 32;
        let invalid_key = (0u8..=u8::MAX)
            .map(|byte| [byte; 32])
            .find(|bytes| VerifyingKey::from_bytes(bytes).is_err())
            .unwrap();
        bytes[key_offset..key_offset + 32].copy_from_slice(&invalid_key);

        assert_eq!(
            parse(Script::from_bytes(&bytes)).unwrap_err(),
            MinterWitnessScriptError::InvalidAuthenticationPrefix
        );
    }

    #[test]
    fn rejects_legacy_and_noncanonical_prefix_encodings() {
        let script = canonical_script().into_bytes();
        let mut legacy = vec![0x24];
        legacy.extend_from_slice(&script[1..37]);
        legacy.extend_from_slice(&script[69..]);
        let mut noncanonical = vec![0x4c, 68];
        noncanonical.extend_from_slice(&script[1..]);

        for bytes in [legacy, noncanonical] {
            assert!(parse(Script::from_bytes(&bytes)).is_err());
        }
    }
}
