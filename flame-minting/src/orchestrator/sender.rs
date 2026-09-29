use std::sync::Arc;

use btc_integration::{
    BitcoinConfig, BitcoinConnection, BitcoinConnectionError, IdentityManager, MintingSendError,
    VoteSender as BitcoinVoteSender, VoteSignerError,
    btc::rpc::Core31RpcApi,
    identity::storage::{InMemorySecretStorage, MissingSecret},
};
use corepc_client::bitcoin::Txid;
use flamechain::BlockHash;
use tokio::sync::OnceCell;

use crate::minter::ports::VoteSender;

type InitializedSender = BitcoinVoteSender<Core31RpcApi, Arc<InMemorySecretStorage>>;

pub struct MintingSender {
    bitcoin_config: BitcoinConfig,
    identity_manager: Arc<IdentityManager>,
    sender: OnceCell<Arc<InitializedSender>>,
}

impl MintingSender {
    pub fn new(bitcoin_config: BitcoinConfig, identity_manager: Arc<IdentityManager>) -> Self {
        Self {
            bitcoin_config,
            identity_manager,
            sender: OnceCell::new(),
        }
    }

    pub fn identity_manager(&self) -> &IdentityManager {
        &self.identity_manager
    }

    pub async fn startup(&self) -> Result<(), SenderStartupError> {
        self.sender
            .get_or_try_init(|| async {
                let identity = self
                    .identity_manager
                    .identity()
                    .ok_or(SenderStartupError::IdentityNotInitialized)?;
                let connection = BitcoinConnection::votes(
                    self.bitcoin_config.clone(),
                    identity.clone(),
                    self.identity_manager.secret_storage().clone(),
                )
                .await?;
                Ok::<_, SenderStartupError>(connection.get_sender())
            })
            .await?;
        Ok(())
    }

    pub async fn send_vote(&self, height: u32, hash: BlockHash) -> Result<Txid, SenderError> {
        self.sender
            .get()
            .ok_or(SenderError::NotStarted)?
            .send_vote(height, hash)
            .await
            .map_err(SenderError::Send)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SenderStartupError {
    #[error("minting identity has not been initialized")]
    IdentityNotInitialized,
    #[error(transparent)]
    Connection(#[from] BitcoinConnectionError),
}

#[derive(Debug, thiserror::Error)]
pub enum SenderError {
    #[error("minting sender has not been started")]
    NotStarted,
    #[error(transparent)]
    Send(MintingSendError<VoteSignerError<MissingSecret>>),
}

impl VoteSender for MintingSender {
    type Error = SenderError;
    type TransactionId = Txid;

    async fn send_vote(&self, height: u32, hash: BlockHash) -> Result<Txid, Self::Error> {
        self.send_vote(height, hash).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use btc_integration::{BitcoinRpcAuth, IdentityConfig};
    use flamevm::Predicate;

    #[tokio::test]
    async fn startup_errors_leave_sender_uninitialized_and_retryable() {
        let identity = Arc::new(IdentityManager::new(
            Arc::new(InMemorySecretStorage::default()),
            IdentityConfig {
                flame_predicate: Predicate::opaque(Predicate::unspendable_key()),
                access_predicate: Predicate::opaque(Predicate::unspendable_key()),
                validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32])
                    .verifying_key(),
            },
        ));
        let sender = MintingSender::new(
            BitcoinConfig {
                node_rpc_url: "http://127.0.0.1:1".into(),
                auth: BitcoinRpcAuth::None,
            },
            identity.clone(),
        );
        assert!(matches!(
            sender.send_vote(1, BlockHash::from([1; 32])).await,
            Err(SenderError::NotStarted)
        ));
        assert!(matches!(
            sender.startup().await,
            Err(SenderStartupError::IdentityNotInitialized)
        ));
        identity.startup().await.unwrap();
        for _ in 0..2 {
            assert!(matches!(
                sender.startup().await,
                Err(SenderStartupError::Connection(_))
            ));
            assert!(sender.sender.get().is_none());
            assert!(matches!(
                sender.send_vote(1, BlockHash::from([1; 32])).await,
                Err(SenderError::NotStarted)
            ));
        }
    }
}
