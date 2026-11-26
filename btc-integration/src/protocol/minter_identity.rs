use bitcoin::{ScriptBuf, secp256k1::PublicKey};
use ed25519_dalek::VerifyingKey;
use flamevm::Predicate;
use std::hash::{Hash, Hasher};
use thiserror::Error;

use crate::protocol::minter_p2wsh::MinterP2wsh;
use crate::protocol::minter_witness_script;

#[derive(Clone, Debug)]
pub struct MinterIdentity {
    witness_script: ScriptBuf,
    p2wsh: MinterP2wsh,
    flame_predicate: Predicate,
    validator_pubkey: VerifyingKey,
}

impl MinterIdentity {
    /// Derives an identity from the canonical single-key authorization script.
    pub(crate) fn single_key(
        flame_predicate: &Predicate,
        validator_pubkey: &VerifyingKey,
        public_key: &PublicKey,
    ) -> Self {
        let witness_script =
            minter_witness_script::build(flame_predicate, validator_pubkey, public_key);
        Self {
            p2wsh: MinterP2wsh::from_witness_script(witness_script.as_bytes()),
            witness_script,
            flame_predicate: flame_predicate.clone(),
            validator_pubkey: *validator_pubkey,
        }
    }

    pub(crate) fn new(witness_script: ScriptBuf) -> Result<Self, MinterIdentityError> {
        let parsed = minter_witness_script::parse(&witness_script)
            .map_err(|_| MinterIdentityError::InvalidAuthenticationPrefix)?;
        let flame_predicate = parsed.flame_predicate;
        let validator_pubkey = parsed.validator_pubkey;
        let p2wsh = MinterP2wsh::from_witness_script(witness_script.as_bytes());

        Ok(Self {
            witness_script,
            p2wsh,
            flame_predicate,
            validator_pubkey,
        })
    }

    pub const fn witness_script(&self) -> &ScriptBuf {
        &self.witness_script
    }

    pub const fn p2wsh(&self) -> MinterP2wsh {
        self.p2wsh
    }

    pub const fn flame_predicate(&self) -> &Predicate {
        &self.flame_predicate
    }

    pub const fn validator_pubkey(&self) -> &VerifyingKey {
        &self.validator_pubkey
    }
}

impl PartialEq for MinterIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.witness_script == other.witness_script
    }
}

impl Eq for MinterIdentity {}

impl Hash for MinterIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.p2wsh.hash(state);
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MinterIdentityError {
    #[error("Minter witness script does not have a canonical FLMV authentication prefix")]
    InvalidAuthenticationPrefix,
}

#[cfg(test)]
mod tests {
    use super::MinterIdentity;
    use crate::protocol::minter_witness_script;
    use flamevm::Predicate;

    #[test]
    fn validator_pubkey_is_committed_by_identity() {
        let predicate = Predicate::opaque(Predicate::unspendable_key());
        let identities = [0x22, 0x23].map(|seed| {
            let validator_pubkey =
                ed25519_dalek::SigningKey::from_bytes(&[seed; 32]).verifying_key();
            let script = minter_witness_script::build_with_authorization(
                &predicate,
                &validator_pubkey,
                bitcoin::Script::from_bytes(&[0x51]),
            );
            let identity = MinterIdentity::new(script).unwrap();
            assert_eq!(identity.validator_pubkey(), &validator_pubkey);
            identity
        });

        assert_ne!(identities[0], identities[1]);
        assert_ne!(identities[0].p2wsh(), identities[1].p2wsh());
        assert_eq!(
            identities[0].flame_predicate().to_point(),
            identities[1].flame_predicate().to_point()
        );
    }
}
