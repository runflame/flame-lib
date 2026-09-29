use std::sync::Arc;

use corepc_client::bitcoin::secp256k1::{Secp256k1, SecretKey};
use ed25519_dalek::VerifyingKey;
use flamevm::Predicate;
use rand::{RngCore, rngs::OsRng};
use tokio::sync::OnceCell;
use zeroize::Zeroizing;

use crate::MinterIdentity;
use storage::{IdentitySecretStorage, InMemorySecretStorage};

pub mod storage;

#[derive(Clone, Debug)]
pub struct IdentityConfig {
    pub flame_predicate: Predicate,
    pub access_predicate: Predicate,
    pub validator_pubkey: VerifyingKey,
}

pub struct IdentityManager<S = InMemorySecretStorage> {
    secret_storage: Arc<S>,
    config: IdentityConfig,
    identity: OnceCell<MinterIdentity>,
}

impl<S> IdentityManager<S> {
    pub fn new(secret_storage: Arc<S>, config: IdentityConfig) -> Self {
        Self {
            secret_storage,
            config,
            identity: OnceCell::new(),
        }
    }

    pub fn config(&self) -> &IdentityConfig {
        &self.config
    }

    pub fn identity(&self) -> Option<&MinterIdentity> {
        self.identity.get()
    }

    pub fn secret_storage(&self) -> &Arc<S> {
        &self.secret_storage
    }
}

impl<S: IdentitySecretStorage> IdentityManager<S> {
    pub async fn startup(&self) -> Result<&MinterIdentity, IdentityError<S::Error>> {
        self.identity
            .get_or_try_init(|| async {
                let stored = self
                    .secret_storage
                    .load_secret_key()
                    .await
                    .map_err(IdentityError::Storage)?;
                let secret = match stored {
                    Some(secret) => SecretKeyGuard(secret),
                    None => {
                        let generated = generate_secret_key().map_err(IdentityError::Random)?;
                        SecretKeyGuard(
                            self.secret_storage
                                .store_secret_key_if_absent(&generated.0)
                                .await
                                .map_err(IdentityError::Storage)?,
                        )
                    }
                };
                let public_key = secret.0.public_key(&Secp256k1::signing_only());
                Ok(MinterIdentity::single_key(
                    &self.config.flame_predicate,
                    &public_key,
                ))
            })
            .await
    }
}

fn generate_secret_key() -> Result<SecretKeyGuard, rand::Error> {
    let mut bytes = Zeroizing::new([0; 32]);
    loop {
        OsRng.try_fill_bytes(bytes.as_mut())?;
        if let Ok(secret) = SecretKey::from_slice(bytes.as_ref()) {
            return Ok(SecretKeyGuard(secret));
        }
    }
}

struct SecretKeyGuard(SecretKey);

impl Drop for SecretKeyGuard {
    fn drop(&mut self) {
        self.0.non_secure_erase();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityError<E: std::error::Error + 'static> {
    #[error("identity secret storage error: {0}")]
    Storage(#[source] E),
    #[error("failed to generate identity secret: {0}")]
    Random(#[source] rand::Error),
}

#[cfg(test)]
mod tests;
