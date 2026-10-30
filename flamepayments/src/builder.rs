//! Building, signing, and packaging a transfer.
//!
//! The one rule this module exists to enforce: a confidential input travels
//! as a **private witness**. The prover pushes `CellValue::contract(c)` over a
//! contract whose `Token` payload carries [`Commitment::Open`] halves; the
//! emitted bytecode holds only the contract id, `build_tx` collects the
//! public body into the transaction's bag, and `input` restores the private
//! openings on the prover side alone. Nothing about the spent amount reaches
//! the wire.
//!
//! Every output carries its note, sealed by [`crate::note`] and logged in
//! the `Data` entry right after it, so its recipient needs nothing delivered
//! out of band.
//!
//! The VM offers a second way to open a token, `decrypt`, one that puts the
//! quantity and both blinding factors into the bytecode as literals, where
//! every verifier re-executes them and every spent amount becomes public one
//! hop after receipt. This module never emits it on an input;
//! `flamepayments.md` names it and explains the difference. It emits it on
//! a [`OutputSpec::clear`] output only, which `mix` has just created under
//! fresh blinding factors: opening that token publishes its amount and
//! nothing about any other.

use cells::Cell;
use core::fmt;

use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar as DalekScalar;
use flamechain::utreexo::Proof;
use flamechain::BlockTx;
use flamekd::ReceivingAddress;
use flamevm::{
    flavor_from_predicate, Anchor, CellValue, Commitment, Contract, ExternalTx, Limits, Point,
    Predicate, Scalar, ScriptBuilder, Token, TxHeader, UnsignedTx, VMError, Value,
};
use merlin::Transcript;
use musig::{Multisignature, MusigError};
use rand_core::{CryptoRng, RngCore};
use zeroize::Zeroizing;

use crate::note::{self, MEMO_MAX};

/// The most outputs one transfer can carry.
///
/// [`ScriptBuilder::roll_k`] takes a `u8` and encodes it in the opcode's low
/// nibble, so any `k` above 15 would prove one program and publish another.
/// The first of `n` outputs is rolled from depth `n - 1`, which makes 16 the
/// exact encoding bound.
///
/// It is not a reachable one. `mix` range-proves every output over 64 bits,
/// and the prover's shared `BulletproofGens::new(1024, 1)` runs out of
/// multipliers first: a single-input transfer proves at 13 outputs and fails
/// at 14 with `VMError::R1CSProofConstruction`. A bigger `m` lowers that
/// further. This guard is therefore defensive — it keeps a `roll_k` overflow
/// out of the bytecode, but a caller meets the proving ceiling before it.
pub const MAX_OUTPUTS: usize = 16;

/// The secret behind a confidential token: what its recipient spends the
/// output with. [`crate::open_note`] reads it from the output's note.
///
/// Its `Debug` output shows none of the four fields: every one of them is
/// what the token's commitments hide.
#[derive(Clone, Copy)]
pub struct Opening {
    pub qty: u64,
    pub flv: Scalar,
    pub qty_blinding: DalekScalar,
    pub flv_blinding: DalekScalar,
}

impl fmt::Debug for Opening {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Opening").finish_non_exhaustive()
    }
}

/// One contract being spent.
///
/// Only the two constructors below build one, so a confidential input always
/// carries its witness: there is no way to hand `build_transfer` a token with
/// closed commitments and discover the mistake inside `mix`.
pub struct InputSpec {
    contract: Contract,
    proof: Proof,
    signing_key: DalekScalar,
}

impl InputSpec {
    /// A cleartext input: the contract exactly as published. Its payload must
    /// be a bare [`Value::ClearToken`]; a [`Value::Token`] is
    /// [`BuilderError::OpeningMissing`], because a published token's
    /// commitments are closed and would fail inside `mix`.
    pub fn clear(
        published: Contract,
        proof: Proof,
        signing_key: DalekScalar,
    ) -> Result<InputSpec, BuilderError> {
        match published.payload() {
            Value::ClearToken(_) => {}
            Value::Token(_) => return Err(BuilderError::OpeningMissing),
            _ => return Err(BuilderError::PayloadNotToken),
        }
        Ok(InputSpec {
            contract: published,
            proof,
            signing_key,
        })
    }

