//! Times: a block's from its header, a transaction's from its signer and
//! from this node's mempool; and the core chain the blocks anchor to.

use std::fs;

use flamekd::util::{CHANGE, RECEIVING};
use tempfile::TempDir;

use super::*;
use flamechain::{Burn, INITIAL_ALLOTMENT_SPARKS};
use flamevm::{Scalar, Value};

use crate::indexer::{
    core_block_hash, unix_now, CoreBlock, CoreBurns, CoreError, CoreIndexer, MockIndexer,
};

/// A signing time well past `LOCKTIME_TIMESTAMP_THRESHOLD`.
const SIGNED_AT: u32 = 1_800_000_000;

#[test]
fn block_times_are_in_the_header_and_survive_a_restart() {
    let dir = TempDir::new().unwrap();
    let (genesis, cfg) = devnet(dir.path(), &account(&SEED_A));
    let since = unix_now();
    let mut node = Node::open(&genesis, &cfg).unwrap();
    node.mint_block().unwrap();
    node.mint_block().unwrap();

    // Genesis was minted by no one.
    assert_eq!(node.block_time(0), None);
    let times: Vec<u64> = (1..=2).map(|h| node.block_time(h).unwrap()).collect();
    for time in &times {
        assert!((since..=unix_now()).contains(time));
    }
    assert!(times[0] <= times[1]);
    assert_eq!(node.block_summary(1).unwrap().header.time, times[0]);

    // Losing the core indexer's table costs nothing already minted.
    drop(node);
    fs::remove_dir_all(cfg.indexer_dir()).unwrap();
    let node = Node::open(&genesis, &cfg).unwrap();
    assert_eq!(node.tip().height, 2);
    assert_eq!(node.block_time(1), Some(times[0]));
    assert_eq!(node.block_time(2), Some(times[1]));
}

#[test]
fn iblocks_share_a_core_block_until_the_core_chain_grows() {
    let dir = TempDir::new().unwrap();
    let (genesis, cfg) = devnet(dir.path(), &account(&SEED_A));
    let indexer = MockIndexer::open(&cfg.indexer_dir()).unwrap();
    let mut node = Node::open_with(&genesis, &cfg, Box::new(indexer.clone())).unwrap();
    let core = |node: &Node, height| node.block_summary(height).unwrap().header.core_block_hash;

    // The first ask mines the first core block; later iblocks reuse it.
    node.mint_block().unwrap();
    node.mint_block().unwrap();
    assert_eq!(core(&node, 1), core_block_hash(1));
    assert_eq!(core(&node, 2), core_block_hash(1));
    assert_eq!(indexer.len(), 1);

    assert_eq!(indexer.mine().unwrap().height, 2);
    node.mint_block().unwrap();
    assert_eq!(core(&node, 3), core_block_hash(2));
}

#[test]
fn a_transaction_carries_its_signing_time_and_admission_time() {
    let dir = TempDir::new().unwrap();
    let a = account(&SEED_A);
    let (genesis, cfg) = devnet(dir.path(), &a);
    let mut node = Node::open(&genesis, &cfg).unwrap();

    let id = genesis.contracts[0].id.0;
    let input = InputSpec::clear(
        contract_of(&node, &id),
        live(&node, &id),
        a.spending_key_at(RECEIVING, 0).unwrap(),
    )
    .unwrap();
    let header = TxHeader {
        version: 1,
        locktime: SIGNED_AT,
    };
    let to_change = output(&a, CHANGE, 0, GENESIS_SPARKS - FEE);
    let unsigned = build_transfer(
        std::slice::from_ref(&input),
        &[to_change],
        FEE,
        header,
        limits(),
        &mut rng(90),
    )
    .unwrap();
    let tx = block_tx(
        sign(unsigned, &[input.signing_key()]).unwrap(),
        limits(),
        vec![input.proof().clone()],
    );

    let since = unix_now();
    let txid = node.submit(&submitted(&tx)).unwrap();
    let pending = node.pending_transactions(1).unwrap().transactions;
    assert_eq!(pending[0].locktime, SIGNED_AT);
    let received_at = pending[0].received_at;
    assert!((since..=unix_now()).contains(&received_at));

    node.mint_block().unwrap();
    let detail = node.transaction(flamed_rpc::TxId(txid.0), false).unwrap();
    assert_eq!(detail.summary.locktime, SIGNED_AT);
    assert_eq!(detail.received_at, Some(received_at));
    assert_eq!(detail.block_time, node.block_time(1));
    assert!(detail.block_time.is_some());

    // A wallet's history reads the creating block's time beside each
    // output; the genesis allocation has none.
    let change = a.predicate_at(CHANGE, 0).unwrap().to_point().to_bytes();
    let hits = node.scan(&[change], 0);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].time, node.block_time(1));
    let allocation = genesis.contracts[0].predicate.0;
    assert_eq!(node.scan(&[allocation], 0)[0].time, None);
}

