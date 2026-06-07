//! Fluent `Program` builder + `ProgramItem` wire-form wrapper.

use std::collections::VecDeque;

use crate::errors::VMError;
use crate::int253::Int253;
use readerwriter::Encodable;

use crate::ops::Instruction;
use crate::string::String;

/// A program is a list of [`Instruction`]s. Build with the fluent
/// methods (`alloc`, `add`, `eq`, `verify`, …) and call `to_bytecode()`
/// / `to_witnesses()` to derive the prover/verifier views.
#[derive(Clone, Debug, Default)]
pub struct Program {
    instructions: Vec<Instruction>,
    /// Build-time only: active loop scopes for `build_break` /
    /// `build_continue`. Balanced (pushed/popped) by `build_loop` /
    /// `build_while`, so any finished program leaves this empty.
    loop_scopes: Vec<LoopScope>,
}

/// One enclosing loop during `build_*` construction: the back-edge
/// target and the forward exit-jumps (structural exit + `build_break`s)
/// awaiting backpatch to the loop's end label. See ADR 0015.
#[derive(Clone, Debug)]
struct LoopScope {
    top: u32,
    end_jumps: Vec<usize>,
}

impl Program {
    /// Constructs an empty program.
    pub fn new() -> Self {
        Self { instructions: Vec::new(), loop_scopes: Vec::new() }
    }

    /// Parses a bytecode slice into a Program. Witness-bearing
    /// instructions land as `Alloc(None)` etc. — useful for verifier
    /// inspection or for splicing existing bytecode into a fresh
    /// prover-authored Program.
    pub fn parse(bytes: &[u8]) -> Result<Self, VMError> {
        let mut r: &[u8] = bytes;
        let mut p = Self::new();
        while !r.is_empty() {
            let instr = Instruction::parse(&mut r)?;
            p.instructions.push(instr);
        }
        Ok(p)
    }

    /// Returns the underlying instruction slice.
    pub fn instructions(&self) -> &[Instruction] {
        &self.instructions
    }

    /// Consumes the Program and returns the underlying
    /// `Vec<Instruction>` — what the VM walks. The Run constructor
    /// takes this directly; both prover and verifier feed the VM
    /// through this single path.
    pub fn into_instructions(self) -> Vec<Instruction> {
        self.instructions
    }

    /// Appends an arbitrary `Instruction`. Used by the fluent builder
    /// methods and by callers that want to inject typed variants
    /// directly.
    pub fn push_instr(&mut self, instr: Instruction) -> &mut Self {
        self.instructions.push(instr);
        self
    }

    /// Appends a sequence of bytecode bytes by parsing them into
    /// Instructions and pushing each. Useful for composing programs
    /// from snippets. Errors if any byte sequence doesn't decode.
    pub fn raw_bytes(mut self, bytes: &[u8]) -> Result<Self, VMError> {
        let other = Self::parse(bytes)?;
        self.instructions.extend(other.instructions);
        Ok(self)
    }

