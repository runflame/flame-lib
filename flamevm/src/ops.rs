//! Definition of all instructions in FlameVM,
//! their codes and decoding/encoding utility functions.

use curve25519_dalek::scalar::Scalar;
use readerwriter::Reader;

use crate::errors::VMError;
use crate::int253::Int253;
use crate::string::String;

// ── Opcode bytes ─────────────────────────────────────────────────────

const OP_PUSH_SMALL_MAX: u8 = 0x0f;
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
const OP_DUPK_BASE: u8 = 0x20;
const OP_DUPK_MAX: u8 = 0x2f;
const OP_ROLLK_BASE: u8 = 0x30;
const OP_ROLLK_MAX: u8 = 0x3f;
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
const OP_KECCAK256: u8 = 0x4e;
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
const OP_SCALAR: u8 = 0x5a;
const OP_COMMIT: u8 = 0x5b;
const OP_ALLOC: u8 = 0x5c;
const OP_EXPR: u8 = 0x5d;
const OP_RANGE: u8 = 0x5e;
const OP_SIZE: u8 = 0x5f;
const OP_DICT: u8 = 0x60;
const OP_PUT: u8 = 0x61;
const OP_REPLACE: u8 = 0x62;
const OP_GET: u8 = 0x63;
const OP_GETOPT: u8 = 0x64;
const OP_GETDUP: u8 = 0x65;
const OP_FIRST: u8 = 0x66;
const OP_LAST: u8 = 0x67;
const OP_NEXT: u8 = 0x68;
const OP_MERLIN: u8 = 0x69;
const OP_MERLINWRITE: u8 = 0x6a;
const OP_MERLINREAD: u8 = 0x6b;
const OP_SHA256: u8 = 0x6c;
const OP_SHA512: u8 = 0x6d;
const OP_SHA3: u8 = 0x6e;
const OP_LOG: u8 = 0x6f;
const OP_AMOUNT: u8 = 0x70;
const OP_ISSUE: u8 = 0x71;
const OP_RETIRE: u8 = 0x72;
const OP_BORROW: u8 = 0x73;
const OP_MERGE: u8 = 0x74;
const OP_SPLIT: u8 = 0x75;
const OP_MIX: u8 = 0x76;
const OP_DECRYPT: u8 = 0x77;
const OP_ISSUEFLV: u8 = 0x78;
const OP_VERIFY: u8 = 0x79;
const OP_FEE: u8 = 0x7a;
const OP_RUN: u8 = 0x7b;
const OP_LOOP: u8 = 0x7c;
const OP_SWITCH: u8 = 0x7d;
const OP_RETURN: u8 = 0x7e;
const OP_TYPE: u8 = 0x7f;
const OP_BREAKK_BASE: u8 = 0x80;
const OP_BREAKK_MAX: u8 = 0x8f;
const OP_INPUT: u8 = 0x90;
const OP_CELL: u8 = 0x91;
const OP_OUTPUT: u8 = 0x92;
const OP_OPEN: u8 = 0x93;
const OP_SEND: u8 = 0x94;
const OP_CALL: u8 = 0x95;
const OP_LOAD: u8 = 0x96;
const OP_SAVE: u8 = 0x97;
const OP_SIGNTX: u8 = 0x98;
const OP_SIGNCALL: u8 = 0x99;
const OP_ACTORID: u8 = 0x9c;
const OP_ANCHOR: u8 = 0x9d;
const OP_CALLERID: u8 = 0xa0;
const OP_METHOD: u8 = 0xa1;

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
    Merlin,                // label merlin → merlin
    MerlinWrite,           // m label s merlinwrite → m
    MerlinRead,            // m label n merlinread → m s
    Sha256,                // s sha256 → x
    Sha512,                // s sha512 → x
    Sha3,                  // s sha3 → x
    Log,                   // s log → ø
    Amount,                // t amount → t qty flv
    Issue,                 // qty tag issue → T
    Retire,                // t retire → ø
    Borrow,                // qty flv borrow → -T +T
    Merge,                 // a b merge → {c 1 | a b 0}
    Split,                 // a q split → a' b
    Mix,                   // tokens… cmts… m n mix → tokens
    Decrypt,               // T f' f q' q decrypt → CT
    IssueFlv,              // cid tag issueflv → int
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
    Actorid,               // ø actorid → s
    Anchor,                // ø anchor → s
    Callerid,              // ø callerid → s
    Method,                // ø method → int
    Ext(u8),               // unknown opcode byte; produced by the parser for any unassigned tag
}

