use corepc_client::bitcoin::{ScriptBuf, secp256k1::PublicKey};
use flamevm::Predicate;
use thiserror::Error;

use crate::protocol::minter_p2wsh::MinterP2wsh;
use crate::protocol::minter_witness_script;

#[derive(Clone, Debug)]
pub struct MinterIdentity {
    witness_script: ScriptBuf,
    p2wsh: MinterP2wsh,
    flame_predicate: Predicate,
}

impl MinterIdentity {
    /// Derives an identity from the canonical single-key authorization script.
    pub fn single_key(flame_predicate: &Predicate, public_key: &PublicKey) -> Self {
        let witness_script = minter_witness_script::build(flame_predicate, public_key);
        Self {
            p2wsh: MinterP2wsh::from_witness_script(witness_script.as_bytes()),
            witness_script,
            flame_predicate: flame_predicate.clone(),
        }
    }

    pub fn new(witness_script: ScriptBuf) -> Result<Self, MinterIdentityError> {
        let flame_predicate = minter_witness_script::parse_authentication_prefix(&witness_script)
            .map_err(|_| MinterIdentityError::InvalidAuthenticationPrefix)?;
        let p2wsh = MinterP2wsh::from_witness_script(witness_script.as_bytes());

        Ok(Self {
            witness_script,
            p2wsh,
            flame_predicate,
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
}

impl PartialEq for MinterIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.witness_script == other.witness_script
    }
}

impl Eq for MinterIdentity {}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MinterIdentityError {
    #[error("Minter witness script does not have a canonical FLMV authentication prefix")]
    InvalidAuthenticationPrefix,
}

#[cfg(test)]
mod tests {
    use super::{MinterIdentity, MinterIdentityError};
    use crate::protocol::minter_witness_script;
    use flamevm::Predicate;

    #[test]
    fn derives_identity_from_the_complete_witness_script() {
        let predicate = Predicate::opaque(Predicate::unspendable_key());
        let script = witness_script(&predicate);

        let identity = MinterIdentity::new(script.clone()).expect("valid Minter identity");

        assert_eq!(identity.witness_script(), &script);
        assert_eq!(identity.flame_predicate().to_point(), predicate.to_point());
        assert_eq!(
            identity.p2wsh(),
            crate::protocol::MinterP2wsh::from_witness_script(script.as_bytes())
        );
    }

    #[test]
    fn requires_the_canonical_prefix_but_not_an_authorization_suffix() {
        assert_eq!(
            MinterIdentity::new(vec![0x23; 39].into()).unwrap_err(),
            MinterIdentityError::InvalidAuthenticationPrefix
        );

        let predicate = Predicate::opaque(Predicate::unspendable_key());
        let mut prefix_only = witness_script(&predicate).into_bytes();
        prefix_only.pop();
        MinterIdentity::new(prefix_only.into()).expect("the FLMV prefix is sufficient");
    }

    #[test]
    fn identity_includes_the_complete_witness_script() {
        let predicate = Predicate::opaque(Predicate::unspendable_key());
        let first = MinterIdentity::new(witness_script(&predicate)).expect("valid Minter identity");
        let mut other_script = witness_script(&predicate).into_bytes();
        *other_script.last_mut().expect("authorization suffix") = 0x52;
        let second = MinterIdentity::new(other_script.into()).expect("valid Minter identity");

        assert_ne!(first, second);
        assert_eq!(
            first.flame_predicate().to_point(),
            second.flame_predicate().to_point()
        );
    }

    fn witness_script(predicate: &Predicate) -> corepc_client::bitcoin::ScriptBuf {
        minter_witness_script::build_with_authorization(
            predicate,
            corepc_client::bitcoin::Script::from_bytes(&[0x51]),
        )
    }
}
