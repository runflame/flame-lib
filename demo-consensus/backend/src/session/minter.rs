use std::sync::Arc;

use anyhow::Result;
use btc_integration::{
    BitcoinConnection, IdentityManager, MinterIdentity, TestAcquisitionConfig, TestSender,
    btc::rpc::Core31RpcApi, identity::storage::InMemorySecretStorage,
};
use corepc_client::bitcoin::{Address, Network};
use flame_minting::consensus::{ConsensusStorage, storage::InMemoryConsensusStorage};

use crate::types::{MinterId, MinterSnapshot};

use super::bitcoin::BitcoinRegtest;

pub(super) struct DemoMinter {
    pub id: MinterId,
    pub name: String,
    pub identity: MinterIdentity,
    pub sender: Arc<TestSender<Core31RpcApi, Arc<InMemorySecretStorage>>>,
}

impl DemoMinter {
    pub async fn create(
        id: MinterId,
        name: String,
        manager: &IdentityManager,
        bitcoin: &BitcoinRegtest,
    ) -> Result<Self> {
        let identity = manager.startup().await?.clone();
        let config = manager.config();
        let connection = BitcoinConnection::regtest(
            bitcoin.config(),
            identity.clone(),
            manager.secret_storage().clone(),
            TestAcquisitionConfig {
                wallet_rpc_url: bitcoin.wallet_rpc_url(),
                access_predicate: config.access_predicate.clone(),
                validator_pubkey: config.validator_pubkey,
            },
        )
        .await?;
        bitcoin.fund_minter(&identity).await?;
        Ok(Self {
            id,
            name,
            identity,
            sender: connection.get_sender(),
        })
    }

    pub async fn snapshot(&self, consensus: &InMemoryConsensusStorage) -> Result<MinterSnapshot> {
        Ok(MinterSnapshot {
            id: self.id,
            name: self.name.clone(),
            p2wsh_address: Address::p2wsh(self.identity.witness_script(), Network::Regtest)
                .to_string(),
            automatic_voting: false,
            is_double_signed: consensus
                .is_minter_double_signed(&self.identity.p2wsh())
                .await?,
        })
    }
}
