use std::sync::Arc;

use anyhow::{Context, Result};
use bitcoin::{BlockHash, Transaction, TxOut, Txid, hashes::Hash};
use btc_integration::{
    BitcoinConfig, BitcoinRpcAuth, BtcWallet,
    wallet::impls::RpcWallet,
    watcher::{BlockId, ChainSource, impls::RpcSource},
};

use crate::types::{BitcoinBlockHash, BitcoinBlockSnapshot, BitcoinChainSnapshot, BitcoinTxid};

use super::bitcoin_tip;

pub(super) struct BitcoinRegtest {
    node: Arc<bitcoind::BitcoinD>,
    source: RpcSource,
    /// The wallet of the node that has coins: it pays for the burns and
    /// for the first output of every minter.
    payer: RpcWallet,
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
        let config = BitcoinConfig {
            node_rpc_url: node.rpc_url(),
            auth: BitcoinRpcAuth::CookieFile(node.params.cookie_file.clone()),
        };
        let mut bitcoin = Self {
            source: RpcSource::new(&config)?,
            payer: RpcWallet::new(&config, "default")?,
            node,
            start_height: 0,
            blocks: Vec::new(),
        };
        bitcoin.start_height = bitcoin.tip().await?.height;
        Ok(bitcoin)
    }

    pub fn config(&self) -> BitcoinConfig {
        BitcoinConfig {
            node_rpc_url: self.node.rpc_url(),
            auth: BitcoinRpcAuth::CookieFile(self.node.params.cookie_file.clone()),
        }
    }

    pub async fn tip(&self) -> Result<BlockId> {
        // Any best block differs from the zero hash: the answer comes at once.
        Ok(self
            .source
            .wait_for_new_block(BlockHash::all_zeros())
            .await?)
    }

    pub async fn mine_block(&self) -> Result<BlockId> {
        let node = self.node.clone();
        tokio::task::spawn_blocking(move || node.client.generate_to_descriptor(1, "raw(51)"))
            .await??;
        self.tip().await
    }

    /// Pays `output` from the wallet that has coins.
    pub async fn pay(&self, output: TxOut) -> Result<Txid> {
        Ok(self.payer.send(output).await?)
    }

    pub async fn mempool(&self) -> Result<Vec<BitcoinTxid>> {
        let node = self.node.clone();
        let transactions =
            tokio::task::spawn_blocking(move || node.client.get_raw_mempool()).await??;
        Ok(transactions.0.into_iter().map(BitcoinTxid).collect())
    }

    pub async fn snapshot(&mut self) -> Result<(BitcoinChainSnapshot, Vec<Transaction>)> {
        let tip = self.tip().await?;
        let next_height = self.start_height + self.blocks.len() as u64;
        for height in next_height..=tip.height {
            let id = self.source.block_at(height).await?;
            let id = id.context("the best chain became shorter")?;
            let (block, _) = self
                .source
                .block(id.hash)
                .await?
                .context("the node does not have a block of its best chain")?;
            self.blocks.push(BitcoinBlockSnapshot {
                tip: bitcoin_tip(id),
                parent_hash: Some(BitcoinBlockHash(block.header.prev_blockhash.to_string())),
                transactions: block
                    .txdata
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
