use std::sync::Arc;

use anyhow::Result;
use btc_integration::{
    BitcoinConfig, BitcoinFacade, BitcoinRpcAuth, BtcBlockTip, MinterIdentity,
    btc::rpc::Core31RpcApi,
};
use corepc_client::bitcoin::{Address, Amount, Network, Transaction};

use crate::types::{BitcoinBlockHash, BitcoinBlockSnapshot, BitcoinChainSnapshot, BitcoinTxid};

use super::bitcoin_tip;

pub(super) struct BitcoinRegtest {
    node: Arc<bitcoind::BitcoinD>,
    rpc: BitcoinFacade<Core31RpcApi>,
    start_height: u64,
    blocks: Vec<BitcoinBlockSnapshot>,
}

impl BitcoinRegtest {
    pub async fn start() -> Result<Self> {
        let node = tokio::task::spawn_blocking(|| -> Result<_> {
            let node = bitcoind::BitcoinD::new(bitcoind::exe_path()?)?;
            let address = node.client.new_address()?;
            node.client.generate_to_address(2, &address)?;
            node.client.generate_to_descriptor(99, "raw(51)")?;
            Ok(Arc::new(node))
        })
        .await??;
        let rpc = BitcoinFacade::new(Arc::new(Core31RpcApi::new(
            &node.rpc_url(),
            BitcoinRpcAuth::CookieFile(node.params.cookie_file.clone()),
        )?));
        let start_height = rpc.best_block_tip().await?.height;
        Ok(Self {
            node,
            rpc,
            start_height,
            blocks: Vec::new(),
        })
    }

    pub fn config(&self) -> BitcoinConfig {
        BitcoinConfig {
            node_rpc_url: self.node.rpc_url(),
            auth: BitcoinRpcAuth::CookieFile(self.node.params.cookie_file.clone()),
        }
    }

    pub fn wallet_rpc_url(&self) -> String {
        self.node.rpc_url_with_wallet("default")
    }

    pub async fn tip(&self) -> Result<BtcBlockTip> {
        Ok(self.rpc.best_block_tip().await?)
    }

    pub async fn mine_block(&self) -> Result<BtcBlockTip> {
        let node = self.node.clone();
        tokio::task::spawn_blocking(move || node.client.generate_to_descriptor(1, "raw(51)"))
            .await??;
        self.tip().await
    }

    pub async fn fund_minter(&self, identity: &MinterIdentity) -> Result<()> {
        let node = self.node.clone();
        let address = Address::p2wsh(identity.witness_script(), Network::Regtest);
        tokio::task::spawn_blocking(move || {
            node.client
                .send_to_address(&address, Amount::from_sat(50_000))
        })
        .await??;
        Ok(())
    }

    pub async fn mempool(&self) -> Result<Vec<BitcoinTxid>> {
        let node = self.node.clone();
        let transactions =
            tokio::task::spawn_blocking(move || node.client.get_raw_mempool()).await??;
        Ok(transactions.0.into_iter().map(BitcoinTxid).collect())
    }

    pub async fn snapshot(&mut self) -> Result<(BitcoinChainSnapshot, Vec<Transaction>)> {
        let tip = self.tip().await?;
        let next_height = self
            .blocks
            .last()
            .map_or(self.start_height, |block| block.tip.height + 1);
        for height in next_height..=tip.height {
            let (hash, transactions) = self.rpc.transactions_at_height(height).await?;
            let header = self.rpc.block_header_info(hash).await?;
            self.blocks.push(BitcoinBlockSnapshot {
                tip: bitcoin_tip(header.tip),
                parent_hash: header
                    .previous_block_hash
                    .map(|hash| BitcoinBlockHash(hash.to_string())),
                transactions: transactions
                    .iter()
                    .map(|tx| BitcoinTxid(tx.compute_txid().to_string()))
                    .collect(),
            });
        }
        let node = self.node.clone();
        let pending = tokio::task::spawn_blocking(move || -> Result<Vec<Transaction>> {
            let mut txids = node.client.get_raw_mempool()?.0;
            txids.sort();
            txids
                .into_iter()
                .map(|txid| {
                    Ok(node
                        .client
                        .get_raw_transaction(txid.parse()?)?
                        .transaction()?)
                })
                .collect()
        })
        .await??;
        Ok((
            BitcoinChainSnapshot {
                start_height: self.start_height,
                tip: bitcoin_tip(tip),
                blocks: self.blocks.clone(),
                mempool: pending
                    .iter()
                    .map(|tx| BitcoinTxid(tx.compute_txid().to_string()))
                    .collect(),
            },
            pending,
        ))
    }
}
