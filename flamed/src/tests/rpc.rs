//! Every method, over real HTTP, through the generated client.
//!
//! The point of this round trip is the wire: the same seven calls a wallet
//! will make, and one error code for every method that has one.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use flamed_rpc::{
    codes, BlockTxEnvelope, ClientError, ContractId, FlamedApiClient, HttpClientBuilder,
    PredicatePoint, ProofResult, TxId, TxStatusResult, MAX_SCAN_PREDICATES,
};
use flamekd::util::{CHANGE, RECEIVING};
use flamepayments::InputSpec;
use tempfile::TempDir;

use super::*;
use crate::node::SharedNode;

/// Everything the HTTP round trip needs to have happened first.
struct Fixture {
    node: SharedNode,
    genesis_id: [u8; 32],
    b_id: [u8; 32],
    b_predicate: [u8; 32],
    /// A transaction carrying a proof that block 2 invalidated.
    stale: Vec<u8>,
    _dir: TempDir,
}

/// A devnet two blocks in: A has paid B, then swept its own change, so a
/// proof B held after block 1 no longer proves anything.
fn fixture() -> Fixture {
    let dir = TempDir::new().expect("temp dir");
    let a = account(&SEED_A);
    let b = account(&SEED_B);
    let (genesis, cfg) = devnet(dir.path(), &a);
    let mut node = Node::open(&genesis, &cfg).expect("open");

    let genesis_id = genesis.contracts[0].id.0;
    let payment = 400 * FLAME;
    let change = GENESIS_SPARKS - payment - FEE;

    let input = InputSpec::clear(
        contract_of(&node, &genesis_id),
        live(&node, &genesis_id),
        a.spending_key_at(RECEIVING, 0).expect("key"),
    )
    .expect("clear input");
    let (to_b, b_opening) = output(&b, RECEIVING, 0, payment, 0);
    let (to_change, a_change_opening) = output(&a, CHANGE, 0, change, 1);
    let (packaged, contracts) = signed_transfer(vec![input], &[to_b, to_change], FEE);
    let b_id = contracts[0].id();
    let a_change_id = contracts[1].id();
    node.submit(&submitted(&packaged)).expect("A's payment");
    node.mint_block().expect("block 1");

    let b_proof_at_1 = live(&node, &b_id);

    // Block 2 deletes a leaf and inserts one, which moves B's path.
    let input = InputSpec::confidential(
        &contract_of(&node, &a_change_id),
        &a_change_opening,
        live(&node, &a_change_id),
        a.spending_key_at(CHANGE, 0).expect("key"),
    )
    .expect("confidential input");
    let (to_a, _) = output(&a, RECEIVING, 1, change - FEE, 2);
    let (packaged, _) = signed_transfer(vec![input], &[to_a], FEE);
    node.submit(&submitted(&packaged)).expect("A's sweep");
    node.mint_block().expect("block 2");

    let stale_input = InputSpec::confidential(
        &contract_of(&node, &b_id),
        &b_opening,
        b_proof_at_1,
        b.spending_key_at(RECEIVING, 0).expect("key"),
    )
    .expect("confidential input");
    let (to_a, _) = output(&a, RECEIVING, 2, 100 * FLAME, 3);
    let (to_b_change, _) = output(&b, CHANGE, 0, payment - 100 * FLAME - FEE, 4);
    let (stale, _) = signed_transfer(vec![stale_input], &[to_a, to_b_change], FEE);

    Fixture {
        b_predicate: b
            .predicate_at(RECEIVING, 0)
            .expect("predicate")
            .to_point()
            .to_bytes(),
        genesis_id,
        b_id,
        stale: submitted(&stale),
        node: Arc::new(Mutex::new(node)),
        _dir: dir,
    }
}

