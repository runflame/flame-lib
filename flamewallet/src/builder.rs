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
//! The VM offers a second way to open a token, one that puts the quantity
//! and both blinding factors into the bytecode as literals, where every
//! verifier re-executes them and every spent amount becomes public one hop
//! after receipt. This module never emits that opcode; `flamewallet.md`
//! names it and explains the difference.

use curve25519_dalek::scalar::Scalar as DalekScalar;
use flamechain::utreexo::Proof;
use flamechain::BlockTx;
use flamevm::{
    Commitment, Contract, ExternalTx, Limits, Predicate, Scalar, ScriptBuilder, String as VmString,
    Token, TxHeader, UnsignedTx, VMError, Value,
};
use musig::{Multisignature, MusigError};

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

/// The secret behind a confidential token: what the sender kept so the
/// recipient can spend the output later.
#[derive(Clone, Copy, Debug)]
pub struct Opening {
    pub qty: u64,
    pub flv: Scalar,
    pub qty_blinding: DalekScalar,
    pub flv_blinding: DalekScalar,
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

/// One contract being created. The blinding factors are the sender's to
/// choose and the recipient's to learn: together with `qty` and `flv` they
/// are the [`Opening`] that lets the recipient spend this output.
#[derive(Clone, Debug)]
pub struct OutputSpec {
    pub predicate: Predicate,
    pub qty: u64,
    pub flv: Scalar,
    pub qty_blinding: DalekScalar,
    pub flv_blinding: DalekScalar,
}

impl OutputSpec {
    /// The opening this output's recipient needs in order to spend it.
    ///
    /// The published contract holds only the two commitment points, so this
    /// is the one record the output can ever be spent from: the sender keeps
    /// it and delivers it out of band. Deriving it from the spec keeps the
    /// four secrets written in a single place — a hand-built [`Opening`] that
    /// disagrees with the output it describes produces a contract nobody can
    /// open, and the mistake only surfaces once the transaction has
    /// confirmed.
    pub fn opening(&self) -> Opening {
        Opening {
            qty: self.qty,
            flv: self.flv,
            qty_blinding: self.qty_blinding,
            flv_blinding: self.flv_blinding,
        }
    }
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
/// per output i: roll_k(n-1-i) if > 0;  push_point(predicate)  output
/// ```
///
/// `signtx` pushes the contract's single payload Value and no count, so with
/// a bare token payload there is nothing to drop. A zero fee emits no `fee`
/// opcode. The roll before each `output` is what keeps output *i* paired
/// with `outputs[i]`: `mix` leaves the tokens in spec order and `output`
/// pops from the top, which would otherwise reverse them.
pub fn build_transfer(
    inputs: &[InputSpec],
    outputs: &[OutputSpec],
    fee: u64,
    header: TxHeader,
    limits: Limits,
) -> Result<UnsignedTx, BuilderError> {
    if outputs.len() > MAX_OUTPUTS {
        return Err(BuilderError::TooManyOutputs(outputs.len()));
    }

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

    // TODO: outputs are emitted in caller order, so change tends to land last
    // and is clusterable by the standard change-position heuristic — this
    // builder hides the amounts but not the sender/change distinction. Fix by
    // ordering outputs on their qty commitment point (uniform under a fresh
    // blinding, so no RNG is needed) and having callers match openings to
    // outputs through the log instead of by position. Belongs with Phase 3's
    // change-selection policy.
    for output in outputs {
        program = program
            .push_str(VmString::commitment(Commitment::blinded_with_factor(
                Scalar::from(output.qty),
                output.qty_blinding,
            )))
            .push_str(VmString::commitment(Commitment::blinded_with_factor(
                output.flv,
                output.flv_blinding,
            )));
    }
    program = program
        .push_int(mix_inputs as u64)
        .push_int(outputs.len() as u64)
        .mix();

    for (index, output) in outputs.iter().enumerate() {
        let depth = outputs.len() - 1 - index;
        if depth > 0 {
            program = program.roll_k(depth as u8);
        }
        program = program
            .push_point(output.predicate.to_point().to_bytes())
            .output();
    }

    Ok(program.build_tx(header, limits)?)
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
