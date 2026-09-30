use std::{sync::Arc, time::Duration};

use bitcoind::anyhow::{Context, Result};
use btc_integration::{
    AuthenticatedMintingVote, UncheckedMintingVote, btc::rpc::Core31RpcApi,
    protocol::validate_transaction_votes,
};
use btc_integration::{BitcoinConfig, BitcoinFacade, BitcoinRpcAuth, BtcBlockTip, MintingVoteData};
use corepc_client::bitcoin::Txid;
use tokio::time::{sleep, timeout};

pub struct BitcoinRegtest {
    pub node: bitcoind::BitcoinD,
    pub rpc: BitcoinFacade<Core31RpcApi>,
}

impl BitcoinRegtest {
    pub fn new() -> Result<Self> {
        let node = bitcoind::BitcoinD::from_downloaded()?;
        let address = node.client.new_address()?;
        node.client.generate_to_address(2, &address)?;
        node.client.generate_to_descriptor(99, "raw(51)")?;
        let rpc = BitcoinFacade::new(Arc::new(Core31RpcApi::new(
            &node.rpc_url(),
            BitcoinRpcAuth::CookieFile(node.params.cookie_file.clone()),
        )?));
        Ok(Self { node, rpc })
    }

    pub fn config(&self) -> BitcoinConfig {
        BitcoinConfig {
            node_rpc_url: self.node.rpc_url(),
            auth: BitcoinRpcAuth::CookieFile(self.node.params.cookie_file.clone()),
        }
    }

    pub async fn mine_block(&self) -> Result<BtcBlockTip> {
        self.node.client.generate_to_descriptor(1, "raw(51)")?;
        Ok(self.rpc.best_block_tip().await?)
    }

    pub async fn wait_for_vote(&self, expected: &MintingVoteData) -> Result<Txid> {
        timeout(Duration::from_secs(10), async {
            loop {
                for txid in self.node.client.get_raw_mempool()?.0 {
                    let txid = txid.parse()?;
                    let transaction = self.node.client.get_raw_transaction(txid)?.transaction()?;
                    if UncheckedMintingVote::from_tx(&transaction)
                        .iter()
                        .any(|vote| &vote.output().data == expected)
                    {
                        return Ok(txid);
                    }
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("timed out waiting for the automatic vote in the mempool")?
    }

    pub async fn votes_in_block(
        &self,
        block: BtcBlockTip,
    ) -> Result<Vec<AuthenticatedMintingVote>> {
        let transactions = self
            .rpc
            .transactions_with_prevouts_in_block(block.hash)
            .await?;
        let mut votes = Vec::new();
        for transaction in &transactions {
            votes.extend(validate_transaction_votes(transaction)?);
        }
        Ok(votes)
    }
}
