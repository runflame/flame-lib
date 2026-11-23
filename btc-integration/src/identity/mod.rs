//! The identity of a minter: its public configuration and the Bitcoin key
//! that signs its endorsements.

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
    /// The identity of the minter that signs with `key`.
    pub fn identity(&self, key: &impl IdentityKey) -> MinterIdentity {
        MinterIdentity::single_key(
            &self.flame_predicate,
            &self.validator_pubkey,
            &key.public_key(),
        )
    }
}

/// The Bitcoin key of a minter. Its public key is in the witness script of
/// the minter, and it signs the inputs of the endorsements.
///
/// The secret stays inside the implementation, so a key in the memory of the
/// process and a key on a hardware device look the same from here.
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

/// The key could not sign.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct IdentityKeyError(pub String);
