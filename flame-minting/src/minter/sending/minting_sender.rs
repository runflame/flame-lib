use crate::minter::sending::endorsement_sender::EndorsementSender;
use bitcoin::Txid;
use btc_integration::{
    BitcoinConfig, MinterIdentity, MinterWallet, WalletError, identity::impls::LocalKey,
    wallet::impls::RpcWatchOnlyWallet,
};
use flamechain::BlockHash;
use std::sync::Arc;
use tokio::sync::OnceCell;

pub struct MintingSender {
    bitcoin_config: BitcoinConfig,
    identity: MinterIdentity,
    key: Arc<LocalKey>,
    wallet: OnceCell<MinterWallet<RpcWatchOnlyWallet, Arc<LocalKey>>>,
}

impl MintingSender {
    pub fn new(
        bitcoin_config: BitcoinConfig,
        identity: MinterIdentity,
        key: Arc<LocalKey>,
    ) -> Self {
        Self {
            bitcoin_config,
            identity,
            key,
            wallet: OnceCell::new(),
        }
    }

    pub fn identity(&self) -> &MinterIdentity {
        &self.identity
    }

    pub async fn startup(&self) -> Result<(), SenderStartupError> {
        self.wallet
            .get_or_try_init(|| async {
                let script = self.identity.witness_script();
                let watch_only = RpcWatchOnlyWallet::open(&self.bitcoin_config, script).await?;
                MinterWallet::new(watch_only, self.key.clone(), self.identity.clone())
            })
            .await?;
        Ok(())
    }

    pub async fn send_endorsement(
        &self,
        height: u32,
        hash: BlockHash,
    ) -> Result<Txid, SenderError> {
        let wallet = self.wallet.get().ok_or(SenderError::NotStarted)?;
        Ok(wallet.send_endorsement(height, hash).await?)
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct SenderStartupError(#[from] pub WalletError);

#[derive(Debug, thiserror::Error)]
pub enum SenderError {
    #[error("minting sender has not been started")]
    NotStarted,
    #[error(transparent)]
    Send(#[from] WalletError),
}

impl EndorsementSender for MintingSender {
    type Error = SenderError;
    type TransactionId = Txid;

    async fn send_endorsement(&self, height: u32, hash: BlockHash) -> Result<Txid, Self::Error> {
        self.send_endorsement(height, hash).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use btc_integration::{BitcoinRpcAuth, IdentityConfig};
    use flamevm::Predicate;

    #[tokio::test]
    async fn startup_errors_leave_sender_uninitialized_and_retryable() {
        let key = LocalKey::generate();
        let identity = IdentityConfig {
            flame_predicate: Predicate::opaque(Predicate::unspendable_key()),
            access_predicate: Predicate::opaque(Predicate::unspendable_key()),
            validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        }
        .identity(&key);
        let sender = MintingSender::new(
            BitcoinConfig {
                node_rpc_url: "http://127.0.0.1:1".into(),
                auth: BitcoinRpcAuth::None,
            },
            identity,
            Arc::new(key),
        );
        for _ in 0..2 {
            assert!(matches!(
                sender.startup().await,
                Err(SenderStartupError(WalletError::Unreachable(_)))
            ));
            assert!(sender.wallet.get().is_none());
            assert!(matches!(
                sender.send_endorsement(1, BlockHash::from([1; 32])).await,
                Err(SenderError::NotStarted)
            ));
        }
    }
}
