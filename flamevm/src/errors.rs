//! Errors related to proving and verifying proofs.
use bulletproofs::r1cs::R1CSError;
use readerwriter::ReadError;
use thiserror::Error;

/// Represents an error in proof creation, verification, or parsing.
#[derive(Error, Debug)]
pub enum VMError {
    /// This error occurs when a value can not be decoded from its compact wire format.
    #[error("encoding error: {0}")]
    Encoding(#[from] ReadError),

    /// This error occurs when VM is left with some items on the stack at the end of a frame.
    #[error("Stack is not cleared by the script")]
    StackNotClean,

    /// This error occurs when the dispatcher encounters an unrecognized opcode byte.
    #[error("Unknown opcode: {0:#x}")]
    UnknownOpcode(u8),

    /// This error occurs when VM does not have enough items on the stack.
    #[error("Stack underflow")]
    StackUnderflow,

    /// This error occurs when an instruction requires a copyable type, but a linear type is encountered.
    #[error("Item is not a copyable type")]
    TypeNotCopyable,

    /// This error occurs when an instruction requires a droppable type, but a non-droppable type is encountered.
    #[error("Item is not a droppable type")]
    TypeNotDroppable,

    /// This error occurs when an opcode reads past the end of the current script.
    #[error("Unexpected end of script")]
    UnexpectedEndOfScript,

    /// This error occurs when an instruction requires an Int253.
    #[error("Item is not an Int253")]
    TypeNotInt253,

    /// This error occurs when 32 bytes do not encode a canonical Int253.
    #[error("Invalid Int253 encoding")]
    InvalidInt253Encoding,

    /// This error occurs when an index is out of range for the addressed stack or buffer.
    #[error("Index out of range")]
    IndexOutOfRange,

    /// A stack-controlled allocation (`writezeros`, `tread`) exceeds the
    /// frame's transient-memory cap.
    #[error("transient allocation exceeds memory limit")]
    MemLimitExceeded,

    /// The frame's gas budget is exhausted (per-instruction metering,
    /// including instructions scanned while skipping to a label), or a
    /// call grants more gas than the caller has remaining.
    #[error("out of gas")]
    OutOfGas,

    /// This error occurs when an instruction requires a String.
    #[error("Item is not a String")]
    TypeNotString,

    /// This error occurs when `verify` sees a zero value on top of the stack.
    #[error("verify failed: zero value")]
    VerifyFailed,

    /// A `label` appeared out of sequence: its number is past the next
    /// expected index, or a re-visited label's position doesn't match.
    #[error("label out of order")]
    LabelOutOfOrder,

    /// A `jump`/`jumpif` target label was never found before end-of-program.
    #[error("jump target label not found")]
    LabelNotFound,

    /// This error occurs when `return k` is invoked with a bad k.
    #[error("Bad return arity")]
    BadReturnArity,

    /// This error occurs when `return` is invoked at the outermost call frame.
    #[error("`return` at outermost frame — run off the end for a clean exit")]
    ReturnAtRoot,

    /// This error occurs when `divmod` sees a zero divisor.
    #[error("Division by zero")]
    DivByZero,

    /// This error occurs when `mod252` sees a String longer than 64 bytes.
    #[error("String too long for mod252 (max 64 bytes)")]
    StringTooLongForModReduction,

    /// This error occurs when `size` is applied to a type that has no length.
    #[error("Type has no length")]
    TypeHasNoLength,

    /// This error occurs when `eq` operands cannot be compared (same-variant linear types).
    #[error("Operand types are not comparable")]
    TypeNotComparable,

    /// This error occurs when bitwise operands have different sizes.
    #[error("Bitwise operands have different sizes")]
    BitwiseSizeMismatch,

    /// This error occurs when a bit count is outside the permitted range.
    #[error("Bit count out of range")]
    BitCountOutOfRange,

    /// This error occurs when an instruction requires a Dict.
    #[error("Item is not a Dict")]
    TypeNotDict,

    /// This error occurs when `put` or `dict` tries to occupy an existing key.
    #[error("Dict key already occupied")]
    DictKeyOccupied,

    /// This error occurs when `get` is called with a key not in the dictionary.
    #[error("Dict key not found")]
    DictKeyNotFound,

    /// This error occurs when an instruction requires a Merlin transcript.
    #[error("Item is not a Merlin transcript")]
    TypeNotMerlin,

    /// This error occurs when an instruction requires a Cell.
    #[error("Item is not a Cell")]
    TypeNotCell,

    /// This error occurs when a CallProof does not verify against the cell's predicate.
    #[error("CallProof does not match the cell's predicate")]
    CallProofMismatch,

    /// This error occurs when `cell` or `output` is invoked without a seeded anchor.
    #[error("No anchor available — input or seed required first")]
    AnchorMissing,

    /// This error occurs when a non-portable item lands in a cell payload.
    #[error("Non-portable item in cell payload")]
    NonPortableInOutput,

    /// This error occurs when `signcall` sees a signature that is not 64 bytes.
    #[error("Bad signature byte length")]
    BadSignatureBytes,

    /// This error occurs when an instruction requires a Point.
    #[error("Item is not a Point")]
    TypeNotPoint,

    /// This error occurs when a MultiscalarMul has a non-decompressable point.
    #[error("MultiscalarMul contains non-decompressable point")]
    MsmInvalidPoint,

    /// This error occurs when a CallProof's wire pieces are malformed.
    #[error("Malformed CallProof")]
    MalformedCallProof,

    /// This error occurs when `PredicateTree::new` is called with zero programs.
    #[error("PredicateTree must have at least one program")]
    EmptyPredicateTree,

    /// This error occurs when a compressed Ristretto point fails to decompress.
    #[error("Invalid Ristretto point")]
    InvalidPoint,

    /// This error occurs when a program index is outside `0..programs.len()`.
    #[error("Program index out of range")]
    ProgramIndexOutOfRange,

    /// This error occurs when an external-only opcode is invoked from internal context.
    #[error("Opcode is external-context only")]
    ExternalOnly,

    /// This error occurs when `input` bytes do not decode as a canonical wire-encoded Cell.
    #[error("Malformed cell encoding")]
    MalformedCellEncoding,

    /// This error occurs when an actor-state Dict can't be encoded
    /// to its canonical wire form (a value variant without an
    /// encoder slipped in). Distinct from `NonPortableInState`,
    /// which is the portability gate at op_save.
    #[error("Malformed actor state")]
    MalformedActorState,

    /// This error occurs when Address bytes do not decode to a known wire shape.
    #[error("Malformed Address")]
    MalformedAddress,

    /// This error occurs when the registry has no actor at the requested ID.
    #[error("Actor not found in registry")]
    ActorNotFound,

    /// This error occurs when an operation targets a frozen actor.
    #[error("Actor is frozen")]
    ActorFrozen,

    /// This error occurs when an actor has no method at the requested key.
    #[error("Method not found on actor")]
    MethodNotFound,

    /// This error occurs when `deploy` is called against an existing actor ID.
    #[error("Actor already exists at this id")]
    ActorAlreadyExists,

    /// This error occurs when a registry-touching opcode is invoked without a registry.
    #[error("ActorRegistry unavailable in this context")]
    RegistryUnavailable,

    /// Load/call against an actor whose state is currently checked out
    /// (a frame `load`ed it and hasn't `save`d it back). The state
    /// itself is the re-entrancy lock — see ADR 0017.
    #[error("Actor state is checked out (empty)")]
    ActorEmpty,

    /// `op_save` against an actor that isn't checked out — the script
    /// never `load`ed it, so saving would clobber live state.
    #[error("op_save without a matching op_load (actor not checked out)")]
    SaveWithoutLoad,

    /// This error occurs when a send payload value has no canonical encoder.
    #[error("Non-portable value in send payload")]
    NonPortableInSend,

    /// This error occurs when an instruction requires a ClearToken.
    #[error("Item is not a ClearToken")]
    TypeNotClearToken,

    /// This error occurs when an instruction requires a Token.
    #[error("Item is not a Token")]
    TypeNotToken,

    /// This error occurs when `split` is called with an out-of-range quantity.
    #[error("split quantity out of range")]
    TokenSplitOutOfRange,

    /// This error occurs when an opcode requires an actor context but the frame has none.
    #[error("Opcode requires actor context")]
    OpcodeRequiresActorContext,

    /// This error occurs when an opcode requires a predicate (CellOpen) context but the frame has none.
    #[error("Opcode requires predicate context")]
    OpcodeRequiresPredicateContext,

    /// This error occurs when `op_save` is called with an actor-state
    /// Dict that contains non-portable values (Cell, Merlin, Variable,
    /// negative ClearToken, WideToken, Expression, Constraint,
    /// MultiscalarMul). Long-term storage requires portable values
    /// only — see `flamevm/spec.md` §save.
    #[error("Non-portable value in actor state Dict")]
    NonPortableInState,

    /// This error occurs when a token opcode needs a live constraint system but has none.
    #[error("Token opcode branch requires a live constraint system")]
    TokenRequiresCS,

    /// This error occurs when `mix` is invoked with zero inputs or outputs.
    #[error("mix needs at least one input and one output")]
    MixDegenerate,

    /// This error occurs when the deferred signature batch check fails.
    #[error("Deferred batch signature verification failed")]
    BatchSignatureVerificationFailed,

    /// This error occurs when a TxBound signature is required but not provided.
    #[error("TxBound signature required but not provided")]
    MissingTxBoundSignature,

    /// This error occurs when a TxBound signature is provided but no TxBound items exist.
    #[error("TxBound signature provided but no TxBound items present")]
    SpuriousTxBoundSignature,

    /// This error occurs when an R1CSError is returned from the ConstraintSystem.
    #[error("R1CSError returned when trying to build R1CS instance")]
    R1CSError(R1CSError),

    /// This error occurs when a prover expects some witness data, but it is missing.
    #[error("Prover witness is missing")]
    WitnessMissing,

    /// This error occurs when R1CS proof construction fails.
    #[error("R1CS proof construction failed")]
    R1CSProofConstruction,

    /// This error occurs when R1CS proof verification fails.
    #[error("R1CS proof verification failed")]
    InvalidR1CSProof,

    /// This error occurs when an instruction requires a Variable.
    #[error("Item is not a Variable")]
    TypeNotVariable,

    /// This error occurs when an instruction requires an Expression.
    #[error("Item is not an Expression")]
    TypeNotExpression,

    /// This error occurs when an instruction requires a Constraint.
    #[error("Item is not a Constraint")]
    TypeNotConstraint,

    /// This error occurs when a value is outside the bit-range for a rangeproof.
    #[error("Value out of bit-range for rangeproof")]
    InvalidBitrange,

    /// This error occurs when a false cleartext constraint is verified.
    #[error("Cleartext constraint is false")]
    CleartextConstraintFalse,

    /// This error occurs when tx attempts to add a fee beyond the limit.
    #[error("Fee is too high")]
    FeeTooHigh,

    /// This error occurs when `fee` is given a negative quantity.
    #[error("fee qty must be non-negative")]
    FeeQtyNegative,
}
