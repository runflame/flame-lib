//! Definition of all instructions in FlameVM,
//! their codes and decoding/encoding utility functions.

use curve25519_dalek::scalar::Scalar;
use readerwriter::{Encodable, Reader, WriteError, Writer};

use crate::errors::VMError;
use crate::int253::Int253;
use crate::string::String;

// ── Opcode bytes ─────────────────────────────────────────────────────

// Byte map: see `flamevm/spec.md` §"Instruction table" for the
// canonical view. The high nibble is the block; low nibble is either
// the slot or, for k-class ops (`push:k`, `dup:k`, `roll:k`,
// `break:k`), the inline operand.

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
const OP_PUSHSTR: u8 = 0x19;
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

// 0x4X — String
const OP_READBITS: u8 = 0x40;
const OP_READINT: u8 = 0x41;
const OP_READSTR: u8 = 0x42;
const OP_READPOINT: u8 = 0x43;
const OP_WRITEBITS: u8 = 0x44;
const OP_WRITEINT: u8 = 0x45;
const OP_APPEND: u8 = 0x46;
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

// 0xaX — Control flow
const OP_VERIFY: u8 = 0xa0;
const OP_RUN: u8 = 0xa1;
const OP_LOOP: u8 = 0xa2;
const OP_SWITCH: u8 = 0xa3;
const OP_RETURN: u8 = 0xa4;
const OP_TYPE: u8 = 0xa5;

// 0xbX — break:k
const OP_BREAKK_BASE: u8 = 0xb0;
const OP_BREAKK_MAX: u8 = 0xbf;

// 0xcX — Cells & predicates
const OP_INPUT: u8 = 0xc0;
const OP_CELL: u8 = 0xc1;
const OP_OUTPUT: u8 = 0xc2;
const OP_OPEN: u8 = 0xc3;
const OP_SIGNTX: u8 = 0xc4;
const OP_SIGNCALL: u8 = 0xc5;

// 0xdX — Actors
const OP_SEND: u8 = 0xd0;
const OP_CALL: u8 = 0xd1;
const OP_LOAD: u8 = 0xd2;
const OP_SAVE: u8 = 0xd3;

// 0xeX — Frame introspection
const OP_ACTORID: u8 = 0xe0;
const OP_ANCHOR: u8 = 0xe1;
const OP_CALLERID: u8 = 0xe2;
const OP_METHOD: u8 = 0xe3;
const OP_GAS: u8 = 0xe4;
const OP_GASLIMIT: u8 = 0xe5;
const OP_BYTES: u8 = 0xe6;
const OP_MEMLIMIT: u8 = 0xe7;
const OP_NEWBYTES: u8 = 0xe8;

// 0xfX — Tx & chain info (chain-info opcodes 0xf2-0xf7 are reserved
// for future implementation; the parser routes them through `Ext`.)
const OP_TIMELOCK: u8 = 0xf0;
const OP_VERSION: u8 = 0xf1;

// ── Instruction enum ─────────────────────────────────────────────────