    /// Serializes the program's bytecode (canonical; matches what the
    /// VM dispatcher expects).
    pub fn to_bytecode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for instr in &self.instructions {
            // `Vec<u8>` writer is infallible.
            instr.encode(&mut out).expect("Vec writer never fails");
        }
        out
    }

    /// Builds the witness queue in opcode order. Each witness-bearing
    /// instruction (currently only `Alloc`) contributes exactly one
    /// queue entry; other instructions contribute none.
    pub fn to_witnesses(&self) -> VecDeque<Option<Int253>> {
        self.instructions.iter().filter_map(|i| i.witness()).collect()
    }

    // ── stack literals & manipulation ───────────────────

    /// `push:k` / `pushint{8,16,64,128}` / `pushint` — encoder picks
    /// the narrowest opcode width.
    pub fn push_int<T: Into<Int253>>(mut self, v: T) -> Self {
        self.instructions.push(Instruction::PushInt(v.into()));
        self
    }

    /// `pushstr` (0x19).
    pub fn push_str<T: Into<String>>(mut self, s: T) -> Self {
        self.instructions.push(Instruction::PushStr(s.into()));
        self
    }

    /// `pushstr` (0x19) carrying a witness-bearing sub-script. The
    /// prover pushes the inner Program's instructions (witness
    /// slots intact) wrapped in `String::Script`; downstream
    /// `op_open` / `op_signcall` walk those
    /// instructions directly. Verifier-side bytecode encodes to
    /// the compiled bytes of `inner.to_bytecode()`, so both sides
    /// see the same wire form.
    pub fn push_script(mut self, inner: Program) -> Self {
        self.instructions.push(Instruction::PushStr(String::script(
            inner.into_instructions(),
        )));
        self
    }

    /// `pushpoint` (0x1a) from raw 32 bytes (the verifier-style
    /// `Point::Opaque`). For witness-bearing points use
    /// [`Program::push_point_typed`].
    pub fn push_point(mut self, bytes: [u8; 32]) -> Self {
        self.instructions
            .push(Instruction::PushPoint(crate::crypto::Point::from_bytes(bytes)));
        self
    }

    /// `pushpoint` (0x1a) with a typed `Point`. Use this on the prover
    /// side to attach a `Point::Commitment` / `Point::Predicate`
    /// witness; both encode to the canonical 32 bytes on the wire.
    pub fn push_point_typed(mut self, p: crate::crypto::Point) -> Self {
        self.instructions.push(Instruction::PushPoint(p));
        self
    }

    /// `pushtoken` (0x1b).
    pub fn pushtoken(mut self) -> Self {
        self.instructions.push(Instruction::PushToken);
        self
    }

    /// `drop` (0x1c).
    pub fn drop_(mut self) -> Self {
        self.instructions.push(Instruction::Drop);
        self
    }

    /// `nop` (0x1d).
    pub fn nop(mut self) -> Self {
        self.instructions.push(Instruction::Nop);
        self
    }

    /// `dup` (0x1e) — pops `k` from the stack.
    pub fn dup(mut self) -> Self {
        self.instructions.push(Instruction::Dup);
        self
    }

    /// `roll` (0x1f).
    pub fn roll(mut self) -> Self {
        self.instructions.push(Instruction::Roll);
        self
    }

    /// `dup:k` (0x20..=0x2f) — `k ≤ 15`.
    pub fn dup_k(mut self, k: u8) -> Self {
        self.instructions.push(Instruction::DupK(k));
        self
    }

    /// `roll:k` (0x30..=0x3f) — `k ≤ 15`.
    pub fn roll_k(mut self, k: u8) -> Self {
        self.instructions.push(Instruction::RollK(k));
        self
    }

    // ── String ops ──────────────────────────────────────

    pub fn read_bits(mut self) -> Self { self.instructions.push(Instruction::ReadBits); self }
    pub fn read_int(mut self) -> Self { self.instructions.push(Instruction::ReadInt); self }
    pub fn read_str(mut self) -> Self { self.instructions.push(Instruction::ReadStr); self }
    pub fn read_point(mut self) -> Self { self.instructions.push(Instruction::ReadPoint); self }
    pub fn write_bits(mut self) -> Self { self.instructions.push(Instruction::WriteBits); self }
    pub fn write_int(mut self) -> Self { self.instructions.push(Instruction::WriteInt); self }
    pub fn append(mut self) -> Self { self.instructions.push(Instruction::Append); self }
    pub fn write_zeros(mut self) -> Self { self.instructions.push(Instruction::WriteZeros); self }
    pub fn bit_not(mut self) -> Self { self.instructions.push(Instruction::BitNot); self }
    pub fn bit_or(mut self) -> Self { self.instructions.push(Instruction::BitOr); self }
    pub fn bit_and(mut self) -> Self { self.instructions.push(Instruction::BitAnd); self }
    pub fn bit_xor(mut self) -> Self { self.instructions.push(Instruction::BitXor); self }
    pub fn shift_left(mut self) -> Self { self.instructions.push(Instruction::ShiftLeft); self }
    pub fn shift_right(mut self) -> Self { self.instructions.push(Instruction::ShiftRight); self }

    // ── Int253 arithmetic ───────────────────────────────

    pub fn abs(mut self) -> Self { self.instructions.push(Instruction::Abs); self }
    pub fn eq(mut self) -> Self { self.instructions.push(Instruction::Eq); self }
    pub fn neg(mut self) -> Self { self.instructions.push(Instruction::Neg); self }
    pub fn add(mut self) -> Self { self.instructions.push(Instruction::Add); self }
    pub fn mul(mut self) -> Self { self.instructions.push(Instruction::Mul); self }
    pub fn divmod(mut self) -> Self { self.instructions.push(Instruction::DivMod); self }
    pub fn mod252(mut self) -> Self { self.instructions.push(Instruction::Mod252); self }
    pub fn not(mut self) -> Self { self.instructions.push(Instruction::Not); self }
    pub fn and(mut self) -> Self { self.instructions.push(Instruction::And); self }
    pub fn or(mut self) -> Self { self.instructions.push(Instruction::Or); self }
    pub fn size(mut self) -> Self { self.instructions.push(Instruction::Size); self }

    // ── CS opcodes ─────────────────────────────────────

    /// `alloc` (0x5c) — allocates a low-level R1CS variable. `witness`
    /// = `Some(int)` on the prover side (fills the cleartext value
    /// the constraint system uses), `None` on the verifier side.
    pub fn alloc(mut self, witness: Option<Int253>) -> Self {
        self.instructions.push(Instruction::Alloc(witness));
        self
    }

    /// `expr` (0x5d).
    pub fn expr(mut self) -> Self {
        self.instructions.push(Instruction::Expr);
        self
    }

    /// `range` (0x5e) — `expr n → expr`. Adds an n-bit range proof
    /// (n ∈ [1, 64]; popped as `Int253` from the stack). The
    /// Expression is consumed and pushed back unchanged.
    pub fn range(mut self) -> Self {
        self.instructions.push(Instruction::Range);
        self
    }

    /// `scalar` (0x5a) — `string → expr`. Lifts a 32-byte String
    /// (parsed as `Int253`) into a constant Expression.
    pub fn scalar(mut self) -> Self {
        self.instructions.push(Instruction::Scalar);
        self
    }

    /// `commit` (0x5b) — `string → var`. Promotes a 32-byte String
    /// (parsed as a Pedersen commitment) into a Variable.
    pub fn commit(mut self) -> Self {
        self.instructions.push(Instruction::Commit);
        self
    }

    // ── Dict ops ────────────────────────────────────────

    pub fn dict(mut self) -> Self { self.instructions.push(Instruction::Dict); self }
    pub fn put(mut self) -> Self { self.instructions.push(Instruction::Put); self }
    pub fn replace(mut self) -> Self { self.instructions.push(Instruction::Replace); self }
    pub fn get(mut self) -> Self { self.instructions.push(Instruction::Get); self }
    pub fn get_opt(mut self) -> Self { self.instructions.push(Instruction::GetOpt); self }
    pub fn get_dup(mut self) -> Self { self.instructions.push(Instruction::GetDup); self }
    pub fn first(mut self) -> Self { self.instructions.push(Instruction::First); self }
    pub fn last(mut self) -> Self { self.instructions.push(Instruction::Last); self }
    pub fn next(mut self) -> Self { self.instructions.push(Instruction::Next); self }

    // ── Cryptography ────────────────────────────────────

    /// `transcript` (0x80) — open a fresh Merlin transcript.
    pub fn transcript(mut self) -> Self { self.instructions.push(Instruction::Transcript); self }
    /// `twrite` (0x81) — absorb `(label, data)` into the transcript.
    pub fn twrite(mut self) -> Self { self.instructions.push(Instruction::TWrite); self }
    /// `tread` (0x82) — squeeze `n` challenge bytes under `label`.
    pub fn tread(mut self) -> Self { self.instructions.push(Instruction::TRead); self }
    pub fn sha256(mut self) -> Self { self.instructions.push(Instruction::Sha256); self }
    pub fn sha512(mut self) -> Self { self.instructions.push(Instruction::Sha512); self }
    pub fn sha3(mut self) -> Self { self.instructions.push(Instruction::Sha3); self }
    pub fn keccak256(mut self) -> Self { self.instructions.push(Instruction::Keccak256); self }

    /// `log` (0x87) — see [`Instruction::Log`].
    pub fn log(mut self) -> Self {
        self.instructions.push(Instruction::Log);
        self
    }

    // ── Tokens ──────────────────────────────────────────

    pub fn amount(mut self) -> Self { self.instructions.push(Instruction::Amount); self }
    /// `issuepriv` (0x91) — confidential mint under the enclosing
    /// predicate. See [`Instruction::IssuePriv`].
    pub fn issuepriv(mut self) -> Self { self.instructions.push(Instruction::IssuePriv); self }
    /// `issueprivflv` (0x92) — consumer-side flavor helper for
    /// `issuepriv`. See [`Instruction::IssuePrivFlv`].
    pub fn issueprivflv(mut self) -> Self { self.instructions.push(Instruction::IssuePrivFlv); self }
    /// `issuepub` (0x93) — cleartext mint under the enclosing actor.
    /// See [`Instruction::IssuePub`].
    pub fn issuepub(mut self) -> Self { self.instructions.push(Instruction::IssuePub); self }
    /// `issuepubflv` (0x94) — consumer-side flavor helper for
    /// `issuepub` (renamed from `issueflv`). See [`Instruction::IssuePubFlv`].
    pub fn issuepubflv(mut self) -> Self { self.instructions.push(Instruction::IssuePubFlv); self }
    pub fn retire(mut self) -> Self { self.instructions.push(Instruction::Retire); self }
    pub fn borrow(mut self) -> Self { self.instructions.push(Instruction::Borrow); self }
    pub fn merge(mut self) -> Self { self.instructions.push(Instruction::Merge); self }
    pub fn split(mut self) -> Self { self.instructions.push(Instruction::Split); self }

    /// `mix` (0x99) — see [`Instruction::Mix`].
    pub fn mix(mut self) -> Self {
        self.instructions.push(Instruction::Mix);
        self
    }

    /// `decrypt` (0x9a) — see [`Instruction::Decrypt`].
    pub fn decrypt(mut self) -> Self {
        self.instructions.push(Instruction::Decrypt);
        self
    }

    // ── control flow ────────────────────────────────────

    pub fn verify(mut self) -> Self { self.instructions.push(Instruction::Verify); self }

    /// `fee` (0x9b) — external-only.
    pub fn fee(mut self) -> Self { self.instructions.push(Instruction::Fee); self }

    /// `label:n` (0xa1) — low-level marker. Prefer the `build_*`
    /// combinators, which number labels in appearance order for you.
    pub fn label(mut self, n: u32) -> Self {
        self.instructions.push(Instruction::Label(n));
        self
    }

    /// `jump:n` (0xa2) — unconditional jump to label `n`. Low-level.
    pub fn jump(mut self, n: u32) -> Self {
        self.instructions.push(Instruction::Jump(n));
        self
    }

    /// `jumpif:n` (0xa3) — pop an Int253; jump to label `n` iff non-zero.
    pub fn jumpif(mut self, n: u32) -> Self {
        self.instructions.push(Instruction::JumpIf(n));
        self
    }

    /// `return` (0xa4). Method named `return_` because `return` is a Rust keyword.
    pub fn return_(mut self) -> Self { self.instructions.push(Instruction::Return); self }

    pub fn type_(mut self) -> Self { self.instructions.push(Instruction::Type); self }

    // ── structured control-flow combinators (ADR 0015) ──────
    // Emit `label`/`jump`/`jumpif` with appearance-order label numbers
    // and forward-jump backpatching, so callers never compute label
    // numbers by hand.

    /// Next label number = count of labels already emitted (labels are
    /// numbered in appearance order).
    fn next_label(&self) -> u32 {
        self.instructions
            .iter()
            .filter(|i| matches!(i, Instruction::Label(_)))
            .count() as u32
    }

    /// Emits `label` with the next sequential number; returns it.
    fn emit_label(&mut self) -> u32 {
        let n = self.next_label();
        self.instructions.push(Instruction::Label(n));
        n
    }

    /// Pushes a placeholder forward jump; returns its index for backpatch.
    fn push_jump_placeholder(&mut self, conditional: bool) -> usize {
        let idx = self.instructions.len();
        self.instructions.push(if conditional {
            Instruction::JumpIf(0)
        } else {
            Instruction::Jump(0)
        });
        idx
    }

    /// Fills a placeholder jump (at `idx`) with its resolved label number,
    /// preserving conditional-vs-unconditional.
    fn backpatch(&mut self, idx: usize, target: u32) {
        self.instructions[idx] = match self.instructions[idx] {
            Instruction::JumpIf(_) => Instruction::JumpIf(target),
            _ => Instruction::Jump(target),
        };
    }

    /// Emits `label END` and backpatches every recorded exit-jump
    /// (structural exit + breaks) to it. Pops the loop scope.
    fn close_loop_scope(&mut self) {
        let scope = self.loop_scopes.pop().expect("close_loop_scope: no scope");
        let n_end = self.emit_label();
        for idx in scope.end_jumps {
            self.backpatch(idx, n_end);
        }
    }

    /// `if (cond) { then }` — `cond` is whatever the preceding builder
    /// calls left on the stack (Int253; non-zero = true).
    pub fn build_if(self, then: impl FnOnce(Self) -> Self) -> Self {
        self.build_if_else(then, |p| p)
    }

    /// `if (cond) { then } else { els }`. Compiles to
    /// `jumpif THEN; <els>; jump END; label THEN; <then>; label END`.
    pub fn build_if_else(
        mut self,
        then: impl FnOnce(Self) -> Self,
        els: impl FnOnce(Self) -> Self,
    ) -> Self {
        let j_then = self.push_jump_placeholder(true);
        self = els(self);
        let j_end = self.push_jump_placeholder(false);
        let n_then = self.emit_label();
        self.backpatch(j_then, n_then);
        self = then(self);
        let n_end = self.emit_label();
        self.backpatch(j_end, n_end);
        self
    }

    /// `while (cond) { body }`. `cond` re-emits the condition each
    /// iteration. Supports `build_break` / `build_continue`. Compiles to
    /// `label TOP; <cond>; jumpif BODY; jump END; label BODY; <body>;
    ///  jump TOP; label END`.
    pub fn build_while(
        mut self,
        cond: impl FnOnce(Self) -> Self,
        body: impl FnOnce(Self) -> Self,
    ) -> Self {
        let top = self.emit_label();
        self.loop_scopes.push(LoopScope { top, end_jumps: Vec::new() });
        self = cond(self);
        let j_body = self.push_jump_placeholder(true);
        let j_end = self.push_jump_placeholder(false);
        self.loop_scopes.last_mut().expect("while scope").end_jumps.push(j_end);
        let n_body = self.emit_label();
        self.backpatch(j_body, n_body);
        self = body(self);
        self = self.jump(top);
        self.close_loop_scope();
        self
    }

    /// `loop { body }` — infinite; exit via `build_break`. Compiles to
    /// `label TOP; <body>; jump TOP; label END`.
    pub fn build_loop(mut self, body: impl FnOnce(Self) -> Self) -> Self {
        let top = self.emit_label();
        self.loop_scopes.push(LoopScope { top, end_jumps: Vec::new() });
        self = body(self);
        self = self.jump(top);
        self.close_loop_scope();
        self
    }

    /// `break` — jump to the innermost enclosing loop's end.
    pub fn build_break(mut self) -> Self {
        let idx = self.push_jump_placeholder(false);
        self.loop_scopes
            .last_mut()
            .expect("build_break outside a loop")
            .end_jumps
            .push(idx);
        self
    }

    /// `continue` — jump to the innermost enclosing loop's top.
    pub fn build_continue(self) -> Self {
        let top = self.loop_scopes.last().expect("build_continue outside a loop").top;
        self.jump(top)
    }

    // ── Cell + I/O ───────────────────────────────────

    /// `input` (0x90). Witness data (open commitments on Token
    /// payloads) rides on the pushed String value — call
    /// `push_str(String::cell(c))` before this on the prover side;
    /// verifiers push `String::Opaque(cell.to_bytes())`.
    pub fn input(mut self) -> Self {
        self.instructions.push(Instruction::Input);
        self
    }
    pub fn cell(mut self) -> Self { self.instructions.push(Instruction::Cell); self }
    pub fn output(mut self) -> Self { self.instructions.push(Instruction::Output); self }
    pub fn open(mut self) -> Self { self.instructions.push(Instruction::Open); self }
    pub fn signtx(mut self) -> Self { self.instructions.push(Instruction::Signtx); self }
    pub fn signcall(mut self) -> Self { self.instructions.push(Instruction::Signcall); self }

    // ── Actor invocation + state (0x94..=0x97) ─────────

    pub fn send(mut self) -> Self { self.instructions.push(Instruction::Send); self }
    pub fn call(mut self) -> Self { self.instructions.push(Instruction::Call); self }
    pub fn load(mut self) -> Self { self.instructions.push(Instruction::Load); self }
    pub fn save(mut self) -> Self { self.instructions.push(Instruction::Save); self }

    // ── Tx-level & frame introspection (0xe0..=0xf1) ───

    pub fn timelock(mut self) -> Self { self.instructions.push(Instruction::Timelock); self }
    pub fn version(mut self) -> Self { self.instructions.push(Instruction::Version); self }
    pub fn selfid(mut self) -> Self { self.instructions.push(Instruction::Selfid); self }
    pub fn anchor(mut self) -> Self { self.instructions.push(Instruction::Anchor); self }
    pub fn gas(mut self) -> Self { self.instructions.push(Instruction::Gas); self }
    pub fn bytes(mut self) -> Self { self.instructions.push(Instruction::Bytes); self }
    pub fn callerid(mut self) -> Self { self.instructions.push(Instruction::Callerid); self }
    pub fn method(mut self) -> Self { self.instructions.push(Instruction::Method); self }
    pub fn gaslimit(mut self) -> Self { self.instructions.push(Instruction::Gaslimit); self }
    pub fn memlimit(mut self) -> Self { self.instructions.push(Instruction::Memlimit); self }
    pub fn newbytes(mut self) -> Self { self.instructions.push(Instruction::Newbytes); self }
}

