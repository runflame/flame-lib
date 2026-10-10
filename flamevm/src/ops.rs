//! Definition of all instructions in FlameVM,
//! their codes and decoding/encoding utility functions.

use crate::crypto::Point;
use crate::errors::VMError;
use crate::scalar::Scalar;
use crate::string::String;
use cells::CellRef;
use core::convert::TryFrom;
use std::sync::Arc;

// ── Opcode bytes ─────────────────────────────────────────────────────

// Byte map: see `docs/flamevm.md` §"Instruction table" for the
// canonical view. The high nibble is the block; low nibble is either
// the slot or, for k-class ops (`push:k`, `dup:k`, `roll:k`), the
// inline operand. `label`/`jump`/`jumpif` carry a sub-varint operand.

// 0x0X — push:k (inline small int)
const OP_PUSH_SMALL_MAX: u8 = 0x0f;

// 0x1X — stack & literals
const OP_PUSHINT8_POS: u8 = 0x10;
const OP_PUSHINT8_NEG: u8 = 0x11;
const OP_PUSHINT16_POS: u8 = 0x12;
const OP_PUSHINT16_NEG: u8 = 0x13;
const OP_PUSHINT64_POS: u8 = 0x14;
const OP_PUSHINT64_NEG: u8 = 0x15;
const OP_PUSHINT128_POS: u8 = 0x16;
const OP_PUSHINT128_NEG: u8 = 0x17;
const OP_PUSHINT_FULL: u8 = 0x18;
// Legacy host-source marker only; not a VM opcode.
const SOURCE_BYTES: u8 = 0x19;
const OP_PUSHPOINT: u8 = 0x1a;
const OP_PUSHTOKEN: u8 = 0x1b;
const OP_DROP: u8 = 0x1c;
const OP_NOP: u8 = 0x1d;
const OP_DUP: u8 = 0x1e;
const OP_ROLL: u8 = 0x1f;

// 0x2X — dup:k
const OP_DUPK_BASE: u8 = 0x20;
const OP_DUPK_MAX: u8 = 0x2f;

// 0x3X — roll:k
const OP_ROLLK_BASE: u8 = 0x30;
const OP_ROLLK_MAX: u8 = 0x3f;

// 0x4X — Cell bytes/refs and remaining String operations
const OP_READUINT: u8 = 0x40;
const OP_READSCALAR: u8 = 0x41;
const OP_READBYTES: u8 = 0x42;
const OP_READPOINT: u8 = 0x43;
const OP_WRITEUINT: u8 = 0x44;
const OP_WRITESCALAR: u8 = 0x45;
const OP_APPENDBYTES: u8 = 0x46;
const OP_APPENDREFS: u8 = 0x4e;
const OP_BUILDER: u8 = 0x4f;
const OP_SLICE: u8 = 0xa8;
const OP_ENDCELL: u8 = 0xa9;
const OP_WRITEZEROS: u8 = 0x47;
const OP_BITNOT: u8 = 0x48;
const OP_BITOR: u8 = 0x49;
const OP_BITAND: u8 = 0x4a;
const OP_BITXOR: u8 = 0x4b;
const OP_SHIFTLEFT: u8 = 0x4c;
const OP_SHIFTRIGHT: u8 = 0x4d;

// 0x5X — Math & logic (incl. `size`)
const OP_ABS: u8 = 0x50;
const OP_EQ: u8 = 0x51;
const OP_NEG: u8 = 0x52;
const OP_ADD: u8 = 0x53;
const OP_MUL: u8 = 0x54;
const OP_DIVMOD: u8 = 0x55;
const OP_MOD252: u8 = 0x56;
const OP_NOT: u8 = 0x57;
const OP_AND: u8 = 0x58;
const OP_OR: u8 = 0x59;
const OP_SIZE: u8 = 0x5a;

// 0x6X — Constraints
const OP_SCALAR: u8 = 0x60;
const OP_COMMIT: u8 = 0x61;
const OP_ALLOC: u8 = 0x62;
const OP_EXPR: u8 = 0x63;
const OP_RANGE: u8 = 0x64;

// 0x7X — Dict
const OP_DICT: u8 = 0x70;
const OP_PUT: u8 = 0x71;
const OP_REPLACE: u8 = 0x72;
const OP_GET: u8 = 0x73;
const OP_GETOPT: u8 = 0x74;
const OP_GETDUP: u8 = 0x75;
const OP_FIRST: u8 = 0x76;
const OP_LAST: u8 = 0x77;
const OP_NEXT: u8 = 0x78;

// 0x8X — Cryptography
const OP_TRANSCRIPT: u8 = 0x80;
const OP_TWRITE: u8 = 0x81;
const OP_TREAD: u8 = 0x82;
const OP_SHA256: u8 = 0x83;
const OP_SHA512: u8 = 0x84;
const OP_SHA3: u8 = 0x85;
const OP_KECCAK256: u8 = 0x86;
const OP_LOG: u8 = 0x87;
const OP_CELLHASH: u8 = 0x88;

// 0x9X — Tokens. Mint opcodes and their consumer-side flavor helpers
// are paired (priv ↔ privflv, pub ↔ pubflv) for adjacency.
const OP_AMOUNT: u8 = 0x90;
const OP_ISSUEPRIV: u8 = 0x91;
const OP_ISSUEPRIVFLV: u8 = 0x92;
const OP_ISSUEPUB: u8 = 0x93;
const OP_ISSUEPUBFLV: u8 = 0x94;
const OP_RETIRE: u8 = 0x95;
const OP_BORROW: u8 = 0x96;
const OP_MERGE: u8 = 0x97;
const OP_SPLIT: u8 = 0x98;
const OP_MIX: u8 = 0x99;
const OP_DECRYPT: u8 = 0x9a;
const OP_FEE: u8 = 0x9b;