impl Instruction {
    /// Appends this instruction's canonical bytecode to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Instruction::PushInt(i) => encode_push_int(i, out),
            Instruction::PushStr(s) => {
                out.push(OP_PUSHSTR);
                encode_sub_varint(s.len() as u64, out);
                // Use `bytes_view` so witness-bearing variants
                // (Commitment / Scalar / Predicate) serialize to their
                // canonical opaque bytes. `as_bytes` would panic for
                // those — the verifier's wire form must match
                // regardless of which variant the prover used.
                out.extend_from_slice(&s.bytes_view());
            }
            Instruction::PushPoint(p) => {
                out.push(OP_PUSHPOINT);
                // Always serializes the canonical 32-byte form;
                // witness variants compute their compressed point.
                out.extend_from_slice(&p.to_bytes());
            }
            Instruction::PushToken => out.push(OP_PUSHTOKEN),
            Instruction::Drop => out.push(OP_DROP),
            Instruction::Nop => out.push(OP_NOP),
            Instruction::Dup => out.push(OP_DUP),
            Instruction::Roll => out.push(OP_ROLL),
            Instruction::DupK(k) => {
                debug_assert!(*k <= 0x0f, "DupK arg must fit in 4 bits");
                out.push(OP_DUPK_BASE + (*k & 0x0f));
            }
            Instruction::RollK(k) => {
                debug_assert!(*k <= 0x0f, "RollK arg must fit in 4 bits");
                out.push(OP_ROLLK_BASE + (*k & 0x0f));
            }
            Instruction::ReadBits => out.push(OP_READBITS),
            Instruction::ReadInt => out.push(OP_READINT),
            Instruction::ReadStr => out.push(OP_READSTR),
            Instruction::ReadPoint => out.push(OP_READPOINT),
            Instruction::WriteBits => out.push(OP_WRITEBITS),
            Instruction::WriteInt => out.push(OP_WRITEINT),
            Instruction::Append => out.push(OP_APPEND),
            Instruction::WriteZeros => out.push(OP_WRITEZEROS),
            Instruction::BitNot => out.push(OP_BITNOT),
            Instruction::BitOr => out.push(OP_BITOR),
            Instruction::BitAnd => out.push(OP_BITAND),
            Instruction::BitXor => out.push(OP_BITXOR),
            Instruction::ShiftLeft => out.push(OP_SHIFTLEFT),
            Instruction::ShiftRight => out.push(OP_SHIFTRIGHT),
            Instruction::Keccak256 => out.push(OP_KECCAK256),
            Instruction::Abs => out.push(OP_ABS),
            Instruction::Eq => out.push(OP_EQ),
            Instruction::Neg => out.push(OP_NEG),
            Instruction::Add => out.push(OP_ADD),
            Instruction::Mul => out.push(OP_MUL),
            Instruction::DivMod => out.push(OP_DIVMOD),
            Instruction::Mod252 => out.push(OP_MOD252),
            Instruction::Not => out.push(OP_NOT),
            Instruction::And => out.push(OP_AND),
            Instruction::Or => out.push(OP_OR),
            Instruction::Size => out.push(OP_SIZE),
            Instruction::Scalar => out.push(OP_SCALAR),
            Instruction::Commit => out.push(OP_COMMIT),
            Instruction::Alloc(_) => out.push(OP_ALLOC),
            Instruction::Expr => out.push(OP_EXPR),
            Instruction::Range => out.push(OP_RANGE),
            Instruction::Dict => out.push(OP_DICT),
            Instruction::Put => out.push(OP_PUT),
            Instruction::Replace => out.push(OP_REPLACE),
            Instruction::Get => out.push(OP_GET),
            Instruction::GetOpt => out.push(OP_GETOPT),
            Instruction::GetDup => out.push(OP_GETDUP),
            Instruction::First => out.push(OP_FIRST),
            Instruction::Last => out.push(OP_LAST),
            Instruction::Next => out.push(OP_NEXT),
            Instruction::Merlin => out.push(OP_MERLIN),
            Instruction::MerlinWrite => out.push(OP_MERLINWRITE),
            Instruction::MerlinRead => out.push(OP_MERLINREAD),
            Instruction::Sha256 => out.push(OP_SHA256),
            Instruction::Sha512 => out.push(OP_SHA512),
            Instruction::Sha3 => out.push(OP_SHA3),
            Instruction::Log => out.push(OP_LOG),
            Instruction::Amount => out.push(OP_AMOUNT),
            Instruction::Issue => out.push(OP_ISSUE),
            Instruction::Retire => out.push(OP_RETIRE),
            Instruction::Borrow => out.push(OP_BORROW),
            Instruction::Merge => out.push(OP_MERGE),
            Instruction::Split => out.push(OP_SPLIT),
            Instruction::Mix => out.push(OP_MIX),
            Instruction::Decrypt => out.push(OP_DECRYPT),
            Instruction::IssueFlv => out.push(OP_ISSUEFLV),
            Instruction::Verify => out.push(OP_VERIFY),
            Instruction::Fee => out.push(OP_FEE),
            Instruction::Run => out.push(OP_RUN),
            Instruction::Loop => out.push(OP_LOOP),
            Instruction::Switch => out.push(OP_SWITCH),
            Instruction::Return => out.push(OP_RETURN),
            Instruction::Type => out.push(OP_TYPE),
            Instruction::BreakK(k) => {
                debug_assert!(*k <= 0x0f, "BreakK arg must fit in 4 bits");
                out.push(OP_BREAKK_BASE + (*k & 0x0f));
            }
            // Witness (if any) never crosses the wire — prover-side
            // only. Encoded form is the bare opcode byte.
            Instruction::Input => out.push(OP_INPUT),
            Instruction::Cell => out.push(OP_CELL),
            Instruction::Output => out.push(OP_OUTPUT),
            Instruction::Open => out.push(OP_OPEN),
            Instruction::Send => out.push(OP_SEND),
            Instruction::Call => out.push(OP_CALL),
            Instruction::Load => out.push(OP_LOAD),
            Instruction::Save => out.push(OP_SAVE),
            Instruction::Signtx => out.push(OP_SIGNTX),
            Instruction::Signcall => out.push(OP_SIGNCALL),
            Instruction::Actorid => out.push(OP_ACTORID),
            Instruction::Anchor => out.push(OP_ANCHOR),
            Instruction::Callerid => out.push(OP_CALLERID),
            Instruction::Method => out.push(OP_METHOD),
            Instruction::Ext(b) => out.push(*b),
        }
    }

    /// Reads exactly one Instruction (opcode + inline parameter bytes)
    /// from `reader`. Errors:
    ///
    /// - `VMError::UnexpectedEndOfScript` — reader ran out of bytes.
    /// - `VMError::InvalidInt253Encoding` — a `pushint` payload's
    ///   magnitude isn't a canonical Ristretto scalar, or a `pushint`
    ///   full encoded negative zero.
    ///
    /// Unknown opcode bytes return `Instruction::Ext(b)` rather than
    /// erroring — mirrors zkvm's extension-opcode handling so future
    /// protocol versions can introduce new opcodes without breaking
    /// older verifiers.
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
                let len = read_sub_varint(reader)? as usize;
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
            OP_MERLIN => Ok(Instruction::Merlin),
            OP_MERLINWRITE => Ok(Instruction::MerlinWrite),
            OP_MERLINREAD => Ok(Instruction::MerlinRead),
            OP_SHA256 => Ok(Instruction::Sha256),
            OP_SHA512 => Ok(Instruction::Sha512),
            OP_SHA3 => Ok(Instruction::Sha3),
            OP_LOG => Ok(Instruction::Log),
            OP_AMOUNT => Ok(Instruction::Amount),
            OP_ISSUE => Ok(Instruction::Issue),
            OP_RETIRE => Ok(Instruction::Retire),
            OP_BORROW => Ok(Instruction::Borrow),
            OP_MERGE => Ok(Instruction::Merge),
            OP_SPLIT => Ok(Instruction::Split),
            OP_MIX => Ok(Instruction::Mix),
            OP_DECRYPT => Ok(Instruction::Decrypt),
            OP_ISSUEFLV => Ok(Instruction::IssueFlv),
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
            OP_ACTORID => Ok(Instruction::Actorid),
            OP_ANCHOR => Ok(Instruction::Anchor),
            OP_CALLERID => Ok(Instruction::Callerid),
            OP_METHOD => Ok(Instruction::Method),
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
fn encode_push_int(i: &Int253, out: &mut Vec<u8>) {
    let bytes = i.to_bytes();
    let neg = i.is_negative();
    let mag_scalar = i.abs_scalar();
    let mag_bytes = mag_scalar.to_bytes();

    // Try push:k (only for 0..=15 and non-negative).
    if !neg {
        // Magnitude fits in u64 and ≤ 15?
        if mag_bytes[8..].iter().all(|&b| b == 0) {
            let mut lo = [0u8; 8];
            lo.copy_from_slice(&mag_bytes[..8]);
            let v = u64::from_le_bytes(lo);
            if v <= OP_PUSH_SMALL_MAX as u64 {
                out.push(v as u8);
                return;
            }
        }
    }

    // Try pushint8 / 16 / 64 / 128 if magnitude fits.
    if mag_bytes[16..].iter().all(|&b| b == 0) {
        // Magnitude fits in u128.
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&mag_bytes[..16]);
        let v = u128::from_le_bytes(buf);
        // Pick smallest width that holds v.
        if v <= u8::MAX as u128 {
            out.push(if neg { OP_PUSHINT8_NEG } else { OP_PUSHINT8_POS });
            out.push(v as u8);
            return;
        }
        if v <= u16::MAX as u128 {
            out.push(if neg { OP_PUSHINT16_NEG } else { OP_PUSHINT16_POS });
            out.extend_from_slice(&(v as u16).to_le_bytes());
            return;
        }
        if v <= u64::MAX as u128 {
            out.push(if neg { OP_PUSHINT64_NEG } else { OP_PUSHINT64_POS });
            out.extend_from_slice(&(v as u64).to_le_bytes());
            return;
        }
        // 16-byte fits but not 8.
        out.push(if neg { OP_PUSHINT128_NEG } else { OP_PUSHINT128_POS });
        out.extend_from_slice(&v.to_le_bytes());
        return;
    }

    // Else: pushint full (32-byte sign-magnitude form).
    out.push(OP_PUSHINT_FULL);
    out.extend_from_slice(&bytes);
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
    let scalar = Scalar::from_canonical_bytes(scalar_bytes)
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

