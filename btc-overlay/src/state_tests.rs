use corepc_client::bitcoin::{Sequence, TxIn, Witness};

use super::*;
use crate::headers::MemoryHeaders;

fn spend(id: u8, vout: u32, burn: u64) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([id; 32]), vout),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[vec![0x30], vec![0x51]]),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(burn),
            script_pubkey: ScriptBuf::new_op_return([id]),
        }],
    }
}

fn injected(state: &State, header: &Header) -> Vec<Txid> {
    let block = state.block(&header.hash()).expect("connected");
    block
        .injected
        .iter()
        .map(|entry| entry.transaction.compute_txid())
        .collect()
}

/// A started state with the chain's first block connected.
fn started(headers: &MemoryHeaders) -> State {
    let mut state = State::in_memory();
    let genesis = tip_of(headers);
    state.start(genesis.height).unwrap();
    state.connect(genesis).unwrap();
    state
}

fn tip_of(headers: &MemoryHeaders) -> Header {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(crate::headers::HeaderSource::tip(headers))
        .unwrap()
}

#[test]
fn a_new_block_takes_the_queue_and_the_next_one_is_empty() {
    let headers = MemoryHeaders::new(1);
    let mut state = started(&headers);
    let tx = spend(1, 0, 0);
    state.submit(tx.clone(), Amount::ZERO).unwrap();

    let first = headers.mine(0);
    state.connect(first).unwrap();
    let second = headers.mine(0);
    state.connect(second).unwrap();

    assert_eq!(injected(&state, &first), vec![tx.compute_txid()]);
    assert!(injected(&state, &second).is_empty());
    assert_eq!(state.queued(), 0);
}

#[test]
fn a_conflicting_submission_replaces_the_queued_one() {
    let headers = MemoryHeaders::new(1);
    let mut state = started(&headers);
    state.submit(spend(1, 0, 0), Amount::ZERO).unwrap();
    let replacement = spend(1, 0, 5);
    state
        .submit(replacement.clone(), Amount::from_sat(5))
        .unwrap();

    let block = headers.mine(0);
    state.connect(block).unwrap();

    assert_eq!(injected(&state, &block), vec![replacement.compute_txid()]);
}

#[test]
fn submissions_are_checked_like_core_does() {
    let headers = MemoryHeaders::new(1);
    let mut state = started(&headers);
    let tx = spend(1, 0, 10);

    let error = state.submit(tx.clone(), Amount::from_sat(9)).unwrap_err();
    assert_eq!(error.code(), -25);

    state.submit(tx.clone(), Amount::from_sat(10)).unwrap();
    state.connect(headers.mine(0)).unwrap();
    assert_eq!(
        state.submit(tx, Amount::from_sat(10)).unwrap_err().code(),
        -27
    );
    assert_eq!(
        state
            .submit(spend(1, 0, 0), Amount::ZERO)
            .unwrap_err()
            .code(),
        -25
    );
}

#[test]
fn reorgs_return_transactions_and_keep_contents_per_hash() {
    let headers = MemoryHeaders::new(1);
    let mut state = started(&headers);
    let tx = spend(1, 0, 0);
    state.submit(tx.clone(), Amount::ZERO).unwrap();
    let a = headers.mine(0);
    state.connect(a).unwrap();

    // A competing block replaces `a`; the transaction moves into it.
    let b = headers.mine(1);
    state.disconnect_tip().unwrap();
    assert_eq!(state.queued(), 1);
    state.connect(b).unwrap();
    assert_eq!(injected(&state, &b), vec![tx.compute_txid()]);

    // Back to `a`: it keeps its contents and the queue does not get the transaction twice.
    state.disconnect_tip().unwrap();
    state.connect(a).unwrap();
    assert_eq!(injected(&state, &a), vec![tx.compute_txid()]);
    assert_eq!(state.queued(), 0);
    assert_eq!(state.hash_at(a.height), Some(a.hash()));
}

