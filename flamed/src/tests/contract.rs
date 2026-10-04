//! Contract RPC reads archived payloads and spend status at one tip.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use flamed_rpc::{
    BlockId, BlockTxEnvelope, CellId, ClientError, ContractEnvelope, ContractId, ContractResult,
    FlamedApiClient, FlamedApiServer, HttpClientBuilder, PredicatePoint, ProofResult, TipResult,
    TxId, TxValue,
};
use flamevm::{Anchor, BagOfCells, CellEncode, CellEnvelope, CellError, Dict, Value};
use jsonrpsee::{core::client::ClientT, rpc_params};
use tempfile::TempDir;

use super::*;

#[tokio::test]
async fn contract_tracks_spending_and_survives_replay() {
    let dir = TempDir::new().unwrap();
    let a = account(&SEED_A);
    let (genesis, cfg) = devnet(dir.path(), &a);
    let node = Node::open(&genesis, &cfg).unwrap();
    let genesis_id = genesis.contracts[0].id;
    let original = contract_of(&node, &genesis_id.0);
    let input = InputSpec::clear(
        original.clone(),
        live(&node, &genesis_id.0),
        a.spending_key_at(util::RECEIVING, 0).unwrap(),
    )
    .unwrap();
    let (transfer, outputs) = signed_transfer(
        vec![input],
        &[output(&a, util::CHANGE, 0, GENESIS_SPARKS - FEE)],
        FEE,
        &mut rng(73),
    );
    let output = &outputs[0];
    let output_id = ContractId(output.id());
    let shared = Arc::new(Mutex::new(node));
    let (addr, handle) = crate::rpc::serve(Arc::clone(&shared), cfg.rpc_bind)
        .await
        .unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();

    let initial = client.contract(genesis_id).await.unwrap();
    assert_eq!(initial.height, 0);
    assert_eq!(initial.created_block, genesis.genesis_hash);
    assert_eq!(initial.tip, client.tip().await.unwrap());
    assert_eq!(initial.anchor, original.anchor.0);
    assert!(initial.payload_error.is_none());
    let Some(TxValue::ClearToken { quantity, .. }) = &initial.decoded_payload else {
        panic!("expected the clear genesis allocation")
    };
    assert_eq!(quantity, &GENESIS_SPARKS.to_string());
    let ProofResult::Unspent { proof } = &initial.status else {
        panic!("expected an unspent contract")
    };
    let proof = crate::cells::proof_from_bytes(&proof.0).unwrap();
    assert!(shared.lock().unwrap().verify_proof(&genesis_id.0, &proof));

    let txid = client
        .submit_tx(BlockTxEnvelope(submitted(&transfer)))
        .await
        .unwrap();
    assert_eq!(client.contract(genesis_id).await.unwrap(), initial);
    shared.lock().unwrap().mint_block().unwrap();
    let block = client.block(1).await.unwrap();
    let spent = client.contract(genesis_id).await.unwrap();
    assert_eq!(spent.status, ProofResult::Spent { height: 1, txid });
    assert_eq!(spent.created_block, initial.created_block);
    assert_eq!(spent.bytes, initial.bytes);
    assert_eq!(spent.anchor, initial.anchor);
    assert_eq!(spent.decoded_payload, initial.decoded_payload);
    assert_eq!(spent.tip, client.tip().await.unwrap());

    let created = client.contract(output_id).await.unwrap();
    assert_eq!(created.height, 1);
    assert_eq!(created.txid, txid);
    assert_eq!(created.created_block, block.summary.id);
    assert_eq!(created.tip, spent.tip);
    assert_eq!(created.anchor, output.anchor.0);
    assert_eq!(created.predicate.0, output.predicate.to_point().to_bytes());
    assert_eq!(created.status, client.proof(output_id).await.unwrap());
    assert!(created.payload_error.is_none());
    let Value::Token(token) = output.payload() else {
        panic!("expected a confidential output")
    };
    assert_eq!(
        created.decoded_payload,
        Some(TxValue::Token {
            quantity_commitment: token.qty().to_point().to_bytes(),
            flavor_commitment: token.flv().to_point().to_bytes(),
        })
    );

    let json: serde_json::Value = client
        .request("contract", rpc_params![output_id])
        .await
        .unwrap();
    assert!(json.get("snapshot").is_none());
    assert!(json.get("id").is_none());
    assert_eq!(json["anchor"], hex::encode(output.anchor.0));
    assert!(json["bytes"].is_string());
    assert_eq!(
        serde_json::from_value::<ContractResult>(json).unwrap(),
        created
    );
    let ClientError::Call(error) = client.contract(ContractId([0xff; 32])).await.unwrap_err()
    else {
        panic!("expected a missing contract error")
    };
    assert_eq!(error.code(), flamed_rpc::codes::NOT_FOUND);
    let invalid: Result<ContractResult, _> = client.request("contract", rpc_params!["bad"]).await;
    let ClientError::Call(error) = invalid.unwrap_err() else {
        panic!("expected invalid params")
    };
    assert_eq!(error.code(), flamed_rpc::ErrorCode::InvalidParams.code());
    drop(client);
    handle.stop().unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle.stopped())
        .await
        .unwrap();
    drop(shared);

    let reopened = Node::open(&genesis, &cfg).unwrap();
    let rpc = crate::rpc::FlamedRpc::new(Arc::new(Mutex::new(reopened)));
    assert_eq!(rpc.contract(genesis_id).await.unwrap(), spent);
    assert_eq!(rpc.contract(output_id).await.unwrap(), created);
}