// 0xaX — Control flow. label/jump/jumpif carry a label-number operand
// (sub-varint); see ADR 0015.
const OP_VERIFY: u8 = 0xa0;
const OP_LABEL: u8 = 0xa1;
const OP_JUMP: u8 = 0xa2;
const OP_JUMPIF: u8 = 0xa3;
const OP_RETURN: u8 = 0xa4;
const OP_TYPE: u8 = 0xa5;
const OP_PUSHCELL: u8 = 0xa6;
const OP_EXEC: u8 = 0xa7;

// 0xcX — Contracts & predicates
const OP_INPUT: u8 = 0xc0;
const OP_CONTRACT: u8 = 0xc1;
const OP_OUTPUT: u8 = 0xc2;
const OP_OPEN: u8 = 0xc3;
const OP_SIGNTX: u8 = 0xc4;
const OP_SIGNCALL: u8 = 0xc5;

// 0xdX — Actors
const OP_SEND: u8 = 0xd0;
const OP_CALL: u8 = 0xd1;
const OP_LOAD: u8 = 0xd2;
const OP_SAVE: u8 = 0xd3;
const OP_SETCODE: u8 = 0xd4;
const OP_ADDSTORAGE: u8 = 0xd5;
const OP_QUOTESTORAGE: u8 = 0xd6;

// 0xeX — Frame introspection
const OP_SELFID: u8 = 0xe0;
const OP_ANCHOR: u8 = 0xe1;
const OP_CALLERID: u8 = 0xe2;
const OP_GAS: u8 = 0xe4;
const OP_GASLIMIT: u8 = 0xe5;
const OP_USAGE: u8 = 0xe6;
// 0xe7 was the explicit transient-memory limit and remains reserved.
const OP_CAPACITY: u8 = 0xe8;

// 0xfX — Tx & chain info (chain-info opcodes 0xf2-0xf7 are reserved
// for future implementation; the parser routes them through `Ext`.)
const OP_TIMELOCK: u8 = 0xf0;
const OP_VERSION: u8 = 0xf1;
const OP_HEIGHT: u8 = 0xf2;

// ── Instruction enum ─────────────────────────────────────────────────

