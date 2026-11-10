use std::{sync::Arc, time::Duration};

use bitcoind::anyhow::{Context, Result};
use btc_integration::{
    AuthenticatedMintingEndorsement, UncheckedMintingEndorsement, btc::rpc::Core31RpcApi,
    protocol::validate_transaction_endorsements,
};
use btc_integration::{
    BitcoinConfig, BitcoinFacade, BitcoinNetwork, BitcoinRpcAuth, BtcBlockTip,
    MintingEndorsementData,
};
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
            network: BitcoinNetwork::Regtest,
        }
    }

    pub async fn mine_block(&self) -> Result<BtcBlockTip> {
        self.node.client.generate_to_descriptor(1, "raw(51)")?;
        Ok(self.rpc.best_block_tip().await?)
    }

    pub async fn wait_for_endorsement(&self, expected: &MintingEndorsementData) -> Result<Txid> {
        timeout(Duration::from_secs(10), async {
            loop {
                for txid in self.node.client.get_raw_mempool()?.0 {
                    let txid = txid.parse()?;
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
        block: BtcBlockTip,
    ) -> Result<Vec<AuthenticatedMintingEndorsement>> {
        let transactions = self
            .rpc
            .transactions_with_prevouts_in_block(block.hash)
            .await?;
        let mut endorsements = Vec::new();
        for transaction in &transactions {
            endorsements.extend(validate_transaction_endorsements(transaction)?);
        }
        Ok(endorsements)
    }
}