    /// A confidential input, rebuilt with open commitments from `opening`.
    ///
    /// Refused unless the rebuilt contract's id equals the published one.
    /// That equality **is** the opening check, and it is made here, before
    /// any script exists: a Contract cell encodes the commitment points
    /// only, so a token the caller can open and the same token as published
    /// share an id exactly when the opening is the right one.
    pub fn confidential(
        published: &Contract,
        opening: &Opening,
        proof: Proof,
        signing_key: DalekScalar,
    ) -> Result<InputSpec, BuilderError> {
        match published.payload() {
            Value::Token(_) => {}
            Value::ClearToken(_) => return Err(BuilderError::OpeningNotNeeded),
            _ => return Err(BuilderError::PayloadNotToken),
        }
        let token = Token::from_opening(
            Scalar::from(opening.qty),
            opening.flv,
            opening.qty_blinding,
            opening.flv_blinding,
        )
        .expect("a u64 quantity is always inside the 64-bit range");
        let contract = Contract::new(
            published.predicate.to_opaque(),
            published.anchor,
            Value::Token(token),
        )?;
        if contract.id() != published.id() {
            return Err(BuilderError::OpeningMismatch);
        }
        Ok(InputSpec {
            contract,
            proof,
            signing_key,
        })
    }

    /// The key that authorizes this input through `signtx`.
    pub fn signing_key(&self) -> DalekScalar {
        self.signing_key
    }

    /// The Utreexo membership proof the chain will check for this input.
    pub fn proof(&self) -> &Proof {
        &self.proof
    }
}

/// One contract being created: `qty` of `flv` to `address`, with a memo
/// for its recipient.
///
/// The output is locked by `Predicate::opaque(S)` over the address's
/// spending key, and its note is sealed to the address's viewing key.
#[derive(Clone, Debug)]
pub struct OutputSpec {
    pub address: ReceivingAddress,
    pub qty: u64,
    pub flv: Scalar,
    pub memo: Vec<u8>,
    /// Publish a `ClearToken` instead: its amount and flavor in the clear,
    /// and no note. The `Data` entry after it holds `memo` as plain bytes,
    /// which anyone reads and its recipient's wallet ignores.
    pub clear: bool,
}

/// Builds the unsigned transfer.
///
/// The script, in order:
///
/// ```text
/// per input:    push_literal(CellValue::contract(contract))  input  signtx
/// fee > 0:      push_int(fee)  fee                    one more mix input
/// per output:   push_literal(CellValue::commitment(qty))  push_literal(CellValue::commitment(flv))
///               push_int(m)  push_int(n)  mix
/// per output i: roll_k(n-1-i) if > 0
///               clear: push_int(flv)  push_int(f')  push_int(qty)  push_int(q')
///                      decrypt
///               push_point(S_i)  output  pushcell(note_i)  log
/// ```
///
/// `signtx` pushes the contract's single payload Value and no count, so with
/// a bare token payload there is nothing to drop. A zero fee emits no `fee`
/// opcode. The outputs are emitted sorted by their quantity commitment,
/// compared as bytes, not in `outputs` order: under a fresh blinding factor
/// that order says nothing about which output is the change. The roll before
/// each `output` keeps every commitment, predicate and note in that one
/// order: `mix` leaves the tokens in the order they were pushed and `output`
/// pops from the top, which would otherwise reverse them. `pushcell(note)
/// log` leaves the stack as it found it. A clear output's `decrypt` opens
/// the token `mix` just made for it, under blinding factors drawn for it
/// alone, into the `ClearToken` it publishes; its note is its memo.
///
/// `rng` draws each output's `r`, hedged as `payments.md` "Sender
/// randomness" recommends: a Merlin transcript binds the header, the
/// inputs, the fee and the outputs, and is rekeyed with the inputs' signing
/// keys before `rng` finalizes it. A weak `rng` then still gives a transfer
/// rebuilt with anything changed, or signed by other keys, a different `r`.
pub fn build_transfer<R: RngCore + CryptoRng>(
    inputs: &[InputSpec],
    outputs: &[OutputSpec],
    fee: u64,
    header: TxHeader,
    limits: Limits,
    rng: &mut R,
) -> Result<UnsignedTx, BuilderError> {
    if outputs.len() > MAX_OUTPUTS {
        return Err(BuilderError::TooManyOutputs(outputs.len()));
    }
    if let Some(output) = outputs.iter().find(|output| output.memo.len() > MEMO_MAX) {
        return Err(BuilderError::MemoTooLong(output.memo.len()));
    }

    let mut rng = hedged_rng(&header, inputs, fee, outputs, rng);
    let sealed = seal(outputs, &mut rng);

    let mut program = ScriptBuilder::new();
    for input in inputs {
        program = program
            .push_cell(CellValue::contract(input.contract.clone()).map_err(VMError::from)?)
            .input()
            .signtx();
    }

    // The fee debt is one more token `mix` has to balance.
    let mut mix_inputs = inputs.len();
    if fee > 0 {
        program = program.push_int(fee).fee();
        mix_inputs += 1;
    }

    Ok(mix_and_output(program, mix_inputs, sealed)?.build_tx(header, limits)?)
}