#[test]
fn contract_preserves_full_payloads_and_available_cells() {
    let payload = Value::Dict(Dict::from_values(vec![
        Value::String(
            flamevm::String::Opaque(vec![0xab; 257])
        );
        2_001
    ]));
    let source = Contract::new(
        account(&SEED_A).predicate_at(util::RECEIVING, 0).unwrap(),
        Anchor([0x12; 32]),
        payload.clone(),
    )
    .unwrap();
    let bytes = source.to_envelope().unwrap().encode();
    let id = ContractId(source.id());
    let mut snapshot = ContractResult {
        height: 0,
        txid: TxId([0; 32]),
        predicate: PredicatePoint(source.predicate.to_point().to_bytes()),
        bytes: ContractEnvelope(bytes),
        created_block: BlockId([0; 32]),
        status: ProofResult::Unknown,
        tip: TipResult {
            hash: BlockId([0; 32]),
            height: 0,
            contract_root: [0; 32],
        },
        anchor: [0; 32],
        decoded_payload: None,
        payload_error: None,
    };
    let full = crate::inspect::contract(id, snapshot.clone()).unwrap();
    assert_eq!(full.bytes, snapshot.bytes);
    assert_eq!(full.anchor, source.anchor.0);
    assert!(full.payload_error.is_none());
    let Some(TxValue::Dict { entries }) = full.decoded_payload else {
        panic!("expected the full dictionary")
    };
    assert_eq!(entries.len(), 2_001);
    assert!(entries.iter().all(|entry| entry.value
        == TxValue::String {
            bytes: vec![0xab; 257]
        }));

    let root = source.to_cell().unwrap();
    let flamevm::CellRef::Resident(dict) = &root.refs()[0] else {
        panic!("expected a dictionary cell")
    };
    let mut cells = BagOfCells::new();
    cells.insert(dict.clone()).unwrap();
    cells.insert(root.into()).unwrap();
    snapshot.bytes = ContractEnvelope(CellEnvelope::new(source.id(), cells).unwrap().encode());
    let partial = crate::inspect::contract(id, snapshot.clone()).unwrap();
    assert_eq!(partial.bytes, snapshot.bytes);
    assert_eq!(partial.anchor, source.anchor.0);
    assert!(partial.payload_error.is_none());
    let Some(TxValue::Cells { root, cells }) = partial.decoded_payload else {
        panic!("expected available payload cells")
    };
    assert_eq!(root, CellId(payload.to_cell().unwrap().id()));
    assert_eq!(cells.len(), 2);
    assert!(cells
        .iter()
        .flat_map(|cell| &cell.refs)
        .any(|id| !cells.iter().any(|cell| cell.id == *id)));

    assert!(matches!(
        crate::inspect::contract(ContractId([0xff; 32]), snapshot.clone()),
        Err(CellError::InvalidFormat)
    ));
    snapshot.bytes = ContractEnvelope(vec![0]);
    assert!(crate::inspect::contract(id, snapshot).is_err());
}
