use std::future::Future;
use std::sync::Arc;

use bitcoin::secp256k1::{Message, PublicKey, ecdsa};
use ed25519_dalek::VerifyingKey;
use flamevm::Predicate;
use thiserror::Error;

use crate::MinterIdentity;

pub mod impls;

#[derive(Clone, Debug)]
pub struct IdentityConfig {
    pub flame_predicate: Predicate,
    pub access_predicate: Predicate,
    pub validator_pubkey: VerifyingKey,
}

impl IdentityConfig {
    pub fn identity(&self, key: &impl IdentityKey) -> MinterIdentity {
        MinterIdentity::single_key(
            &self.flame_predicate,
            &self.validator_pubkey,
            &key.public_key(),
        )
    }
}

pub trait IdentityKey: Send + Sync {
    fn public_key(&self) -> PublicKey;

    /// Signs the 32-byte digest as it is given.
    fn sign(
        &self,
        digest: Message,
    ) -> impl Future<Output = Result<ecdsa::Signature, IdentityKeyError>> + Send;
}

impl<K: IdentityKey> IdentityKey for Arc<K> {
    fn public_key(&self) -> PublicKey {
        self.as_ref().public_key()
    }

    async fn sign(&self, digest: Message) -> Result<ecdsa::Signature, IdentityKeyError> {
        self.as_ref().sign(digest).await
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct IdentityKeyError(pub String);