/// A decoded instruction. See `spec.md` for stack semantics; each
/// variant's inline comment shows its stack diagram.
#[derive(Clone, Debug)]
pub enum Instruction {
    PushInt(Scalar), // ø push → int
    /// Host-only byte literal, compiled to pushcell and a child Cell.
    BytesLiteral(Arc<String>),
    PushPoint(Point), // ø pushpoint → point (witness-bearing on prover)
    PushToken,        // flv pushtoken → token
    /// The optional reference is compiler input, not an inline bytecode operand.
    PushCell(Option<CellRef>), // ø pushcell → cell
    Exec,             // cell exec → ø (tail execution in this frame)
    Drop,             // x drop → ø
    Nop,              // ø nop → ø
    Dup,              // x(k) … x(0) k dup → x(k) … x(0) x(k)
    Roll,             // x(k) … x(0) k roll → x(k-1) … x(0) x(k)
    DupK(u8),         // x(k) … x(0) dup:k → x(k) … x(0) x(k)
    RollK(u8),        // x(k) … x(0) roll:k → x(k-1) … x(0) x(k)
    ReadUint,         // s n readuint → s' x 1 | s 0
    ReadScalar,       // s readscalar → s' x 1 | s 0
    ReadBytes,        // s b n readbytes → s' b' 1 | s b 0
    ReadPoint,        // s readpoint → s' p 1 | s 0
    WriteUint,        // b x n writeuint → b'
    WriteScalar,      // b x writescalar → b'
    AppendBytes,      // s b appendbytes → s' b'
    AppendRefs,       // s b appendrefs → s' b'
    Builder,          // ø builder → b
    Slice,            // c slice → s
    EndCell,          // b endcell → c
    WriteZeros,       // s n writezeros → s'
    BitNot,           // b bitnot → b'
    BitOr,            // b s bitor → b'
    BitAnd,           // b s bitand → b'
    BitXor,           // b s bitxor → b'
    ShiftLeft,        // b n shiftleft → b'
    ShiftRight,       // b n shiftright → b'
    Keccak256,        // s b keccak256 → b'
    Abs,              // x abs → |x| s
    Eq,               // a b eq → a b {0|1} or constraint
    Neg,              // x neg → -x
    Add,              // x y add → z
    Mul,              // x y mul → z
    DivMod,           // x z divmod → d r
    Mod252,           // s mod252 → int
    Not,              // x not → y
    And,              // a b and → c
    Or,               // a b or → c
    Size,             // x size → x n
    Scalar,           // s scalar → expr
    Commit,           // s commit → var
    Alloc(Option<Scalar>), // ø alloc → expr
    Expr,             // var expr → expr
    Range,            // x n range → x (scalar anywhere; expression external-only)
    Dict,             // val key … val key n dict → dict
    Put,              // dict k v put → dict'
    Replace,          // dict k v replace → dict' {prev 1 | 0}
    Get,              // dict k get → dict' k v
    GetOpt,           // dict k getopt → dict' {v 1 | 0}
    GetDup,           // dict k getdup → dict {v 1 | 0}
    First,            // dict first → dict {k 1 | 0}
    Last,             // dict last → dict {k 1 | 0}
    Next,             // dict k next → dict {k' 1 | 0}
    Transcript,       // label transcript → merlin (opens a Merlin transcript)
    TWrite,           // m label s twrite → m
    TRead,            // m label n tread → m s
    Sha256,           // s b sha256 → b'
    Sha512,           // s b sha512 → b'
    Sha3,             // s b sha3 → b'
    CellHash,         // c level b cellhash → b'
    Log,              // s log → ø
    Amount,           // t amount → t qty flv
    IssuePriv,        // qty:Variable tag issuepriv    → T  (predicate context)
    IssuePrivFlv, // pred tag    issueprivflv      → int (consumer-side flv helper for issuepriv)
    IssuePub,     // qty:Scalar tag issuepub → CT (InternalRoot / ActorCall)
    IssuePubFlv,  // cid tag     issuepubflv       → int (consumer-side flv helper for issuepub)
    Retire,       // t retire → ø
    Borrow,       // qty flv borrow → -T +T
    Merge,        // a b merge → {c 1 | a b 0}
    Split,        // a q split → a' b
    Mix,          // tokens… cmts… m n mix → tokens
    Decrypt,      // T f f' q q' decrypt → CT
    Verify,       // x verify → ø
    Fee,          // qty fee → -WT
    Label(u32),   // ø label:n → ø  (operand: label number)
    Jump(u32),    // ø jump:n → ø   (unconditional)
    JumpIf(u32),  // x jumpif:n → ø (jump iff x ≠ 0)
    Return,       // a(k-1) … a(0) k return → ø
    Type,         // x type → x code
    Input,        // s input → contract
    Contract,     // payload pred contract → contract
    Output,       // payload pred output → ø
    Open,         // contract ik nbrs pos script gas portable-args… k open → results… k'
    Send,         // portable-args… k refund gas addr send → ø (anonymous outside actor frames)
    Call,         // portable-args… k gas addr call → results… k' 1 | args… k 0 (actor-only)
    Load,         // ø load → value (actor-only)
    Save,         // value save → ø (actor-only)
    Setcode,      // code setcode → ø (actor-only)
    AddStorage,   // q addstorage → {debt 1 | 0} (actor-only)
    QuoteStorage, // q quotestorage → {fee 1 | 0} (actor-only)
    Signtx,       // contract signtx → payload (external-only)
    Signcall,     // contract script sig gas portable-args… m signcall → results… k'
    Timelock,     // ø timelock → n {0|1}
    Version,      // ø version → n
    Selfid,       // ø selfid → s (actor-only)
    Anchor,       // ø anchor → s
    Gas,          // ø gas → n
    Usage,        // ø usage → n (actor-only)
    Callerid,     // ø callerid → s (called frame; ContractOpen uses read-only id)
    Gaslimit,     // ø gaslimit → n
    Capacity,     // h capacity → n (actor-only)
    Height,       // ø height → n
    Ext(u8),      // unknown opcode byte; produced by the parser for any unassigned tag
}

