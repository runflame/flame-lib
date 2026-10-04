//! Transaction details and pagination over the existing block index.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use flamed_rpc::{
    ClientError, ExecutionData, FlamedApiClient, HttpClientBuilder, TransactionResult,
    TransactionsResult, TxEntry as RpcTxEntry, TxId, MAX_PAGE_SIZE,
};
use flamevm::{ScriptBuilder, String as VmString};
use jsonrpsee::{core::client::ClientT, rpc_params};
use tempfile::TempDir;

use super::*;
use crate::NodeError;

#[test]
fn transaction_pages_follow_execution_order_across_blocks_and_replay() {
    let dir = TempDir::new().unwrap();
    let (genesis, cfg) = devnet(dir.path(), &account(&SEED_A));
    let mut node = Node::open(&genesis, &cfg).unwrap();
    let empty = node.transactions(None, MAX_PAGE_SIZE).unwrap();
    assert!(empty.transactions.is_empty());
    assert!(!empty.has_more);

    let (actor, change) = block::deploy(&mut node, &genesis);
    node.mint_block().unwrap();

    let change = block::send_messages(&mut node, actor, change, &[1_000_000, 1_000_000]);
    node.mint_block().unwrap();

    let mut expected: Vec<_> = (1..=3)
        .flat_map(|height| node.block(height).unwrap().executions)
        .collect();
    expected.reverse();
    assert_eq!(expected.len(), 5);

    let all = node.transactions(None, MAX_PAGE_SIZE).unwrap();
    assert_eq!(all.transactions, expected);
    assert!(!all.has_more);

    // Each cursor excludes itself, including at a block boundary.
    for (index, summary) in expected.iter().enumerate() {
        let page = node.transactions(Some(summary.id), MAX_PAGE_SIZE).unwrap();
        assert_eq!(page.transactions, expected[index + 1..]);
        assert!(!page.has_more);
    }
    for limit in [1, 2, 3, 5, MAX_PAGE_SIZE] {
        let mut before = None;
        let mut collected = Vec::new();
        loop {
            let page = node.transactions(before, limit).unwrap();
            assert!(!page.transactions.is_empty());
            assert!(page.transactions.len() <= limit as usize);
            before = page.transactions.last().map(|t| t.id);
            collected.extend(page.transactions);
            assert_eq!(page.has_more, collected.len() < expected.len());
            if !page.has_more {
                break;
            }
            assert!(collected.len() < expected.len());
        }
        assert_eq!(collected, expected);
    }

    let first = node.transactions(None, 2).unwrap();
    let cursor = first.transactions.last().unwrap().id;
    block::send_messages(&mut node, actor, change, &[1]);
    node.mint_block().unwrap();
    let rest = node.transactions(Some(cursor), MAX_PAGE_SIZE).unwrap();
    assert_eq!([first.transactions, rest.transactions].concat(), expected);
    let all = node.transactions(None, MAX_PAGE_SIZE).unwrap();
    assert_eq!(all.transactions.len(), 7);
    assert!(matches!(
        all.transactions[0].execution,
        ExecutionData::InternalFailed { .. }
    ));

    for limit in [0, MAX_PAGE_SIZE + 1, u32::MAX] {
        assert!(matches!(
            node.transactions(None, limit),
            Err(NodeError::Limit { .. })
        ));
    }
    assert!(matches!(
        node.transactions(Some(TxId([0xff; 32])), 1),
        Err(NodeError::NotFound(_))
    ));
    drop(node);
    let node = Node::open(&genesis, &cfg).unwrap();
    assert_eq!(node.transactions(None, MAX_PAGE_SIZE).unwrap(), all);
    assert_eq!(
        node.transactions(Some(cursor), MAX_PAGE_SIZE)
            .unwrap()
            .transactions,
        expected[2..]
    );
}

#[test]
fn transaction_details_preserve_effects_and_location_after_replay() {
    let dir = TempDir::new().unwrap();
    let (genesis, cfg) = devnet(dir.path(), &account(&SEED_A));
    let mut node = Node::open(&genesis, &cfg).unwrap();
    let (actor, change) = block::deploy(&mut node, &genesis);
    let change = block::send_messages(&mut node, actor, change, &[1_000_000]);
    node.mint_block().unwrap();
    block::send_messages(&mut node, actor, change, &[1]);
    node.mint_block().unwrap();

    let archived = crate::store::BlockStore::open(&cfg.blocks_path())
        .unwrap()
        .replay(genesis.chain.params())
        .unwrap();

    let mut details = Vec::new();
    for height in 1..=3 {
        let block = node.block(height).unwrap();
        for summary in block.executions {
            let detail = node.transaction(summary.id, true).unwrap();
            assert_eq!(detail.summary, summary);
            assert_eq!(detail.height, height);
            assert_eq!(detail.block, block.summary.id);
            assert!(!detail.log.is_empty());
            let effects = detail.effects.as_ref().unwrap();
            assert!(!effects.is_empty());
            if summary.execution == ExecutionData::External {
                let tx = &archived[height as usize - 1].transactions[0];
                assert_eq!(tx.tx.txid.0, summary.id.0);
                assert!(effects.iter().any(|e| matches!(e, RpcTxEntry::Send { .. })));
            } else {
                assert!(effects
                    .iter()
                    .any(|e| matches!(e, RpcTxEntry::Receive { .. })));
            }
            details.push(detail);
        }
    }
    let deployment = details[1].effects.as_ref().unwrap();
    assert!(deployment
        .iter()
        .any(|e| matches!(e, RpcTxEntry::ActorDeploy { .. })));
    assert!(deployment
        .iter()
        .any(|e| matches!(e, RpcTxEntry::ActorSave { .. })));
    assert!(deployment
        .iter()
        .any(|e| matches!(e, RpcTxEntry::SetCode { .. })));
    assert!(deployment
        .iter()
        .any(|e| matches!(e, RpcTxEntry::StoragePurchase { .. })));
    assert!(details[3]
        .effects
        .as_ref()
        .unwrap()
        .iter()
        .any(|e| matches!(e, RpcTxEntry::ActorSave { .. })));
    assert!(matches!(
        details[5].summary.execution,
        ExecutionData::InternalFailed { .. }
    ));
    assert!(details[5]
        .effects
        .as_ref()
        .unwrap()
        .iter()
        .any(|e| matches!(e, RpcTxEntry::Output { .. })));
    drop(node);
    let node = Node::open(&genesis, &cfg).unwrap();
    for detail in details {
        assert_eq!(node.transaction(detail.summary.id, true).unwrap(), detail);
    }
}

