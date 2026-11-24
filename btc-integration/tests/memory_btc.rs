//! Bitcoin in memory as a node: it checks the transactions it is sent, keeps
//! them until a block takes them, and the test decides which block that is.
//!
//! The transactions here are real: paid and signed by a wallet, or by the
//! wallet of a minter.

use std::sync::Mutex;

use bitcoin::secp256k1::SecretKey;
use bitcoin::{
    Amount, Block, OutPoint, ScriptBuf, Transaction, TxIn, TxOut, Txid, absolute, transaction,
};
use btc_integration::identity::impls::LocalKey;
use btc_integration::protocol::{BtcBlockRecords, BurnData, MintingEndorsementData};
use btc_integration::wallet::impls::MemoryWallet;
use btc_integration::watcher::impls::{MemoryBtc, Rejected};
use btc_integration::watcher::{BlockId, ChainSource, Prevouts};
use btc_integration::{
    BtcFundedTransaction, BtcWallet, IdentityConfig, MinterWallet, WalletError, WatchOnlyWallet,
};
use flamechain::BlockHash;
use flamevm::Predicate;
use futures::FutureExt;

const COIN: Amount = Amount::from_sat(1_000_000);

/// Bitcoin with a wallet that has one coin.
fn setup() -> (MemoryBtc, MemoryWallet, OutPoint) {
    let btc = MemoryBtc::new();
    let key = SecretKey::from_slice(&[0x11; 32]).unwrap();
    let wallet = MemoryWallet::new(btc.clone(), key);
    let coin = btc.mine_coins(wallet.script_pubkey(), COIN);
    (btc, wallet, coin)
}

/// A script that is not the wallet's; `byte` tells such scripts apart.
fn somebody(byte: u8) -> ScriptBuf {
    ScriptBuf::new_p2wsh(&ScriptBuf::from_bytes(vec![byte]).wscript_hash())
}

fn unsigned(output: TxOut) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: Vec::new(),
        output: vec![output],
    }
}

fn pay(script_pubkey: ScriptBuf, sat: u64) -> TxOut {
    TxOut {
        value: Amount::from_sat(sat),
        script_pubkey,
    }
}

/// A payment of the wallet, signed and not sent.
fn payment(wallet: &MemoryWallet, script_pubkey: ScriptBuf, sat: u64) -> Transaction {
    let signed = wallet.payment(pay(script_pubkey, sat));
    signed.expect("the wallet has coins")
}

/// A block as a node gets it from Bitcoin.
fn block(btc: &MemoryBtc, id: BlockId) -> (Block, Prevouts) {
    let block = btc.source().block(id.hash).now_or_never().unwrap();
    block.unwrap().expect("the block is mined")
}

#[test]
fn a_transaction_waits_until_a_block_takes_it() {
    let (btc, wallet, coin) = setup();
    let paying = payment(&wallet, somebody(1), 10_000);

    let txid = btc.submit(paying.clone()).unwrap();
    assert_eq!(btc.mempool(), [txid]);

    // The miner leaves it out of a block.
    let without = btc.mine_only(&[]);
    assert_eq!(block(&btc, without).0.txdata, []);
    assert_eq!(btc.mempool(), [txid]);

    let with = btc.mine();
    let (mined, prevouts) = block(&btc, with);
    assert_eq!(mined.txdata, [paying]);
    assert_eq!(btc.mempool(), []);
    // The block comes with the coin the payment spent.
    let [spent] = prevouts.as_slice() else {
        panic!("one transaction, got {prevouts:?}");
    };
    let coin_script = wallet.script_pubkey();
    assert_eq!(mined.txdata[0].input[0].previous_output, coin);
    assert_eq!(
        spent[..],
        [Some(TxOut {
            value: COIN,
            script_pubkey: coin_script
        })]
    );
}

