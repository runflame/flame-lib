use crate::rpc::{BtcBlockTip, Core31RpcApi, RpcApi};
use bitcoind::anyhow::Context;
use bitcoind::mtype::SignRawTransaction;
use corepc_client::bitcoin::{Address, Amount};
use corepc_client::client_sync::Auth;
use corepc_client::client_sync::v17::{Input, Output};
use std::str::FromStr;
use std::sync::Arc;
use tokio::task::JoinHandle;

pub struct TestContext {
    pub rpc: Arc<Core31RpcApi>,
    pub node: bitcoind::BitcoinD,
    pub rpc_url: String,
    pub cookie_file: std::path::PathBuf,
}

impl TestContext {
    pub fn wait_for_next_block(&self, prev_block: BtcBlockTip) -> JoinHandle<BtcBlockTip> {
        let rpc = Arc::clone(&self.rpc);

        tokio::spawn(async move {
            rpc.wait_for_next_block(prev_block)
                .await
                .expect("wait for block 102")
        })
    }

    pub fn generate_next_block(
        &self,
    ) -> bitcoind::anyhow::Result<corepc_client::bitcoin::BlockHash> {
        let generated = self
            .node
            .client
            .generate_to_descriptor(1, "raw(51)")
            .expect("generate the next block");

        let hash = generated.0.into_iter().next().expect("one generated block");

        Ok(corepc_client::bitcoin::BlockHash::from_str(&hash)?)
    }

    pub fn send_simple_transaction(
        &self,
        recipient: Address,
        amount: Amount,
    ) -> bitcoind::anyhow::Result<SignRawTransaction> {
        let utxo = self
            .node
            .client
            .list_unspent()?
            .0
            .into_iter()
            .find(|utxo| utxo.spendable)
            .context("a mature spendable coinbase output")?;
        let input = Input {
            txid: utxo.txid.parse()?,
            vout: u64::try_from(utxo.vout).context("non-negative UTXO index")?,
            sequence: None,
        };
        let raw_transaction = self
            .node
            .client
            .create_raw_transaction(&[input], &[Output::new(recipient, amount)])?
            .transaction()
            .context("decode the raw transaction")?;
        let funded_transaction = self
            .node
            .client
            .fund_raw_transaction(&raw_transaction)?
            .transaction()
            .context("decode the funded transaction")?;
        let signed_transaction = self
            .node
            .client
            .sign_raw_transaction_with_wallet(&funded_transaction)?
            .into_model()
            .context("decode the signed transaction")?;

        Ok(signed_transaction)
    }
}

pub fn setup() -> bitcoind::anyhow::Result<TestContext> {
    let node = bitcoind::BitcoinD::from_downloaded()?;
    let rpc_url = format!("http://{}", node.params.rpc_socket);
    let cookie_file = node.params.cookie_file.clone();
    let rpc = Arc::new(Core31RpcApi::new(
        &rpc_url,
        Auth::CookieFile(cookie_file.clone()),
    )?);

    let mining_address = node.client.new_address()?;

    node.client.generate_to_address(2, &mining_address)?;
    node.client.generate_to_descriptor(99, "raw(51)")?;

    Ok(TestContext {
        rpc,
        node,
        rpc_url,
        cookie_file,
    })
}
