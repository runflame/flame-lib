//! The shapes build, their metrics are what the tables assume, and every
//! send the benchmarks time works.
//!
//! Proving is slow without optimizations, so the shapes are built once for
//! every test here to share.

use std::sync::OnceLock;

use flamechain::BlockTx;
use flamepayments::BuilderError;
use flamevm::{TxEntry, TxLog, VMError};
use merlin::Transcript;
use musig::{Multisignature, VerificationKey};

use crate::first_send::{contract_bytes, contract_from_bytes, Scratch};
use crate::fixtures::{self, SendError, Shape, Transfer, OVER_CAPACITY, SHAPES};

fn built() -> &'static (Vec<Shape>, Transfer) {
    static BUILT: OnceLock<(Vec<Shape>, Transfer)> = OnceLock::new();
    BUILT.get_or_init(fixtures::shapes_and_over_capacity)
}

fn shapes() -> &'static [Shape] {
    &built().0
}

fn count(log: &TxLog, input: bool) -> usize {
    log.entries()
        .iter()
        .filter(|entry| match entry {
            TxEntry::Input(_) => input,
            TxEntry::Output(_) => !input,
            _ => false,
        })
        .count()
}

#[test]
fn every_shape_verifies_with_its_inputs_and_outputs() {
    let params = fixtures::chain_params();
    assert_eq!(shapes().len(), SHAPES.len());
    for (shape, spec) in shapes().iter().zip(SHAPES) {
        assert_eq!(shape.spec, spec);
        let (log, metrics) = shape
            .tx
            .verify_with_metrics(fixtures::limits())
            .unwrap_or_else(|e| panic!("{} verifies: {e:?}", spec.label));
        assert_eq!(count(&log, true), spec.inputs, "{} inputs", spec.label);
        assert_eq!(count(&log, false), spec.outputs, "{} outputs", spec.label);
        assert_eq!(metrics.multiplications, shape.metrics.multiplications);
        assert_eq!(metrics.gas_used, shape.metrics.gas_used);
        assert_eq!(
            metrics.total_fee,
            fixtures::FEE,
            "{} pays the fee",
            spec.label
        );
        assert!(
            metrics.multiplications <= params.limits.max_multiplications_per_transaction,
            "{}: {} multipliers",
            spec.label,
            metrics.multiplications
        );
        assert_eq!(
            shape.tx.txid, shape.txid,
            "{} signs its own txid",
            spec.label
        );
        assert_eq!(log.effect_id(), shape.tx.effect_id());
        assert_eq!(shape.transfer.inputs.len(), spec.inputs);
        assert_eq!(shape.transfer.outputs.len(), spec.outputs);
    }
}

#[test]
fn every_note_opens_for_its_owner() {
    for shape in shapes() {
        let log = shape.tx.verify(fixtures::limits()).expect("verify");
        assert_eq!(shape.transfer.outputs.len(), shape.spec.outputs);
        for payment in &shape.transfer.outputs {
            let owner = payment.path.owner();
            let (contract, received) = fixtures::open(&log, &owner);
            assert!(received.memo.is_empty(), "every shape has an empty memo");
            assert_eq!(received.opening.qty, payment.output.qty);
            assert_eq!(
                contract.predicate.to_point(),
                owner.address.spending_key().compress()
            );
            // The path re-derives the address the output was paid to.
            assert_eq!(owner.address, payment.output.address);
        }
    }
}