#[test]
fn a_transaction_that_does_not_hold_is_rejected() {
    let (btc, wallet, coin) = setup();
    let spend_coin = TxIn {
        previous_output: coin,
        ..TxIn::default()
    };

    // The signature covers the outputs: a changed one breaks it.
    let mut changed = payment(&wallet, somebody(1), 10_000);
    changed.output[0].script_pubkey = somebody(2);
    assert!(matches!(btc.submit(changed), Err(Rejected::Script(_))));

    // No signature at all.
    let mut unsigned_spend = unsigned(TxOut {
        value: Amount::from_sat(10_000),
        script_pubkey: somebody(1),
    });
    unsigned_spend.input.push(spend_coin.clone());
    assert!(matches!(
        btc.submit(unsigned_spend.clone()),
        Err(Rejected::Script(_))
    ));

    // More is paid than the coin holds.
    let mut overspend = unsigned_spend.clone();
    overspend.output[0].value = COIN + Amount::from_sat(1);
    assert_eq!(
        btc.submit(overspend),
        Err(Rejected::Overspend {
            inputs: COIN,
            outputs: COIN + Amount::from_sat(1),
        })
    );

    // An output nobody has.
    let nothing = OutPoint::new(coin.txid, 7);
    let mut from_nothing = unsigned_spend.clone();
    from_nothing.input[0].previous_output = nothing;
    assert_eq!(
        btc.submit(from_nothing),
        Err(Rejected::MissingInput { outpoint: nothing })
    );

    // The coin named by two inputs.
    let mut twice = unsigned_spend.clone();
    twice.input.push(spend_coin);
    assert_eq!(
        btc.submit(twice),
        Err(Rejected::DuplicateInput { outpoint: coin })
    );

    let no_inputs = unsigned(unsigned_spend.output[0].clone());
    assert_eq!(btc.submit(no_inputs), Err(Rejected::NoInputs));
    assert_eq!(btc.mempool(), []);
}

#[test]
fn an_output_is_spent_once() {
    let (btc, wallet, coin) = setup();
    // Both payments spend the only coin of the wallet.
    let first = payment(&wallet, somebody(1), 10_000);
    let second = payment(&wallet, somebody(2), 10_000);

    btc.submit(first).unwrap();
    assert_eq!(
        btc.submit(second.clone()),
        Err(Rejected::SpentInMempool { outpoint: coin })
    );

    btc.mine();
    assert_eq!(
        btc.submit(second),
        Err(Rejected::MissingInput { outpoint: coin })
    );
}

#[test]
fn a_wallet_does_not_spend_a_coin_that_a_waiting_transaction_spends() {
    let (btc, wallet, _) = setup();
    btc.mine_coins(wallet.script_pubkey(), Amount::from_sat(500_000));
    let send = |to: u8| {
        let sent = wallet.send(pay(somebody(to), 10_000)).now_or_never();
        sent.unwrap()
    };

    // The first payment takes one coin, the second one the other.
    let first = send(1).unwrap();
    let second = send(2).unwrap();
    assert_eq!(btc.mempool(), [first, second]);
    // Both coins are spent now, and the change is not confirmed.
    assert_eq!(send(3), Err(WalletError::NoConfirmedOutputs));
    assert_eq!(btc.mempool(), [first, second]);
}

#[test]
fn a_reorg_makes_the_transactions_of_the_removed_block_wait_again() {
    let (btc, wallet, _) = setup();
    let paying = payment(&wallet, somebody(1), 10_000);
    let txid = btc.submit(paying.clone()).unwrap();
    let removed = btc.mine();
    assert_eq!(btc.mempool(), []);

    // Another block takes the place of the one with the payment.
    let replacement = btc.reorg(1, 1);
    assert_eq!(replacement.height, removed.height);
    assert_eq!(block(&btc, replacement).0.txdata, []);
    assert_eq!(btc.mempool(), [txid]);

    let with = btc.mine();
    assert_eq!(block(&btc, with).0.txdata, [paying]);
}

#[test]
fn a_transaction_that_the_new_branch_made_impossible_does_not_wait() {
    let (btc, wallet, coin) = setup();
    let first = payment(&wallet, somebody(1), 10_000);
    let second = payment(&wallet, somebody(2), 10_000);
    let below = btc.best();
    btc.submit(first.clone()).unwrap();
    btc.mine();

    // The block with the first payment leaves the chain, and the block that
    // replaces it has the second one, which spends the same coin.
    btc.set_best(below);
    assert_eq!(btc.mempool(), [first.compute_txid()]);
    btc.mine_with(vec![second]);

    assert_eq!(btc.mempool(), []);
    assert_eq!(
        btc.submit(first),
        Err(Rejected::MissingInput { outpoint: coin })
    );
}

fn nobody() -> Predicate {
    Predicate::opaque(Predicate::unspendable_key())
}

/// A new key of a minter and the identity it gives.
fn minter_key() -> (LocalKey, IdentityConfig) {
    let config = IdentityConfig {
        flame_predicate: nobody(),
        access_predicate: nobody(),
        validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
    };
    (LocalKey::generate(), config)
}

/// The wallet of a minter with a new key, whose script has a confirmed
/// output. `wallet` pays for that output, and `watch_only` finds it.
fn minter<W: WatchOnlyWallet>(
    btc: &MemoryBtc,
    wallet: &MemoryWallet,
    watch_only: W,
) -> MinterWallet<W, LocalKey> {
    let (key, config) = minter_key();
    let minter = config.identity(&key);
    let funding = payment(wallet, minter.witness_script().to_p2wsh(), 100_000);
    btc.submit(funding).unwrap();
    btc.mine();
    MinterWallet::new(watch_only, key, minter).expect("the key is the one in the script")
}

