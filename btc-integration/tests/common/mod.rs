//! What the regtest tests share: a bitcoind of its own for a test.
//!
//! The binary of bitcoind is downloaded at build time.

// Every test file compiles this module for itself and uses a part of it.
#![allow(dead_code)]

use std::str::FromStr;

use bitcoin::{Address, Amount, Block, BlockHash, TxOut, Txid};
use bitcoind::anyhow::Context;
use btc_integration::wallet::impls::RpcWallet;
use btc_integration::watcher::impls::RpcSource;
use btc_integration::watcher::{ChainSource, Prevouts};
use btc_integration::{BitcoinConfig, BitcoinRpcAuth, BtcWallet};

pub struct TestContext {
    /// The wallet of the node that has coins.
    pub wallet: RpcWallet,
    /// Reads the blocks of the node the way the watcher does.
    pub source: RpcSource,
    pub node: bitcoind::BitcoinD,
    pub cookie_file: std::path::PathBuf,
}

impl TestContext {
    pub fn bitcoin_config(&self) -> BitcoinConfig {
        bitcoin_config(&self.node)
    }

    /// A block of the node with the outputs its inputs spend.
    pub async fn block(&self, hash: BlockHash) -> bitcoind::anyhow::Result<(Block, Prevouts)> {
        self.source
            .block(hash)
            .await?
            .context("the node does not have the block")
    }

    pub fn generate_next_block(&self) -> bitcoind::anyhow::Result<BlockHash> {
        let generated = self
            .node
            .client
            .generate_to_descriptor(1, "raw(51)")
            .expect("generate the next block");

        let hash = generated.0.into_iter().next().expect("one generated block");

        Ok(BlockHash::from_str(&hash)?)
    }

    /// Pays `amount` to `recipient` from the wallet that has coins, and mines
    /// the block that confirms the payment.
    pub async fn pay(&self, recipient: Address, amount: Amount) -> bitcoind::anyhow::Result<Txid> {
        let output = TxOut {
            value: amount,
            script_pubkey: recipient.script_pubkey(),
        };
        let txid = self.wallet.send(output).await?;
        self.generate_next_block()?;
        Ok(txid)
    }
}

fn bitcoin_config(node: &bitcoind::BitcoinD) -> BitcoinConfig {
    BitcoinConfig {
        node_rpc_url: node.rpc_url(),
        auth: BitcoinRpcAuth::CookieFile(node.params.cookie_file.clone()),
    }
}

pub fn setup() -> bitcoind::anyhow::Result<TestContext> {
    let node = bitcoind::BitcoinD::from_downloaded()?;
    let cookie_file = node.params.cookie_file.clone();
    let bitcoin = bitcoin_config(&node);
    let wallet = RpcWallet::new(&bitcoin, "default")?;
    let source = RpcSource::new(&bitcoin)?;

    let mining_address = node.client.new_address()?;

    node.client.generate_to_address(2, &mining_address)?;
    node.client.generate_to_descriptor(99, "raw(51)")?;

    Ok(TestContext {
        wallet,
        source,
        node,
        cookie_file,
    })
}