/// Builds and signs a confidential issuance: `qty` of a new flavor, minted
/// under the predicate of `issuer`'s contract, and paid out with that
/// contract's own token to `outputs`.
///
/// The minted flavor is [`issued_flavor`] of that predicate and `tag`, so
/// coins minted from one address under one tag are one fungible flavor;
/// the same tag minted from another address is another. `outputs` must
/// balance per flavor: the issuer's token less `fee` in its flavor, and
/// `qty` of the new one.
///
/// `issuepriv` runs only inside a contract's own frame, so the issuer is
/// spent with `signcall` rather than `signtx`. A `signcall` signature
/// covers the program and nothing else: not the transaction, not the
/// contract. Anything the program leaves to its caller, anyone holding a
/// copy of the published program and signature could decide instead, on
/// any contract under the same key. The program therefore decides
/// everything itself:
///
/// ```text
/// drop                                    the call's unit argument
/// anchor <anchor> eq verify drop drop     this contract and no other
/// <qty commitment> commit <tag> issuepriv the minted token
/// fee > 0: <fee> fee
/// mix, outputs and notes                  as `build_transfer`'s
/// ```
///
/// The anchor a contract's frame starts with is split from the contract's
/// id, which is spent once, so the signature is good for that one spend;
/// and a copy of the transaction can only publish exactly these outputs.
/// The frame ends with an empty stack, which returns unit, and the root
/// discards it: the root holds nothing but the call.
///
/// The transaction carries no `signtx`, and so no envelope signature. It
/// comes back with the contracts it creates, each with its note, in the
/// order they are published.
#[allow(clippy::too_many_arguments)]
pub fn build_issue<R: RngCore + CryptoRng>(
    issuer: &InputSpec,
    tag: &[u8],
    qty: u64,
    outputs: &[OutputSpec],
    fee: u64,
    header: TxHeader,
    limits: Limits,
    rng: &mut R,
) -> Result<(ExternalTx, Vec<Created>), BuilderError> {
    if outputs.len() > MAX_OUTPUTS {
        return Err(BuilderError::TooManyOutputs(outputs.len()));
    }
    if let Some(output) = outputs.iter().find(|output| output.memo.len() > MEMO_MAX) {
        return Err(BuilderError::MemoTooLong(output.memo.len()));
    }

    let mut rng = hedged_rng(&header, core::slice::from_ref(issuer), fee, outputs, rng);
    let (program, signature) = issue_program(issuer, tag, qty, outputs, fee, &mut rng)?;
    signcall_tx(&issuer.contract, program, signature, header, limits)
}

/// The signed program of [`build_issue`], and its signature.
pub(crate) fn issue_program<R: RngCore + CryptoRng>(
    issuer: &InputSpec,
    tag: &[u8],
    qty: u64,
    outputs: &[OutputSpec],
    fee: u64,
    rng: &mut R,
) -> Result<(ScriptBuilder, [u8; 64]), BuilderError> {
    let minted = Commitment::blinded_with_factor(Scalar::from(qty), nonzero_scalar(rng));
    let sealed = seal(outputs, rng);

    // `input` sets the root's anchor to the contract's id, and nothing
    // between it and `signcall` consumes one.
    let (anchor, _) = Anchor(issuer.contract.id()).split();
    let mut program = ScriptBuilder::new()
        .drop_()
        .anchor()
        .push_literal(CellValue::from_bytes(anchor.0.to_vec()).map_err(VMError::from)?)
        .eq()
        .verify()
        // `eq` leaves both operands.
        .drop_()
        .drop_()
        .push_point_typed(Point::commitment(minted))
        .commit()
        .push_literal(CellValue::from_bytes(tag.to_vec()).map_err(VMError::from)?)
        .issuepriv();
    // The issuer's token, the minted one, and the fee debt.
    let mut mix_inputs = 2;
    if fee > 0 {
        program = program.push_int(fee).fee();
        mix_inputs += 1;
    }
    let program = mix_and_output(program, mix_inputs, sealed)?;

    let root = program.to_cell().map_err(VMError::from)?;
    let signature =
        musig::Signature::sign(&mut flamevm::signcall_transcript(&root), issuer.signing_key);
    Ok((program, signature.to_bytes()))
}