// ── ProgramItem ─────────────────────────────────────────────────────

/// A view of a program:
///
/// - `Bytecode(Vec<u8>)` — verifier's view (opaque bytes).
/// - `Program(Program)` — prover's view (typed instructions plus
///   witness-bearing variants).
///
/// Both encode to the same bytecode; only the in-memory shape differs.
#[derive(Clone, Debug)]
pub enum ProgramItem {
    /// Verifier's opaque bytecode form.
    Bytecode(Vec<u8>),
    /// Prover's witness-bearing form.
    Program(Program),
}

impl ProgramItem {
    /// Returns the canonical bytecode for this item. Allocates only if
    /// the variant is `Program(_)`.
    pub fn to_bytecode(&self) -> Vec<u8> {
        match self {
            ProgramItem::Bytecode(b) => b.clone(),
            ProgramItem::Program(p) => p.to_bytecode(),
        }
    }

    /// Downcasts to a `Program`; errors `TypeNotProgram` for the
    /// bytecode case. Used by prover-side Delegate's `new_run`.
    pub fn into_program(self) -> Result<Program, VMError> {
        match self {
            ProgramItem::Program(p) => Ok(p),
            ProgramItem::Bytecode(_) => Err(VMError::UnexpectedEndOfScript),
        }
    }

    /// Downcasts to opaque bytecode; errors for the `Program` case
    /// (with `UnexpectedEndOfScript` as the closest existing code).
    /// Used by verifier-side Delegate's `new_run`.
    pub fn into_bytecode(self) -> Result<Vec<u8>, VMError> {
        match self {
            ProgramItem::Bytecode(b) => Ok(b),
            ProgramItem::Program(p) => Ok(p.to_bytecode()),
        }
    }
}

impl From<Program> for ProgramItem {
    fn from(p: Program) -> Self {
        ProgramItem::Program(p)
    }
}

impl From<Vec<u8>> for ProgramItem {
    fn from(b: Vec<u8>) -> Self {
        ProgramItem::Bytecode(b)
    }
}
