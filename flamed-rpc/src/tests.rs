//! What the wire actually looks like. Every assertion here is a JSON literal,
//! because that — not the Rust type — is the thing two programs have to agree
//! on.

use crate::types::{
    BlockId, ContractEnvelope, ContractId, PredicatePoint, ProofBytes, ProofResult, ScanEntry,
    ScanResult, SpentAt, TipResult, TxId, TxStatusResult,
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