/// A transaction that spends `contract` through `program`, and does
/// nothing else.
pub(crate) fn signcall_tx(
    contract: &Contract,
    program: ScriptBuilder,
    signature: [u8; 64],
    header: TxHeader,
    limits: Limits,
) -> Result<(ExternalTx, Vec<Created>), BuilderError> {
    let unsigned = ScriptBuilder::new()
        .push_cell(CellValue::contract(contract.clone()).map_err(VMError::from)?)
        .input()
        .push_script(program)
        .map_err(VMError::from)?
        .push_literal(CellValue::from_bytes(signature.to_vec()).map_err(VMError::from)?)
        // Charged up front and refunded unspent; the root's own steps are
        // a handful of pushes.
        .push_int(limits.gas / 2)
        .push_int(0u64)
        .signcall()
        .verify()
        .drop_()
        .build_tx(header, limits)?;
    let created = note::outputs_with_notes(unsigned.log())
        .into_iter()
        .map(|(contract, note)| {
            let note = note.expect("every output is followed by its note");
            (contract.clone(), note.to_vec())
        })
        .collect();
    Ok((unsigned.without_signature()?, created))
}

/// A contract a transaction creates, and the note that follows it.
pub type Created = (Contract, Vec<u8>);

/// The flavor [`build_issue`] mints under `issuer` and `tag`.
pub fn issued_flavor(issuer: &Predicate, tag: &[u8]) -> Scalar {
    flavor_from_predicate(issuer, tag)
}

/// Seals every output, then puts them in the order they are published in.
fn seal<R: RngCore + CryptoRng>(outputs: &[OutputSpec], rng: &mut R) -> Vec<SealedOutput> {
    let mut sealed: Vec<SealedOutput> = outputs
        .iter()
        .map(|output| SealedOutput::new(output, rng))
        .collect();
    sealed.sort_by_key(|output| output.qty_point.to_bytes());
    sealed
}

/// `mix` over the `mix_inputs` tokens on top of the stack, then every
/// output and its note.
fn mix_and_output(
    mut program: ScriptBuilder,
    mix_inputs: usize,
    sealed: Vec<SealedOutput>,
) -> Result<ScriptBuilder, BuilderError> {
    let count = sealed.len();

    // The commitments go to the script by move, never by copy: each one
    // holds its blinding factor.
    let (commitments, published): (Vec<_>, Vec<_>) = sealed
        .into_iter()
        .map(|output| {
            (
                (output.qty, output.flv),
                (output.predicate, output.opening, output.note),
            )
        })
        .unzip();

    for (qty, flv) in commitments {
        program = program
            .push_literal(CellValue::commitment(qty))
            .push_literal(CellValue::commitment(flv));
    }
    program = program
        .push_int(mix_inputs as u64)
        .push_int(count as u64)
        .mix();

    for (index, (predicate, opening, note)) in published.into_iter().enumerate() {
        let depth = count - 1 - index;
        if depth > 0 {
            program = program.roll_k(depth as u8);
        }
        if let Some(opening) = opening {
            program = program
                .push_int(opening.flv)
                .push_int(opening.flv_blinding)
                .push_int(opening.qty)
                .push_int(opening.qty_blinding)
                .decrypt();
        }
        program = program
            .push_point(predicate.to_bytes())
            .output()
            .push_cell(Cell::new(note, vec![]).map_err(VMError::from)?)
            .log();
    }
    Ok(program)
}

/// One output, sealed: its commitments, the point it is sorted by, and its
/// note. The note's secrets are gone by the time this exists: they were
/// wiped as soon as the commitments were built from them. A clear output
/// keeps its opening instead, which the script publishes.
struct SealedOutput {
    predicate: CompressedRistretto,
    qty: Commitment,
    flv: Commitment,
    qty_point: CompressedRistretto,
    opening: Option<ClearOpening>,
    note: Vec<u8>,
}

/// What `decrypt` takes to open a clear output's token: public by design.
struct ClearOpening {
    qty: u64,
    qty_blinding: DalekScalar,
    flv: Scalar,
    flv_blinding: DalekScalar,
}

