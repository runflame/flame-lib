use std::sync::Arc;

use corepc_client::{
    bitcoin::Network,
    client_sync::{Auth, Error as RpcError},
};
use ed25519_dalek::VerifyingKey;
use flamevm::Predicate;

use crate::{
    btc::{bitcoin_facade::BitcoinFacade, minter_wallet, rpc::Core31RpcApi},
    protocol::{
        MinterIdentity, ProtocolIndexer, SecretStorage, Sender, TestSigner, VoteSigner,
        minter_witness_script,
    },
};

#[derive(Clone)]
pub struct BitcoinConfig {
    pub node_rpc_url: String,
    pub auth: Auth,
}

pub struct TestAcquisitionConfig {
    pub wallet_rpc_url: String,
    pub access_predicate: Predicate,
    pub validator_pubkey: VerifyingKey,
}

pub struct BitcoinConnection<S> {
    bitcoin: Arc<BitcoinFacade<Core31RpcApi>>,
    sender: Arc<Sender<Core31RpcApi, S>>,
}

impl<S: SecretStorage> BitcoinConnection<VoteSigner<Core31RpcApi, S>> {
    pub async fn votes(
        config: BitcoinConfig,
        identity: MinterIdentity,
        secret_storage: S,
    ) -> Result<Self, BitcoinConnectionError> {
        validate_identity(&identity)?;
        let bitcoin = Arc::new(BitcoinFacade::new(Arc::new(Core31RpcApi::new(
            &config.node_rpc_url,
            config.auth.clone(),
        )?)));
        let wallet = prepare_wallet(&config, &identity).await?;
        let signer = Arc::new(VoteSigner::new(secret_storage, wallet));
        let sender = Arc::new(Sender::new(Arc::clone(&bitcoin), signer, identity));
        Ok(Self { bitcoin, sender })
    }
}

impl<S: SecretStorage> BitcoinConnection<TestSigner<Core31RpcApi, S>> {
    /// Connects to regtest with a sender supporting votes and acquisitions.
    pub async fn regtest(
        config: BitcoinConfig,
        identity: MinterIdentity,
        secret_storage: S,
        acquisition: TestAcquisitionConfig,
    ) -> Result<Self, BitcoinConnectionError> {
        validate_identity(&identity)?;
        let bitcoin =
            connect_regtest_endpoint(&config.node_rpc_url, config.auth.clone(), "node").await?;
        let acquisition_wallet = connect_regtest_endpoint(
            &acquisition.wallet_rpc_url,
            config.auth.clone(),
            "acquisition wallet",
        )
        .await?;
        let wallet = prepare_wallet(&config, &identity).await?;
        let signer = Arc::new(TestSigner::new(
            acquisition_wallet,
            VoteSigner::new(secret_storage, wallet),
            acquisition.access_predicate,
            acquisition.validator_pubkey,
        ));
        let sender = Arc::new(Sender::new(Arc::clone(&bitcoin), signer, identity));
        Ok(Self { bitcoin, sender })
    }
}

async fn prepare_wallet(
    config: &BitcoinConfig,
    identity: &MinterIdentity,
) -> Result<Arc<BitcoinFacade<Core31RpcApi>>, BitcoinConnectionError> {
    let rpc = minter_wallet::prepare(&config.node_rpc_url, config.auth.clone(), identity).await?;
    Ok(Arc::new(BitcoinFacade::new(Arc::new(rpc))))
}

impl<S> BitcoinConnection<S> {
    pub fn get_sender(&self) -> Arc<Sender<Core31RpcApi, S>> {
        Arc::clone(&self.sender)
    }

    pub fn create_indexer(&self) -> ProtocolIndexer<Core31RpcApi> {
        ProtocolIndexer::new(Arc::clone(&self.bitcoin))
    }
}

fn validate_identity(identity: &MinterIdentity) -> Result<(), BitcoinConnectionError> {
    let parsed = minter_witness_script::parse(identity.witness_script())
        .map_err(|_| BitcoinConnectionError::UnsupportedAuthorization)?;
    minter_witness_script::single_key_public_key(parsed.authorization)
        .ok_or(BitcoinConnectionError::UnsupportedAuthorization)?;
    Ok(())
}

async fn connect_regtest_endpoint(
    url: &str,
    auth: Auth,
    endpoint: &'static str,
) -> Result<Arc<BitcoinFacade<Core31RpcApi>>, BitcoinConnectionError> {
    let rpc = Arc::new(Core31RpcApi::new(url, auth)?);
    let actual = rpc.network().await?;
    if actual != Network::Regtest {
        return Err(BitcoinConnectionError::UnexpectedNetwork {
            endpoint,
            actual,
            expected: vec![Network::Regtest],
        });
    }
    Ok(Arc::new(BitcoinFacade::new(rpc)))
}

#[derive(Debug, thiserror::Error)]
pub enum BitcoinConnectionError {
    #[error("Bitcoin Core RPC error: {0}")]
    Rpc(#[from] RpcError),
    #[error("{endpoint} uses {actual}, expected one of {expected:?}")]
    UnexpectedNetwork {
        endpoint: &'static str,
        actual: Network,
        expected: Vec<Network>,
    },
    #[error("the built-in signers require <compressed-pubkey> OP_CHECKSIG authorization")]
    UnsupportedAuthorization,
}
