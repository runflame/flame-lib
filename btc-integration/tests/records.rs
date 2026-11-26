//! How a consumer reads the Flame transactions of a Bitcoin block: the burns
//! and the endorsements.
//!
//! The transactions here are put together by hand and carry no signatures:
//! `MemoryBtc::mine_with` puts them into a block as they are.

use bitcoin::{
    Amount, OutPoint, Script, ScriptBuf, Transaction, TxIn, TxOut, Witness, absolute, transaction,
};
use btc_integration::protocol::{
    BtcBlockRecords, BtcRecordsError, BtcRecordsEvent, BtcRecordsWatcher, BurnData, MinterP2wsh,
    MintingEndorsementData, MintingEndorsementProcessingError, minter_witness_script,
};
use btc_integration::watcher::impls::{MemoryBtc, MemorySource, MemoryStorage};
use btc_integration::watcher::{BlockId, BtcStorage, BtcWatcher, Prevouts};
use flamechain::BlockHash;
use flamevm::Predicate;
use futures::FutureExt;

/// Bitcoin with one block and a watcher that starts from it.
fn setup() -> (MemoryBtc, BtcWatcher<MemorySource, MemoryStorage>) {
    let btc = MemoryBtc::new();
    let watcher = BtcWatcher::new(btc.source(), MemoryStorage::new(), btc.best());
    (btc, watcher)
}

/// The Flame transactions of a block, read the way a consumer does it: from
/// the storage of the watcher.
fn records(
    watcher: &mut BtcWatcher<MemorySource, MemoryStorage>,
    block: BlockId,
) -> Result<BtcBlockRecords, BtcRecordsError> {
    // Every event that is ready: the blocks it names are in the storage.
    while let Some(event) = watcher.next().now_or_never() {
        event.expect("the source answered");
    }
    let records = BtcBlockRecords::from_storage(watcher.storage(), block).now_or_never();
    records.expect("the storage in memory answers at once")
}

fn predicate() -> Predicate {
    Predicate::opaque(Predicate::unspendable_key())
}

/// The witness script of a minter.
fn witness_script() -> ScriptBuf {
    minter_witness_script::build_with_authorization(
        &predicate(),
        &ed25519_dalek::SigningKey::from_bytes(&[1; 32]).verifying_key(),
        Script::from_bytes(&[0x51]),
    )
}

fn transaction(input: TxIn, output: TxOut) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![input],
        output: vec![output],
    }
}

/// An input that spends nothing, like the input of a coinbase.
fn no_input() -> TxIn {
    TxIn::default()
}

/// Pays `sat` to `script_pubkey`.
fn pay(script_pubkey: ScriptBuf, sat: u64) -> Transaction {
    let output = TxOut {
        value: Amount::from_sat(sat),
        script_pubkey,
    };
    transaction(no_input(), output)
}

/// Burns `sat` for the minter with `witness_script`.
fn burn(witness_script: &ScriptBuf, sat: u64) -> (Transaction, BurnData) {
    let minter = MinterP2wsh::from_witness_script(witness_script.as_bytes());
    let data = BurnData::new(minter, predicate());
    (pay(data.to_script(), sat), data)
}

/// The endorsement of a core block.
fn endorsement(core_height: u32) -> MintingEndorsementData {
    MintingEndorsementData::V1 {
        flame_block_height: core_height,
        flame_block_hash: BlockHash::from([core_height as u8; 32]),
    }
}

/// An endorsement transaction: its input spends `spends` and shows
/// `witness_script`, its output carries the endorsement.
fn endorse(witness_script: &ScriptBuf, spends: OutPoint, core_height: u32) -> Transaction {
    let input = TxIn {
        previous_output: spends,
        witness: Witness::from_slice(&[witness_script.as_bytes()]),
        ..TxIn::default()
    };
    let output = TxOut {
        value: Amount::ZERO,
        script_pubkey: endorsement(core_height).to_script(),
    };
    transaction(input, output)
}