/// A decoded instruction. See `spec.md` for stack semantics; each
/// variant's inline comment shows its stack diagram.
#[derive(Clone, Debug)]
pub enum Instruction {
    PushInt(Int253),       // ø push → int
    PushStr(String),       // ø pushstr → str
    PushPoint(crate::crypto::Point), // ø pushpoint → point (witness-bearing on prover)
    PushToken,             // flv pushtoken → token
    Drop,                  // x drop → ø
    Nop,                   // ø nop → ø
    Dup,                   // x(k) … x(0) k dup → x(k) … x(0) x(k)
    Roll,                  // x(k) … x(0) k roll → x(k-1) … x(0) x(k)
    DupK(u8),              // x(k) … x(0) dup:k → x(k) … x(0) x(k)
    RollK(u8),             // x(k) … x(0) roll:k → x(k-1) … x(0) x(k)
    ReadBits,              // s n readbits → s' x 1 | s 0
    ReadInt,               // s readint → s' x 1 | s 0
    ReadStr,               // s n readstr → s' s'' 1 | s 0
    ReadPoint,             // s readpoint → s' p 1 | s 0
    WriteBits,             // s x n writebits → s'
    WriteInt,              // s x writeint → s'
    Append,                // s s' append → s''
    WriteZeros,            // s n writezeros → s'
    BitNot,                // s bitnot → s'
    BitOr,                 // a b bitor → c
    BitAnd,                // a b bitand → c
    BitXor,                // a b bitxor → c
    ShiftLeft,             // a n shiftleft → b c
    ShiftRight,            // a n shiftright → b c
    Keccak256,             // s keccak256 → x
    Abs,                   // x abs → |x| s
    Eq,                    // a b eq → a b {0|1} or constraint
    Neg,                   // x neg → -x
    Add,                   // x y add → z
    Mul,                   // x y mul → z
    DivMod,                // x z divmod → d r
    Mod252,                // s mod252 → int
    Not,                   // x not → y
    And,                   // a b and → c
    Or,                    // a b or → c
    Size,                  // x size → x n
    Scalar,                // s scalar → expr
    Commit,                // s commit → var
    Alloc(Option<Int253>), // ø alloc → expr
    Expr,                  // var expr → expr
    Range,                 // expr n range → expr
    Dict,                  // val key … val key n dict → dict
    Put,                   // dict k v put → dict'
    Replace,               // dict k v replace → dict' {prev 1 | 0}
    Get,                   // dict k get → dict' k v
    GetOpt,                // dict k getopt → dict' {v 1 | 0}
    GetDup,                // dict k getdup → dict {v 1 | 0}
    First,                 // dict first → dict {k 1 | 0}
    Last,                  // dict last → dict {k 1 | 0}
    Next,                  // dict k next → dict {k' 1 | 0}
    Transcript,            // label transcript → merlin (opens a Merlin transcript)
    TWrite,                // m label s twrite → m
    TRead,                 // m label n tread → m s
    Sha256,                // s sha256 → x
    Sha512,                // s sha512 → x
    Sha3,                  // s sha3 → x
    Log,                   // s log → ø
    Amount,                // t amount → t qty flv
    IssuePriv,             // qty:Variable tag issuepriv    → T  (predicate context)
    IssuePrivFlv,          // pred tag    issueprivflv      → int (consumer-side flv helper for issuepriv)
    IssuePub,              // qty:Int253  tag issuepub      → CT (actor context)
    IssuePubFlv,           // cid tag     issuepubflv       → int (consumer-side flv helper for issuepub)
    Retire,                // t retire → ø
    Borrow,                // qty flv borrow → -T +T
    Merge,                 // a b merge → {c 1 | a b 0}
    Split,                 // a q split → a' b
    Mix,                   // tokens… cmts… m n mix → tokens
    Decrypt,               // T f' f q' q decrypt → CT
    Verify,                // x verify → ø
    Fee,                   // qty flv fee → -WT
    Run,                   // s run → …
    Loop,                  // ø loop → ø
    Switch,                // x a b switch → …
    Return,                // a(k-1) … a(0) k return → ø
    Type,                  // x type → x code
    BreakK(u8),            // ø break:k → ø
    Input,                 // s input → cell
    Cell,                  // items… k pred cell → cell
    Output,                // items… k pred output → ø
    Open,                  // cell ik nbrs pos script gas bytes args… k open → results… k'
    Send,                  // args… k refund gas bytes method addr send → ø
    Call,                  // args… k gas bytes method addr call → results… k'
    Load,                  // ø load → dict
    Save,                  // dict save → ø
    Signtx,                // cell signtx → items… k
    Signcall,               // cell script sig gas bytes args… m signcall → results… k'
    Timelock,              // ø timelock → n {0|1}
    Version,               // ø version → n
    Actorid,               // ø actorid → s
    Anchor,                // ø anchor → s
    Gas,                   // ø gas → n
    Bytes,                 // ø bytes → n
    Callerid,              // ø callerid → s
    Method,                // ø method → int
    Gaslimit,              // ø gaslimit → n
    Memlimit,              // ø memlimit → n
    Newbytes,              // ø newbytes → n
    Ext(u8),               // unknown opcode byte; produced by the parser for any unassigned tag
}