impl Instruction {
    /// Writes this instruction's canonical bytecode to `w`.
    pub fn encode(&self, w: &mut Vec<u8>) {
        let op = |w: &mut Vec<u8>, b: u8| w.push(b);
        match self {
            Instruction::PushInt(i) => encode_push_int(i, w),
            Instruction::BytesLiteral(_) => op(w, OP_PUSHCELL),
            Instruction::PushPoint(p) => {
                op(w, OP_PUSHPOINT);
                // Always serializes the canonical 32-byte form;
                // witness variants compute their compressed point.
                w.extend_from_slice(&p.to_bytes())
            }
            Instruction::PushToken => op(w, OP_PUSHTOKEN),
            Instruction::PushCell(_) => op(w, OP_PUSHCELL),
            Instruction::Exec => op(w, OP_EXEC),
            Instruction::Drop => op(w, OP_DROP),
            Instruction::Nop => op(w, OP_NOP),
            Instruction::Dup => op(w, OP_DUP),
            Instruction::Roll => op(w, OP_ROLL),
            Instruction::DupK(k) => {
                debug_assert!(*k <= 0x0f, "DupK arg must fit in 4 bits");
                op(w, OP_DUPK_BASE + (*k & 0x0f))
            }
            Instruction::RollK(k) => {
                debug_assert!(*k <= 0x0f, "RollK arg must fit in 4 bits");
                op(w, OP_ROLLK_BASE + (*k & 0x0f))
            }
            Instruction::ReadUint => op(w, OP_READUINT),
            Instruction::ReadScalar => op(w, OP_READSCALAR),
            Instruction::ReadBytes => op(w, OP_READBYTES),
            Instruction::ReadPoint => op(w, OP_READPOINT),
            Instruction::WriteUint => op(w, OP_WRITEUINT),
            Instruction::WriteScalar => op(w, OP_WRITESCALAR),
            Instruction::AppendBytes => op(w, OP_APPENDBYTES),
            Instruction::AppendRefs => op(w, OP_APPENDREFS),
            Instruction::Builder => op(w, OP_BUILDER),
            Instruction::Slice => op(w, OP_SLICE),
            Instruction::EndCell => op(w, OP_ENDCELL),
            Instruction::WriteZeros => op(w, OP_WRITEZEROS),
            Instruction::BitNot => op(w, OP_BITNOT),
            Instruction::BitOr => op(w, OP_BITOR),
            Instruction::BitAnd => op(w, OP_BITAND),
            Instruction::BitXor => op(w, OP_BITXOR),
            Instruction::ShiftLeft => op(w, OP_SHIFTLEFT),
            Instruction::ShiftRight => op(w, OP_SHIFTRIGHT),
            Instruction::Keccak256 => op(w, OP_KECCAK256),
            Instruction::Abs => op(w, OP_ABS),
            Instruction::Eq => op(w, OP_EQ),
            Instruction::Neg => op(w, OP_NEG),
            Instruction::Add => op(w, OP_ADD),
            Instruction::Mul => op(w, OP_MUL),
            Instruction::DivMod => op(w, OP_DIVMOD),
            Instruction::Mod252 => op(w, OP_MOD252),
            Instruction::Not => op(w, OP_NOT),
            Instruction::And => op(w, OP_AND),
            Instruction::Or => op(w, OP_OR),
            Instruction::Size => op(w, OP_SIZE),
            Instruction::Scalar => op(w, OP_SCALAR),
            Instruction::Commit => op(w, OP_COMMIT),
            Instruction::Alloc(_) => op(w, OP_ALLOC),
            Instruction::Expr => op(w, OP_EXPR),
            Instruction::Range => op(w, OP_RANGE),
            Instruction::Dict => op(w, OP_DICT),
            Instruction::Put => op(w, OP_PUT),
            Instruction::Replace => op(w, OP_REPLACE),
            Instruction::Get => op(w, OP_GET),
            Instruction::GetOpt => op(w, OP_GETOPT),
            Instruction::GetDup => op(w, OP_GETDUP),
            Instruction::First => op(w, OP_FIRST),
            Instruction::Last => op(w, OP_LAST),
            Instruction::Next => op(w, OP_NEXT),
            Instruction::Transcript => op(w, OP_TRANSCRIPT),
            Instruction::TWrite => op(w, OP_TWRITE),
            Instruction::TRead => op(w, OP_TREAD),
            Instruction::Sha256 => op(w, OP_SHA256),
            Instruction::Sha512 => op(w, OP_SHA512),
            Instruction::Sha3 => op(w, OP_SHA3),
            Instruction::CellHash => op(w, OP_CELLHASH),
            Instruction::Log => op(w, OP_LOG),
            Instruction::Amount => op(w, OP_AMOUNT),
            Instruction::IssuePriv => op(w, OP_ISSUEPRIV),
            Instruction::IssuePrivFlv => op(w, OP_ISSUEPRIVFLV),
            Instruction::IssuePub => op(w, OP_ISSUEPUB),
            Instruction::IssuePubFlv => op(w, OP_ISSUEPUBFLV),
            Instruction::Retire => op(w, OP_RETIRE),
            Instruction::Borrow => op(w, OP_BORROW),
            Instruction::Merge => op(w, OP_MERGE),
            Instruction::Split => op(w, OP_SPLIT),
            Instruction::Mix => op(w, OP_MIX),
            Instruction::Decrypt => op(w, OP_DECRYPT),
            Instruction::Verify => op(w, OP_VERIFY),
            Instruction::Fee => op(w, OP_FEE),
            Instruction::Label(n) => {
                op(w, OP_LABEL);
                write_subvarint(w, *n as u64)
            }
            Instruction::Jump(n) => {
                op(w, OP_JUMP);
                write_subvarint(w, *n as u64)
            }
            Instruction::JumpIf(n) => {
                op(w, OP_JUMPIF);
                write_subvarint(w, *n as u64)
            }
            Instruction::Return => op(w, OP_RETURN),
            Instruction::Type => op(w, OP_TYPE),
            // Witness (if any) never crosses the wire — prover-side
            // only. Encoded form is the bare opcode byte.
            Instruction::Input => op(w, OP_INPUT),
            Instruction::Contract => op(w, OP_CONTRACT),
            Instruction::Output => op(w, OP_OUTPUT),
            Instruction::Open => op(w, OP_OPEN),
            Instruction::Send => op(w, OP_SEND),
            Instruction::Call => op(w, OP_CALL),
            Instruction::Load => op(w, OP_LOAD),
            Instruction::Save => op(w, OP_SAVE),
            Instruction::Setcode => op(w, OP_SETCODE),
            Instruction::AddStorage => op(w, OP_ADDSTORAGE),
            Instruction::QuoteStorage => op(w, OP_QUOTESTORAGE),
            Instruction::Signtx => op(w, OP_SIGNTX),
            Instruction::Signcall => op(w, OP_SIGNCALL),
            Instruction::Timelock => op(w, OP_TIMELOCK),
            Instruction::Version => op(w, OP_VERSION),
            Instruction::Selfid => op(w, OP_SELFID),
            Instruction::Anchor => op(w, OP_ANCHOR),
            Instruction::Gas => op(w, OP_GAS),
            Instruction::Usage => op(w, OP_USAGE),
            Instruction::Callerid => op(w, OP_CALLERID),
            Instruction::Gaslimit => op(w, OP_GASLIMIT),
            Instruction::Capacity => op(w, OP_CAPACITY),
            Instruction::Height => op(w, OP_HEIGHT),
            Instruction::Ext(b) => op(w, *b),
        }
    }
}