fn endorsement(core_height: u32) -> MintingEndorsementData {
    MintingEndorsementData::V1 {
        flame_block_height: core_height,
        flame_block_hash: BlockHash::from([core_height as u8; 32]),
    }
}

#[test]
fn a_minter_wallet_needs_the_key_of_the_minter_script() {
    let (_, wallet, _) = setup();
    let (key, config) = minter_key();
    let minter = config.identity(&key);

    let (another_key, _) = minter_key();
    let wrong = MinterWallet::new(wallet.clone(), another_key, minter.clone());
    assert!(matches!(wrong, Err(WalletError::WrongKey)));
    assert!(MinterWallet::new(wallet, key, minter).is_ok());
}

#[test]
fn a_burn_and_an_endorsement_go_through_bitcoin_in_memory() {
    let (btc, wallet, _) = setup();
    let minter = minter(&btc, &wallet, wallet.clone());
    let identity = minter.minter().clone();
    let endorse = |core_height| {
        let sent = minter.send(endorsement(core_height).to_output());
        sent.now_or_never()
            .expect("Bitcoin in memory answers at once")
    };

    // Anybody burns for the minter; here the wallet with the coin does.
    let burn = Amount::from_sat(25_000);
    let burning = BurnData::new(identity.p2wsh(), nobody()).to_output(burn);
    let burned = wallet.send(burning).now_or_never().unwrap().unwrap();
    let endorsed = endorse(1).unwrap();
    assert_eq!(btc.mempool(), [burned, endorsed]);
    // The change of the endorsement is not confirmed, so the next one cannot
    // be sent.
    assert_eq!(endorse(2), Err(WalletError::NoConfirmedOutputs));

    let id = btc.mine();
    let (mined, prevouts) = block(&btc, id);
    let records = BtcBlockRecords::from_block(id, &mined, &prevouts).unwrap();
    let [found] = records.burns.as_slice() else {
        panic!("one burn, got {:?}", records.burns);
    };
    assert_eq!((found.txid(), found.amount()), (burned, burn));
    assert_eq!(found.data().minter_p2wsh, identity.p2wsh());
    let [found] = records.endorsements.as_slice() else {
        panic!("one endorsement, got {:?}", records.endorsements);
    };
    assert_eq!(found.txid(), endorsed);
    assert_eq!(found.output().data, endorsement(1));
    assert_eq!(found.auth().minter(), &identity);

    // With the change confirmed the minter endorses again.
    let endorsed = endorse(2).unwrap();
    assert_eq!(btc.mempool(), [endorsed]);
}

/// A watch-only wallet that keeps the signed transaction instead of sending
/// it.
struct Keeps {
    wallet: MemoryWallet,
    kept: Mutex<Option<Transaction>>,
}

impl WatchOnlyWallet for &Keeps {
    async fn fund_from_script(
        &self,
        transaction: &Transaction,
        script_pubkey: &ScriptBuf,
        input_weight: u64,
    ) -> Result<BtcFundedTransaction, WalletError> {
        let funded = self
            .wallet
            .fund_from_script(transaction, script_pubkey, input_weight);
        funded.await
    }

    async fn publish(&self, transaction: &Transaction) -> Result<Txid, WalletError> {
        *self.kept.lock().unwrap() = Some(transaction.clone());
        Ok(transaction.compute_txid())
    }
}

#[test]
fn nobody_can_change_the_endorsement_of_a_minter() {
    let (btc, wallet, _) = setup();
    let keeps = Keeps {
        wallet: wallet.clone(),
        kept: Mutex::new(None),
    };
    let minter = minter(&btc, &wallet, &keeps);

    // An endorsement of core block 1, signed by the minter and not sent.
    let endorsing = minter.send(endorsement(1).to_output()).now_or_never();
    endorsing.unwrap().unwrap();
    let signed = keeps.kept.lock().unwrap().take().unwrap();

    // Somebody makes it endorse another block. The signature of the minter
    // covers the outputs, so Bitcoin does not take it.
    let mut changed = signed.clone();
    let mut output = changed.output.iter_mut();
    let endorsed = output
        .find(|output| output.script_pubkey == endorsement(1).to_script())
        .expect("the endorsement output");
    endorsed.script_pubkey = endorsement(2).to_script();
    assert!(matches!(btc.submit(changed), Err(Rejected::Script(_))));

    btc.submit(signed).unwrap();
}