impl Encodable for Instruction {
    /// Writes this instruction's canonical bytecode to `w`.
    fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        let op = |w: &mut dyn Writer, b: u8| w.write_u8(b"op", b);
        match self {
            Instruction::PushInt(i) => encode_push_int(i, w),
            Instruction::PushStr(s) => {
                op(w, OP_PUSHSTR)?;
                crate::encoding::write_subvarint(w, s.len() as u64)?;
                // Use `bytes_view` so witness-bearing variants
                // (Commitment / Scalar / Predicate) serialize to
                // their canonical opaque bytes. `as_bytes` would
                // panic for those — the verifier's wire form must
                // match regardless of which variant the prover
                // used.
                w.write(b"pushstr.bytes", &s.to_bytes_vec())
            }
            Instruction::PushPoint(p) => {
                op(w, OP_PUSHPOINT)?;
                // Always serializes the canonical 32-byte form;
                // witness variants compute their compressed point.
                w.write(b"pushpoint.bytes", &p.to_bytes())
            }
            Instruction::PushToken => op(w, OP_PUSHTOKEN),
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
            Instruction::ReadBits => op(w, OP_READBITS),
            Instruction::ReadInt => op(w, OP_READINT),
            Instruction::ReadStr => op(w, OP_READSTR),
            Instruction::ReadPoint => op(w, OP_READPOINT),
            Instruction::WriteBits => op(w, OP_WRITEBITS),
            Instruction::WriteInt => op(w, OP_WRITEINT),
            Instruction::Append => op(w, OP_APPEND),
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
            Instruction::Run => op(w, OP_RUN),
            Instruction::Loop => op(w, OP_LOOP),
            Instruction::Switch => op(w, OP_SWITCH),
            Instruction::Return => op(w, OP_RETURN),
            Instruction::Type => op(w, OP_TYPE),
            Instruction::BreakK(k) => {
                debug_assert!(*k <= 0x0f, "BreakK arg must fit in 4 bits");
                op(w, OP_BREAKK_BASE + (*k & 0x0f))
            }
            // Witness (if any) never crosses the wire — prover-side
            // only. Encoded form is the bare opcode byte.
            Instruction::Input => op(w, OP_INPUT),
            Instruction::Cell => op(w, OP_CELL),
            Instruction::Output => op(w, OP_OUTPUT),
            Instruction::Open => op(w, OP_OPEN),
            Instruction::Send => op(w, OP_SEND),
            Instruction::Call => op(w, OP_CALL),
            Instruction::Load => op(w, OP_LOAD),
            Instruction::Save => op(w, OP_SAVE),
            Instruction::Signtx => op(w, OP_SIGNTX),
            Instruction::Signcall => op(w, OP_SIGNCALL),
            Instruction::Timelock => op(w, OP_TIMELOCK),
            Instruction::Version => op(w, OP_VERSION),
            Instruction::Actorid => op(w, OP_ACTORID),
            Instruction::Anchor => op(w, OP_ANCHOR),
            Instruction::Gas => op(w, OP_GAS),
            Instruction::Bytes => op(w, OP_BYTES),
            Instruction::Callerid => op(w, OP_CALLERID),
            Instruction::Method => op(w, OP_METHOD),
            Instruction::Gaslimit => op(w, OP_GASLIMIT),
            Instruction::Memlimit => op(w, OP_MEMLIMIT),
            Instruction::Newbytes => op(w, OP_NEWBYTES),
            Instruction::Ext(b) => op(w, *b),
        }
    }
}

impl Instruction {

