//! The node, in process, over five blocks and a restart.
//!
//! No HTTP here: this is the node itself answering the questions a wallet
//! asks, with every transaction built by `flamewallet`.

use curve25519_dalek::ristretto::CompressedRistretto;
use flamechain::mempool::MempoolError;
use flamechain::utreexo::UtreexoError;
use flamechain::Blockchain;
use flamekd::util::{CHANGE, RECEIVING};
use flamevm::{Anchor, ClearToken, Predicate, Scalar, Value};
use flamewallet::InputSpec;
use tempfile::TempDir;

use super::*;
use crate::cells::{contract_bytes, proof_bytes};
use crate::config::{ChainParamsFile, GenesisFile};
use crate::node::{Node, NodeError, TxStatus};

/// The predicate points of a list of an account's addresses.
fn predicates(account: &Account, addresses: &[(u32, u32)]) -> Vec<[u8; 32]> {
    addresses
        .iter()
        .map(|(branch, n)| {
            account
                .predicate_at(*branch, *n)
                .expect("predicate")
                .to_point()
                .to_bytes()
        })
        .collect()
}

#[test]
fn a_node_serves_two_wallets_across_five_blocks() {
    let dir = TempDir::new().expect("temp dir");
    let a = account(&SEED_A);
    let b = account(&SEED_B);
    let (genesis, cfg) = devnet(dir.path(), &a);

    // 1-2. A node opened on a fresh devnet holds one spendable contract.
    let mut node = Node::open(&genesis, &cfg).expect("open");
    let genesis_id = genesis.contracts[0].id.0;
    assert_eq!(node.tip().height, 0);
    assert_eq!(node.utxo_count(), 1);
    let genesis_proof = live(&node, &genesis_id);
    assert!(
        node.verify_proof(&genesis_id, &genesis_proof),
        "the genesis proof verifies against the genesis accumulator"
    );

    // 3. A pays B 400 Flame and keeps the change.
    let payment = 400 * FLAME;
    let change = GENESIS_SPARKS - payment - FEE;
    let a_key = a.spending_key_at(RECEIVING, 0).expect("key");
    let input = InputSpec::clear(contract_of(&node, &genesis_id), genesis_proof, a_key)
        .expect("a cleartext allocation is a clear input");
    let (to_b, b_opening) = output(&b, RECEIVING, 0, payment, 0);
    let (to_change, a_change_opening) = output(&a, CHANGE, 0, change, 1);
    let (packaged, contracts) = signed_transfer(vec![input], &[to_b, to_change], FEE);
    let b_id = contracts[0].id();
    let a_change_id = contracts[1].id();

    let txid = node.submit(&submitted(&packaged)).expect("A's payment");
    assert_eq!(node.tx_status(&txid), TxStatus::Mempool);
    assert_eq!(node.mempool_len(), 1);

    node.mint_block().expect("block 1");
    match node.tx_status(&txid) {
        TxStatus::Confirmed { height, .. } => assert_eq!(height, 1),
        other => panic!("expected the payment to be confirmed, got {other:?}"),
    }
    assert!(
        matches!(
            node.proof(&genesis_id),
            ProofStatus::Spent { height: 1, .. }
        ),
        "the allocation was spent at height 1"
    );
    let b_proof_at_1 = live(&node, &b_id);
    let a_change_proof_at_1 = live(&node, &a_change_id);
    assert!(node.verify_proof(&b_id, &b_proof_at_1));
    assert!(node.verify_proof(&a_change_id, &a_change_proof_at_1));

    // 4. Two empty blocks. Normalizing an unmodified forest returns the
    //    same roots, so an empty block moves no merkle path at all.
    node.mint_block().expect("block 2");
    node.mint_block().expect("block 3");
    assert_eq!(node.tip().height, 3);
    assert!(node.verify_proof(&b_id, &live(&node, &b_id)));
    assert!(node.verify_proof(&a_change_id, &live(&node, &a_change_id)));
    assert_eq!(
        proof_bytes(&live(&node, &b_id)),
        proof_bytes(&b_proof_at_1),
        "an empty block leaves every proof exactly where it was"
    );

    // 5. A restart rebuilds all of it from blocks.bin alone.
    let tip_before = node.tip();
    let b_bytes_before = proof_bytes(&live(&node, &b_id));
    let change_bytes_before = proof_bytes(&live(&node, &a_change_id));
    drop(node);

    let mut node = Node::open(&genesis, &cfg).expect("reopen");
    assert_eq!(node.tip(), tip_before);
    assert_eq!(node.block_count(), 3);
    assert_eq!(node.mempool_len(), 0, "the mempool starts empty every time");
    assert_eq!(proof_bytes(&live(&node, &b_id)), b_bytes_before);
    assert_eq!(
        proof_bytes(&live(&node, &a_change_id)),
        change_bytes_before,
        "a proof rebuilt by replay is the one built block by block"
    );

    // 6. A sweeps its own change. This block deletes a leaf and inserts
    //    one, which is what actually invalidates everyone else's proofs.
    let swept = change - FEE;
    let a_change_key = a.spending_key_at(CHANGE, 0).expect("key");
    let input = InputSpec::confidential(
        &contract_of(&node, &a_change_id),
        &a_change_opening,
        live(&node, &a_change_id),
        a_change_key,
    )
    .expect("A can open its own change");
    let (to_a1, _) = output(&a, RECEIVING, 1, swept, 2);
    let (packaged, contracts) = signed_transfer(vec![input], &[to_a1], FEE);
    let a_recv1_id = contracts[0].id();
    node.submit(&submitted(&packaged)).expect("A's sweep");
    node.mint_block().expect("block 4");
    assert!(matches!(
        node.proof(&a_change_id),
        ProofStatus::Spent { height: 4, .. }
    ));

    // 7. B spends what it received, rebuilding the input the way a
    //    recipient must: the contract as published, plus the opening A sent
    //    out of band.
    let b_payment = 100 * FLAME;
    let b_change = payment - b_payment - FEE;
    let b_key = b.spending_key_at(RECEIVING, 0).expect("key");
    let b_contract = contract_of(&node, &b_id);
    let (to_a2, _) = output(&a, RECEIVING, 2, b_payment, 3);
    let (to_b_change, _) = output(&b, CHANGE, 0, b_change, 4);

    // First with the proof B has held since block 1. Block 4 moved it, and
    // a Catchup repairs a proof across one block only — this refusal is the
    // whole reason the node keeps a UtxoSet at all.
    let stale = InputSpec::confidential(&b_contract, &b_opening, b_proof_at_1, b_key)
        .expect("B can open its own contract");
    let (packaged, _) = signed_transfer(vec![stale], &[to_a2.clone(), to_b_change.clone()], FEE);
    let refusal = node
        .submit(&submitted(&packaged))
        .expect_err("a proof from before block 4 no longer proves membership");
    assert!(
        matches!(
            refusal,
            NodeError::Mempool(MempoolError::Utreexo(UtreexoError::InvalidProof))
        ),
        "expected an invalid proof, got {refusal}"
    );

    // Then with the proof the node holds, fetched through the batched form.
    let batched = node.proofs(&[b_id, a_recv1_id]);
    assert_eq!(batched.len(), 2);
    let ProofStatus::Unspent(b_proof_now) = &batched[0].1 else {
        panic!("B's contract is unspent");
    };
    let input = InputSpec::confidential(&b_contract, &b_opening, b_proof_now.clone(), b_key)
        .expect("B can open its own contract");
    let (packaged, contracts) = signed_transfer(vec![input], &[to_a2, to_b_change], FEE);
    let a_recv2_id = contracts[0].id();
    node.submit(&submitted(&packaged))
        .expect("the node's own proof is accepted");
    node.mint_block().expect("block 5");
    assert!(node.verify_proof(&a_recv2_id, &live(&node, &a_recv2_id)));

    // 8. What A can learn about itself without holding any index.
    let a_predicates = predicates(
        &a,
        &[(RECEIVING, 0), (RECEIVING, 1), (RECEIVING, 2), (CHANGE, 0)],
    );
    let hits = node.scan(&a_predicates, 0);
    assert_eq!(
        hits.iter().map(|hit| hit.height).collect::<Vec<_>>(),
        vec![0, 1, 4, 5],
        "every contract A ever held, in height order"
    );
    assert_eq!(hits[0].id.0, genesis_id);
    assert_eq!(hits[1].id.0, a_change_id);
    assert_eq!(hits[2].id.0, a_recv1_id);
    assert_eq!(hits[3].id.0, a_recv2_id);
    assert_eq!(hits[0].spent.expect("the allocation is spent").height, 1);
    assert_eq!(hits[1].spent.expect("the change is spent").height, 4);
    assert!(hits[2].spent.is_none(), "the swept output is unspent");
    assert!(hits[3].spent.is_none(), "what B sent back is unspent");

    // `since_height` is the wallet's resume point.
    let recent = node.scan(&a_predicates, 4);
    assert_eq!(recent.len(), 2);

    // B sees its own two contracts and nothing of A's.
    let b_hits = node.scan(&predicates(&b, &[(RECEIVING, 0), (CHANGE, 0)]), 0);
    assert_eq!(b_hits.len(), 2);
    assert_eq!(b_hits[0].id.0, b_id);
    assert_eq!(b_hits[0].spent.expect("B spent it").height, 5);

    // And the bytes the node hands out decode to the contract they claim.
    let published = contract_of(&node, &a_recv2_id);
    assert_eq!(
        published.predicate.to_point().to_bytes(),
        a.predicate_at(RECEIVING, 2)
            .expect("predicate")
            .to_point()
            .to_bytes()
    );
    assert_eq!(published.id(), a_recv2_id);
}

