use async_trait::async_trait;
use corepc_client::bitcoin::secp256k1::SecretKey;
use tokio::sync::Mutex;

use crate::SecretStorage;

use super::SecretKeyGuard;

#[async_trait]
pub trait IdentitySecretStorage: SecretStorage {
    async fn load_secret_key(&self) -> Result<Option<SecretKey>, Self::Error>;

    async fn store_secret_key_if_absent(
        &self,
        secret_key: &SecretKey,
    ) -> Result<SecretKey, Self::Error>;
}

#[derive(Default)]
pub struct InMemorySecretStorage {
    secret: Mutex<Option<SecretKeyGuard>>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("identity secret has not been initialized")]
pub struct MissingSecret;

#[async_trait]
impl SecretStorage for InMemorySecretStorage {
    type Error = MissingSecret;

    async fn get_secret_key(&self) -> Result<SecretKey, Self::Error> {
        self.load_secret_key().await?.ok_or(MissingSecret)
    }
}

#[async_trait]
impl IdentitySecretStorage for InMemorySecretStorage {
    async fn load_secret_key(&self) -> Result<Option<SecretKey>, Self::Error> {
        Ok(self.secret.lock().await.as_ref().map(|secret| secret.0))
    }

    async fn store_secret_key_if_absent(
        &self,
        secret_key: &SecretKey,
    ) -> Result<SecretKey, Self::Error> {
        let mut stored = self.secret.lock().await;
        Ok(stored.get_or_insert_with(|| SecretKeyGuard(*secret_key)).0)
    }
}
