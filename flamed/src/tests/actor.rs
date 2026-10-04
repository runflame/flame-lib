//! Actor RPC reads current committed data without changing execution state.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use flamed_rpc::{
    ActorCode, ActorId, ActorResult, ActorStateEnvelope, BlockId, CellId, ClientError,
    FlamedApiClient, HttpClientBuilder, TxValue,
};
use flamevm::{
    BagOfCells, CellEncode, CellEnvelope, Dict, ScriptBuilder, String as VmString, Value,
};
use jsonrpsee::{core::client::ClientT, rpc_params};
use tempfile::TempDir;

use super::*;

#[tokio::test]
async fn actor_tracks_updates_expiry_and_replay() {
    let dir = TempDir::new().unwrap();
    let a = account(&SEED_A);
    let (_, cfg) = devnet(dir.path(), &a);
    let mut params = chainparams(&a);
    params.storage.lease_duration_blocks = Some(3);
    let path = cfg.genesis.as_ref().unwrap();
    crate::genesis::write(&params, path).unwrap();
    let genesis = GenesisFile::load(path).unwrap();
    let mut node = Node::open(&genesis, &cfg).unwrap();
    let (actor, change) = block::deploy(&mut node, &genesis);
    let tip = node.tip();
    let shared = Arc::new(Mutex::new(node));
    let (addr, handle) = crate::rpc::serve(shared.clone(), cfg.rpc_bind)
        .await
        .unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();

    let deployed = client.actor(actor).await.unwrap();
    assert_eq!(deployed.id, actor);
    assert_eq!(deployed.height, 1);
    assert_eq!(deployed.block, BlockId(tip.hash.into_bytes()));
    assert_eq!(deployed.storage_capacity, 1024);
    assert!(deployed.storage_used > 0);
    assert!(deployed.storage_used <= deployed.storage_capacity);
    let code = &deployed.code.as_ref().unwrap().0;
    assert_eq!(flamevm::code_root(code), deployed.code_hash);
    assert_eq!(code.len() as u64, deployed.code_size);
    let state = &deployed.state.as_ref().unwrap().0;
    let mut gas = state.len() as u64 * 4;
    let envelope = CellEnvelope::decode(state, state.len(), &mut gas).unwrap();
    assert_eq!(envelope.root(), deployed.state_hash);
    let Some(TxValue::Dict { entries }) = &deployed.decoded_state else {
        panic!("expected a decoded dictionary")
    };
    assert_eq!(entries[0].key, "0");
    assert_eq!(entries[0].value, TxValue::Scalar { value: "0".into() });
    assert!(deployed.state_error.is_none() && deployed.code_error.is_none());
    assert_eq!(
        deployed
            .instructions
            .iter()
            .map(|op| op.text.as_str())
            .collect::<Vec<_>>(),
        ["load", "push 0", "get", "push 1", "add", "put", "save"]
    );
    assert_eq!(deployed.instructions[0].offset, 0);
    assert!(deployed
        .instructions
        .windows(2)
        .all(|pair| pair[0].offset < pair[1].offset));
    assert!(deployed.instructions.last().unwrap().offset < code.len() as u64);
    assert_eq!(client.actor(actor).await.unwrap(), deployed);
    assert_eq!(shared.lock().unwrap().tip(), tip);
    let json: serde_json::Value = client.request("actor", rpc_params![actor]).await.unwrap();
    assert!(json.get("snapshot").is_none());
    assert_eq!(json["id"], actor.to_string());
    let code_json = json["code"].to_string();
    let bytes =
        flamed_rpc::codec::base64::deserialize(&mut serde_json::Deserializer::from_str(&code_json))
            .unwrap();
    assert_eq!(&bytes, code);
    assert_eq!(
        serde_json::from_value::<ActorResult>(json).unwrap(),
        deployed
    );

    {
        let mut node = shared.lock().unwrap();
        block::send_messages(&mut node, actor, change, &[1_000_000]);
        node.mint_block().unwrap();
    }
    let updated = client.actor(actor).await.unwrap();
    assert_eq!(updated.height, 2);
    assert_eq!(updated.code_hash, deployed.code_hash);
    assert_ne!(updated.state_hash, deployed.state_hash);
    let Some(TxValue::Dict { entries }) = &updated.decoded_state else {
        panic!("expected updated state")
    };
    assert_eq!(entries[0].value, TxValue::Scalar { value: "1".into() });
    shared.lock().unwrap().mint_block().unwrap();
    assert!(client.actor(actor).await.unwrap().state.is_some());
    shared.lock().unwrap().mint_block().unwrap();
    let frozen = client.actor(actor).await.unwrap();
    assert_eq!(frozen.height, 4);
    assert_eq!(frozen.code_hash, updated.code_hash);
    assert_eq!(frozen.state_hash, updated.state_hash);
    assert_eq!(frozen.code_size, updated.code_size);
    assert_eq!(frozen.state_size, updated.state_size);
    assert_eq!(frozen.storage_capacity, 0);
    assert!(frozen.code.is_none() && frozen.state.is_none());
    assert!(frozen.decoded_state.is_none() && frozen.instructions.is_empty());
    assert_eq!(
        frozen.state_error.as_deref(),
        Some("State body is unavailable")
    );
    assert_eq!(
        frozen.code_error.as_deref(),
        Some("Code body is unavailable")
    );

    let ClientError::Call(error) = client.actor(ActorId([0xff; 32])).await.unwrap_err() else {
        panic!("expected a missing actor error")
    };
    assert_eq!(error.code(), flamed_rpc::codes::NOT_FOUND);
    let invalid: Result<ActorResult, _> = client.request("actor", rpc_params!["bad"]).await;
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
    let node = Node::open(&genesis, &cfg).unwrap();
    assert_eq!(crate::inspect::actor(node.actor(actor).unwrap()), frozen);
}

