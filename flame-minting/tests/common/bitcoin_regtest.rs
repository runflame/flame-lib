use std::time::Duration;

use bitcoin::{BlockHash, Txid, hashes::Hash};
use bitcoind::anyhow::{Context, Result};
use btc_integration::{
    AuthenticatedMintingEndorsement, BitcoinConfig, BitcoinRecords, BitcoinRpcAuth,
    MintingEndorsementData, UncheckedMintingEndorsement,
    wallet::impls::RpcWallet,
    watcher::{BlockId, ChainSource, impls::RpcSource},
};
use tokio::time::{sleep, timeout};

/// A regtest bitcoind of its own, with a wallet that has coins.
pub struct BitcoinRegtest {
    pub node: bitcoind::BitcoinD,
    /// The wallet of the node that has coins.
    pub wallet: RpcWallet,
    source: RpcSource,
}

impl BitcoinRegtest {
    pub fn new() -> Result<Self> {
        let node = bitcoind::BitcoinD::from_downloaded()?;
        let address = node.client.new_address()?;
        node.client.generate_to_address(2, &address)?;
        node.client.generate_to_descriptor(99, "raw(51)")?;
        let config = bitcoin_config(&node);
        Ok(Self {
            wallet: RpcWallet::new(&config, "default")?,
            source: RpcSource::new(&config)?,
            node,
        })
    }

    pub fn bitcoin_config(&self) -> BitcoinConfig {
        bitcoin_config(&self.node)
    }

    /// The best block of the node.
    pub async fn best(&self) -> Result<BlockId> {
        // Any best block differs from the zero hash: the answer comes at once.
        Ok(self
            .source
            .wait_for_new_block(BlockHash::all_zeros())
            .await?)
    }

    pub async fn mine_block(&self) -> Result<BlockId> {
        self.node.client.generate_to_descriptor(1, "raw(51)")?;
        self.best().await
    }

    pub async fn wait_for_endorsement(&self, expected: &MintingEndorsementData) -> Result<Txid> {
        timeout(Duration::from_secs(10), async {
            loop {
                for txid in self.node.client.get_raw_mempool()?.0 {
                    let txid: Txid = txid.parse()?;
                    let transaction = self.node.client.get_raw_transaction(txid)?.transaction()?;
                    if UncheckedMintingEndorsement::from_tx(&transaction)
                        .iter()
                        .any(|endorsement| &endorsement.output().data == expected)
                    {
                        return Ok(txid);
                    }
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("timed out waiting for the automatic endorsement in the mempool")?
    }

    pub async fn endorsements_in_block(
        &self,
        block: BlockId,
    ) -> Result<Vec<AuthenticatedMintingEndorsement>> {
        let found = self.source.block(block.hash).await?;
        let (block, prevouts) = found.context("the node does not have the block")?;
        Ok(BitcoinRecords::from_block(&block, &prevouts)?.endorsements)
    }
}

fn bitcoin_config(node: &bitcoind::BitcoinD) -> BitcoinConfig {
    BitcoinConfig {
        node_rpc_url: node.rpc_url(),
        auth: BitcoinRpcAuth::CookieFile(node.params.cookie_file.clone()),
    }
}