    /// Reads exactly one Instruction (opcode + inline parameter bytes)
    /// from `reader`. Errors:
    ///
    /// - `VMError::UnexpectedEndOfScript` — reader ran out of bytes.
    /// - `VMError::InvalidInt253Encoding` — a `pushint` payload's
    ///   magnitude isn't a canonical Ristretto scalar, or a `pushint`
    ///   full encoded negative zero.
    ///
    /// Unknown opcode bytes return `Instruction::Ext(b)` rather than
    /// erroring, so future protocol versions can introduce new opcodes
    /// without breaking older verifiers.
    pub fn parse(reader: &mut impl Reader) -> Result<Instruction, VMError> {
        let byte = reader.read_u8().map_err(|_| VMError::UnexpectedEndOfScript)?;
        match byte {
            // push:k
            0x00..=OP_PUSH_SMALL_MAX => {
                Ok(Instruction::PushInt(Int253::from(byte as u64)))
            }
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
            OP_PUSHSTR => {
                let len = crate::encoding::read_subvarint(reader)
                    .map_err(|_| VMError::UnexpectedEndOfScript)? as usize;
                let mut buf = vec![0u8; len];
                reader
                    .read(&mut buf)
                    .map_err(|_| VMError::UnexpectedEndOfScript)?;
                Ok(Instruction::PushStr(String::from(buf)))
            }
            OP_PUSHPOINT => {
                let mut buf = [0u8; 32];
                reader
                    .read(&mut buf)
                    .map_err(|_| VMError::UnexpectedEndOfScript)?;
                Ok(Instruction::PushPoint(crate::crypto::Point::from_bytes(buf)))
            }
            OP_PUSHTOKEN => Ok(Instruction::PushToken),
            OP_DROP => Ok(Instruction::Drop),
            OP_NOP => Ok(Instruction::Nop),
            OP_DUP => Ok(Instruction::Dup),
            OP_ROLL => Ok(Instruction::Roll),
            OP_DUPK_BASE..=OP_DUPK_MAX => Ok(Instruction::DupK(byte - OP_DUPK_BASE)),
            OP_ROLLK_BASE..=OP_ROLLK_MAX => Ok(Instruction::RollK(byte - OP_ROLLK_BASE)),
            OP_READBITS => Ok(Instruction::ReadBits),
            OP_READINT => Ok(Instruction::ReadInt),
            OP_READSTR => Ok(Instruction::ReadStr),
            OP_READPOINT => Ok(Instruction::ReadPoint),
            OP_WRITEBITS => Ok(Instruction::WriteBits),
            OP_WRITEINT => Ok(Instruction::WriteInt),
            OP_APPEND => Ok(Instruction::Append),
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
            OP_RUN => Ok(Instruction::Run),
            OP_LOOP => Ok(Instruction::Loop),
            OP_SWITCH => Ok(Instruction::Switch),
            OP_RETURN => Ok(Instruction::Return),
            OP_TYPE => Ok(Instruction::Type),
            OP_BREAKK_BASE..=OP_BREAKK_MAX => {
                Ok(Instruction::BreakK(byte - OP_BREAKK_BASE))
            }
            OP_INPUT => Ok(Instruction::Input),
            OP_CELL => Ok(Instruction::Cell),
            OP_OUTPUT => Ok(Instruction::Output),
            OP_OPEN => Ok(Instruction::Open),
            OP_SEND => Ok(Instruction::Send),
            OP_CALL => Ok(Instruction::Call),
            OP_LOAD => Ok(Instruction::Load),
            OP_SAVE => Ok(Instruction::Save),
            OP_SIGNTX => Ok(Instruction::Signtx),
            OP_SIGNCALL => Ok(Instruction::Signcall),
            OP_TIMELOCK => Ok(Instruction::Timelock),
            OP_VERSION => Ok(Instruction::Version),
            OP_ACTORID => Ok(Instruction::Actorid),
            OP_ANCHOR => Ok(Instruction::Anchor),
            OP_GAS => Ok(Instruction::Gas),
            OP_BYTES => Ok(Instruction::Bytes),
            OP_CALLERID => Ok(Instruction::Callerid),
            OP_METHOD => Ok(Instruction::Method),
            OP_GASLIMIT => Ok(Instruction::Gaslimit),
            OP_MEMLIMIT => Ok(Instruction::Memlimit),
            OP_NEWBYTES => Ok(Instruction::Newbytes),
            _ => Ok(Instruction::Ext(byte)),
        }
    }

    /// Returns this instruction's contribution to the prover's witness
    /// queue — `Some(w)` for variants that own a witness slot, `None`
    /// otherwise. Walked by `Program::to_witnesses`.
    pub fn witness(&self) -> Option<Option<Int253>> {
        match self {
            Instruction::Alloc(w) => Some(*w),
            _ => None,
        }
    }
}

// ── Internal encode helpers ──────────────────────────────────────────

