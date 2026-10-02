//! Block RPC covers the consensus execution order, HTTP and archive replay.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flamed_rpc::{
    ActorId, BlockId, BlockResult, ClientError, ExecutionData, FlamedApiClient, HttpClientBuilder,
    TxId,
};
use flamevm::{ActorID, CellEncode, ScriptBuilder, String as VmString};
use jsonrpsee::{core::client::ClientT, rpc_params};
use tempfile::TempDir;

use super::*;

fn counter_code() -> Vec<u8> {
    ScriptBuilder::new()
        .load()
        .push_int(0u64)
        .get()
        .push_int(1u64)
        .add()
        .put()
        .save()
        .to_bytecode()
}

fn deploy(node: &mut Node, genesis: &GenesisFile) -> (ActorId, ContractID) {
    let input = contract_of(node, &genesis.contracts[0].id.0);
    let constructor = ScriptBuilder::new()
        .push_int(1_024u64)
        .addstorage()
        .verify()
        .merge()
        .verify()
        .push_int(1u64)
        .push_int(0u64)
        .push_int(0u64)
        .push_int(2u64)
        .dict()
        .push_str(counter_code())
        .setcode()
        .load()
        .drop_()
        .save()
        .to_bytecode();
    let actor = ActorID::Constructor(constructor);
    let unsigned = ScriptBuilder::new()
        .push_str(VmString::contract(input.clone()))
        .input()
        .signtx()
        .push_int(20 * FLAME)
        .split()
        .push_int(1u64)
        .push_str(input.predicate.to_point().to_bytes().to_vec())
        .push_int(1_000_000u64)
        .push_str(actor.to_envelope().unwrap().encode())
        .send()
        .push_point(input.predicate.to_point().to_bytes())
        .output()
        .build_tx(header(), limits())
        .unwrap();
    let change = created(unsigned.log())[0].id();
    let a = account(&SEED_A);
    let signed = sign(unsigned, &[a.spending_key_at(util::RECEIVING, 0).unwrap()]).unwrap();
    let tx = block_tx(signed, limits(), vec![live(node, &input.id())]);
    node.submit(&submitted(&tx)).unwrap();
    node.mint_block().unwrap();
    (ActorId(actor.to_hash()), change)
}

fn send_messages(
    node: &mut Node,
    actor: ActorId,
    input_id: ContractID,
    gas_limits: &[u64],
) -> ContractID {
    let input = contract_of(node, &input_id);
    let mut script = ScriptBuilder::new()
        .push_str(VmString::contract(input.clone()))
        .input()
        .signtx();
    for gas in gas_limits {
        script = script
            .push_int(0u64)
            .push_str(input.predicate.to_point().to_bytes().to_vec())
            .push_int(*gas)
            .push_str(actor.0.to_vec())
            .send();
    }
    let unsigned = script
        .push_point(input.predicate.to_point().to_bytes())
        .output()
        .build_tx(header(), limits())
        .unwrap();
    let change = created(unsigned.log())[0].id();
    let a = account(&SEED_A);
    let signed = sign(unsigned, &[a.spending_key_at(util::RECEIVING, 0).unwrap()]).unwrap();
    let tx = block_tx(signed, limits(), vec![live(node, &input_id)]);
    node.submit(&submitted(&tx)).unwrap();
    change
}

