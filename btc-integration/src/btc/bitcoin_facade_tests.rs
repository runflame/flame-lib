use super::*;
use crate::btc::rpc::{BtcFundOptions, BtcSignedTransaction, BtcUnspentOutput};
use async_trait::async_trait;
use corepc_client::bitcoin::{OutPoint, TxOut, absolute, hashes::Hash, transaction};
use std::{collections::VecDeque, sync::Mutex};

#[derive(Default)]
struct StubRpc {
    utxos: Vec<BtcUnspentOutput>,
    tips: Mutex<VecDeque<BtcBlockTip>>,
    waits: Mutex<Vec<(BlockHash, u64)>>,
    funding: Mutex<Vec<(Transaction, BtcFundOptions)>>,
    fail_first_funding: bool,
    corrupt_inputs: bool,
    reverse_inputs: bool,
    incomplete_signature: bool,
}

#[async_trait]
impl RpcApi for StubRpc {
    async fn best_block_tip(&self) -> Result<BtcBlockTip> {
        unreachable!()
    }
    async fn block_header_info(&self, _: BlockHash) -> Result<BtcBlockHeaderInfo> {
        unreachable!()
    }
    async fn transactions_in_block(&self, _: BlockHash) -> Result<Vec<Transaction>> {
        unreachable!()
    }
    async fn transactions_with_prevouts_in_block(
        &self,
        _: BlockHash,
    ) -> Result<Vec<BtcTransactionWithPrevouts>> {
        unreachable!()
    }
    async fn block_hash_at_height(&self, _: u64) -> Result<BlockHash> {
        unreachable!()
    }
    async fn send_raw_transaction(&self, _: &Transaction, _: Option<Amount>) -> Result<Txid> {
        panic!("funding and signing must not publish")
    }
    async fn wait_for_new_block(
        &self,
        current_tip: BlockHash,
        timeout_ms: u64,
    ) -> Result<BtcBlockTip> {
        self.waits.lock().unwrap().push((current_tip, timeout_ms));
        Ok(self.tips.lock().unwrap().pop_front().expect("queued tip"))
    }
    async fn list_unspent(&self) -> Result<Vec<BtcUnspentOutput>> {
        Ok(self.utxos.clone())
    }
    async fn fund_raw_transaction(
        &self,
        tx: &Transaction,
        options: &BtcFundOptions,
    ) -> Result<Transaction> {
        let mut calls = self.funding.lock().unwrap();
        calls.push((tx.clone(), options.clone()));
        if self.fail_first_funding && calls.len() == 1 {
            return Err(Error::Returned("insufficient funds".into()));
        }
        let mut tx = tx.clone();
        if self.corrupt_inputs {
            tx.input[0].previous_output = OutPoint::null();
        }
        if self.reverse_inputs {
            tx.input.reverse();
        }
        Ok(tx)
    }
    async fn sign_raw_transaction_with_wallet(
        &self,
        tx: &Transaction,
    ) -> Result<BtcSignedTransaction> {
        Ok(BtcSignedTransaction {
            transaction: tx.clone(),
            complete: !self.incomplete_signature,
            errors: vec!["missing key".into()],
        })
    }
}

fn template() -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![],
        output: vec![TxOut {
            value: Amount::from_sat(100),
            script_pubkey: ScriptBuf::new(),
        }],
    }
}

fn utxo(id: u8, value: u64) -> BtcUnspentOutput {
    BtcUnspentOutput {
        outpoint: OutPoint::new(Txid::from_byte_array([id; 32]), 0),
        prevout: TxOut {
            value: Amount::from_sat(value),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        },
        address: "script-change-address".into(),
        confirmations: 1,
        spendable: true,
        safe: true,
    }
}

#[tokio::test]
async fn unchanged_tip_is_polled_again_in_facade() {
    let previous = BtcBlockTip {
        hash: BlockHash::from_byte_array([1; 32]),
        height: 1,
    };
    let next = BtcBlockTip {
        hash: BlockHash::from_byte_array([2; 32]),
        height: 2,
    };
    let rpc = Arc::new(StubRpc {
        tips: Mutex::new(VecDeque::from([previous, previous, next])),
        ..Default::default()
    });
    let bitcoin = BitcoinFacade::new(Arc::clone(&rpc));
    assert_eq!(bitcoin.wait_for_next_block(previous).await.unwrap(), next);
    assert_eq!(*rpc.waits.lock().unwrap(), vec![(previous.hash, 45_000); 3]);
}

#[tokio::test]
async fn script_funding_filters_sorts_retries_and_aligns_prevouts() {
    let small = utxo(1, 200);
    let large = utxo(2, 500);
    let mut unsafe_utxo = utxo(3, 10_000);
    unsafe_utxo.safe = false;
    let mut unconfirmed = utxo(4, 10_000);
    unconfirmed.confirmations = 0;
    let mut other_script = utxo(5, 10_000);
    other_script.prevout.script_pubkey = ScriptBuf::new();
    let rpc = Arc::new(StubRpc {
        utxos: vec![
            small.clone(),
            unsafe_utxo,
            unconfirmed,
            other_script,
            large.clone(),
        ],
        fail_first_funding: true,
        reverse_inputs: true,
        ..Default::default()
    });
    let bitcoin = BitcoinFacade::new(Arc::clone(&rpc));
    let funded = bitcoin
        .fund_transaction_from_script(&template(), &large.prevout.script_pubkey, 300)
        .await
        .unwrap();
    assert_eq!(
        funded
            .inputs
            .iter()
            .map(|input| input.outpoint)
            .collect::<Vec<_>>(),
        vec![small.outpoint, large.outpoint]
    );
    assert_eq!(funded.inputs[0].prevout, small.prevout);
    assert_eq!(funded.inputs[1].prevout, large.prevout);
    let calls = rpc.funding.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0.input.len(), 1);
    assert_eq!(calls[1].1.add_inputs, Some(false));
    assert_eq!(calls[1].1.include_watching, Some(true));
    assert_eq!(calls[1].1.change_address.as_ref(), Some(&large.address));
    assert_eq!(
        calls[1].1.input_weights,
        vec![(large.outpoint, 300), (small.outpoint, 300)]
    );
}

#[tokio::test]
async fn invalid_funding_result_is_rejected() {
    let coin = utxo(1, 500);
    let rpc = Arc::new(StubRpc {
        utxos: vec![coin.clone()],
        corrupt_inputs: true,
        ..Default::default()
    });
    let bitcoin = BitcoinFacade::new(Arc::clone(&rpc));
    assert!(
        bitcoin
            .fund_transaction_from_script(&template(), &coin.prevout.script_pubkey, 300)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn incomplete_wallet_signature_is_rejected() {
    let mut unsafe_utxo = utxo(1, 500);
    unsafe_utxo.safe = false;
    let coin = utxo(2, 500);
    let rpc = Arc::new(StubRpc {
        utxos: vec![unsafe_utxo, coin.clone()],
        incomplete_signature: true,
        ..Default::default()
    });
    let bitcoin = BitcoinFacade::new(Arc::clone(&rpc));
    let original = template();
    let error = bitcoin
        .fund_and_sign_with_wallet(&original)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("missing key"));
    assert!(original.input.is_empty());
    assert_eq!(
        rpc.funding.lock().unwrap()[0].0.input[0].previous_output,
        coin.outpoint
    );
}