/// Encodes `i` using the narrowest opcode pair that fits. The
/// resulting byte sequence matches what the VM's byte-dispatch handler
/// expects to parse.
fn encode_push_int(i: &Int253, w: &mut impl Writer) -> Result<(), WriteError> {
    let bytes = i.to_bytes();
    let neg = i.is_negative();
    let mag_scalar = i.abs_scalar();
    let mag_bytes = mag_scalar.to_bytes();

    // Try push:k (only for 0..=15 and non-negative).
    if !neg {
        if mag_bytes[8..].iter().all(|&b| b == 0) {
            let mut lo = [0u8; 8];
            lo.copy_from_slice(&mag_bytes[..8]);
            let v = u64::from_le_bytes(lo);
            if v <= OP_PUSH_SMALL_MAX as u64 {
                return w.write_u8(b"pushsmall", v as u8);
            }
        }
    }

    // Try pushint8 / 16 / 64 / 128 if magnitude fits.
    if mag_bytes[16..].iter().all(|&b| b == 0) {
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&mag_bytes[..16]);
        let v = u128::from_le_bytes(buf);
        if v <= u8::MAX as u128 {
            w.write_u8(b"pushint.tag", if neg { OP_PUSHINT8_NEG } else { OP_PUSHINT8_POS })?;
            return w.write_u8(b"pushint8", v as u8);
        }
        if v <= u16::MAX as u128 {
            w.write_u8(b"pushint.tag", if neg { OP_PUSHINT16_NEG } else { OP_PUSHINT16_POS })?;
            return w.write(b"pushint16", &(v as u16).to_le_bytes());
        }
        if v <= u64::MAX as u128 {
            w.write_u8(b"pushint.tag", if neg { OP_PUSHINT64_NEG } else { OP_PUSHINT64_POS })?;
            return w.write(b"pushint64", &(v as u64).to_le_bytes());
        }
        // 16-byte fits but not 8.
        w.write_u8(b"pushint.tag", if neg { OP_PUSHINT128_NEG } else { OP_PUSHINT128_POS })?;
        return w.write(b"pushint128", &v.to_le_bytes());
    }

    // Else: pushint full (32-byte sign-magnitude form).
    w.write_u8(b"pushint.tag", OP_PUSHINT_FULL)?;
    w.write(b"pushint.full", &bytes)
}

/// Reads `width_bytes` little-endian bytes from `reader` as the
/// magnitude of a pushint{N} instruction; combines with `negative` to
/// produce an `Int253`. Returns `VMError::InvalidInt253Encoding` if
/// the scalar bytes don't form a canonical scalar (only possible at
/// width=16 with a magnitude exceeding ℓ — impossible in practice for
/// any non-malicious encoder but worth guarding).
fn parse_pushint_n(
    reader: &mut impl Reader,
    width_bytes: usize,
    negative: bool,
) -> Result<Instruction, VMError> {
    debug_assert!(width_bytes <= 16);
    let mut buf = [0u8; 16];
    reader
        .read(&mut buf[..width_bytes])
        .map_err(|_| VMError::UnexpectedEndOfScript)?;
    let mag = u128::from_le_bytes(buf);
    let mut scalar_bytes = [0u8; 32];
    scalar_bytes[..16].copy_from_slice(&mag.to_le_bytes());
    let scalar = Option::<Scalar>::from(Scalar::from_canonical_bytes(scalar_bytes))
        .ok_or(VMError::InvalidInt253Encoding)?;
    Ok(Instruction::PushInt(Int253::from_parts(negative, scalar)))
}

fn parse_pushint_full(reader: &mut impl Reader) -> Result<Instruction, VMError> {
    let mut buf = [0u8; 32];
    reader
        .read(&mut buf)
        .map_err(|_| VMError::UnexpectedEndOfScript)?;
    let int = Int253::from_bytes(buf).ok_or(VMError::InvalidInt253Encoding)?;
    Ok(Instruction::PushInt(int))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_witness_is_discarded_in_bytecode() {
        let buf = Instruction::Alloc(Some(Int253::from(42u64))).encode_to_vec();
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
        // 0x4f remains unassigned (0x4e is keccak256, 0x50 is abs).
        // 0xf2..=0xf7 are reserved for the planned chain-info family
        // (height/blockhash/blockburn/blockweight/blockrate/chainstate).
        // 0xff is a sentinel "definitely unassigned" byte for fuzzing
        // future extensions.
        let unused = [0x4f, 0xf2, 0xff];
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