impl Instruction {
    /// Canonical bytecode; this is not a Cell record.
    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.encoded_size());
        self.encode(&mut bytes);
        bytes
    }

    /// Canonical instruction size without serializing or allocating.
    pub fn encoded_size(&self) -> usize {
        match self {
            Self::PushInt(value) => 1 + push_int_parts(value).1,
            Self::PushPoint(_) => 33,
            Self::Label(n) | Self::Jump(n) | Self::JumpIf(n) => 1 + subvarint_size(u64::from(*n)),
            _ => 1,
        }
    }

    pub(crate) fn source_size(&self) -> usize {
        match self {
            Self::BytesLiteral(value) => 1 + subvarint_size(value.len() as u64) + value.len(),
            _ => self.encoded_size(),
        }
    }

    /// Compatibility source for byte-only host APIs, never native execution.
    pub(crate) fn encode_source(&self, bytes: &mut Vec<u8>) {
        if let Self::BytesLiteral(value) = self {
            bytes.push(SOURCE_BYTES);
            write_subvarint(bytes, value.len() as u64);
            bytes.extend_from_slice(&value.to_bytes_vec());
        } else {
            self.encode(bytes);
        }
    }

    /// Parses legacy host source; native VM execution uses `parse` instead.
    pub fn parse_source(reader: &mut &[u8]) -> Result<Self, VMError> {
        if reader.first() != Some(&SOURCE_BYTES) {
            return Self::parse(reader);
        }
        take_bytes(reader, 1)?;
        let length = usize::try_from(read_subvarint(reader)?).map_err(|_| VMError::OutOfGas)?;
        if length > reader.len() {
            return Err(VMError::UnexpectedEndOfScript);
        }
        String::check_length(length)?;
        Ok(Self::BytesLiteral(Arc::new(String::from(
            take_bytes(reader, length)?.to_vec(),
        ))))
    }

    /// Reads exactly one Instruction (opcode + inline parameter bytes)
    /// from `reader`. Errors:
    ///
    /// - `VMError::UnexpectedEndOfScript` — reader ran out of bytes.
    /// - `VMError::InvalidScalarEncoding` — a `pushint` payload is not
    ///   a canonical scalar or does not use its minimal width.
    ///
    /// Unknown opcode bytes return `Instruction::Ext(b)` rather than
    /// erroring, so future protocol versions can introduce new opcodes
    /// without breaking older verifiers.
    pub fn parse(reader: &mut &[u8]) -> Result<Instruction, VMError> {
        let byte = take_bytes(reader, 1)?[0];
        match byte {
            // push:k
            0x00..=OP_PUSH_SMALL_MAX => Ok(Instruction::PushInt(Scalar::from(byte as u64))),
            // pushint{8,16,64,128} pos/neg
            OP_PUSHINT8_POS => parse_pushint_n(reader, 1, false),
            OP_PUSHINT8_NEG => parse_pushint_n(reader, 1, true),
            OP_PUSHINT16_POS => parse_pushint_n(reader, 2, false),
            OP_PUSHINT16_NEG => parse_pushint_n(reader, 2, true),
            OP_PUSHINT64_POS => parse_pushint_n(reader, 8, false),
            OP_PUSHINT64_NEG => parse_pushint_n(reader, 8, true),
            OP_PUSHINT128_POS => parse_pushint_n(reader, 16, false),
            OP_PUSHINT128_NEG => parse_pushint_n(reader, 16, true),
            OP_PUSHINT_FULL => parse_pushint_full(reader),
            OP_PUSHPOINT => {
                let mut buf = [0u8; 32];
                buf.copy_from_slice(take_bytes(reader, 32)?);
                Ok(Instruction::PushPoint(Point::from_bytes(buf)))
            }
            OP_PUSHTOKEN => Ok(Instruction::PushToken),
            OP_PUSHCELL => Ok(Instruction::PushCell(None)),
            OP_EXEC => Ok(Instruction::Exec),
            OP_DROP => Ok(Instruction::Drop),
            OP_NOP => Ok(Instruction::Nop),
            OP_DUP => Ok(Instruction::Dup),
            OP_ROLL => Ok(Instruction::Roll),
            OP_DUPK_BASE..=OP_DUPK_MAX => Ok(Instruction::DupK(byte - OP_DUPK_BASE)),
            OP_ROLLK_BASE..=OP_ROLLK_MAX => Ok(Instruction::RollK(byte - OP_ROLLK_BASE)),
            OP_READUINT => Ok(Instruction::ReadUint),
            OP_READSCALAR => Ok(Instruction::ReadScalar),
            OP_READBYTES => Ok(Instruction::ReadBytes),
            OP_READPOINT => Ok(Instruction::ReadPoint),
            OP_WRITEUINT => Ok(Instruction::WriteUint),
            OP_WRITESCALAR => Ok(Instruction::WriteScalar),
            OP_APPENDBYTES => Ok(Instruction::AppendBytes),
            OP_APPENDREFS => Ok(Instruction::AppendRefs),
            OP_BUILDER => Ok(Instruction::Builder),
            OP_SLICE => Ok(Instruction::Slice),
            OP_ENDCELL => Ok(Instruction::EndCell),
            OP_WRITEZEROS => Ok(Instruction::WriteZeros),
            OP_BITNOT => Ok(Instruction::BitNot),
            OP_BITOR => Ok(Instruction::BitOr),
            OP_BITAND => Ok(Instruction::BitAnd),
            OP_BITXOR => Ok(Instruction::BitXor),
            OP_SHIFTLEFT => Ok(Instruction::ShiftLeft),
            OP_SHIFTRIGHT => Ok(Instruction::ShiftRight),
            OP_KECCAK256 => Ok(Instruction::Keccak256),
            OP_ABS => Ok(Instruction::Abs),
            OP_EQ => Ok(Instruction::Eq),
            OP_NEG => Ok(Instruction::Neg),
            OP_ADD => Ok(Instruction::Add),
            OP_MUL => Ok(Instruction::Mul),
            OP_DIVMOD => Ok(Instruction::DivMod),
            OP_MOD252 => Ok(Instruction::Mod252),
            OP_NOT => Ok(Instruction::Not),
            OP_AND => Ok(Instruction::And),
            OP_OR => Ok(Instruction::Or),
            OP_SIZE => Ok(Instruction::Size),
            OP_SCALAR => Ok(Instruction::Scalar),
            OP_COMMIT => Ok(Instruction::Commit),
            OP_ALLOC => Ok(Instruction::Alloc(None)),
            OP_EXPR => Ok(Instruction::Expr),
            OP_RANGE => Ok(Instruction::Range),
            OP_DICT => Ok(Instruction::Dict),
            OP_PUT => Ok(Instruction::Put),
            OP_REPLACE => Ok(Instruction::Replace),
            OP_GET => Ok(Instruction::Get),
            OP_GETOPT => Ok(Instruction::GetOpt),
            OP_GETDUP => Ok(Instruction::GetDup),
            OP_FIRST => Ok(Instruction::First),
            OP_LAST => Ok(Instruction::Last),
            OP_NEXT => Ok(Instruction::Next),
            OP_TRANSCRIPT => Ok(Instruction::Transcript),
            OP_TWRITE => Ok(Instruction::TWrite),
            OP_TREAD => Ok(Instruction::TRead),
            OP_SHA256 => Ok(Instruction::Sha256),
            OP_SHA512 => Ok(Instruction::Sha512),
            OP_SHA3 => Ok(Instruction::Sha3),
            OP_CELLHASH => Ok(Instruction::CellHash),
            OP_LOG => Ok(Instruction::Log),
            OP_AMOUNT => Ok(Instruction::Amount),
            OP_ISSUEPRIV => Ok(Instruction::IssuePriv),
            OP_ISSUEPRIVFLV => Ok(Instruction::IssuePrivFlv),
            OP_ISSUEPUB => Ok(Instruction::IssuePub),
            OP_ISSUEPUBFLV => Ok(Instruction::IssuePubFlv),
            OP_RETIRE => Ok(Instruction::Retire),
            OP_BORROW => Ok(Instruction::Borrow),
            OP_MERGE => Ok(Instruction::Merge),
            OP_SPLIT => Ok(Instruction::Split),
            OP_MIX => Ok(Instruction::Mix),
            OP_DECRYPT => Ok(Instruction::Decrypt),
            OP_VERIFY => Ok(Instruction::Verify),
            OP_FEE => Ok(Instruction::Fee),
            OP_LABEL => parse_label_op(reader, Instruction::Label),
            OP_JUMP => parse_label_op(reader, Instruction::Jump),
            OP_JUMPIF => parse_label_op(reader, Instruction::JumpIf),
            OP_RETURN => Ok(Instruction::Return),
            OP_TYPE => Ok(Instruction::Type),
            OP_INPUT => Ok(Instruction::Input),
            OP_CONTRACT => Ok(Instruction::Contract),
            OP_OUTPUT => Ok(Instruction::Output),
            OP_OPEN => Ok(Instruction::Open),
            OP_SEND => Ok(Instruction::Send),
            OP_CALL => Ok(Instruction::Call),
            OP_LOAD => Ok(Instruction::Load),
            OP_SAVE => Ok(Instruction::Save),
            OP_SETCODE => Ok(Instruction::Setcode),
            OP_ADDSTORAGE => Ok(Instruction::AddStorage),
            OP_QUOTESTORAGE => Ok(Instruction::QuoteStorage),
            OP_SIGNTX => Ok(Instruction::Signtx),
            OP_SIGNCALL => Ok(Instruction::Signcall),
            OP_TIMELOCK => Ok(Instruction::Timelock),
            OP_VERSION => Ok(Instruction::Version),
            OP_SELFID => Ok(Instruction::Selfid),
            OP_ANCHOR => Ok(Instruction::Anchor),
            OP_GAS => Ok(Instruction::Gas),
            OP_USAGE => Ok(Instruction::Usage),
            OP_CALLERID => Ok(Instruction::Callerid),
            OP_GASLIMIT => Ok(Instruction::Gaslimit),
            OP_CAPACITY => Ok(Instruction::Capacity),
            OP_HEIGHT => Ok(Instruction::Height),
            _ => Ok(Instruction::Ext(byte)),
        }
    }

    /// Returns this instruction's contribution to the prover's witness
    /// queue — `Some(w)` for variants that own a witness slot, `None`
    /// otherwise. Walked by `ScriptBuilder::to_witnesses`.
    pub fn witness(&self) -> Option<Option<Scalar>> {
        match self {
            Instruction::Alloc(w) => Some(*w),
            _ => None,
        }
    }
}

