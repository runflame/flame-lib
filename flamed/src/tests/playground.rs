//! A mint playground as the core chain, served by a stand-in that answers
//! the two routes the indexer reads.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;

use flamechain::INITIAL_ALLOTMENT_SPARKS;
use flamekd::util::RECEIVING;
use flamevm::{Scalar, Value};
use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::indexer::{core_block_hash, CoreError};
use crate::node::NodeError;
use crate::playground::PlaygroundIndexer;

/// Serves `answers` in order, one connection each: the request line it
/// expects, and the JSON body it answers with.
fn playground(answers: Vec<(String, serde_json::Value)>) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        for (expected, body) in answers {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert_eq!(request.trim_end(), format!("GET {expected} HTTP/1.1"));
            let mut line = String::new();
            while line != "\r\n" {
                line.clear();
                reader.read_line(&mut line).unwrap();
            }
            let body = body.to_string();
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    (url, server)
}

#[test]
fn the_playground_s_burns_mint_on_its_blocks() {
    let dir = TempDir::new().unwrap();
    let a = account(&SEED_A);
    let b = account(&SEED_B);
    let (genesis, cfg) = devnet(dir.path(), &a);
    let alice = a.address_at(RECEIVING, 0).unwrap();
    let bob = b.address_at(RECEIVING, 0).unwrap();
    let (url, server) = playground(vec![
        // Before the playground's first block the chain stays on genesis.
        ("/api/core/tip".into(), json!({"height": 0, "time": 0})),
        ("/api/core/tip".into(), json!({"height": 2, "time": 1})),
        (
            "/api/core/blocks?from=1&to=2".into(),
            json!([
                {"height": 1, "time": 0, "burns": [
                    {"address": alice.to_bech32(Network::Testnet), "sats": 3},
                    {"address": bob.to_bech32(Network::Testnet), "sats": 1},
                ]},
                {"height": 2, "time": 1, "burns": []},
            ]),
        ),
    ]);
    let indexer = PlaygroundIndexer::new(&url, Network::Testnet).unwrap();
    let mut node = Node::open_with(&genesis, &cfg, Box::new(indexer)).unwrap();

    node.mint_block().unwrap();
    let first = node.block_summary(1).unwrap().header.clone();
    assert_eq!((first.core_height, first.core_block_hash), (0, [0; 32]));

    node.mint_block().unwrap();
    server.join().unwrap();
    let second = node.block_summary(2).unwrap().header.clone();
    assert_eq!(second.core_height, 2);
    assert_eq!(second.core_block_hash, core_block_hash(2));

    // Core block 1's allotment split 3:1; core block 2 had no burns.
    for (account, sparks) in [
        (&a, INITIAL_ALLOTMENT_SPARKS / 4 * 3),
        (&b, INITIAL_ALLOTMENT_SPARKS / 4),
    ] {
        let predicate = account
            .predicate_at(RECEIVING, 0)
            .unwrap()
            .to_point()
            .to_bytes();
        let hits = node.scan(&[predicate], 1);
        assert_eq!(hits.len(), 1);
        let contract = contract_of(&node, &hits[0].id.0);
        let Value::ClearToken(token) = contract.payload() else {
            panic!("a mint output holds cleartext flames");
        };
        assert_eq!(token.qty(), Scalar::from(sparks));
    }
}

#[test]
fn a_delegated_burn_pays_its_service_the_commission_of_its_block() {
    let dir = TempDir::new().unwrap();
    let a = account(&SEED_A);
    let b = account(&SEED_B);
    let (genesis, cfg) = devnet(dir.path(), &a);
    let service = a
        .address_at(RECEIVING, 0)
        .unwrap()
        .to_bech32(Network::Testnet);
    let delegator = b
        .address_at(RECEIVING, 0)
        .unwrap()
        .to_bech32(Network::Testnet);
    let (url, server) = playground(vec![
        ("/api/core/tip".into(), json!({"height": 1, "time": 0})),
        (
            "/api/core/blocks?from=1&to=1".into(),
            json!([{"height": 1, "time": 0, "burns": [
                {"address": service, "sats": 1},
                {"address": delegator, "sats": 1,
                 "service": {"address": service, "commission_bps": 200}},
            ]}]),
        ),
    ]);
    let indexer = PlaygroundIndexer::new(&url, Network::Testnet).unwrap();
    let mut node = Node::open_with(&genesis, &cfg, Box::new(indexer)).unwrap();
    node.mint_block().unwrap();
    server.join().unwrap();

    // Each burned half the block; the service takes 2% of the delegator's.
    let half = INITIAL_ALLOTMENT_SPARKS / 2;
    for (account, sparks) in [(&a, half + half / 50), (&b, half - half / 50)] {
        let predicate = account
            .predicate_at(RECEIVING, 0)
            .unwrap()
            .to_point()
            .to_bytes();
        let hits = node.scan(&[predicate], 1);
        assert_eq!(hits.len(), 1);
        let contract = contract_of(&node, &hits[0].id.0);
        let Value::ClearToken(token) = contract.payload() else {
            panic!("a mint output holds cleartext flames");
        };
        assert_eq!(token.qty(), Scalar::from(sparks));
    }
}

#[test]
fn a_playground_on_another_network_or_behind_the_chain_is_an_error() {
    let dir = TempDir::new().unwrap();
    let a = account(&SEED_A);
    let (genesis, cfg) = devnet(dir.path(), &a);
    let mainnet = a
        .address_at(RECEIVING, 0)
        .unwrap()
        .to_bech32(Network::Mainnet);
    let (url, server) = playground(vec![
        ("/api/core/tip".into(), json!({"height": 1, "time": 0})),
        (
            "/api/core/blocks?from=1&to=1".into(),
            json!([{"height": 1, "time": 0, "burns": [{"address": mainnet, "sats": 1}]}]),
        ),
        ("/api/core/tip".into(), json!({"height": 1, "time": 0})),
        (
            "/api/core/blocks?from=1&to=1".into(),
            json!([{"height": 1, "time": 0, "burns": []}]),
        ),
        // Reset under the node: its tip is now behind the anchor.
        ("/api/core/tip".into(), json!({"height": 0, "time": 0})),
    ]);
    let indexer = PlaygroundIndexer::new(&url, Network::Testnet).unwrap();
    let mut node = Node::open_with(&genesis, &cfg, Box::new(indexer)).unwrap();

    assert!(matches!(
        node.mint_block(),
        Err(NodeError::Indexer(CoreError::Playground { .. }))
    ));
    assert_eq!(node.tip().height, 0);
    node.mint_block().unwrap();
    assert!(matches!(
        node.mint_block(),
        Err(NodeError::Indexer(CoreError::Inconsistent(_)))
    ));
    server.join().unwrap();
    assert_eq!(node.tip().height, 1);

    assert!(PlaygroundIndexer::new("https://example.org", Network::Testnet).is_err());
    assert!(PlaygroundIndexer::new("http://host:1/path", Network::Testnet).is_err());
}
