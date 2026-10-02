//! What the wire actually looks like. Every assertion here is a JSON literal,
//! because that — not the Rust type — is the thing two programs have to agree
//! on.

use crate::types::{
    BlockId, ContractEnvelope, ContractId, NoteEnvelope, PredicatePoint, ProofBytes, ProofResult,
    ScanEntry, ScanResult, SpentAt, TipResult, TxId, TxStatusResult,
};

#[test]
fn an_id_is_a_bare_hex_string() {
    let id = BlockId([0xab; 32]);
    let json = serde_json::to_string(&id).expect("serialize");
    assert_eq!(json, format!("\"{}\"", "ab".repeat(32)));
    assert_eq!(
        serde_json::from_str::<BlockId>(&json).expect("deserialize"),
        id
    );
}

#[test]
fn an_id_refuses_the_wrong_length() {
    let short = format!("\"{}\"", "ab".repeat(31));
    assert!(serde_json::from_str::<ContractId>(&short).is_err());
    assert!(serde_json::from_str::<TxId>("\"nothex\"").is_err());
    // A JSON array is what a bare `transparent` newtype would have produced.
    assert!(serde_json::from_str::<PredicatePoint>("[1,2,3]").is_err());
}

#[test]
fn a_blob_is_bare_base64() {
    let bytes = ProofBytes(vec![0x01, 0x02, 0x03, 0xff]);
    let json = serde_json::to_string(&bytes).expect("serialize");
    assert_eq!(json, "\"AQID/w==\"");
    assert_eq!(
        serde_json::from_str::<ProofBytes>(&json).expect("deserialize"),
        bytes
    );
    assert!(serde_json::from_str::<ContractEnvelope>("\"not base64!\"").is_err());
}

#[test]
fn a_tip_names_its_fields() {
    let tip = TipResult {
        hash: BlockId([0x11; 32]),
        height: 7,
        contract_root: [0x22; 32],
    };
    let json = serde_json::to_value(tip).expect("serialize");
    assert_eq!(json["height"], 7);
    assert_eq!(json["hash"], "11".repeat(32));
    assert_eq!(json["contract_root"], "22".repeat(32));
}

