//! Building, signing, and packaging a transfer.
//!
//! The one rule this module exists to enforce: a confidential input travels
//! as a **private witness**. The prover pushes `String::contract(c)` over a
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
//! The VM offers a second way to open a token, one that puts the quantity
//! and both blinding factors into the bytecode as literals, where every
//! verifier re-executes them and every spent amount becomes public one hop
//! after receipt. This module never emits that opcode; `flamepayments.md`
//! names it and explains the difference.

use cells::Cell;
use core::fmt;

use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar as DalekScalar;
use flamechain::utreexo::Proof;
use flamechain::BlockTx;
use flamekd::ReceivingAddress;
use flamevm::{
    Commitment, Contract, ExternalTx, Limits, Scalar, ScriptBuilder, String as VmString, Token,
    TxHeader, UnsignedTx, VMError, Value,
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
}

/// Builds the unsigned transfer.
///
/// The script, in order:
///
/// ```text
/// per input:    push_str(String::contract(contract))  input  signtx
/// fee > 0:      push_int(fee)  fee                    one more mix input
/// per output:   push_str(commitment(qty))  push_str(commitment(flv))
///               push_int(m)  push_int(n)  mix
/// per output i: roll_k(n-1-i) if > 0;  push_point(S_i)  output
///               pushcell(note_i)  log
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
/// log` leaves the stack as it found it.
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

    // Seal every output, then put them in the order they are published in.
    let mut rng = hedged_rng(&header, inputs, fee, outputs, rng);
    let mut sealed: Vec<SealedOutput> = outputs
        .iter()
        .map(|output| SealedOutput::new(output, &mut rng))
        .collect();
    sealed.sort_by_key(|output| output.qty_point.to_bytes());
    let count = sealed.len();

    // The commitments go to the script by move, never by copy: each one
    // holds its blinding factor.
    let (commitments, published): (Vec<_>, Vec<_>) = sealed
        .into_iter()
        .map(|output| ((output.qty, output.flv), (output.predicate, output.note)))
        .unzip();

    let mut program = ScriptBuilder::new();
    for input in inputs {
        program = program
            .push_str(VmString::contract(input.contract.clone()))
            .input()
            .signtx();
    }

    // The fee debt is one more token `mix` has to balance.
    let mut mix_inputs = inputs.len();
    if fee > 0 {
        program = program.push_int(fee).fee();
        mix_inputs += 1;
    }

    for (qty, flv) in commitments {
        program = program
            .push_str(VmString::commitment(qty))
            .push_str(VmString::commitment(flv));
    }
    program = program
        .push_int(mix_inputs as u64)
        .push_int(count as u64)
        .mix();

    for (index, (predicate, note)) in published.into_iter().enumerate() {
        let depth = count - 1 - index;
        if depth > 0 {
            program = program.roll_k(depth as u8);
        }
        program = program
            .push_point(predicate.to_bytes())
            .output()
            .push_cell(Cell::new(note, vec![]).map_err(VMError::from)?)
            .log();
    }

    Ok(program.build_tx(header, limits)?)
}

/// One output, sealed: its commitments, the point it is sorted by, and its
/// note. The note's secrets are gone by the time this exists: they were
/// wiped as soon as the commitments were built from them.
struct SealedOutput {
    predicate: CompressedRistretto,
    qty: Commitment,
    flv: Commitment,
    qty_point: CompressedRistretto,
    note: Vec<u8>,
}

impl SealedOutput {
    fn new<R: RngCore + CryptoRng>(output: &OutputSpec, rng: &mut R) -> SealedOutput {
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
