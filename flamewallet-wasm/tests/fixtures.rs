//! Writes `test/fixtures.json`, the chain bytes `test/api.test.mjs` builds
//! transfers from:
//!
//! ```sh
//! FLAME_FIXTURES_OUT=$PWD/flamewallet-wasm/test/fixtures.json \
//!   cargo test -p flamewallet-wasm --test fixtures -- --ignored
//! ```
//!
//! Two scenarios, each a single-input, two-output payment: spending a
//! cleartext genesis allocation, and spending the confidential output that
//! payment created. Both proofs are valid at the tip after block 1, so a test
//! can rebuild either transfer as often as it likes.

use std::fmt::Write as _;

use curve25519_dalek::ristretto::CompressedRistretto;
use flamechain::codec::{contract_bytes, proof_bytes};
use flamechain::utreexo::{Catchup, Proof};
use flamechain::{utreexo_hasher, BlockTx, Blockchain, ChainParams, ContractLeaf};
use flamevm::{Anchor, ClearToken, Contract, ContractID, Predicate, Scalar, Value, FLAME_FLAVOR};
use flamewallet_ffi::{
    KeyPath, Network, Transfer, TransferInput, TransferOutput, TransferRequest, Wallet, CHANGE,
    RECEIVING,
};

const A_SEED: [u8; 64] = [0x41; 64];
const B_SEED: [u8; 64] = [0x42; 64];
const FLAME: u64 = flamechain::SPARKS_PER_FLAME;
const ALLOCATION: u64 = 1_000 * FLAME;
const PAYMENT: u64 = 400 * FLAME;
const FEE: u64 = 1_000;
const GAS: u64 = 10_000_000;

fn receiving(index: u32) -> KeyPath {
    KeyPath {
        branch: RECEIVING,
        index,
    }
}

fn change() -> KeyPath {
    KeyPath {
        branch: CHANGE,
        index: 0,
    }
}

fn served_proof(catchup: &Catchup, id: ContractID, proof: Proof) -> Vec<u8> {
    let updated = catchup
        .update_proof(&ContractLeaf(id), proof, &utreexo_hasher::<ContractLeaf>())
        .expect("the catchup accepts the proof");
    proof_bytes(&updated)
}

fn connect(chain: &mut Blockchain, transfer: &Transfer) -> flamechain::AppliedBlock {
    let params = ChainParams::default();
    let tx = BlockTx::from_bytes_bounded(&transfer.block_tx, params.version, params.limits)
        .expect("the published bytes decode");
    let block = chain.build_block([1; 32], vec![tx]).expect("build block");
    chain.connect(&block).expect("connect block")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn request_json(request: &TransferRequest) -> String {
    let inputs: Vec<String> = request
        .inputs
        .iter()
        .map(|input| {
            let opening = match &input.opening {
                None => "null".to_owned(),
                Some(o) => format!(
                    r#"{{"qty":"{}","flavor":"{}","qtyBlinding":"{}","flavorBlinding":"{}"}}"#,
                    o.qty,
                    hex(&o.flavor),
                    hex(&o.qty_blinding),
                    hex(&o.flavor_blinding)
                ),
            };
            format!(
                r#"{{"contract":"{}","proof":"{}","path":{{"branch":{},"index":{}}},"opening":{}}}"#,
                hex(&input.contract),
                hex(&input.proof),
                input.path.branch,
                input.path.index,
                opening
            )
        })
        .collect();
    let outputs: Vec<String> = request
        .outputs
        .iter()
        .map(|output| {
            format!(
                r#"{{"predicate":"{}","qty":"{}"}}"#,
                hex(&output.predicate),
                output.qty
            )
        })
        .collect();
    format!(
        r#"{{"inputs":[{}],"outputs":[{}],"fee":"{}","gas":"{}"}}"#,
        inputs.join(","),
        outputs.join(","),
        request.fee,
        request.gas
    )
}

#[test]
#[ignore = "writes test/fixtures.json; run by hand"]
fn write_fixtures() {
    let out = std::env::var("FLAME_FIXTURES_OUT").expect("FLAME_FIXTURES_OUT names the file");
    let alice = Wallet::new(A_SEED.to_vec(), Network::Testnet, 1).expect("wallet");
    let bob = Wallet::new(B_SEED.to_vec(), Network::Testnet, 1).expect("wallet");
    let alice_at = alice.address(receiving(0)).expect("address");
    let bob_at = bob.address(receiving(0)).expect("address");

    let allocation = Contract::new(
        Predicate::opaque(CompressedRistretto(
            alice_at.predicate.clone().try_into().expect("32 bytes"),
        )),
        Anchor([0x07; 32]),
        Value::ClearToken(ClearToken::new(Scalar::from(ALLOCATION), FLAME_FLAVOR)),
    )
    .expect("a clear token is portable");
    let (mut chain, genesis) =
        Blockchain::devnet_genesis(ChainParams::default(), &[allocation.id()]).expect("genesis");

    let clear = TransferRequest {
        inputs: vec![TransferInput {
            contract: contract_bytes(&allocation).expect("encode"),
            proof: served_proof(&genesis, allocation.id(), Proof::Transient),
            path: receiving(0),
            opening: None,
        }],
        outputs: vec![
            TransferOutput {
                predicate: bob_at.predicate.clone(),
                qty: PAYMENT,
                flavor: None,
            },
            TransferOutput {
                predicate: alice.address(change()).expect("change").predicate,
                qty: ALLOCATION - PAYMENT - FEE,
                flavor: None,
            },
        ],
        fee: FEE,
        gas: GAS,
        locktime: 0,
    };
    let paid = alice.build_transfer(clear.clone()).expect("build");
    let applied = connect(&mut chain, &paid);
    let to_bob = &paid.outputs[0];
    let to_bob_id: ContractID = to_bob.contract_id.clone().try_into().expect("32 bytes");

    // Allocation's proof went stale with block 1; Bob's is fresh.
    let back = 100 * FLAME;
    let confidential = TransferRequest {
        inputs: vec![TransferInput {
            contract: to_bob.contract.clone(),
            proof: served_proof(&applied.catchup, to_bob_id, Proof::Transient),
            path: receiving(0),
            opening: Some(to_bob.opening.clone()),
        }],
        outputs: vec![
            TransferOutput {
                predicate: alice_at.predicate.clone(),
                qty: back,
                flavor: None,
            },
            TransferOutput {
                predicate: bob.address(change()).expect("change").predicate,
                qty: PAYMENT - back - FEE,
                flavor: None,
            },
        ],
        fee: FEE,
        gas: GAS,
        locktime: 0,
    };

    let json = format!(
        r#"{{"network":"testnet","scenarios":{{"clear":{{"seed":"{}","request":{}}},"confidential":{{"seed":"{}","request":{}}}}}}}"#,
        hex(&A_SEED),
        request_json(&clear),
        hex(&B_SEED),
        request_json(&confidential)
    );
    std::fs::write(&out, json).expect("write fixtures");
}