#[test]
fn deriving_a_genesis_twice_gives_the_same_bytes() {
    let dir = TempDir::new().expect("temp dir");
    let a = account(&SEED_A);
    let chainparams = chainparams(&a);

    let first = dir.path().join("first.json");
    let second = dir.path().join("second.json");
    crate::genesis::write(&chainparams, &first).expect("derive");
    crate::genesis::write(&chainparams, &second).expect("derive again");

    assert_eq!(
        std::fs::read(&first).expect("read"),
        std::fs::read(&second).expect("read"),
        "the same network definition gives the same genesis, byte for byte"
    );
}

#[test]
fn a_genesis_that_lies_about_money_is_refused() {
    let dir = TempDir::new().expect("temp dir");
    let a = account(&SEED_A);
    let (genesis, cfg) = devnet(dir.path(), &a);

    // The hash commits the ids through the accumulator root, and nothing
    // else in the file. Every other field is checked against the bytes.
    let mut tampered = genesis.clone();
    tampered.contracts[0].qty_sparks += 1;
    assert!(matches!(
        Node::open(&tampered, &cfg),
        Err(NodeError::Genesis(
            crate::genesis::GenesisError::ContractFieldMismatch {
                index: 0,
                field: "qty_sparks"
            }
        ))
    ));

    let mut tampered = genesis.clone();
    tampered.contracts[0].id = flamed_rpc::ContractId([0x99; 32]);
    assert!(matches!(
        Node::open(&tampered, &cfg),
        Err(NodeError::Genesis(
            crate::genesis::GenesisError::ContractIdMismatch { index: 0 }
        ))
    ));

    // A file written under another `FLAME_FLAVOR` is not tampered: its
    // bytes, id and hash all follow from the token it holds, so every check
    // above passes and only the flavor check can say what is wrong. Zero is
    // the flavor such files were written with.
    let foreign_flavor = Scalar::ZERO;
    assert_ne!(foreign_flavor, FLAME_FLAVOR);
    let record = &genesis.contracts[0];
    let foreign = Contract::new(
        Predicate::opaque(CompressedRistretto(record.predicate.0)),
        Anchor(record.anchor),
        Value::ClearToken(ClearToken::new(
            Scalar::from(record.qty_sparks),
            foreign_flavor,
        )),
    )
    .expect("a cleartext token is portable");
    let (chain, _) = Blockchain::devnet_genesis(genesis.chain.params(), &[foreign.id()])
        .expect("seed the foreign genesis");
    let mut foreign_file = genesis.clone();
    foreign_file.contracts[0].id = flamed_rpc::ContractId(foreign.id());
    foreign_file.contracts[0].bytes = contract_bytes(&foreign).expect("encode").into();
    foreign_file.genesis_hash = flamed_rpc::BlockId(chain.tip().into_bytes());
    assert!(matches!(
        Node::open(&foreign_file, &cfg),
        Err(NodeError::Genesis(
            crate::genesis::GenesisError::ForeignFlavor { index: 0 }
        ))
    ));

    let mut tampered = genesis.clone();
    tampered.genesis_hash = flamed_rpc::BlockId([0x77; 32]);
    assert!(matches!(
        Node::open(&tampered, &cfg),
        Err(NodeError::GenesisHashMismatch { .. })
    ));

    // The good file still opens, so the refusals above are about the
    // tampering and not about the fixture.
    let path = dir.path().join("genesis.json");
    assert_eq!(GenesisFile::load(&path).expect("reload"), genesis);
    Node::open(&genesis, &cfg).expect("the untampered genesis opens");
}

