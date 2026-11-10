use std::{fmt, str::FromStr, sync::Arc};

use corepc_client::{
    bitcoin::Network,
    client_sync::{Auth, Error as RpcError},
};
use flamevm::Predicate;

use crate::{
    btc::{bitcoin_facade::BitcoinFacade, minter_wallet, rpc::Core31RpcApi},
    protocol::{
        EndorsementSigner, MinterIdentity, ProtocolIndexer, SecretStorage, Sender, TestSigner,
        minter_witness_script,
    },
};

/// The Bitcoin a node follows. Every endpoint is checked against it on connect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitcoinNetwork {
    /// A regtest bitcoind; the only network where test acquisitions are sent.
    Regtest,
    /// The overlay service: real mainnet headers with simulated burns. It reports
    /// the mainnet chain and also answers `getoverlayinfo`.
    Overlay,
    /// A real mainnet bitcoind.
    Mainnet,
}

impl fmt::Display for BitcoinNetwork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Regtest => "regtest",
            Self::Overlay => "overlay",
            Self::Mainnet => "mainnet",
        })
    }
}

impl FromStr for BitcoinNetwork {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "regtest" => Ok(Self::Regtest),
            "overlay" => Ok(Self::Overlay),
            "mainnet" => Ok(Self::Mainnet),
            _ => Err(format!(
                "unknown Bitcoin network {value:?}, expected regtest, overlay or mainnet"
            )),
        }
    }
}

#[derive(Clone)]
pub struct BitcoinConfig {
    pub node_rpc_url: String,
    pub auth: Auth,
    pub network: BitcoinNetwork,
}

pub struct TestAcquisitionConfig {
    pub wallet_rpc_url: String,
    pub access_predicate: Predicate,
}

pub struct BitcoinConnection<S> {
    bitcoin: Arc<BitcoinFacade<Core31RpcApi>>,
    sender: Arc<Sender<Core31RpcApi, S>>,
}

impl<S: SecretStorage> BitcoinConnection<EndorsementSigner<Core31RpcApi, S>> {
    pub async fn endorsements(
        config: BitcoinConfig,
        identity: MinterIdentity,
        secret_storage: S,
    ) -> Result<Self, BitcoinConnectionError> {
        validate_identity(&identity)?;
        let bitcoin = connect_endpoint(
            &config.node_rpc_url,
            config.auth.clone(),
            config.network,
            "node",
        )
        .await?;
        let wallet = prepare_wallet(&config, &identity).await?;
        let signer = Arc::new(EndorsementSigner::new(secret_storage, wallet));
        let sender = Arc::new(Sender::new(Arc::clone(&bitcoin), signer, identity));
        Ok(Self { bitcoin, sender })
    }
}

impl<S: SecretStorage> BitcoinConnection<TestSigner<Core31RpcApi, S>> {
    /// Connects to regtest with a sender supporting endorsements and acquisitions.
    pub async fn regtest(
        config: BitcoinConfig,
        identity: MinterIdentity,
        secret_storage: S,
        acquisition: TestAcquisitionConfig,
    ) -> Result<Self, BitcoinConnectionError> {
        validate_identity(&identity)?;
        if config.network != BitcoinNetwork::Regtest {
            return Err(BitcoinConnectionError::TestAcquisitionsOutsideRegtest(
                config.network,
            ));
        }
        let bitcoin = connect_endpoint(
            &config.node_rpc_url,
            config.auth.clone(),
            config.network,
            "node",
        )
        .await?;
        let acquisition_wallet = connect_endpoint(
            &acquisition.wallet_rpc_url,
            config.auth.clone(),
            config.network,
            "acquisition wallet",
        )
        .await?;
        let wallet = prepare_wallet(&config, &identity).await?;
        let signer = Arc::new(TestSigner::new(
            acquisition_wallet,
            EndorsementSigner::new(secret_storage, wallet),
            acquisition.access_predicate,
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

/// Connects a read-only protocol indexer: no wallet, no sender.
pub async fn connect_indexer(
    config: &BitcoinConfig,
) -> Result<ProtocolIndexer<Core31RpcApi>, BitcoinConnectionError> {
    let bitcoin = connect_endpoint(
        &config.node_rpc_url,
        config.auth.clone(),
        config.network,
        "node",
    )
    .await?;
    Ok(ProtocolIndexer::new(bitcoin))
}

async fn connect_endpoint(
    url: &str,
    auth: Auth,
    expected: BitcoinNetwork,
    endpoint: &'static str,
) -> Result<Arc<BitcoinFacade<Core31RpcApi>>, BitcoinConnectionError> {
    let rpc = Arc::new(Core31RpcApi::new(url, auth)?);
    let chain = rpc.network().await?;
    let actual = match chain {
        Network::Regtest => Some(BitcoinNetwork::Regtest),
        // A mainnet node configured as Mainnet must never be the overlay's fake burns.
        Network::Bitcoin if rpc.is_overlay().await? => Some(BitcoinNetwork::Overlay),
        Network::Bitcoin => Some(BitcoinNetwork::Mainnet),
        _ => None,
    };
    if actual != Some(expected) {
        return Err(BitcoinConnectionError::UnexpectedNetwork {
            endpoint,
            expected,
            actual: actual.map_or_else(|| chain.to_string(), |network| network.to_string()),
        });
    }
    Ok(Arc::new(BitcoinFacade::new(rpc)))
}

#[derive(Debug, thiserror::Error)]
pub enum BitcoinConnectionError {
    #[error("Bitcoin Core RPC error: {0}")]
    Rpc(#[from] RpcError),
    #[error("{endpoint} is {actual}, expected {expected}")]
    UnexpectedNetwork {
        endpoint: &'static str,
        expected: BitcoinNetwork,
        actual: String,
    },
    #[error("test acquisitions are sent only on regtest, not on {0}")]
    TestAcquisitionsOutsideRegtest(BitcoinNetwork),
    #[error("the built-in signers require <compressed-pubkey> OP_CHECKSIG authorization")]
    UnsupportedAuthorization,
}