#[test]
fn only_blocks_extending_the_tip_connect() {
    let headers = MemoryHeaders::new(1);
    let mut state = started(&headers);
    let a = headers.mine(0);
    let b = headers.mine(0);
    assert!(matches!(state.connect(b), Err(StateError::Invalid(_))));
    state.connect(a).unwrap();
    assert!(matches!(state.start(0), Err(StateError::Invalid(_))));
}

#[test]
fn prevouts_come_from_known_outputs_or_the_witness_script() {
    let headers = MemoryHeaders::new(1);
    let mut state = started(&headers);
    let parent = spend(1, 0, 0);
    let mut child = spend(2, 0, 0);
    child.input[0].previous_output = OutPoint::new(parent.compute_txid(), 0);
    state.submit(parent.clone(), Amount::ZERO).unwrap();
    state.submit(child, Amount::ZERO).unwrap();
    let block = headers.mine(0);
    state.connect(block).unwrap();

    let entries = state.block(&block.hash()).unwrap().entries();
    assert_eq!(entries[0].prevouts, vec![None], "coinbase");
    assert_eq!(
        entries[1].prevouts,
        vec![Some(TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new_p2wsh(&WScriptHash::hash(&[0x51])),
        })]
    );
    assert_eq!(entries[2].prevouts, vec![Some(parent.output[0].clone())]);
}

#[test]
fn replaying_the_log_restores_the_same_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("overlay.log");
    let headers = MemoryHeaders::new(1);
    let tip = tip_of(&headers);
    let a;
    {
        let mut state = State::open(&path).unwrap();
        state.start(tip.height).unwrap();
        state.connect(tip).unwrap();
        state.submit(spend(1, 0, 0), Amount::ZERO).unwrap();
        a = headers.mine(0);
        state.connect(a).unwrap();
        state.submit(spend(2, 0, 0), Amount::ZERO).unwrap();
    }

    let state = State::open(&path).unwrap();
    assert_eq!(state.tip().map(Block::hash), Some(a.hash()));
    assert_eq!(injected(&state, &a), vec![spend(1, 0, 0).compute_txid()]);
    assert_eq!(state.queued(), 1);
}

#[test]
fn wallets_and_minted_coins_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("overlay.log");
    let headers = MemoryHeaders::new(1);
    let script = ScriptBuf::new_p2wsh(&WScriptHash::hash(&[0x51]));
    let coins;
    {
        let mut state = State::open(&path).unwrap();
        let tip = tip_of(&headers);
        state.start(tip.height).unwrap();
        state.connect(tip).unwrap();
        state.create_wallet("minter").unwrap();
        state.watch("minter", &script).unwrap();
        coins = state.unspent("minter").unwrap();
        assert_eq!(coins.len(), wallet::COIN_FLOOR);
    }
    let mut state = State::open(&path).unwrap();
    assert_eq!(
        state.unspent("minter").unwrap(),
        coins,
        "no new coins minted"
    );
}

#[test]
fn spent_coins_are_replaced_and_change_is_funded() {
    let headers = MemoryHeaders::new(1);
    let mut state = started(&headers);
    let script = ScriptBuf::new_p2wsh(&WScriptHash::hash(&[0x51]));
    state.create_wallet("minter").unwrap();
    state.watch("minter", &script).unwrap();
    let coin = state.unspent("minter").unwrap()[0].clone();

    let mut endorsement = spend(0, 0, 0);
    endorsement.input[0].previous_output = coin.outpoint;
    let weights = HashMap::from([(coin.outpoint, 400)]);
    let funded = state.fund(endorsement, script.clone(), &weights).unwrap();
    assert_eq!(funded.change_position, Some(1));
    let change = &funded.transaction.output[1];
    assert_eq!(change.script_pubkey, script);
    assert_eq!(change.value + funded.fee, coin.output.value);
    assert!(funded.fee.to_sat() < 300, "{}", funded.fee);

    // Spent while queued: the wallet still has its floor of confirmed coins.
    state.submit(funded.transaction, Amount::ZERO).unwrap();
    let coins = state.unspent("minter").unwrap();
    assert_eq!(coins.len(), wallet::COIN_FLOOR);
    assert!(!coins.iter().any(|c| c.outpoint == coin.outpoint));
}