/// Sub-varint encoder matching the bytecode dispatcher's `read_sub_varint` semantics.
fn encode_sub_varint(value: u64, out: &mut Vec<u8>) {
    const SUBVAR_U16_BASE: u64 = 256;
    const SUBVAR_U32_BASE: u64 = 65_792;
    const SUBVAR_U64_BASE: u64 = 4_295_033_088;
    if value <= 255 {
        out.push(0); // tag
        out.push(value as u8);
    } else if value < SUBVAR_U32_BASE {
        out.push(1); // tag
        out.extend_from_slice(&((value - SUBVAR_U16_BASE) as u16).to_le_bytes());
    } else if value < SUBVAR_U64_BASE {
        out.push(2); // tag
        out.extend_from_slice(&((value - SUBVAR_U32_BASE) as u32).to_le_bytes());
    } else {
        out.push(3); // tag
        out.extend_from_slice(&value.wrapping_sub(SUBVAR_U64_BASE).to_le_bytes());
    }
}

/// Sub-varint decoder matching the bytecode dispatcher's `read_sub_varint`
/// semantics.
fn read_sub_varint(reader: &mut impl Reader) -> Result<u64, VMError> {
    const SUBVAR_U16_BASE: u64 = 256;
    const SUBVAR_U32_BASE: u64 = 65_792;
    const SUBVAR_U64_BASE: u64 = 4_295_033_088;
    let tag = reader.read_u8().map_err(|_| VMError::UnexpectedEndOfScript)?;
    match tag {
        0 => Ok(reader.read_u8().map_err(|_| VMError::UnexpectedEndOfScript)? as u64),
        1 => {
            let mut buf = [0u8; 2];
            reader.read(&mut buf).map_err(|_| VMError::UnexpectedEndOfScript)?;
            Ok(SUBVAR_U16_BASE + u16::from_le_bytes(buf) as u64)
        }
        2 => {
            let mut buf = [0u8; 4];
            reader.read(&mut buf).map_err(|_| VMError::UnexpectedEndOfScript)?;
            Ok(SUBVAR_U32_BASE + u32::from_le_bytes(buf) as u64)
        }
        3 => {
            let mut buf = [0u8; 8];
            reader.read(&mut buf).map_err(|_| VMError::UnexpectedEndOfScript)?;
            Ok(SUBVAR_U64_BASE.wrapping_add(u64::from_le_bytes(buf)))
        }
        _ => Err(VMError::UnexpectedEndOfScript),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(instr: Instruction) -> Instruction {
        let mut buf = Vec::new();
        instr.encode(&mut buf);
        let mut r: &[u8] = &buf;
        Instruction::parse(&mut r).expect("parses")
    }

    fn assert_pushint(orig: Int253) {
        let r = roundtrip(Instruction::PushInt(orig));
        match r {
            Instruction::PushInt(i) => assert_eq!(i, orig, "PushInt round-trip"),
            other => panic!("PushInt didn't round-trip: got {:?}", other),
        }
    }

    #[test]
    fn pushint_roundtrips_all_widths() {
        assert_pushint(Int253::from(0u64));
        assert_pushint(Int253::from(15u64));
        assert_pushint(Int253::from(16u64));
        assert_pushint(Int253::from(255u64));
        assert_pushint(Int253::from(256u64));
        assert_pushint(Int253::from(65_535u64));
        assert_pushint(Int253::from(65_536u64));
        assert_pushint(Int253::from(u64::MAX));
        assert_pushint(Int253::from(-1i64));
        assert_pushint(Int253::from(-255i64));
        assert_pushint(Int253::from(-256i64));
        assert_pushint(Int253::from(-65_536i64));
    }

    #[test]
    fn pushint_picks_smallest_width() {
        // 0..=15: 1 byte (push:k)
        let mut buf = Vec::new();
        Instruction::PushInt(Int253::from(7u64)).encode(&mut buf);
        assert_eq!(buf, vec![0x07]);

        // 16..=255: 2 bytes (pushint8)
        let mut buf = Vec::new();
        Instruction::PushInt(Int253::from(42u64)).encode(&mut buf);
        assert_eq!(buf, vec![0x10, 0x2a]);

        // Negative: pushint8 negative branch
        let mut buf = Vec::new();
        Instruction::PushInt(Int253::from(-42i64)).encode(&mut buf);
        assert_eq!(buf, vec![0x11, 0x2a]);

        // u64 max: 9 bytes (pushint64)
        let mut buf = Vec::new();
        Instruction::PushInt(Int253::from(u64::MAX)).encode(&mut buf);
        assert_eq!(buf.len(), 9);
        assert_eq!(buf[0], OP_PUSHINT64_POS);
    }

    #[test]
    fn pushstr_roundtrip() {
        let s = String::from(b"hello world".to_vec());
        let r = roundtrip(Instruction::PushStr(s.clone()));
        match r {
            Instruction::PushStr(s2) => assert_eq!(s2.as_bytes(), s.as_bytes()),
            other => panic!("PushStr didn't round-trip: got {:?}", other),
        }
    }

    #[test]
    fn pushpoint_roundtrip() {
        let pt = [0x42u8; 32];
        let r = roundtrip(Instruction::PushPoint(crate::crypto::Point::from_bytes(pt)));
        match r {
            Instruction::PushPoint(p) => assert_eq!(p.to_bytes(), pt),
            _ => panic!("PushPoint didn't round-trip"),
        }
    }

    #[test]
    fn dupk_rollk_breakk_roundtrip() {
        for k in 0..=15u8 {
            let r = roundtrip(Instruction::DupK(k));
            match r {
                Instruction::DupK(k2) => assert_eq!(k2, k),
                _ => panic!("DupK didn't round-trip k={}", k),
            }
            let r = roundtrip(Instruction::RollK(k));
            match r {
                Instruction::RollK(k2) => assert_eq!(k2, k),
                _ => panic!("RollK didn't round-trip k={}", k),
            }
            let r = roundtrip(Instruction::BreakK(k));
            match r {
                Instruction::BreakK(k2) => assert_eq!(k2, k),
                _ => panic!("BreakK didn't round-trip k={}", k),
            }
        }
    }

    #[test]
    fn alloc_witness_is_discarded_in_bytecode() {
        let mut buf = Vec::new();
        Instruction::Alloc(Some(Int253::from(42u64))).encode(&mut buf);
        assert_eq!(buf, vec![OP_ALLOC]);

        let mut r: &[u8] = &buf;
        let parsed = Instruction::parse(&mut r).expect("parses");
        match parsed {
            Instruction::Alloc(w) => assert_eq!(w, None),
            _ => panic!("Alloc parse failed"),
        }
    }

    #[test]
    fn zero_param_opcodes_roundtrip() {
        // Spot-check a representative zero-param variant per phase.
        let cases = [
            Instruction::Drop,
            Instruction::Nop,
            Instruction::Dup,
            Instruction::Roll,
            Instruction::ReadBits,
            Instruction::Append,
            Instruction::Keccak256,
            Instruction::Abs,
            Instruction::Add,
            Instruction::DivMod,
            Instruction::Size,
            Instruction::Expr,
            Instruction::Dict,
            Instruction::First,
            Instruction::Merlin,
            Instruction::Sha256,
            Instruction::Amount,
            Instruction::IssueFlv,
            Instruction::Verify,
            Instruction::Return,
            Instruction::Input,
            Instruction::Cell,
            Instruction::Signtx,
            Instruction::Signcall,
        ];
        for c in cases {
            let mut buf = Vec::new();
            c.encode(&mut buf);
            assert_eq!(buf.len(), 1, "zero-param opcode must encode to 1 byte");
            let mut r: &[u8] = &buf;
            let parsed = Instruction::parse(&mut r).expect("parses");
            // Use debug formatting to compare (Instruction lacks PartialEq).
            assert_eq!(
                format!("{:?}", parsed),
                format!("{:?}", c),
                "round-trip mismatch"
            );
        }
    }

    #[test]
    fn ext_opcode_for_unknown_bytes() {
        // 0x4f / 0x9a remain unassigned (0x6f is `log`, 0x7a is
        // `fee`). 0xff is a sentinel "definitely unassigned" byte
        // for fuzzing future extensions.
        let unused = [0x4f, 0x9a, 0xff];
        for b in unused {
            let mut r: &[u8] = &[b];
            let parsed = Instruction::parse(&mut r).expect("parses");
            match parsed {
                Instruction::Ext(x) => assert_eq!(x, b),
                _ => panic!("expected Ext({:#x})", b),
            }
        }
    }

    #[test]
    fn parse_pushstr_with_long_payload() {
        // Round-trip a string with length above the sub-varint
        // immediate range (>255 bytes).
        let payload = vec![0xab; 1000];
        let s = String::from(payload.clone());
        let r = roundtrip(Instruction::PushStr(s.clone()));
        match r {
            Instruction::PushStr(s2) => assert_eq!(s2.as_bytes(), payload.as_slice()),
            _ => panic!("PushStr long-payload round-trip failed"),
        }
    }
}