#[test]
fn every_shapes_send_rebuilds_verifies_and_opens_its_notes() {
    for shape in shapes() {
        let label = shape.spec.label;
        let bytes = shape
            .transfer
            .send(&mut fixtures::rng(fixtures::SEND_RNG_SEED))
            .unwrap_or_else(|e| panic!("{label} sends: {e:?}"));
        let params = fixtures::chain_params();
        let sent = BlockTx::from_bytes_bounded(&bytes, params.version, params.limits)
            .unwrap_or_else(|e| panic!("{label} decodes: {e:?}"));
        assert_eq!(sent.proofs.len(), shape.spec.inputs);
        let (log, metrics) = sent
            .tx
            .verify_with_metrics(fixtures::limits())
            .unwrap_or_else(|e| panic!("{label} verifies: {e:?}"));
        assert_eq!(metrics.multiplications, shape.metrics.multiplications);
        assert_eq!(metrics.gas_used, shape.metrics.gas_used);
        assert_eq!(sent.tx.encoded_size(), shape.tx.encoded_size());
        // The send bench checks new processes' packages against these bytes.
        assert_eq!(shape.decode, fixtures::DecodeMethod::BlockTx);
        assert_eq!(
            bytes.len(),
            shape.bytes.len(),
            "{label}: same length every time"
        );
        assert_ne!(sent.tx.txid, shape.txid, "{label}: a fresh r, a fresh send");
        assert_eq!(log.effect_id(), sent.tx.effect_id());
        for payment in &shape.transfer.outputs {
            let (_, received) = fixtures::open(&log, &payment.path.owner());
            assert_eq!(received.opening.qty, payment.output.qty);
        }
    }
}

#[test]
fn the_sign_step_computes_what_sign_does() {
    let shape = &shapes()[1];
    let transfer = &shape.transfer;
    let inputs = transfer.prepare().expect("prepare");
    let unsigned = transfer
        .build(
            &inputs,
            &transfer.output_specs(),
            &mut fixtures::rng(fixtures::SEND_RNG_SEED),
        )
        .expect("build");
    let (signature, txid) = fixtures::signature(&unsigned, &transfer.keys()).expect("sign");
    let instructions = unsigned.signing_instructions();
    assert_eq!(txid, instructions.txid);
    let items: Vec<_> = instructions
        .items
        .iter()
        .map(|(key, contract)| (VerificationKey::from_compressed(*key), *contract))
        .collect();
    let mut transcript = Transcript::new(b"flamevm.signtx");
    transcript.append_message(b"txid", &txid.0);
    assert!(signature.verify_multi(&mut transcript, items).is_ok());

    // Attached, it verifies as the transaction's own signature.
    let tx = unsigned.sign(signature);
    assert!(tx.verify(fixtures::limits()).is_ok());
    let packaged = fixtures::package(fixtures::copy_tx(&tx), shape.spec.inputs).expect("package");
    assert_eq!(
        packaged,
        fixtures::package(tx, shape.spec.inputs).expect("package")
    );
}

#[test]
fn one_output_past_the_capacity_does_not_prove() {
    let transfer = &built().1;
    assert_eq!(transfer.inputs.len(), OVER_CAPACITY.inputs);
    assert_eq!(transfer.outputs.len(), OVER_CAPACITY.outputs);
    assert!(
        matches!(
            transfer.send(&mut fixtures::rng(fixtures::SEND_RNG_SEED)),
            Err(SendError::Build(BuilderError::Vm(
                VMError::R1CSProofConstruction
            )))
        ),
        "1 → 14 fails to prove, for the proof's capacity"
    );
}