#[test]
fn blocks_include_internal_executions_in_order_and_survive_replay() {
    let dir = TempDir::new().unwrap();
    let (genesis, cfg) = devnet(dir.path(), &account(&SEED_A));
    let mut node = Node::open(&genesis, &cfg).unwrap();
    let genesis_block = node.block(0).unwrap();
    assert_eq!(genesis_block.summary.id, genesis.genesis_hash);
    assert_eq!(genesis_block.summary.header.height, 0);
    assert_eq!(genesis_block.summary.header.parent, BlockId([0; 32]));
    assert_eq!(genesis_block.summary.size_bytes, 0);
    assert!(genesis_block.executions.is_empty());

    let (actor, change) = deploy(&mut node, &genesis);
    let deployed = node.block(1).unwrap();
    assert_eq!(deployed.summary.header.parent, genesis_block.summary.id);
    assert_eq!(deployed.summary.transactions, 1);
    assert_eq!(deployed.summary.internal, 1);
    assert_eq!(deployed.summary.failed, 0);
    assert_eq!(deployed.executions.len(), 2);
    let external = &deployed.executions[0];
    let internal = &deployed.executions[1];
    assert_eq!(external.execution, ExecutionData::External);
    assert_eq!(
        (external.inputs, external.outputs, external.messages),
        (1, 1, 1)
    );
    assert_eq!(
        internal.execution,
        ExecutionData::Internal {
            parent: external.id,
            actor,
        }
    );

    // Two receives with the same parent must remain distinct and ordered.
    let change = send_messages(&mut node, actor, change, &[1_000_000, 1_000_000]);
    node.mint_block().unwrap();
    let updated = node.block(2).unwrap();
    assert_eq!(updated.summary.internal, 2);
    assert_eq!(updated.executions.len(), 3);
    assert_ne!(updated.executions[1].id, updated.executions[2].id);
    for execution in &updated.executions[1..] {
        assert_eq!(
            execution.execution,
            ExecutionData::Internal {
                parent: updated.executions[0].id,
                actor,
            }
        );
    }

    // A failed call is also a consensus execution, with its own id and error.
    send_messages(&mut node, actor, change, &[1]);
    node.mint_block().unwrap();
    let failed = node.block(3).unwrap();
    assert_eq!(
        (
            failed.summary.transactions,
            failed.summary.internal,
            failed.summary.failed
        ),
        (1, 1, 1)
    );
    let bounce = &failed.executions[1];
    let ExecutionData::InternalFailed {
        parent,
        actor: target,
        error,
    } = &bounce.execution
    else {
        panic!("expected failed internal execution")
    };
    assert_eq!(*parent, failed.executions[0].id);
    assert_eq!(*target, actor);
    assert!(!error.is_empty());

    node.mint_block().unwrap();
    let empty = node.block(4).unwrap();
    assert_eq!(
        (
            empty.summary.transactions,
            empty.summary.internal,
            empty.summary.failed
        ),
        (0, 0, 0)
    );
    assert!(empty.executions.is_empty());
    let expected = vec![genesis_block, deployed, updated, failed, empty];
    let tip = node.tip();
    let count = node.utxo_count();
    for block in &expected {
        assert_eq!(node.block(block.summary.header.height).unwrap(), *block);
    }
    assert_eq!(node.tip(), tip);
    assert_eq!(node.utxo_count(), count);
    drop(node);

    let node = Node::open(&genesis, &cfg).unwrap();
    for block in expected {
        assert_eq!(node.block(block.summary.header.height).unwrap(), block);
    }
    assert_eq!(node.tip(), tip);
}

#[test]
fn failed_delivery_summary_matches_its_archived_refund() {
    let dir = TempDir::new().unwrap();
    let a = account(&SEED_A);
    let (genesis, cfg) = devnet(dir.path(), &a);
    let mut node = Node::open(&genesis, &cfg).unwrap();
    let input = contract_of(&node, &genesis.contracts[0].id.0);
    let predicate = input.predicate.to_point().to_bytes();
    let missing = ActorId([0x77; 32]);
    let unsigned = ScriptBuilder::new()
        .push_str(VmString::contract(input.clone()))
        .input()
        .signtx()
        .push_int(20 * FLAME)
        .split()
        .push_int(1u64)
        .push_str(predicate.to_vec())
        .push_int(1_000_000u64)
        .push_str(missing.0.to_vec())
        .send()
        .push_point(predicate)
        .output()
        .build_tx(header(), limits())
        .unwrap();
    let signed = sign(unsigned, &[a.spending_key_at(util::RECEIVING, 0).unwrap()]).unwrap();
    let tx = block_tx(signed, limits(), vec![live(&node, &input.id())]);
    let txid = node.submit(&submitted(&tx)).unwrap();
    node.mint_block().unwrap();
    let block = node.block(1).unwrap();
    assert_eq!(
        (
            block.summary.transactions,
            block.summary.internal,
            block.summary.failed
        ),
        (1, 1, 1)
    );
    let bounced = &block.executions[1];
    let ExecutionData::InternalFailed {
        parent,
        actor,
        error,
    } = &bounced.execution
    else {
        panic!("expected failed internal execution")
    };
    assert_eq!(*parent, TxId(txid.0));
    assert_eq!(*actor, missing);
    assert_eq!(bounced.outputs, 1);
    assert!(!error.is_empty());
    let refund = node
        .scan(&[predicate], 1)
        .into_iter()
        .find(|output| output.txid == bounced.id)
        .unwrap();
    let id = refund.id.0;
    assert!(node.verify_proof(&id, &live(&node, &id)));
    let contract = contract_of(&node, &id);
    let flamevm::Value::Dict(mut payload) = contract.payload().clone() else {
        panic!("refund payload")
    };
    let flamevm::Value::ClearToken(value) = payload.remove(&flamevm::Scalar::ZERO).unwrap() else {
        panic!("refund token")
    };
    assert_eq!(value.qty().to_u64().unwrap(), 20 * FLAME);
    drop(node);
    let node = Node::open(&genesis, &cfg).unwrap();
    assert_eq!(node.block(1).unwrap(), block);
    assert!(node.verify_proof(&id, &live(&node, &id)));
}

