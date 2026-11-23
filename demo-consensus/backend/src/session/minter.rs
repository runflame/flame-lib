use std::sync::Arc;

use anyhow::Result;
use bitcoin::{Address, Amount, Network, TxOut, Txid};
use btc_integration::{
    BtcWallet, BurnData, IdentityConfig, MinterIdentity, MinterWallet, MintingEndorsementData,
    identity::impls::LocalKey, wallet::impls::RpcWatchOnlyWallet,
};
use flame_minting::consensus::{ConsensusStorage, storage::InMemoryConsensusStorage};
use flamechain::BlockHash;

use crate::types::{MinterId, MinterSnapshot};

use super::bitcoin::BitcoinRegtest;

/// What a minter starts with at its witness script: it pays the fees of the
/// endorsements.
const MINTER_FUNDING: Amount = Amount::from_sat(50_000);

pub(super) struct DemoMinter {
    pub id: MinterId,
    pub name: String,
    pub identity: MinterIdentity,
    burn: BurnData,
    wallet: MinterWallet<RpcWatchOnlyWallet, Arc<LocalKey>>,
}

impl DemoMinter {
    /// The minter of `config` that signs with `key`, with [`MINTER_FUNDING`]
    /// on the way to its witness script.
    pub async fn create(
        id: MinterId,
        name: String,
        config: &IdentityConfig,
        key: Arc<LocalKey>,
        bitcoin: &BitcoinRegtest,
    ) -> Result<Self> {
        let identity = config.identity(&key);
        let script = identity.witness_script();
        // The wallet first: it watches the script from the moment it is
        // opened.
        let watch_only = RpcWatchOnlyWallet::open(&bitcoin.config(), script).await?;
        let funding = TxOut {
            value: MINTER_FUNDING,
            script_pubkey: script.to_p2wsh(),
        };
        bitcoin.pay(funding).await?;
        Ok(Self {
            id,
            name,
            burn: BurnData::new(identity.p2wsh(), config.access_predicate.clone()),
            wallet: MinterWallet::new(watch_only, key, identity.clone())?,
            identity,
        })
    }

    pub async fn send_acquisition(&self, bitcoin: &BitcoinRegtest, amount: Amount) -> Result<Txid> {
        bitcoin.pay(self.burn.to_output(amount)).await
    }

    pub async fn send_endorsement(&self, core_height: u32, hash: BlockHash) -> Result<Txid> {
        let endorsement = MintingEndorsementData::V1 {
            flame_block_height: core_height,
            flame_block_hash: hash,
        };
        Ok(self.wallet.send(endorsement.to_output()).await?)
    }

    pub async fn snapshot(&self, consensus: &InMemoryConsensusStorage) -> Result<MinterSnapshot> {
        Ok(MinterSnapshot {
            id: self.id,
            name: self.name.clone(),
            p2wsh_address: Address::p2wsh(self.identity.witness_script(), Network::Regtest)
                .to_string(),
            automatic_endorsement: false,
            is_double_signed: consensus
                .is_minter_double_signed(&self.identity.p2wsh())
                .await?,
        })
    }
}