/// The JSON-RPC error code a call came back with.
fn code_of(error: ClientError) -> i32 {
    match error {
        ClientError::Call(object) => object.code(),
        other => panic!("expected a JSON-RPC error, got {other}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_method_over_http() {
    let fixture = fixture();
    let (addr, handle) = crate::rpc::serve(
        Arc::clone(&fixture.node),
        "127.0.0.1:0".parse().expect("a literal socket address"),
    )
    .await
    .expect("bind");
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .expect("client");

    // tip
    let tip = client.tip().await.expect("tip");
    assert_eq!(tip.height, 2);
    assert_ne!(tip.contract_root, [0; 32]);

    // proof, in both of its outcomes, and the batched form
    assert!(matches!(
        client.proof(ContractId(fixture.b_id)).await.expect("proof"),
        ProofResult::Unspent { .. }
    ));
    assert!(matches!(
        client
            .proof(ContractId(fixture.genesis_id))
            .await
            .expect("proof"),
        ProofResult::Spent { height: 1, .. }
    ));
    assert!(matches!(
        client.proof(ContractId([0x5a; 32])).await.expect("proof"),
        ProofResult::Unknown
    ));
    let batched = client
        .proofs(vec![ContractId(fixture.b_id), ContractId([0x5a; 32])])
        .await
        .expect("proofs");
    assert_eq!(batched.len(), 2);
    assert_eq!(batched[0].0, ContractId(fixture.b_id));
    assert!(matches!(batched[1].1, ProofResult::Unknown));

    // The blob is only worth serving if it decodes back into a proof that
    // verifies — a `Transient` one would round-trip and prove nothing.
    let ProofResult::Unspent { proof } = &batched[0].1 else {
        panic!("B's contract is unspent");
    };
    let decoded = crate::cells::proof_from_bytes(&proof.0).expect("a served proof decodes");
    {
        let node = fixture.node.lock().expect("not poisoned");
        assert!(
            node.verify_proof(&fixture.b_id, &decoded),
            "the proof on the wire verifies against the accumulator it was taken from"
        );
    }
    let mut padded = proof.0.clone();
    padded.push(0);
    assert!(
        crate::cells::proof_from_bytes(&padded).is_err(),
        "trailing bytes are refused"
    );

    // contract
    let contract = client
        .contract(ContractId(fixture.b_id))
        .await
        .expect("contract");
    assert_eq!(contract.height, 1);
    assert_eq!(contract.predicate, PredicatePoint(fixture.b_predicate));
    assert_eq!(
        crate::cells::contract_from_bytes(&contract.bytes.0)
            .expect("decode")
            .id(),
        fixture.b_id
    );

    // tx_status
    assert!(matches!(
        client.tx_status(TxId([0x11; 32])).await.expect("tx_status"),
        TxStatusResult::Unknown
    ));

    // scan
    let scan = client
        .scan(vec![PredicatePoint(fixture.b_predicate)], 0)
        .await
        .expect("scan");
    assert_eq!(scan.tip_height, 2);
    assert_eq!(scan.outputs.len(), 1);
    assert_eq!(scan.outputs[0].id, ContractId(fixture.b_id));
    assert!(scan.outputs[0].spent.is_none());

    // submit_tx, and the txid it reports, and the status that follows
    let txid = client
        .submit_tx(BlockTxEnvelope(fixture.stale.clone()))
        .await
        .expect_err("a stale proof is refused");
    assert_eq!(code_of(txid), codes::MEMPOOL_REJECTED);

    // One error code per method that has one.
    assert_eq!(
        code_of(client.contract(ContractId([0xcc; 32])).await.unwrap_err()),
        codes::NOT_FOUND
    );
    assert_eq!(
        code_of(
            client
                .submit_tx(BlockTxEnvelope(vec![0xff; 16]))
                .await
                .unwrap_err()
        ),
        codes::INVALID_BYTES
    );
    assert_eq!(
        code_of(
            client
                .scan(vec![PredicatePoint([0; 32]); MAX_SCAN_PREDICATES + 1], 0)
                .await
                .unwrap_err()
        ),
        codes::LIMIT_EXCEEDED
    );

    // A transaction the node does accept shows up as `Mempool`.
    let accepted = {
        let node = fixture.node.lock().expect("not poisoned");
        let a = account(&SEED_A);
        let b = account(&SEED_B);
        // The same opening the fixture handed B, rebuilt from the same
        // output index — a recipient keeps it, it is not on the wire.
        let (_, opening) = output(&b, RECEIVING, 0, 400 * FLAME, 0);
        let input = InputSpec::confidential(
            &contract_of(&node, &fixture.b_id),
            &opening,
            live(&node, &fixture.b_id),
            b.spending_key_at(RECEIVING, 0).expect("key"),
        )
        .expect("confidential input");
        let (to_a, _) = output(&a, RECEIVING, 3, 100 * FLAME, 5);
        let (to_b_change, _) = output(&b, CHANGE, 1, 400 * FLAME - 100 * FLAME - FEE, 6);
        let (packaged, _) = signed_transfer(vec![input], &[to_a, to_b_change], FEE);
        submitted(&packaged)
    };
    let txid = client
        .submit_tx(BlockTxEnvelope(accepted))
        .await
        .expect("a fresh proof is accepted");
    // Still `Mempool` on the next line because this fixture runs no minter:
    // nothing can confirm it between the two calls. Adding one here would
    // make this assertion a race.
    assert!(matches!(
        client.tx_status(txid).await.expect("tx_status"),
        TxStatusResult::Mempool
    ));

    // Drop the client first: the server drains its connections before
    // `stopped()` resolves, and a live keep-alive would stall that.
    drop(client);
    handle.stop().expect("stop");
    tokio::time::timeout(Duration::from_secs(5), handle.stopped())
        .await
        .expect("the server stops");
}