/// An indexer standing in for a real one: its tip never moves, and core
/// blocks 2 and 7 hold burns paying `payee`.
struct FixedIndexer {
    payee: [u8; 32],
}

impl CoreIndexer for FixedIndexer {
    fn tip(&mut self) -> Result<CoreBlock, CoreError> {
        Ok(fixed_core_block(7))
    }

    fn blocks(&mut self, from: u64, to: u64) -> Result<Vec<CoreBurns>, CoreError> {
        Ok((from..=to.min(7))
            .map(|height| CoreBurns {
                block: fixed_core_block(height),
                burns: match height {
                    2 | 7 => vec![Burn {
                        reward_predicate: self.payee,
                        sats: 1_000,
                    }],
                    _ => Vec::new(),
                },
            })
            .collect())
    }
}

fn fixed_core_block(height: u64) -> CoreBlock {
    CoreBlock {
        height,
        hash: core_block_hash(height),
        time: 1_000,
    }
}

#[test]
fn any_indexer_drops_in_and_its_burns_mint() {
    let dir = TempDir::new().unwrap();
    let a = account(&SEED_A);
    let (genesis, cfg) = devnet(dir.path(), &a);
    let payee = a.predicate_at(RECEIVING, 1).unwrap().to_point().to_bytes();
    let since = unix_now();
    let mut node = Node::open_with(&genesis, &cfg, Box::new(FixedIndexer { payee })).unwrap();
    node.mint_block().unwrap();
    node.mint_block().unwrap();
    for height in 1..=2 {
        let summary = node.block_summary(height).unwrap();
        assert_eq!(summary.header.core_block_hash, core_block_hash(7));
        assert_eq!(summary.header.core_height, 7);
        // The minter's clock, not the core block's.
        assert!(summary.header.time >= since);
    }

    // The first block moved over core blocks 1..=7 and paid both burns'
    // allotments in one output; the second stayed on 7 and paid nothing.
    let hits = node.scan(&[payee], 0);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].height, 1);
    let contract = contract_of(&node, &hits[0].id.0);
    let Value::ClearToken(token) = contract.payload() else {
        panic!("a mint output holds cleartext flames");
    };
    assert_eq!(token.qty(), Scalar::from(2 * INITIAL_ALLOTMENT_SPARKS));
    let minted = node.transaction(hits[0].txid, false).unwrap();
    assert_eq!(minted.summary.execution, flamed_rpc::ExecutionData::Mint);

    drop(node);
    let node = Node::open_with(&genesis, &cfg, Box::new(FixedIndexer { payee })).unwrap();
    assert_eq!(node.tip().height, 2);
    assert_eq!(node.scan(&[payee], 0).len(), 1);
}

#[test]
fn the_mock_indexer_mines_in_order_and_remembers() {
    let dir = TempDir::new().unwrap();
    let mut indexer = MockIndexer::open(dir.path()).unwrap();
    let first = indexer.tip().unwrap();
    assert_eq!(first.height, 1);
    assert_eq!(first.hash, core_block_hash(1));
    assert_eq!(indexer.tip().unwrap(), first);
    let second = indexer.mine().unwrap();
    assert_eq!(second.height, 2);
    assert!(second.time >= first.time);
    drop(indexer);

    let mut indexer = MockIndexer::open(dir.path()).unwrap();
    assert_eq!(indexer.len(), 2);
    assert_eq!(indexer.tip().unwrap(), second);
}

#[test]
fn the_mock_indexer_drops_a_torn_line_and_refuses_a_corrupt_one() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(MockIndexer::FILE);
    let line = format!("{} 1700000000\n", hex::encode(core_block_hash(1)));
    fs::write(&path, format!("{line}{}", &line[..20])).unwrap();

    let indexer = MockIndexer::open(dir.path()).unwrap();
    assert_eq!(indexer.len(), 1);
    indexer.mine().unwrap();
    drop(indexer);
    let mut indexer = MockIndexer::open(dir.path()).unwrap();
    assert_eq!(indexer.len(), 2);
    assert_eq!(indexer.tip().unwrap().hash, core_block_hash(2));
    drop(indexer);

    fs::write(&path, format!("{line}not a line\n")).unwrap();
    assert!(matches!(
        MockIndexer::open(dir.path()),
        Err(CoreError::Corrupt { line: 2, .. })
    ));
    // A well-formed line at the wrong height is no better.
    fs::write(&path, format!("{line}{line}")).unwrap();
    assert!(matches!(
        MockIndexer::open(dir.path()),
        Err(CoreError::Corrupt { line: 2, .. })
    ));
}