#[test]
fn a_status_is_an_internally_tagged_enum() {
    // The tagged path deserializes through serde's buffered content, which is
    // why the codec modules take an owned `String` rather than a `&str`.
    let unspent = ProofResult::Unspent {
        proof: ProofBytes(vec![0x00]),
    };
    let json = serde_json::to_string(&unspent).expect("serialize");
    assert_eq!(json, r#"{"status":"unspent","proof":"AA=="}"#);
    assert_eq!(
        serde_json::from_str::<ProofResult>(&json).expect("deserialize"),
        unspent
    );

    assert_eq!(
        serde_json::to_string(&ProofResult::Unknown).expect("serialize"),
        r#"{"status":"unknown"}"#
    );
    assert_eq!(
        serde_json::to_string(&TxStatusResult::Confirmed {
            height: 3,
            block: BlockId([0; 32])
        })
        .expect("serialize"),
        format!(
            r#"{{"status":"confirmed","height":3,"block":"{}"}}"#,
            "00".repeat(32)
        )
    );
}

#[test]
fn an_unspent_scan_entry_omits_its_spend() {
    let entry = ScanEntry {
        id: ContractId([0x33; 32]),
        height: 1,
        txid: TxId([0x44; 32]),
        predicate: PredicatePoint([0x55; 32]),
        bytes: ContractEnvelope(vec![0xde, 0xad]),
        note: None,
        spent: None,
    };
    let result = ScanResult {
        tip_height: 9,
        outputs: vec![entry.clone()],
    };
    let json = serde_json::to_value(&result).expect("serialize");
    assert!(json["outputs"][0].get("spent").is_none());

    let spent = ScanEntry {
        spent: Some(SpentAt {
            height: 2,
            txid: TxId([0x66; 32]),
        }),
        ..entry
    };
    let round_tripped: ScanEntry =
        serde_json::from_str(&serde_json::to_string(&spent).expect("serialize"))
            .expect("deserialize");
    assert_eq!(round_tripped, spent);
}

#[test]
fn a_scan_entry_carries_its_note_as_base64_or_omits_it() {
    let entry = ScanEntry {
        id: ContractId([0x33; 32]),
        height: 1,
        txid: TxId([0x44; 32]),
        predicate: PredicatePoint([0x55; 32]),
        bytes: ContractEnvelope(vec![0xde, 0xad]),
        note: Some(NoteEnvelope(vec![0x01, 0x02, 0x03, 0xff])),
        spent: None,
    };
    let json = serde_json::to_value(&entry).expect("serialize");
    assert_eq!(json["note"], "AQID/w==");
    assert_eq!(
        serde_json::from_value::<ScanEntry>(json).expect("deserialize"),
        entry
    );

    // Without a note the field is absent, not null, and its absence reads
    // back as no note.
    let bare = ScanEntry {
        note: None,
        ..entry
    };
    let json = serde_json::to_value(&bare).expect("serialize");
    assert!(json.get("note").is_none());
    assert_eq!(
        serde_json::from_value::<ScanEntry>(json).expect("deserialize"),
        bare
    );
}

#[test]
fn a_block_preserves_execution_categories_links_and_exact_fees() {
    use crate::{
        ActorId, BlockHeader, BlockResult, BlockSummary, ExecutionData, StateCommitment,
        TransactionSummary,
    };

    let result = BlockResult {
        summary: BlockSummary {
            id: BlockId([0x11; 32]),
            header: BlockHeader {
                version: 1,
                height: 1,
                core_block_hash: [0x99; 32],
                parent: BlockId([0x22; 32]),
                witness_root: [0xaa; 32],
                effects_root: [0x33; 32],
                state: StateCommitment {
                    contracts: [0x55; 32],
                    actors: [0x44; 32],
                    available_storage_units: 8192,
                },
            },
            transactions: 1,
            internal: 1,
            failed: 1,
            size_bytes: 1234,
        },
        executions: vec![TransactionSummary {
            id: TxId([0x66; 32]),
            execution: ExecutionData::InternalFailed {
                parent: TxId([0x77; 32]),
                actor: ActorId([0x88; 32]),
                error: "actor not found".into(),
            },
            fee_sparks: "18446744073709551615".into(),
            inputs: 0,
            outputs: 1,
            messages: 0,
        }],
    };
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "summary": {
                "id": "11".repeat(32),
                "header": {
                    "version": 1, "height": 1, "core_block_hash": "99".repeat(32),
                    "parent": "22".repeat(32), "witness_root": "aa".repeat(32),
                    "effects_root": "33".repeat(32),
                    "state": {
                        "contracts": "55".repeat(32), "actors": "44".repeat(32),
                        "available_storage_units": 8192
                    }
                },
                "transactions": 1, "internal": 1, "failed": 1, "size_bytes": 1234
            },
            "executions": [{
                "id": "66".repeat(32),
                "execution": {
                    "kind": "internal_failed", "parent": "77".repeat(32),
                    "actor": "88".repeat(32), "error": "actor not found"
                },
                "fee_sparks": "18446744073709551615", "inputs": 0, "outputs": 1,
                "messages": 0
            }]
        })
    );
    assert_eq!(serde_json::from_value::<BlockResult>(json).unwrap(), result);
}

#[test]
fn execution_data_is_nested_and_requires_each_variants_fields() {
    use crate::{ActorId, ExecutionData, TransactionSummary};
    use serde_json::json;

    let cases = [
        (ExecutionData::External, json!({"kind": "external"}), vec![]),
        (
            ExecutionData::Internal {
                parent: TxId([0x77; 32]),
                actor: ActorId([0x88; 32]),
            },
            json!({
                "kind": "internal", "parent": "77".repeat(32), "actor": "88".repeat(32)
            }),
            vec!["parent", "actor"],
        ),
        (
            ExecutionData::InternalFailed {
                parent: TxId([0x77; 32]),
                actor: ActorId([0x88; 32]),
                error: "actor not found".into(),
            },
            json!({
                "kind": "internal_failed", "parent": "77".repeat(32),
                "actor": "88".repeat(32), "error": "actor not found"
            }),
            vec!["parent", "actor", "error"],
        ),
    ];
    for (execution, fields, required) in cases {
        let summary = TransactionSummary {
            id: TxId([0x66; 32]),
            execution,
            fee_sparks: "0".into(),
            inputs: 0,
            outputs: 0,
            messages: 0,
        };
        let expected = json!({
            "id": "66".repeat(32), "fee_sparks": "0", "inputs": 0,
            "outputs": 0, "messages": 0, "execution": fields
        });
        assert_eq!(serde_json::to_value(&summary).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<TransactionSummary>(expected.clone()).unwrap(),
            summary
        );
        for field in std::iter::once("kind").chain(required) {
            let mut missing = expected.clone();
            missing["execution"].as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<TransactionSummary>(missing).is_err(),
                "accepted missing {field} in {expected}"
            );
            let mut null = expected.clone();
            null["execution"][field] = serde_json::Value::Null;
            assert!(
                serde_json::from_value::<TransactionSummary>(null).is_err(),
                "accepted null {field} in {expected}"
            );
        }
    }
}