// ── Internal encode helpers ──────────────────────────────────────────

/// Reads a `label`/`jump`/`jumpif` operand: a sub-varint label number,
/// narrowed to `u32` (a label number that large is malformed bytecode).
fn parse_label_op(
    reader: &mut &[u8],
    build: fn(u32) -> Instruction,
) -> Result<Instruction, VMError> {
    let n = read_subvarint(reader).map_err(|_| VMError::UnexpectedEndOfScript)?;
    if n > u32::MAX as u64 {
        return Err(VMError::LabelOutOfOrder);
    }
    Ok(build(n as u32))
}

/// Encodes `i` using the narrowest opcode pair that fits. The
/// resulting byte sequence matches what the VM's byte-dispatch handler
/// expects to parse.
fn encode_push_int(i: &Scalar, bytes: &mut Vec<u8>) {
    let (tag, width, payload) = push_int_parts(i);
    bytes.push(tag);
    bytes.extend_from_slice(&payload[..width]);
}

fn push_int_parts(i: &Scalar) -> (u8, usize, [u8; 32]) {
    if let Some(n) = i.to_u64() {
        if n <= OP_PUSH_SMALL_MAX as u64 {
            return (n as u8, 0, [0; 32]);
        }
    }
    let negative = -*i;
    let is_negative = *i != Scalar::ZERO && negative.to_u128().is_some();
    let compact = if is_negative { negative } else { *i };
    let (positive_tag, width) = match compact.to_u128() {
        Some(n) if n <= u8::MAX as u128 => (OP_PUSHINT8_POS, 1),
        Some(n) if n <= u16::MAX as u128 => (OP_PUSHINT16_POS, 2),
        Some(n) if n <= u64::MAX as u128 => (OP_PUSHINT64_POS, 8),
        Some(_) => (OP_PUSHINT128_POS, 16),
        None => return (OP_PUSHINT_FULL, 32, i.to_bytes()),
    };
    (
        positive_tag + u8::from(is_negative),
        width,
        compact.to_bytes(),
    )
}

