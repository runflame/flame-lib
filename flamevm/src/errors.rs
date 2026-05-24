//! Errors related to proving and verifying proofs.
use bulletproofs::r1cs::R1CSError;
use readerwriter::ReadError;

use thiserror::Error;

/// Represents an error in proof creation, verification, or parsing.
#[derive(Error, Debug)]
pub enum VMError {
    /// Failure decoding a value from its compact wire format.
    #[error("encoding error: {0}")]
    Encoding(#[from] ReadError),

    // /// This error occurs when an individual point operation failed.
    // #[error("Point operation failed.")]
    // PointOperationFailed,

    // /// This error occurs when a point is not a valid compressed Ristretto point
    // #[error("Point decoding failed.")]
    // InvalidPoint,

    // /// This error occurs when data is malformed
    // #[error("Format in invalid")]
    // InvalidFormat,

    // /// This error occurs when there are trailing bytes left unread by the parser.
    // #[error("Invalid trailing bytes.")]
    // TrailingBytes,

    // /// This error occurs when data is malformed
    // #[error("Transaction version does not permit extension instructions.")]
    // ExtensionsNotAllowed,

    // /// This error occurs when an instruction requires a copyable type, but a linear type is encountered.
    // #[error("Item is not a copyable type.")]
    // TypeNotCopyable,

    // /// This error occurs when an instruction requires a droppable type, but a non-droppable type is encountered.
    // #[error("Item is not a droppable type.")]
    // TypeNotDroppable,

    // /// This error occurs when an instruction requires a portable type, but a non-portable type is encountered.
    // #[error("Item is not a portable type.")]
    // TypeNotPortable,

    // /// This error occurs when an instruction requires a string.
    // #[error("Item is not a string.")]
    // TypeNotString,

    // /// This error occurs when an instruction requires a contract type.
    // #[error("Item is not a contract.")]
    // TypeNotContract,

    // /// This error occurs when an instruction requires a variable type.
    // #[error("Item is not a variable.")]
    // TypeNotVariable,

    // /// This error occurs when an instruction requires an expression type.
    // #[error("Item is not an expression.")]
    // TypeNotExpression,

    // /// This error occurs when an instruction requires a predicate string.
    // #[error("Item is not a predicate.")]
    // TypeNotPredicate,

    // /// This error occurs when an instruction requires a commitment string.
    // #[error("Item is not a commitment.")]
    // TypeNotCommitment,

    // /// This error occurs when an instruction requires an output string.
    // #[error("Item is not an output.")]
    // TypeNotOutput,

    // /// This error occurs whn an instruction requires a call proof string.
    // #[error("Item is not a call proof.")]
    // TypeNotCallProof,

    // /// This error occurs when an instruction requires a constraint type.
    // #[error("Item is not a constraint.")]
    // TypeNotConstraint,

    // /// This error occurs when an instruction requires a scalar string.
    // #[error("Item is not a scalar.")]
    // TypeNotScalar,

    // /// This error occurs when an instruction requires a u64 integer.
    // #[error("Item is not a LE64 integer.")]
    // TypeNotU64,

    // /// This error occurs when an instruction requires a u32 integer.
    // #[error("Item is not a LE32 integer.")]
    // TypeNotU32,

    // /// This error occurs when an instruction expects a predicate tree type.
    // #[error("Item is not a predicate tree.")]
    // TypeNotPredicateTree,

    // /// This error occurs when an instruction expects a key type.
    // #[error("Item is not a key.")]
    // TypeNotKey,

    // /// This error occurs when a prover is supposed to provide signed integer.
    // #[error("Item is not a signed integer.")]
    // TypeNotSignedInteger,

    // /// This error occurs when a prover is supposed to provide a script.
    // #[error("Item is not a script")]
    // TypeNotScript,

    // /// This error occurs when a prover has an inconsistent combination of witness data
    // #[error("Witness data is inconsistent.")]
    // InconsistentWitness,

    // /// This error occurs when an instruction requires a value type.
    // #[error("Item is not a value.")]
    // TypeNotValue,

    // /// This error occurs when an instruction requires a value or a wide value.
    // #[error("Item is not a wide value.")]
    // TypeNotWideValue,

    // /// This error occurs when VM does not have enough items on the stack
    // #[error("Stack does not have enough items")]
    // StackUnderflow,

    /// This error occurs when VM is left with some items on the stack
    /// at the end of a call frame (no declared return values to consume them).
    #[error("Stack is not cleared by the script")]
    StackNotClean,

    /// Encountered an opcode byte that the dispatch loop does not recognize
    /// (and the current context does not allow extension opcodes).
    #[error("Unknown opcode: {0:#x}")]
    UnknownOpcode(u8),

    /// An opcode tried to pop more items than the current call stack holds.
    #[error("Stack underflow")]
    StackUnderflow,

    /// An opcode tried to copy (`dup`) a value of a type the spec marks
    /// non-copyable (linear types: tokens, cells, variables, expressions,
    /// constraints, transcripts, multiscalar muls, objects).
    #[error("Item is not a copyable type")]
    TypeNotCopyable,

    /// `drop` was used on a value the spec marks non-droppable (linear
    /// types in general, and non-empty containers / non-zero-qty tokens).
    #[error("Item is not a droppable type")]
    TypeNotDroppable,

    /// An opcode tried to read more inline bytes than the current script
    /// has remaining (e.g. `pushint8` at the very last byte of a script).
    #[error("Unexpected end of script")]
    UnexpectedEndOfScript,

    /// An opcode required a top-of-stack `Int253` but found a different type.
    #[error("Item is not an Int253")]
    TypeNotInt253,

    /// An opcode read 32 bytes that do not encode a canonical Int253
    /// (non-canonical scalar, or negative zero).
    #[error("Invalid Int253 encoding")]
    InvalidInt253Encoding,

    /// An `Int253` index argument was outside `[0, usize::MAX]`, or beyond
    /// the depth of the addressed stack.
    #[error("Index out of range")]
    IndexOutOfRange,

    /// An opcode required a top-of-stack `String` but found a different type.
    #[error("Item is not a String")]
    TypeNotString,

    /// The `verify` opcode saw a zero value on top of the stack.
    #[error("verify failed: zero value")]
    VerifyFailed,

    /// `break:k` attempted to skip more nesting levels than the current
    /// call has available (i.e. tried to break past the call boundary).
    #[error("break exceeds call depth")]
    BreakOutOfCall,

    /// `return k` was called with a `k` that exceeds the callee's stack
    /// depth, or with non-`Int253` k.
    #[error("Bad return arity")]
    BadReturnArity,

    /// `return` invoked at the outermost call frame (no parent to receive
    /// the return values, regardless of arity).
    #[error("`return` at outermost frame — use `break` for early exit")]
    ReturnAtRoot,

    /// `divmod` saw a zero divisor.
    #[error("Division by zero")]
    DivByZero,

    /// `mod252` saw a String longer than 64 bytes.
    #[error("String too long for mod252 (max 64 bytes)")]
    StringTooLongForModReduction,

    /// `size` was applied to a type that has no length defined.
    #[error("Type has no length")]
    TypeHasNoLength,

    /// `eq` cannot compare these operand types (e.g., linear types).
    /// Cross-type comparisons return `0` (false) and never reach
    /// this; same-type non-comparable variants do.
    #[error("Operand types are not comparable")]
    TypeNotComparable,

    /// `bitor` / `bitand` / `bitxor` saw operands of different lengths.
    #[error("Bitwise operands have different sizes")]
    BitwiseSizeMismatch,

    /// `writebits`/`shiftleft`/`shiftright` saw a bit count outside the
    /// permitted range (`writebits`: multiple of 8 and ≤ 256; shifts:
    /// ≤ 256).
    #[error("Bit count out of range")]
    BitCountOutOfRange,

    /// An opcode required a top-of-stack `Dict` but found a different type.
    #[error("Item is not a Dict")]
    TypeNotDict,

    /// `put` or `dict` tried to occupy a key that already exists.
    #[error("Dict key already occupied")]
    DictKeyOccupied,

    /// `get` was called with a key not in the dictionary.
    #[error("Dict key not found")]
    DictKeyNotFound,

    /// An opcode required a top-of-stack `Merlin` but found a different type.
    #[error("Item is not a Merlin transcript")]
    TypeNotMerlin,

    /// An opcode required a top-of-stack `Cell` but found a different type.
    #[error("Item is not a Cell")]
    TypeNotCell,

    /// `open` / `signrun` was given a `CallProof` that doesn't verify
    /// against the cell's predicate (path mismatch, point decompression
    /// failure, etc.).
    #[error("CallProof does not match the cell's predicate")]
    CallProofMismatch,

    /// `cell` / `output` was invoked without a seeded anchor (no prior
    /// `input` or test-only seed).
    #[error("No anchor available — input or seed required first")]
    AnchorMissing,

    /// `output` saw a non-portable item in the payload (cells, tokens
    /// with negative qty, linear types, etc.).
    #[error("Non-portable item in cell payload")]
    NonPortableInOutput,

    /// `signrun` saw signature bytes that aren't 64 bytes long.
    #[error("Bad signature byte length")]
    BadSignatureBytes,

    /// `Point` decoding required a top-of-stack `Point` but found a
    /// different type.
    #[error("Item is not a Point")]
    TypeNotPoint,

    /// `CallProof` decoded from on-stack strings was malformed (wrong
    /// number of components, bad inner Dict structure, neighbor entry
    /// not 32 bytes, etc.).
    #[error("Malformed CallProof")]
    MalformedCallProof,

    /// `PredicateTree::new` was called with zero programs. A predicate
    /// tree must commit to at least one program.
    #[error("PredicateTree must have at least one program")]
    EmptyPredicateTree,

    /// A compressed Ristretto point failed to decompress when building a
    /// predicate / call-proof component.
    #[error("Invalid Ristretto point")]
    InvalidPoint,

    /// `PredicateTree::callproof_for` was called with a `program_index`
    /// outside `0..programs.len()`.
    #[error("Program index out of range")]
    ProgramIndexOutOfRange,

    /// `input` was invoked outside an external transaction context.
    /// Only external txs can consume Utreexo entries.
    #[error("Opcode is external-context only")]
    ExternalOnly,

    /// `input` was given a String whose bytes do not decode as a
    /// canonical wire-encoded Cell — wrong outer shape, wrong anchor
    /// length, non-portable payload value, trailing bytes after the
    /// cell, etc.
    #[error("Malformed cell encoding")]
    MalformedCellEncoding,

    /// `ActorState::from_wrapper_dict` or related helpers were given
    /// a Dict that does not have the canonical 2-entry shape
    /// (`0x00 → public, 0x01 → private`) with both values being Dicts.
    /// Also returned by `vbyte_size` when a `private` payload value
    /// can't be canonically encoded.
    #[error("Malformed ActorState")]
    MalformedActorState,

    /// An opcode required a top-of-stack `ClearToken` but found a
    /// different type (encrypted Token / WideToken / non-token).
    /// Distinct from `TypeNotToken` so scripts can distinguish "wrong
    /// token variant" from "not a token at all".
    #[error("Item is not a ClearToken")]
    TypeNotClearToken,

    /// An opcode required a top-of-stack `Token` but found a different
    /// type (ClearToken / WideToken / non-token).
    #[error("Item is not a Token")]
    TypeNotToken,

    /// `split` was asked to take more quantity from a `ClearToken` than
    /// it holds, or to take a negative quantity, or to split from a
    /// negative-qty source. All three are hard-fails.
    #[error("split quantity out of range")]
    TokenSplitOutOfRange,

    /// An opcode that requires the current call frame to be associated
    /// with an actor identity (e.g. cleartext `issue`) was invoked
    /// from a frame whose `actor()` is `None` — `ExternalRoot` or a
    /// bare `CellOpen` not nested in an actor call. Scripts must
    /// either be running inside an actor's method or pass the actor
    /// identity explicitly (use `issueflv`).
    #[error("Opcode requires actor context")]
    OpcodeRequiresActorContext,

    /// `issue` / `retire` / `borrow` reached with an operand shape
    /// that requires constraint-system machinery (e.g. a `Point` qty
    /// in `issue`'s encrypted branch). Encrypted `issue` is not yet
    /// wired; encrypted `borrow` is implemented via the `Variable`
    /// operand pair, not via raw `Point`.
    #[error("Token opcode branch requires a live constraint system")]
    TokenRequiresCS,

    /// `mix` was invoked with `m == 0` or `n == 0`. The underlying
    /// `spacesuit::cloak` gadget invokes `k_mix` which underflows
    /// when its operand list has length 0 (`0..k-1` with `k = 0`),
    /// so we reject the degenerate shapes at the opcode boundary.
    #[error("mix needs at least one input and one output")]
    MixDegenerate,

    // /// This error occurs when VM's anchor remains unset.
    // #[error("VM anchor is not set via `input`")]
    // AnchorMissing,

    /// Returned from `Verifier::verify` when the deferred-signature
    /// batch check fails — at least one of the accumulated `Explicit`
    /// or `TxBound` signatures didn't verify. Distinct from
    /// `InvalidR1CSProof`: the R1CS proof succeeded, only the
    /// signatures failed.
    #[error("Deferred batch signature verification failed")]
    BatchSignatureVerificationFailed,

    /// `Verifier::verify` saw at least one `DeferredSig::TxBound`
    /// item but the caller passed `None` for `txbound_signature`. A
    /// transaction with `signtx` opcodes is unverifiable without the
    /// aggregate multi-signature.
    #[error("TxBound signature required but not provided")]
    MissingTxBoundSignature,

    /// `Verifier::verify` was given a `txbound_signature` but the
    /// VM recorded no `DeferredSig::TxBound` items. The signature
    /// would never be checked — surface the mismatch to the caller
    /// rather than silently dropping it.
    #[error("TxBound signature provided but no TxBound items present")]
    SpuriousTxBoundSignature,

    // /// This error occurs when R1CS proof verification failed.
    // #[error("R1CS proof is invalid")]
    // InvalidR1CSProof,

    // /// This error occurs when R1CS gadget reports and error due to inconsistent input
    // #[error("R1CS detected inconsistent input")]
    // R1CSInconsistency,

    /// This error occurs when an R1CSError is returned from the ConstraintSystem.
    #[error("R1CSError returned when trying to build R1CS instance")]
    R1CSError(R1CSError),

    /// The prover ran an opcode that needed witness data
    /// (e.g. `alloc`, `commit` on the prover side) but no witness was
    /// available in the witness side-channel. Indicates a Program built
    /// with too few witnesses for the bytecode it produces.
    #[error("Prover witness is missing")]
    WitnessMissing,

    /// `Prover::prove` succeeded internally but
    /// `bulletproofs::r1cs::Prover::prove` reported a constraint-system
    /// or proof-construction error.
    #[error("R1CS proof construction failed")]
    R1CSProofConstruction,

    /// `Verifier::verify` reached `r1cs::Verifier::verify` with a proof
    /// that didn't check out (tampered, wrong CS, etc.).
    #[error("R1CS proof verification failed")]
    InvalidR1CSProof,

    /// An opcode required a top-of-stack `Variable` but found a
    /// different type.
    #[error("Item is not a Variable")]
    TypeNotVariable,

    /// An opcode required a top-of-stack `Expression` but found a
    /// different type.
    #[error("Item is not an Expression")]
    TypeNotExpression,

    /// `verify` was reached with a Constraint operand, but the
    /// constraint cannot be enforced (cleartext false, etc.). Wraps
    /// `CleartextConstraintFalse` for the dispatch path; left as an
    /// alias for now since constraint dispatch shares verify's error
    /// surface.
    #[error("Item is not a Constraint")]
    TypeNotConstraint,

    // /// This error occurs when a prover expects some witness data, but it is missing.
    // #[error("Item misses witness data.")]
    // WitnessMissing,

    /// `range` was reached with a cleartext `Expression::Constant`
    /// that doesn't fit in `[0, 2^n)`. The cleartext branch
    /// short-circuits without touching the CS, so this is a definite
    /// hard fail rather than a deferred proof-construction error.
    #[error("Value out of bit-range for rangeproof")]
    InvalidBitrange,

    // /// This error occurs when a Merkle proof of inclusion is invalid.
    // #[error("Invalid Merkle proof.")]
    // InvalidMerkleProof,

    // /// This error occurs when the predicate tree cannot be constructed.
    // #[error("Invalid predicate tree.")]
    // InvalidPredicateTree,

    // /// This error occurs when a function is called with bad arguments.
    // #[error("Bad arguments")]
    // BadArguments,

    // /// This error occurs when an input is invalid.
    // #[error("Input is invalid")]
    // InvalidInput,

    /// This error occurs when a false cleartext constraint is verified.
    #[error("Cleartext constraint is false")]
    CleartextConstraintFalse,

    /// `fee` was given a `qty` outside `[0, CheckedFee::MAX_FEE]`, or
    /// the running per-tx accumulator would overflow that cap when
    /// `qty` is added. Cap chosen so that `size_bytes × fee` stays
    /// inside `u64` even at the largest reasonable tx size — see
    /// `fees::CheckedFee` for the policy choice.
    #[error("Fee is too high")]
    FeeTooHigh,

    /// `fee` was given a negative `Int253` for `qty`. Fees are
    /// non-negative by construction; the negative half of the
    /// balance shows up as the `-qty` debt half of the returned
    /// `WideToken`, not as the recorded fee amount.
    #[error("fee qty must be non-negative")]
    FeeQtyNegative,

    /// The prover-side witness queue passed to `Instruction::Input`
    /// doesn't have one `TokenWitness` per `Value::Token` entry in
    /// the decoded cell's payload.
    #[error("input witness count does not match cell payload Token count")]
    WitnessCountMismatch,

    /// A `TokenWitness`'s `Commitment::Open` point doesn't match
    /// the decoded cell's `Commitment::Closed` point. The prover
    /// passed a witness for the wrong commitment — fail loudly so
    /// the bug is caught at test time.
    #[error("input witness commitment point does not match cell payload")]
    WitnessPointMismatch,

    /// A `TokenWitness` was constructed with a `Commitment::Closed`
    /// instead of `Commitment::Open`. The
    /// witness path's whole job is to re-attach openings, so a
    /// `Closed` here is a caller bug — without this check the
    /// failure would cascade silently into `WitnessMissing` from
    /// `commit_variable` far from the real cause.
    #[error("input witness commitment must be Open (witness-bearing)")]
    WitnessNotOpen,
}
