use bitcoind::anyhow::Context;
use bitcoind::mtype::SignRawTransaction;
use btc_integration::{BlockTip, Core31RpcApi, RpcApi};
use corepc_client::bitcoin::Address;
use corepc_client::{
    bitcoin::Amount,
    client_sync::Auth,
    client_sync::v31::{Input, Output},
};
use std::str::FromStr;
use std::thread;

struct TestContext {
    pub rpc: Core31RpcApi,
    pub node: bitcoind::BitcoinD,
    pub mining_address: Address,
    pub rpc_url: String,
    pub cookie_file: std::path::PathBuf,
}

impl TestContext {
    fn wait_for_next_block(&self, prev_block: BlockTip) -> thread::JoinHandle<BlockTip> {
        let waiter_url = self.rpc_url.clone();
        let waiter_cookie = self.cookie_file.clone();

        thread::spawn(move || {
            let rpc = Core31RpcApi::new(&waiter_url, Auth::CookieFile(waiter_cookie))
                .expect("create waiting RPC client");

            rpc.wait_for_next_block(prev_block)
                .expect("wait for block 102")
        })
    }

    fn generate_next_block(&self) -> bitcoind::anyhow::Result<corepc_client::bitcoin::BlockHash> {
        let generated = self
            .node
            .client
            .generate_to_address(1, &self.mining_address)
            .expect("generate the next block");

        let hash = generated.0.into_iter().next().expect("one generated block");

        Ok(corepc_client::bitcoin::BlockHash::from_str(&hash)?)
    }

    fn send_simple_transaction(
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

fn setup() -> bitcoind::anyhow::Result<TestContext> {
    let node = bitcoind::BitcoinD::from_downloaded()?;
    let rpc_url = format!("http://{}", node.params.rpc_socket);
    let cookie_file = node.params.cookie_file.clone();
    let rpc = Core31RpcApi::new(&rpc_url, Auth::CookieFile(cookie_file.clone()))?;

    let mining_address = node.client.new_address()?;
    let generated = node.client.generate_to_address(101, &mining_address)?;
    assert_eq!(generated.0.len(), 101);

    Ok(TestContext {
        rpc,
        node,
        mining_address,
        rpc_url,
        cookie_file,
    })
}

#[test]
fn core31_rpc_api_works_with_regtest_blocks_and_transactions() -> bitcoind::anyhow::Result<()> {
    let ctx = setup()?;
    let rpc = &ctx.rpc;

    let (block_hash, transactions_at_height) = rpc
        .transactions_at_height(1)
        .context("get transactions at height 1")?;
    let transactions_by_hash = rpc
        .transactions_in_block(block_hash)
        .context("get transactions by block hash")?;
    assert_eq!(transactions_at_height, transactions_by_hash);
    assert_eq!(transactions_at_height.len(), 1);

    let (previous_tip_hash, _) = rpc
        .transactions_at_height(101)
        .context("get transactions at height 101")?;
    let previous_tip = BlockTip {
        hash: previous_tip_hash,
        height: 101,
    };

    let waiter = ctx.wait_for_next_block(previous_tip);
    let next_hash = ctx.generate_next_block()?;

    let next_tip = waiter.join().expect("wait for next block");

    assert_eq!(next_tip.height, 102);
    assert_eq!(next_tip.hash, next_hash);

    let recipient = ctx.node.client.new_address()?;
    let amount = Amount::from_btc(1.0).expect("valid BTC amount");
    let signed_transaction = ctx
        .send_simple_transaction(recipient, amount)
        .context("send a simple transaction")?;

    assert!(signed_transaction.complete);

    let expected_txid = signed_transaction.tx.compute_txid();
    let published_txid = rpc
        .publish_transaction(&signed_transaction.tx)
        .context("publish the signed transaction")?;
    assert_eq!(published_txid, expected_txid);

    Ok(())
}