#[tokio::test]
async fn block_over_http_matches_archive_and_rejects_invalid_heights() {
    let dir = TempDir::new().unwrap();
    let a = account(&SEED_A);
    let (genesis, cfg) = devnet(dir.path(), &a);
    let mut node = Node::open(&genesis, &cfg).unwrap();
    let id = genesis.contracts[0].id.0;
    let input = InputSpec::clear(
        contract_of(&node, &id),
        live(&node, &id),
        a.spending_key_at(util::RECEIVING, 0).unwrap(),
    )
    .unwrap();
    let (transfer, _) = signed_transfer(
        vec![input],
        &[output(&a, util::CHANGE, 0, GENESIS_SPARKS - FEE)],
        FEE,
        &mut rng(70),
    );
    let txid = node.submit(&submitted(&transfer)).unwrap();
    node.mint_block().unwrap();
    let block = node.block(1).unwrap();
    assert_eq!(
        (
            block.summary.transactions,
            block.summary.internal,
            block.summary.failed
        ),
        (1, 0, 0)
    );
    assert_eq!(block.executions.len(), 1);
    assert_eq!(block.executions[0].id, TxId(txid.0));
    assert_eq!(block.executions[0].fee_sparks, FEE.to_string());
    assert_eq!(
        (
            block.executions[0].inputs,
            block.executions[0].outputs,
            block.executions[0].messages
        ),
        (1, 1, 0)
    );
    let archived = crate::store::BlockStore::open(&cfg.blocks_path())
        .unwrap()
        .replay(genesis.chain.params())
        .unwrap()
        .remove(0);
    assert_eq!(block.summary.id, BlockId(archived.header.id().into_bytes()));
    assert_eq!(block.summary.header.version, archived.header.version);
    assert_eq!(
        block.summary.size_bytes,
        archived.to_bytes().unwrap().len() as u64
    );
    assert_eq!(block.summary.header.height, archived.header.height);
    assert_eq!(
        block.summary.header.core_block_hash,
        archived.header.core_block_hash
    );
    assert_eq!(
        block.summary.header.parent,
        BlockId(archived.header.parent.into_bytes())
    );
    assert_eq!(
        block.summary.header.witness_root,
        archived.header.witness_root
    );
    assert_eq!(
        block.summary.header.effects_root,
        archived.header.effects_root
    );
    assert_eq!(
        block.summary.header.state.actors,
        archived.header.state.actors
    );
    assert_eq!(
        block.summary.header.state.contracts,
        archived.header.state.contracts.0
    );
    assert_eq!(
        block.summary.header.state.available_storage_units,
        archived.header.state.available_storage_units,
    );
    let genesis_block = node.block(0).unwrap();
    let (addr, handle) = crate::rpc::serve(Arc::new(Mutex::new(node)), cfg.rpc_bind)
        .await
        .unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();
    assert_eq!(client.block(0).await.unwrap(), genesis_block);
    assert_eq!(client.block(1).await.unwrap(), block);
    for height in [2, u64::MAX] {
        let ClientError::Call(error) = client.block(height).await.unwrap_err() else {
            panic!("expected JSON-RPC error");
        };
        assert_eq!(error.code(), flamed_rpc::codes::NOT_FOUND);
    }
    for params in [rpc_params![-1], rpc_params!["1"], rpc_params![]] {
        let invalid: Result<BlockResult, _> = client.request("block", params).await;
        let ClientError::Call(error) = invalid.unwrap_err() else {
            panic!("expected invalid params");
        };
        assert_eq!(error.code(), flamed_rpc::ErrorCode::InvalidParams.code());
    }
    drop(client);
    handle.stop().unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle.stopped())
        .await
        .unwrap();
}