#[test]
fn actor_keeps_full_data_and_reports_unavailable_or_malformed_bodies() {
    let state = Value::Dict(Dict::from_values(vec![
        Value::String(VmString::Opaque(
            vec![0xab; 257]
        ));
        2_001
    ]));
    let nop = ScriptBuilder::new().nop().to_bytecode();
    let mut code = nop.repeat(10_001);
    code.extend(ScriptBuilder::new().push_str(vec![0xcd; 257]).to_bytecode());
    let error_offset = code.len();
    let mut incomplete = ScriptBuilder::new().push_str(vec![0xef]).to_bytecode();
    incomplete.pop();
    code.extend(incomplete);
    let state_cell = state.to_cell().unwrap();
    let mut snapshot = ActorResult {
        id: ActorId([1; 32]),
        height: 1,
        block: BlockId([2; 32]),
        code_hash: flamevm::code_root(&code),
        state_hash: state_cell.id(),
        code_size: code.len() as u64,
        state_size: 0,
        storage_used: 0,
        storage_capacity: 0,
        code: Some(ActorCode(code)),
        state: Some(ActorStateEnvelope(state.to_envelope().unwrap().encode())),
        decoded_state: None,
        state_error: None,
        instructions: Vec::new(),
        code_error: None,
    };
    let detail = crate::inspect::actor(snapshot.clone());
    assert_eq!(detail.code, snapshot.code);
    assert_eq!(detail.state, snapshot.state);
    let Some(TxValue::Dict { entries }) = detail.decoded_state else {
        panic!("expected full state")
    };
    assert_eq!(entries.len(), 2_001);
    assert!(entries.iter().all(|entry| entry.value
        == TxValue::String {
            bytes: vec![0xab; 257]
        }));
    assert!(detail.state_error.is_none());
    assert_eq!(detail.instructions.len(), 10_002);
    assert_eq!(
        detail.instructions[10_001].text,
        format!("pushstr 0x{}", "cd".repeat(257))
    );
    assert!(detail
        .code_error
        .unwrap()
        .starts_with(&format!("Byte {error_offset}: ")));

    let mut cells = BagOfCells::new();
    cells.insert(state_cell.into()).unwrap();
    snapshot.state = Some(ActorStateEnvelope(
        CellEnvelope::new(snapshot.state_hash, cells)
            .unwrap()
            .encode(),
    ));
    let partial = crate::inspect::actor(snapshot.clone());
    let Some(TxValue::Cells { root, cells }) = partial.decoded_state else {
        panic!("expected available cells")
    };
    assert_eq!(root, CellId(snapshot.state_hash));
    assert_eq!(cells.len(), 1);
    assert!(!cells[0].refs.is_empty());
    assert!(partial.state_error.is_none());

    snapshot.state = Some(ActorStateEnvelope(vec![0]));
    snapshot.code = Some(ActorCode(vec![]));
    let malformed = crate::inspect::actor(snapshot.clone());
    assert!(malformed.decoded_state.is_none() && malformed.state_error.is_some());
    assert!(malformed.instructions.is_empty() && malformed.code_error.is_none());
    snapshot.state = None;
    snapshot.code = None;
    let missing = crate::inspect::actor(snapshot);
    assert!(missing.decoded_state.is_none() && missing.state_error.is_some());
    assert!(missing.instructions.is_empty() && missing.code_error.is_some());
}