#[test]
fn a_scratch_file_gives_the_child_the_same_transfer() {
    for shape in shapes() {
        let scratch = Scratch::new(shape.spec.id, &shape.transfer).expect("scratch");
        let text = serde_json::to_string(&scratch).expect("serialize");
        let back: Scratch = serde_json::from_str(&text).expect("deserialize");
        let transfer = back.transfer().expect("the child's route");
        for (child, parent) in transfer.inputs.iter().zip(&shape.transfer.inputs) {
            assert_eq!(child.contract.id(), parent.contract.id());
            assert_eq!(child.note, parent.note);
            assert_eq!(child.key, parent.key);
            assert_eq!(child.opening.qty, parent.opening.qty);
            assert_eq!(child.opening.qty_blinding, parent.opening.qty_blinding);
        }
        for (child, parent) in transfer.outputs.iter().zip(&shape.transfer.outputs) {
            assert_eq!(child.output.address, parent.output.address);
            assert_eq!(child.output.qty, parent.output.qty);
        }
        assert!(transfer.prepare().is_ok());
    }

    // A contract survives its envelope, and nothing else decodes as one.
    let contract = &shapes()[0].transfer.inputs[0].contract;
    let bytes = contract_bytes(contract).expect("encode");
    assert_eq!(
        contract_from_bytes(&bytes).expect("decode").id(),
        contract.id()
    );
    let mut padded = bytes;
    padded.push(0);
    assert!(contract_from_bytes(&padded).is_err());

    // Scratch files hold hex, and nothing else passes for it.
    let mut scratch = Scratch::new("1to2", &shapes()[0].transfer).expect("scratch");
    for bad in ["0", "zz", "+f"] {
        scratch.inputs[0].note = bad.to_owned();
        let error = scratch.transfer().err().expect("not hex");
        assert!(error.contains("the note is not hex"), "{bad}: {error}");
    }
}

#[test]
fn every_shape_decodes_as_recorded() {
    for shape in shapes() {
        let decoded = shape.decode_bytes().expect("decode");
        assert_eq!(decoded.txid, shape.tx.txid);
        assert_eq!(decoded.encoded_size(), shape.tx.encoded_size());
    }
}

#[test]
fn the_captured_signature_items_verify() {
    for shape in shapes() {
        let signature = shape.tx.signature.expect("signed");
        assert_eq!(shape.items.len(), shape.spec.inputs, "one item per input");
        let items: Vec<_> = shape
            .items
            .iter()
            .map(|(key, contract)| (VerificationKey::from_compressed(*key), *contract))
            .collect();
        let mut transcript = Transcript::new(b"flamevm.signtx");
        transcript.append_message(b"txid", &shape.txid.0);
        assert!(signature.verify_multi(&mut transcript, items).is_ok());

        // Another txid is another message.
        let items: Vec<_> = shape
            .items
            .iter()
            .map(|(key, contract)| (VerificationKey::from_compressed(*key), *contract))
            .collect();
        let mut transcript = Transcript::new(b"flamevm.signtx");
        transcript.append_message(b"txid", &[0u8; 32]);
        assert!(signature.verify_multi(&mut transcript, items).is_err());
    }
}

#[test]
fn the_synthetic_measurements_succeed() {
    let proof = crate::synthetic::SyntheticProof::new(5);
    assert_eq!(proof.multipliers(), 5);
    assert!(proof.verify().is_ok());
    let pc_gens = bulletproofs::PedersenGens::default();
    assert!(crate::synthetic::prove(crate::synthetic::constrain(&pc_gens, 5)).is_ok());
    assert!(std::ptr::eq(
        crate::synthetic::bulletproof_gens(),
        crate::synthetic::bulletproof_gens()
    ));
    let membership = crate::synthetic::Membership::new(4);
    assert_eq!(membership.leaves(), 16);
    assert!(membership.verify().is_ok());
}

#[test]
fn the_fixtures_file_records_the_shapes() {
    let file = crate::fixtures_file(shapes(), super::sample_cpus(), 12.5);
    assert_eq!(file.utreexo_leaves, [1 << 10, 1 << 16]);
    assert_eq!(file.limits.max_multiplications, 100_000);
    assert_eq!(file.elapsed_secs, 12.5);
    for (recorded, shape) in file.shapes.iter().zip(shapes()) {
        assert_eq!(recorded.bytes, shape.tx.encoded_size());
        assert_eq!(recorded.multiplications, shape.metrics.multiplications);
        assert_eq!(recorded.fee, fixtures::FEE);
        assert_eq!(recorded.decode_method, shape.decode);
    }
    assert_eq!(crate::scaling::thread_counts(16), [1, 2, 4, 8, 16]);
    assert_eq!(crate::scaling::thread_counts(6), [1, 2, 4]);
}