/// Bytecode sub-varints use disjoint ranges, so every number has one encoding.
pub(crate) fn write_subvarint(bytes: &mut Vec<u8>, n: u64) {
    let (tag, width, payload) = match n {
        0..=255 => (0, 1, n),
        256..=65_791 => (1, 2, n - 256),
        65_792..=4_295_033_087 => (2, 4, n - 65_792),
        _ => (3, 8, n - 4_295_033_088),
    };
    bytes.push(tag);
    bytes.extend_from_slice(&payload.to_le_bytes()[..width]);
}

pub(crate) fn read_subvarint(bytes: &mut &[u8]) -> Result<u64, VMError> {
    let (width, base) = match take_bytes(bytes, 1)?[0] {
        0 => (1, 0u64),
        1 => (2, 256),
        2 => (4, 65_792),
        3 => (8, 4_295_033_088),
        _ => return Err(VMError::UnexpectedEndOfScript),
    };
    let mut payload = [0; 8];
    payload[..width].copy_from_slice(take_bytes(bytes, width)?);
    base.checked_add(u64::from_le_bytes(payload))
        .ok_or(VMError::UnexpectedEndOfScript)
}

fn subvarint_size(n: u64) -> usize {
    match n {
        0..=255 => 2,
        256..=65_791 => 3,
        65_792..=4_295_033_087 => 5,
        _ => 9,
    }
}

fn take_bytes<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], VMError> {
    if count > input.len() {
        return Err(VMError::UnexpectedEndOfScript);
    }
    let (head, tail) = input.split_at(count);
    *input = tail;
    Ok(head)
}

/// Reads a compact pushint payload, optionally negating it modulo ℓ.
/// Every payload fits below ℓ; non-minimal widths are rejected.
fn parse_pushint_n(
    reader: &mut &[u8],
    width_bytes: usize,
    negate: bool,
) -> Result<Instruction, VMError> {
    debug_assert!(width_bytes <= 16);
    let mut buf = [0u8; 16];
    buf[..width_bytes].copy_from_slice(take_bytes(reader, width_bytes)?);
    let mag = u128::from_le_bytes(buf);
    // Canonical minimal width: reject a value representable by a narrower
    // class (push:k for residues 0..15, push:0 for zero, the next-smaller
    // pushint for wider forms). The encoder always picks the narrowest.
    let minimal = match width_bytes {
        1 => mag != 0 && (negate || mag > OP_PUSH_SMALL_MAX as u128),
        2 => mag > u8::MAX as u128,
        8 => mag > u16::MAX as u128,
        16 => mag > u64::MAX as u128,
        _ => true,
    };
    if !minimal {
        return Err(VMError::InvalidScalarEncoding);
    }
    let scalar = Scalar::from(mag);
    Ok(Instruction::PushInt(if negate { -scalar } else { scalar }))
}