impl SealedOutput {
    fn new<R: RngCore + CryptoRng>(output: &OutputSpec, rng: &mut R) -> SealedOutput {
        if output.clear {
            let opening = ClearOpening {
                qty: output.qty,
                qty_blinding: nonzero_scalar(rng),
                flv: output.flv,
                flv_blinding: nonzero_scalar(rng),
            };
            let qty =
                Commitment::blinded_with_factor(Scalar::from(output.qty), opening.qty_blinding);
            let flv = Commitment::blinded_with_factor(output.flv, opening.flv_blinding);
            return SealedOutput {
                predicate: output.address.spending_key().compress(),
                qty_point: qty.to_point(),
                qty,
                flv,
                opening: Some(opening),
                note: output.memo.clone(),
            };
        }
        let r = Zeroizing::new(nonzero_scalar(rng));
        let sealed = note::seal(&output.address, &r, output.qty, output.flv, &output.memo);
        let qty =
            Commitment::blinded_with_factor(Scalar::from(output.qty), sealed.secrets.qty_blinding);
        let flv = Commitment::blinded_with_factor(output.flv, sealed.secrets.flv_blinding);
        SealedOutput {
            predicate: output.address.spending_key().compress(),
            qty_point: qty.to_point(),
            qty,
            flv,
            opening: None,
            note: sealed.note,
        }
    }
}

/// The generator every `r` of one transfer is drawn from. The label is this
/// crate's own and no part of the wire format.
fn hedged_rng<R: RngCore + CryptoRng>(
    header: &TxHeader,
    inputs: &[InputSpec],
    fee: u64,
    outputs: &[OutputSpec],
    rng: &mut R,
) -> merlin::TranscriptRng {
    let mut transcript = Transcript::new(b"flamepayments.r");
    transcript.append_u64(b"version", u64::from(header.version));
    transcript.append_u64(b"locktime", u64::from(header.locktime));
    for input in inputs {
        transcript.append_message(b"input", &input.contract.id());
    }
    transcript.append_u64(b"fee", fee);
    for output in outputs {
        transcript.append_message(b"S", output.address.spending_key().compress().as_bytes());
        transcript.append_message(b"V", output.address.viewing_key().compress().as_bytes());
        transcript.append_u64(b"qty", output.qty);
        transcript.append_message(b"flv", output.flv.as_bytes());
        transcript.append_message(b"memo", &output.memo);
        transcript.append_u64(b"clear", u64::from(output.clear));
    }
    inputs
        .iter()
        .fold(transcript.build_rng(), |builder, input| {
            builder.rekey_with_witness_bytes(b"signing_key", input.signing_key.as_bytes())
        })
        .finalize(rng)
}

/// A uniform nonzero scalar: drawn again while zero.
fn nonzero_scalar<R: RngCore + CryptoRng>(rng: &mut R) -> DalekScalar {
    loop {
        let r = DalekScalar::random(rng);
        if r != DalekScalar::ZERO {
            return r;
        }
    }
}

/// Signs the transfer. `keys` are the inputs' signing keys, in input order,
/// which is the order `signtx` recorded them in.
pub fn sign(unsigned: UnsignedTx, keys: &[DalekScalar]) -> Result<ExternalTx, MusigError> {
    let instructions = unsigned.signing_instructions();
    let mut transcript = merlin::Transcript::new(b"flamevm.signtx");
    transcript.append_message(b"txid", &instructions.txid.0);
    let items: Vec<_> = instructions
        .items
        .iter()
        .map(|(key, contract)| (musig::VerificationKey::from_compressed(*key), *contract))
        .collect();
    let signature = musig::Signature::sign_multi(keys, items, &mut transcript)?;
    Ok(unsigned.sign(signature))
}

/// Packages the signed transfer for a block. `proofs` are the inputs'
/// Utreexo membership proofs, in input order.
pub fn block_tx(tx: ExternalTx, limits: Limits, proofs: Vec<Proof>) -> BlockTx {
    BlockTx { tx, limits, proofs }
}

/// A transfer could not be assembled.
///
/// The payload errors carry no input index: [`InputSpec::clear`] and
/// [`InputSpec::confidential`] each validate one input before it has a
/// position, and the error comes straight back from that call, so the
/// caller already knows which input it is about.
#[derive(Debug, thiserror::Error)]
pub enum BuilderError {
    #[error("{0} outputs; roll_k addresses at most {MAX_OUTPUTS} from one mix")]
    TooManyOutputs(usize),

    #[error("a memo of {0} bytes; a note carries at most {MEMO_MAX}")]
    MemoTooLong(usize),

    #[error("the published payload is neither a Token nor a ClearToken")]
    PayloadNotToken,

    #[error("a confidential Token needs its opening")]
    OpeningMissing,

    #[error("a ClearToken needs no opening")]
    OpeningNotNeeded,

    #[error("the opening rebuilds a different contract id")]
    OpeningMismatch,

    #[error(transparent)]
    Vm(#[from] VMError),
}