#[test]
fn a_block_gives_its_burns_and_its_endorsements() {
    let (btc, mut watcher) = setup();
    let minter = witness_script();

    // Block 1 pays to the script of the minter. Block 2 has a transaction
    // that is not ours, a burn for the minter, and an endorsement of the
    // minter, which spends the output of block 1.
    let funding = pay(minter.to_p2wsh(), 10_000);
    let funded = btc.mine_with(vec![funding.clone()]);
    let (burning, burned) = burn(&minter, 25_000);
    let endorsing = endorse(&minter, OutPoint::new(funding.compute_txid(), 0), 7);
    let block = btc.mine_with(vec![
        pay(ScriptBuf::new(), 1_000),
        burning.clone(),
        endorsing.clone(),
    ]);

    let records = records(&mut watcher, block).unwrap();

    let [burn] = records.burns.as_slice() else {
        panic!("one burn, got {:?}", records.burns);
    };
    assert_eq!(burn.txid(), burning.compute_txid());
    assert_eq!(burn.amount(), Amount::from_sat(25_000));
    assert_eq!(burn.data(), &burned);

    let [endorsed] = records.endorsements.as_slice() else {
        panic!("one endorsement, got {:?}", records.endorsements);
    };
    assert_eq!(endorsed.txid(), endorsing.compute_txid());
    assert_eq!(endorsed.output().data, endorsement(7));
    assert_eq!(endorsed.auth().minter().witness_script(), &minter);

    assert_eq!(records.id, block);

    // The block that only pays to the minter has neither.
    let records = self::records(&mut watcher, funded).unwrap();
    let neither = BtcBlockRecords {
        id: funded,
        burns: Vec::new(),
        endorsements: Vec::new(),
    };
    assert_eq!(records, neither);

    // The storage holds the blocks above the genesis block, not the genesis
    // block itself.
    let genesis_id = watcher.view().genesis_id();
    assert_eq!(
        self::records(&mut watcher, genesis_id),
        Err(BtcRecordsError::NotStored(genesis_id))
    );
}

#[test]
fn prevouts_of_another_block_are_an_error() {
    let (btc, mut watcher) = setup();
    let block = btc.mine_with(vec![pay(ScriptBuf::new(), 1_000)]);
    let _ = records(&mut watcher, block);
    let stored = watcher.storage().block(block).now_or_never().unwrap();
    let stored = stored.unwrap().expect("the block of an event is stored");

    assert_eq!(
        BtcBlockRecords::from_block(block, &stored, &Prevouts::default()),
        Err(
            MintingEndorsementProcessingError::TransactionCountMismatch {
                transaction_count: 1,
                prevout_count: 0,
            }
        )
    );
}

#[test]
fn the_records_watcher_gives_the_records_of_the_new_blocks_with_the_event() {
    let (btc, watcher) = setup();
    let genesis_id = btc.best();
    let mut watcher = BtcRecordsWatcher::new(watcher);
    let minter = witness_script();

    let (burning, _) = burn(&minter, 25_000);
    let first = btc.mine_with(vec![burning.clone()]);
    let second = btc.mine();

    let event = watcher.next().now_or_never().expect("the event is ready");
    let Ok(BtcRecordsEvent::NewBlocks { blocks }) = event else {
        panic!("new blocks, got {event:?}");
    };
    let [burned, empty] = blocks.as_slice() else {
        panic!("two blocks, got {blocks:?}");
    };
    assert_eq!((burned.id, empty.id), (first, second));
    assert_eq!(burned.burns[0].txid(), burning.compute_txid());
    assert!(empty.burns.is_empty());
    assert_eq!(watcher.view().best(), second);

    // A reorg names the removed blocks and brings the records of the new
    // ones. The new blocks are empty: the burn is gone with its block.
    let new_best = btc.reorg(2, 3);
    let event = watcher.next().now_or_never().expect("the event is ready");
    let new_blocks = btc.best_chain()[1..]
        .iter()
        .map(|&id| BtcBlockRecords {
            id,
            burns: Vec::new(),
            endorsements: Vec::new(),
        })
        .collect();
    let reorg = BtcRecordsEvent::Reorg {
        fork: genesis_id,
        removed: vec![second, first],
        blocks: new_blocks,
    };
    assert_eq!(event, Ok(reorg));
    assert_eq!(watcher.view().best(), new_best);
}