/// The address `configs/chainparams.toml` allocates the supply to, pinned so
/// the shipped devnet is the one A's seed can spend.
#[test]
fn the_shipped_devnet_pays_account_a() {
    let a = account(&SEED_A);
    let address = a
        .address_at(RECEIVING, 0)
        .expect("address")
        .to_bech32(flamekd::Network::Testnet);
    let shipped = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/configs/chainparams.toml"
    ))
    .expect("the shipped network definition");
    assert!(
        shipped.contains(&address),
        "configs/chainparams.toml allocates to {address}"
    );
}

/// The genesis hash `flamed.md`'s recorded session quotes, pinned so the
/// doc cites a value the suite guards: a change to any consensus encoding,
/// `FLAME_FLAVOR` included, fails here instead of silently dating the doc.
#[test]
fn the_shipped_devnet_has_the_documented_genesis_hash() {
    let shipped = ChainParamsFile::load(Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/configs/chainparams.toml"
    )))
    .expect("the shipped network definition");
    let genesis = crate::genesis::derive(&shipped).expect("derive the shipped genesis");
    assert_eq!(
        hex::encode(genesis.genesis_hash.0),
        "d5363a727a98d2125cd171727b3d2de4a0830a38b26e1887906f4e704067b270",
        "update flamed.md's recorded session along with this value"
    );
}

#[test]
fn a_genesis_that_renumbers_its_allocations_is_refused() {
    let dir = TempDir::new().expect("temp dir");
    let a = account(&SEED_A);
    let (genesis, cfg) = devnet(dir.path(), &a);

    // The index is what the anchor is derived from, so renumbering an
    // allocation describes a different genesis, not the same one.
    let mut tampered = genesis;
    tampered.contracts[0].index = 1;
    assert!(matches!(
        Node::open(&tampered, &cfg),
        Err(NodeError::Genesis(
            crate::genesis::GenesisError::ContractIndexMismatch {
                position: 0,
                index: 1
            }
        ))
    ));
}
