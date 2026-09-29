use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use ed25519_dalek::SigningKey;
use tokio::sync::Barrier;

use super::*;
use crate::{SecretStorage, protocol::minter_witness_script};

fn config() -> IdentityConfig {
    IdentityConfig {
        flame_predicate: Predicate::opaque(Predicate::unspendable_key()),
        access_predicate: Predicate::opaque(
            curve25519_dalek::constants::RISTRETTO_BASEPOINT_POINT.compress(),
        ),
        validator_pubkey: SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
    }
}

#[tokio::test]
async fn generates_and_persists_a_secret_before_publishing_identity() {
    let storage = Arc::new(InMemorySecretStorage::default());
    let manager = IdentityManager::new(storage.clone(), config());
    assert!(manager.identity().is_none());
    assert!(storage.get_secret_key().await.is_err());

    let identity = manager.startup().await.unwrap().clone();
    let secret = storage.get_secret_key().await.unwrap();
    let public_key = secret.public_key(&Secp256k1::signing_only());
    let parsed = minter_witness_script::parse(identity.witness_script()).unwrap();
    assert_eq!(
        minter_witness_script::single_key_public_key(parsed.authorization),
        Some(public_key)
    );

    drop(manager);
    let restarted = IdentityManager::new(storage.clone(), config());
    assert_eq!(restarted.startup().await.unwrap(), &identity);
    assert_eq!(storage.get_secret_key().await.unwrap(), secret);
}

#[tokio::test]
async fn preserves_existing_secret_and_external_configuration() {
    let storage = Arc::new(InMemorySecretStorage::default());
    let secret = SecretKey::from_slice(&[0x41; 32]).unwrap();
    storage.store_secret_key_if_absent(&secret).await.unwrap();
    let external = config();
    let expected = MinterIdentity::single_key(
        &external.flame_predicate,
        &secret.public_key(&Secp256k1::signing_only()),
    );
    let manager = IdentityManager::new(storage.clone(), external.clone());

    assert_eq!(manager.startup().await.unwrap(), &expected);
    assert_eq!(storage.get_secret_key().await.unwrap(), secret);
    assert_eq!(
        manager.config().access_predicate.to_point(),
        external.access_predicate.to_point()
    );
    assert_eq!(manager.config().validator_pubkey, external.validator_pubkey);

    let other = SecretKey::from_slice(&[0x42; 32]).unwrap();
    assert_eq!(
        storage.store_secret_key_if_absent(&other).await.unwrap(),
        secret
    );
}

#[derive(Default)]
struct ControlledStorage {
    inner: InMemorySecretStorage,
    fail_read: AtomicBool,
    fail_write: AtomicBool,
    reads: AtomicUsize,
    writes: AtomicUsize,
    read_barrier: Option<Barrier>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum StorageError {
    #[error("read failed")]
    Read,
    #[error("write failed")]
    Write,
    #[error("secret missing")]
    Missing,
}

#[async_trait]
impl SecretStorage for ControlledStorage {
    type Error = StorageError;

    async fn get_secret_key(&self) -> Result<SecretKey, Self::Error> {
        self.inner
            .get_secret_key()
            .await
            .map_err(|_| StorageError::Missing)
    }
}

#[async_trait]
impl IdentitySecretStorage for ControlledStorage {
    async fn load_secret_key(&self) -> Result<Option<SecretKey>, Self::Error> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_read.swap(false, Ordering::SeqCst) {
            return Err(StorageError::Read);
        }
        let secret = self.inner.load_secret_key().await.unwrap();
        if let Some(barrier) = &self.read_barrier {
            barrier.wait().await;
        }
        Ok(secret)
    }

    async fn store_secret_key_if_absent(
        &self,
        secret: &SecretKey,
    ) -> Result<SecretKey, Self::Error> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if self.fail_write.swap(false, Ordering::SeqCst) {
            return Err(StorageError::Write);
        }
        Ok(self.inner.store_secret_key_if_absent(secret).await.unwrap())
    }
}

#[tokio::test]
async fn read_failure_does_not_create_a_new_secret() {
    let storage = Arc::new(ControlledStorage {
        fail_read: AtomicBool::new(true),
        ..Default::default()
    });
    let manager = IdentityManager::new(storage.clone(), config());

    assert!(matches!(
        manager.startup().await,
        Err(IdentityError::Storage(StorageError::Read))
    ));
    assert!(manager.identity().is_none());
    assert_eq!(storage.writes.load(Ordering::SeqCst), 0);
    assert!(storage.inner.load_secret_key().await.unwrap().is_none());
}

#[tokio::test]
async fn failed_write_leaves_identity_uninitialized_and_can_be_retried() {
    let storage = Arc::new(ControlledStorage {
        fail_write: AtomicBool::new(true),
        ..Default::default()
    });
    let manager = IdentityManager::new(storage.clone(), config());

    assert!(matches!(
        manager.startup().await,
        Err(IdentityError::Storage(StorageError::Write))
    ));
    assert!(manager.identity().is_none());
    assert!(storage.inner.load_secret_key().await.unwrap().is_none());

    manager.startup().await.unwrap();
    assert!(manager.identity().is_some());
    assert!(storage.inner.load_secret_key().await.unwrap().is_some());
}

#[tokio::test]
async fn repeated_and_concurrent_startup_initialize_once() {
    let storage = Arc::new(ControlledStorage::default());
    let manager = IdentityManager::new(storage.clone(), config());
    let (first, second) = tokio::join!(manager.startup(), manager.startup());
    assert_eq!(first.unwrap(), second.unwrap());
    manager.startup().await.unwrap();
    assert_eq!(storage.reads.load(Ordering::SeqCst), 1);
    assert_eq!(storage.writes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_managers_use_the_secret_that_was_stored_first() {
    let storage = Arc::new(ControlledStorage {
        read_barrier: Some(Barrier::new(2)),
        ..Default::default()
    });
    let first = IdentityManager::new(storage.clone(), config());
    let second = IdentityManager::new(storage.clone(), config());
    let (first, second) = tokio::join!(first.startup(), second.startup());
    let first = first.unwrap();
    assert_eq!(first, second.unwrap());
    assert_eq!(storage.writes.load(Ordering::SeqCst), 2);
    let secret = storage.get_secret_key().await.unwrap();
    let expected = MinterIdentity::single_key(
        &config().flame_predicate,
        &secret.public_key(&Secp256k1::signing_only()),
    );
    assert_eq!(first, &expected);
}
