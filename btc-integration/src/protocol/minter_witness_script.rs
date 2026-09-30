//! Encoding and validation of Minter witness scripts.
//!
//! All scripts start with `<FLMV || predicate> OP_DROP`. VoteSigner supports
//! the canonical authorization suffix `<compressed-public-key> OP_CHECKSIG`.

use corepc_client::bitcoin::{Script, ScriptBuf, secp256k1::PublicKey};
use flamevm::Predicate;
use readerwriter::{ReadError, Reader};
use thiserror::Error;

use super::minter_p2wsh::parse_predicate;

const MINTER_AUTH_MAGIC: [u8; 4] = *b"FLMV";
const OP_DROP: u8 = 0x75;
const OP_PUSHBYTES_36: u8 = 0x24;
const OP_PUSHBYTES_33: u8 = 0x21;
const OP_CHECKSIG: u8 = 0xac;
const AUTH_PREFIX_LEN: usize = 1 + MINTER_AUTH_MAGIC.len() + 32 + 1;
const CANONICAL_SCRIPT_LEN: usize = AUTH_PREFIX_LEN + 1 + 33 + 1;
// 41 non-witness bytes and a two-item witness: signature (including sighash) and script.
pub const INPUT_WEIGHT: u64 = 41 * 4 + 1 + (1 + 73) + (1 + CANONICAL_SCRIPT_LEN as u64);

#[derive(Clone, Debug)]
pub struct ParsedMinterWitnessScript<'a> {
    pub flame_predicate: Predicate,
    /// Opaque authorization suffix
    pub authorization: &'a Script,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MinterWitnessScriptError {
    #[error("the Minter witness script has an invalid FLMV prefix")]
    InvalidAuthenticationPrefix,
}

pub fn build(predicate: &Predicate, public_key: &PublicKey) -> ScriptBuf {
    let mut authorization = vec![OP_PUSHBYTES_33];
    authorization.extend_from_slice(&public_key.serialize());
    authorization.push(OP_CHECKSIG);
    build_with_authorization(predicate, Script::from_bytes(&authorization))
}

pub fn build_with_authorization(predicate: &Predicate, authorization: &Script) -> ScriptBuf {
    let mut script = vec![OP_PUSHBYTES_36];
    script.extend_from_slice(&MINTER_AUTH_MAGIC);
    script.extend_from_slice(predicate.to_point().as_bytes());
    script.push(OP_DROP);
    script.extend_from_slice(authorization.as_bytes());
    ScriptBuf::from_bytes(script)
}

/// Validate the FLMV prefix and leave the authorization suffix uninterpreted.
pub fn parse(script: &Script) -> Result<ParsedMinterWitnessScript<'_>, MinterWitnessScriptError> {
    let mut remaining = script.as_bytes();
    let flame_predicate = read_prefix(&mut remaining)
        .map_err(|_| MinterWitnessScriptError::InvalidAuthenticationPrefix)?;
    Ok(ParsedMinterWitnessScript {
        flame_predicate,
        authorization: Script::from_bytes(remaining),
    })
}

pub fn parse_authentication_prefix(script: &Script) -> Result<Predicate, MinterWitnessScriptError> {
    Ok(parse(script)?.flame_predicate)
}

pub fn single_key_public_key(authorization: &Script) -> Option<PublicKey> {
    let bytes = authorization.as_bytes();
    if bytes.len() != 35 || bytes[0] != OP_PUSHBYTES_33 || bytes[34] != OP_CHECKSIG {
        return None;
    }
    PublicKey::from_slice(&bytes[1..34]).ok()
}

fn read_prefix(reader: &mut impl Reader) -> Result<Predicate, ReadError> {
    if reader.read_u8()? != OP_PUSHBYTES_36
        || reader.read_bytes(MINTER_AUTH_MAGIC.len())? != MINTER_AUTH_MAGIC
    {
        return Err(ReadError::InvalidFormat);
    }
    let predicate = reader.read_u8x32()?;
    if reader.read_u8()? != OP_DROP {
        return Err(ReadError::InvalidFormat);
    }
    parse_predicate(&predicate).ok_or(ReadError::InvalidFormat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use corepc_client::bitcoin::secp256k1::{Secp256k1, SecretKey};

    fn canonical_script() -> ScriptBuf {
        let key = SecretKey::from_slice(&[0x41; 32]).unwrap();
        build(
            &Predicate::opaque(Predicate::unspendable_key()),
            &key.public_key(&Secp256k1::new()),
        )
    }

    #[test]
    fn canonical_script_round_trips() {
        let script = canonical_script();
        let parsed = parse(&script).unwrap();
        assert_eq!(
            build_with_authorization(&parsed.flame_predicate, parsed.authorization),
            script
        );
        let public_key = single_key_public_key(parsed.authorization).unwrap();
        assert_eq!(build(&parsed.flame_predicate, &public_key), script);
        assert_eq!(script.len(), 73);
        assert_eq!(INPUT_WEIGHT, 313);
        let mut expected = vec![0x24];
        expected.extend_from_slice(b"FLMV");
        expected.extend_from_slice(Predicate::unspendable_key().as_bytes());
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
    fn prefix_parser_accepts_arbitrary_authorization() {
        let predicate = Predicate::opaque(Predicate::unspendable_key());
        for suffix in [&[][..], &[0x51][..], &[0x52][..], &[0x4c, 0xff][..]] {
            let script = build_with_authorization(&predicate, Script::from_bytes(suffix));
            assert_eq!(
                parse_authentication_prefix(&script).unwrap().to_point(),
                predicate.to_point()
            );
            let parsed = parse(&script).unwrap();
            assert_eq!(parsed.authorization.as_bytes(), suffix);
            assert_eq!(
                build_with_authorization(&parsed.flame_predicate, parsed.authorization),
                script
            );
        }
    }
}