fn parse_pushint_full(reader: &mut &[u8]) -> Result<Instruction, VMError> {
    let mut buf = [0u8; 32];
    buf.copy_from_slice(take_bytes(reader, 32)?);
    let int = Scalar::from_bytes(buf).ok_or(VMError::InvalidScalarEncoding)?;
    // Full form is only for residues whose value and modular negation both
    // exceed 128 bits; either compact endpoint must use its narrower opcode.
    if int.to_u128().is_some() || (-int).to_u128().is_some() {
        return Err(VMError::InvalidScalarEncoding);
    }
    Ok(Instruction::PushInt(int))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytecode_subvarints_are_canonical_and_bounded() {
        for (value, expected) in [
            (0, "0000"),
            (255, "00ff"),
            (256, "010000"),
            (65_791, "01ffff"),
            (65_792, "0200000000"),
            (4_295_033_087, "02ffffffff"),
            (4_295_033_088, "030000000000000000"),
            (u64::MAX, "03fffefefffeffffff"),
        ] {
            let mut encoded = Vec::new();
            write_subvarint(&mut encoded, value);
            assert_eq!(encoded, bytes(expected));
            assert_eq!(subvarint_size(value), encoded.len());
            let mut input = encoded.as_slice();
            assert_eq!(read_subvarint(&mut input).unwrap(), value);
            assert!(input.is_empty());
        }
        assert!(read_subvarint(&mut &[3, 255, 255, 255, 255, 255, 255, 255, 255][..]).is_err());
        assert!(read_subvarint(&mut &[4][..]).is_err());
        assert!(read_subvarint(&mut &[3, 0][..]).is_err());
        for instruction in [
            Instruction::BytesLiteral(Arc::new(String::from(vec![7; 256]))),
            Instruction::Label(u32::MAX),
            Instruction::PushInt(Scalar::from(-256i64)),
            Instruction::Alloc(Some(Scalar::ONE)),
        ] {
            let encoded = instruction.encode_to_vec();
            assert_eq!(instruction.encoded_size(), encoded.len());
            let parsed = Instruction::parse(&mut encoded.as_slice()).unwrap();
            assert_eq!(parsed.encode_to_vec(), encoded);
        }
    }

    fn bytes(hex: &str) -> Vec<u8> {
        hex.as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn golden_pushint_width_boundaries() {
        let u64_next = Scalar::from(u64::MAX as u128 + 1);
        let u128_top = Scalar::from(u128::MAX);
        let u128_next = Scalar::from(u128::MAX) + Scalar::ONE;
        for (value, expected) in [
            (Scalar::ZERO, "00"),
            (Scalar::from(15u64), "0f"),
            (Scalar::from(16u64), "1010"),
            (Scalar::from(255u64), "10ff"),
            (Scalar::from(256u64), "120001"),
            (Scalar::from(65_535u64), "12ffff"),
            (Scalar::from(65_536u64), "140000010000000000"),
            (Scalar::from(u64::MAX), "14ffffffffffffffff"),
            (u64_next, "1600000000000000000100000000000000"),
            (u128_top, "16ffffffffffffffffffffffffffffffff"),
            (
                u128_next,
                "180000000000000000000000000000000001000000000000000000000000000000",
            ),
            (Scalar::from(-1i64), "1101"),
            (Scalar::from(-255i64), "11ff"),
            (-Scalar::from(256u64), "130001"),
            (-Scalar::from(65_535u64), "13ffff"),
            (-Scalar::from(65_536u64), "150000010000000000"),
            (-Scalar::from(u64::MAX), "15ffffffffffffffff"),
            (-u64_next, "1700000000000000000100000000000000"),
            (-u128_top, "17ffffffffffffffffffffffffffffffff"),
            (
                -u128_next,
                "18edd3f55c1a631258d69cf7a2def9de14ffffffffffffffffffffffffffffff0f",
            ),
        ] {
            let encoded = Instruction::PushInt(value).encode_to_vec();
            assert_eq!(encoded, bytes(expected));
            let mut input = encoded.as_slice();
            match Instruction::parse(&mut input).expect("canonical vector") {
                Instruction::PushInt(decoded) => assert_eq!(decoded, value),
                other => panic!("expected PushInt, got {:?}", other),
            }
            assert!(input.is_empty());
        }
    }

    #[test]
    fn pushint_full_rejects_noncanonical_or_compact_residues() {
        let modulus = bytes("edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010");
        let compact = [
            Scalar::ZERO,
            Scalar::from(u128::MAX),
            -Scalar::from(u128::MAX),
        ];
        for payload in compact
            .iter()
            .map(|value| value.to_bytes().to_vec())
            .chain([modulus, vec![0xff; 32]])
        {
            let mut encoded = vec![OP_PUSHINT_FULL];
            encoded.extend_from_slice(&payload);
            assert!(matches!(
                Instruction::parse(&mut encoded.as_slice()),
                Err(VMError::InvalidScalarEncoding)
            ));
        }
    }

    #[test]
    fn pushint_compact_requires_minimal_width() {
        for encoded in [
            vec![OP_PUSHINT8_POS, 0],
            vec![OP_PUSHINT8_POS, 15],
            vec![OP_PUSHINT8_NEG, 0],
            vec![OP_PUSHINT16_POS, 255, 0],
            vec![OP_PUSHINT16_NEG, 255, 0],
        ] {
            assert!(matches!(
                Instruction::parse(&mut encoded.as_slice()),
                Err(VMError::InvalidScalarEncoding)
            ));
        }
    }

    #[test]
    fn alloc_witness_is_discarded_in_bytecode() {
        let buf = Instruction::Alloc(Some(Scalar::from(42u64))).encode_to_vec();
        assert_eq!(buf, vec![OP_ALLOC]);

        let mut r: &[u8] = &buf;
        let parsed = Instruction::parse(&mut r).expect("parses");
        match parsed {
            Instruction::Alloc(w) => assert_eq!(w, None),
            _ => panic!("Alloc parse failed"),
        }
    }

    #[test]
    fn ext_opcode_for_unknown_bytes() {
        // 0x19 was pushstr; literals now use pushcell.
        // 0xf3..=0xf7 remain reserved for the chain-info family after
        // 0xf2 was assigned to height.
        // 0xff is a sentinel "definitely unassigned" byte for fuzzing
        // future extensions.
        let unused = [0x19, 0xf3, 0xff];
        for b in unused {
            let mut r: &[u8] = &[b];
            let parsed = Instruction::parse(&mut r).expect("parses");
            match parsed {
                Instruction::Ext(x) => assert_eq!(x, b),
                _ => panic!("expected Ext({:#x})", b),
            }
        }
    }
}