#[tokio::test]
async fn transaction_rpc_checks_parameters_and_only_returns_confirmed_executions() {
    let dir = TempDir::new().unwrap();
    let (genesis, cfg) = devnet(dir.path(), &account(&SEED_A));
    let mut node = Node::open(&genesis, &cfg).unwrap();
    let (_, change) = block::deploy(&mut node, &genesis);
    let all = node.transactions(None, MAX_PAGE_SIZE).unwrap();
    let details: Vec<_> = all
        .transactions
        .iter()
        .map(|t| node.transaction(t.id, true).unwrap())
        .collect();
    let input = contract_of(&node, &change);
    let unsigned = ScriptBuilder::new()
        .push_str(VmString::contract(input.clone()))
        .input()
        .signtx()
        .push_point(input.predicate.to_point().to_bytes())
        .output()
        .build_tx(header(), limits())
        .unwrap();
    let a = account(&SEED_A);
    let signed = sign(unsigned, &[a.spending_key_at(util::RECEIVING, 0).unwrap()]).unwrap();
    let tx = block_tx(signed, limits(), vec![live(&node, &change)]);
    let pending = TxId(node.submit(&submitted(&tx)).unwrap().0);
    let (addr, handle) = crate::rpc::serve(Arc::new(Mutex::new(node)), cfg.rpc_bind)
        .await
        .unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .unwrap();
    assert_eq!(client.transactions(None, MAX_PAGE_SIZE).await.unwrap(), all);
    for detail in details {
        let id = detail.summary.id;
        assert_eq!(client.tx(id, Some(true)).await.unwrap(), detail);
        let mut without_effects = detail.clone();
        without_effects.effects = None;
        for flag in [None, Some(false)] {
            assert_eq!(client.tx(id, flag).await.unwrap(), without_effects);
        }
        for params in [
            rpc_params![id],
            rpc_params![id, false],
            rpc_params![id, Option::<bool>::None],
        ] {
            let json: serde_json::Value = client.request("tx", params).await.unwrap();
            assert!(json.get("effects").is_none());
            assert!(json.get("raw").is_none());
            assert_eq!(
                serde_json::from_value::<TransactionResult>(json).unwrap(),
                without_effects
            );
        }
        let json: serde_json::Value = client.request("tx", rpc_params![id, true]).await.unwrap();
        assert!(json["effects"].is_array());
        assert_eq!(
            serde_json::from_value::<TransactionResult>(json).unwrap(),
            detail
        );
    }
    let first = client.transactions(None, 1).await.unwrap();
    assert!(first.has_more);
    let second = client
        .transactions(Some(first.transactions[0].id), 1)
        .await
        .unwrap();
    assert!(!second.has_more);
    assert_eq!(
        [first.transactions, second.transactions].concat(),
        all.transactions
    );
    for id in [pending, TxId([0xff; 32])] {
        for error in [
            client.tx(id, None).await.unwrap_err(),
            client.tx(id, Some(false)).await.unwrap_err(),
            client.tx(id, Some(true)).await.unwrap_err(),
            client.transactions(Some(id), 1).await.unwrap_err(),
        ] {
            let ClientError::Call(error) = error else {
                panic!("expected JSON-RPC error")
            };
            assert_eq!(error.code(), flamed_rpc::codes::NOT_FOUND);
        }
    }
    for limit in [0, MAX_PAGE_SIZE + 1, u32::MAX] {
        let ClientError::Call(error) = client.transactions(None, limit).await.unwrap_err() else {
            panic!("expected limit error")
        };
        assert_eq!(error.code(), flamed_rpc::codes::LIMIT_EXCEEDED);
    }
    for params in [
        rpc_params![-1, 1],
        rpc_params![Option::<TxId>::None, -1],
        rpc_params!["bad", 1],
    ] {
        let invalid: Result<TransactionsResult, _> = client.request("transactions", params).await;
        let ClientError::Call(error) = invalid.unwrap_err() else {
            panic!("expected invalid params")
        };
        assert_eq!(error.code(), flamed_rpc::ErrorCode::InvalidParams.code());
    }
    for params in [
        rpc_params!["bad"],
        rpc_params![all.transactions[0].id, "true"],
        rpc_params![all.transactions[0].id, 1],
    ] {
        let invalid: Result<TransactionResult, _> = client.request("tx", params).await;
        let ClientError::Call(error) = invalid.unwrap_err() else {
            panic!("expected invalid params")
        };
        assert_eq!(error.code(), flamed_rpc::ErrorCode::InvalidParams.code());
    }
    drop(client);
    handle.stop().unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle.stopped())
        .await
        .unwrap();
}
