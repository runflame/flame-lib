//! Errors related to proving and verifying proofs.
use bulletproofs::r1cs::R1CSError;
use thiserror::Error;

/// Represents an error in proof creation, verification, or parsing.
#[derive(Error, Debug)]
pub enum VMError {
    #[error("Item is not a Cell")]
    TypeNotCell,
    #[error("Item is not a Slice")]
    TypeNotSlice,
    #[error("Item is not a Builder")]
    TypeNotBuilder,
    #[error("Item is not a Cell, Slice, or Builder")]
    TypeNotByteSource,
    #[error("Item is not a Cell or Slice")]
    TypeNotCellOrSlice,
    /// A Cell was missing, malformed, or exceeded the execution budget.
    #[error(transparent)]
    Cell(#[from] cells::CellError),

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

    /// This error occurs when an instruction requires an Scalar.
    #[error("Item is not an Scalar")]
    TypeNotScalar,

    /// This error occurs when 32 bytes do not encode a canonical Scalar.
    #[error("Invalid Scalar encoding")]
    InvalidScalarEncoding,

    /// This error occurs when an index is out of range for the addressed stack or buffer.
    #[error("Index out of range")]
    IndexOutOfRange,

    /// The frame's gas budget is exhausted (per-instruction metering,
    /// including instructions scanned while skipping to a label), or a
    /// call grants more gas than the caller has remaining.
    #[error("out of gas")]
    OutOfGas,

    /// Nested call/open/signcall depth exceeded `MAX_CALL_DEPTH` — a
    /// structural bound on heap-recursion (re-entrancy is permitted, so
    /// gas alone would otherwise be the only limit on a call cycle).
    #[error("call depth exceeded")]
    CallDepthExceeded,

    /// This error occurs when an instruction requires a String.
    #[error("Item is not a String")]
    TypeNotString,

    /// A VM String must fit in one Cell payload, without snake continuations.
    #[error("String exceeds the single-Cell payload limit")]
    StringTooLong,

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

    /// This error occurs when an instruction requires a Contract.
    #[error("Item is not a Contract")]
    TypeNotContract,

    /// This error occurs when a TaprootProof does not verify against the contract's predicate.
    #[error("TaprootProof does not match the contract's predicate")]
    TaprootProofMismatch,

    /// This error occurs when `contract` or `output` is invoked without a seeded anchor.
    #[error("No anchor available — input or seed required first")]
    AnchorMissing,

    /// This error occurs when a non-portable item lands in a contract payload.
    #[error("Non-portable item in contract payload")]
    NonPortableInOutput,

    /// `signcall` received the wrong byte length or a malformed signature
    /// encoding.
    #[error("Bad signature bytes")]
    BadSignatureBytes,

    /// Immediate verification of an internal `signcall` signature failed.
    #[error("Signature verification failed")]
    SignatureVerificationFailed,

    /// An immediately checked Pedersen opening does not match its Token.
    #[error("Token commitment opening mismatch")]
    CommitmentOpeningMismatch,

    /// This error occurs when an instruction requires a Point.
    #[error("Item is not a Point")]
    TypeNotPoint,

    /// This error occurs when a TaprootProof's wire pieces are malformed.
    #[error("Malformed TaprootProof")]
    MalformedTaprootProof,

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

    /// This error occurs when `input` bytes do not decode as a canonical wire-encoded Contract.
    #[error("Malformed contract encoding")]
    MalformedContractEncoding,

    /// This error occurs when an actor-state Value can't be encoded
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

    /// A negative clear token is a liability and cannot be destroyed to
    /// satisfy balance. It must be matched by an equal positive token.
    #[error("negative token quantity cannot be retired")]
    NegativeTokenRetirement,

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

    /// This error occurs when a value cannot enter an asynchronous Message.
    #[error("Non-portable value in send payload")]
    NonPortableInSend,

    /// Values passed downward into synchronous calls must be portable;
    /// liabilities and VM-local objects may only return upward.
    #[error("Non-portable value in call arguments")]
    NonPortableInCall,

    /// Actor code, state, or lease metadata would exceed current capacity.
    #[error("Actor storage usage exceeds leased capacity")]
    StorageCapacityExceeded,

    /// A checked integer operation in storage accounting overflowed.
    #[error("Storage arithmetic overflow")]
    StorageArithmeticOverflow,

    /// Capacity introspection is defined for the current or a future block.
    #[error("Storage capacity height is in the past")]
    StorageHeightInPast,

    /// Lease expiry made the actor under-capacity at block start; it cannot
    /// execute before its deterministic end-of-block destruction.
    #[error("Actor is pending storage-expiry destruction")]
    ActorPendingDestruction,

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

    /// This error occurs when an opcode requires a predicate (ContractOpen) context but the frame has none.
    #[error("Opcode requires predicate context")]
    OpcodeRequiresPredicateContext,

    /// This error occurs when `op_save` is called with a non-portable
    /// actor-state Value. For a Dict, the O(1) cached portability flag
    /// includes every value successfully inserted into it.
    #[error("Non-portable value in actor state")]
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

    /// A scalar or R1CS assignment is outside the requested bit range.
    #[error("Value outside the requested bit range")]
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
