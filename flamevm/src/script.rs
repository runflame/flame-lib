//! Fluent `ScriptBuilder` + the compiled `Script` value it produces.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

use cells::{
    BagOfCells, CellBuilder, CellDecode, CellEncode, CellError, CellID, CellResolver, CellSlice,
};

use crate::contract::{Contract, ContractID, PredicateTree};
use crate::crypto::Point;
use crate::errors::VMError;
use crate::ops::Instruction;
use crate::scalar::Scalar;
use crate::string::{compile_instructions, String, StringWitness};

/// A program is a list of [`Instruction`]s. Build with the fluent
/// methods (`alloc`, `add`, `eq`, `verify`, …) and call `build_tx` to package
/// bytecode, proof, and the public witness BoC for the verifier. `to_bytecode`
/// alone deliberately omits witnesses and is not a standalone transaction.
#[derive(Clone, Debug, Default)]
pub struct ScriptBuilder {
    instructions: Vec<Instruction>,
    cells: Vec<BagOfCells>,
    scripts: BTreeMap<CellID, Vec<Instruction>>,
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

impl ScriptBuilder {
    /// Constructs an empty program.
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            loop_scopes: Vec::new(),
            cells: Vec::new(),
            scripts: BTreeMap::new(),
        }
    }

    /// Adds public witness bodies before the transaction freezes its BoC.
    pub fn with_cells(mut self, cells: BagOfCells) -> Self {
        self.cells.push(cells);
        self
    }

    /// Attaches prover-only instructions for a branch opened from authenticated
    /// Cells. The key is the canonical snake-code Cell ID, not a caller label.
    pub fn with_script_witness(mut self, script: ScriptBuilder) -> Result<Self, CellError> {
        let id = script_cell(&script.to_bytecode())?.id();
        self.cells.extend(script.cells);
        self.scripts.extend(script.scripts);
        self.scripts.insert(id, script.instructions);
        Ok(self)
    }

    /// Public bodies only. Secret commitment/assignment witnesses never enter
    /// this bag. Nested scripts contribute their embedded Contract witnesses;
    /// their bytecode already lives in literals or selected predicate leaves.
    pub fn cell_witnesses(&self) -> Result<BagOfCells, CellError> {
        let mut bag = BagOfCells::new();
        for cells in &self.cells {
            bag.extend(cells)?;
        }
        for witness in self.witnesses() {
            if let StringWitness::Contract(contract) = witness {
                bag.extend(&BagOfCells::collect(Arc::new(contract.to_cell()?))?)?;
            }
        }
        Ok(bag)
    }

    /// Private Contract witnesses indexed by their public output Cell identity.
    pub fn contract_witnesses(&self) -> BTreeMap<ContractID, Contract> {
        self.witnesses()
            .into_iter()
            .filter_map(|witness| match witness {
                StringWitness::Contract(contract) => Some((contract.id(), contract.clone())),
                _ => None,
            })
            .collect()
    }

    /// Private script overlays; execution must compare their canonical bytecode
    /// with the authenticated program before using any contained witnesses.
    pub fn script_witnesses(&self) -> Result<BTreeMap<CellID, Vec<Instruction>>, CellError> {
        let mut scripts = self.scripts.clone();
        for witness in self.witnesses() {
            if let StringWitness::Script(instructions) = witness {
                let id = script_cell(&compile_instructions(instructions))?.id();
                scripts.insert(id, instructions.clone());
            }
        }
        Ok(scripts)
    }

    fn witnesses(&self) -> Vec<&StringWitness> {
        let mut pending = vec![self.instructions.as_slice()];
        pending.extend(self.scripts.values().map(Vec::as_slice));
        let mut witnesses = Vec::new();
        while let Some(instructions) = pending.pop() {
            for instruction in instructions {
                if let Instruction::PushStr(String::Witness(witness)) = instruction {
                    witnesses.push(witness.as_ref());
                    if let StringWitness::Script(nested) = witness.as_ref() {
                        pending.push(nested);
                    }
                }
            }
        }
        witnesses
    }

    /// Parses a bytecode slice into a ScriptBuilder. Witness-bearing
    /// instructions land as `Alloc(None)` etc. — useful for verifier
    /// inspection or for splicing existing bytecode into a fresh
    /// prover-authored ScriptBuilder.
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

    /// Consumes the ScriptBuilder and returns the underlying
    /// `Vec<Instruction>` — what the VM walks. The Run constructor
    /// takes this directly; both prover and verifier feed the VM
    /// through this single path.
    pub fn into_instructions(self) -> Vec<Instruction> {
        self.instructions
    }

    /// Finishes the builder into an immutable [`Script`] (transparent /
    /// witness-bearing form). The verifier-side opaque form comes from
    /// decoding bytecode, not from a builder.
    pub fn into_script(self) -> Script {
        Script::Transparent(self.instructions)
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
            instr.encode(&mut out);
        }
        out
    }

    /// Builds the witness queue in opcode order. Each witness-bearing
    /// instruction (currently only `Alloc`) contributes exactly one
    /// queue entry; other instructions contribute none.
    pub fn to_witnesses(&self) -> VecDeque<Option<Scalar>> {
        self.instructions
            .iter()
            .filter_map(|i| i.witness())
            .collect()
    }

    // ── stack literals & manipulation ───────────────────

    /// `push:k` / `pushint{8,16,64,128}` / `pushint` — encoder picks
    /// the narrowest opcode width.
    pub fn push_int<T: Into<Scalar>>(mut self, v: T) -> Self {
        self.instructions.push(Instruction::PushInt(v.into()));
        self
    }

    /// `pushstr` (0x19).
    pub fn push_str<T: Into<String>>(mut self, s: T) -> Self {
        self.instructions.push(Instruction::PushStr(s.into()));
        self
    }

    /// `pushstr` (0x19) carrying a witness-bearing sub-script. The
    /// prover pushes the inner ScriptBuilder's instructions (witness
    /// slots intact) wrapped in `String::Witness(StringWitness::Script)`;
    /// downstream `op_open` / `op_signcall` walk those
    /// instructions directly. Verifier-side bytecode encodes to
    /// the compiled bytes of `inner.to_bytecode()`, so both sides
    /// see the same wire form.
    pub fn push_script(mut self, inner: ScriptBuilder) -> Self {
        self.cells.extend(inner.cells);
        self.scripts.extend(inner.scripts);
        self.instructions
            .push(Instruction::PushStr(String::script(inner.instructions)));
        self
    }

    /// `pushpoint` (0x1a) from raw 32 bytes (the verifier-style
    /// `Point::Opaque`). For witness-bearing points use
    /// [`ScriptBuilder::push_point_typed`].
    pub fn push_point(mut self, bytes: [u8; 32]) -> Self {
        self.instructions
            .push(Instruction::PushPoint(Point::from_bytes(bytes)));
        self
    }

    /// `pushpoint` (0x1a) with a typed `Point`. Use this on the prover
    /// side to attach a `Point::Commitment` / `Point::Predicate`
    /// witness; both encode to the canonical 32 bytes on the wire.
    pub fn push_point_typed(mut self, p: Point) -> Self {
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

    pub fn read_bits(mut self) -> Self {
        self.instructions.push(Instruction::ReadBits);
        self
    }
    pub fn read_int(mut self) -> Self {
        self.instructions.push(Instruction::ReadInt);
        self
    }
    pub fn read_str(mut self) -> Self {
        self.instructions.push(Instruction::ReadStr);
        self
    }
    pub fn read_point(mut self) -> Self {
        self.instructions.push(Instruction::ReadPoint);
        self
    }
    pub fn write_bits(mut self) -> Self {
        self.instructions.push(Instruction::WriteBits);
        self
    }
    pub fn write_int(mut self) -> Self {
        self.instructions.push(Instruction::WriteInt);
        self
    }
    pub fn append(mut self) -> Self {
        self.instructions.push(Instruction::Append);
        self
    }
    pub fn write_zeros(mut self) -> Self {
        self.instructions.push(Instruction::WriteZeros);
        self
    }
    pub fn bit_not(mut self) -> Self {
        self.instructions.push(Instruction::BitNot);
        self
    }
    pub fn bit_or(mut self) -> Self {
        self.instructions.push(Instruction::BitOr);
        self
    }
    pub fn bit_and(mut self) -> Self {
        self.instructions.push(Instruction::BitAnd);
        self
    }
    pub fn bit_xor(mut self) -> Self {
        self.instructions.push(Instruction::BitXor);
        self
    }
    pub fn shift_left(mut self) -> Self {
        self.instructions.push(Instruction::ShiftLeft);
        self
    }
    pub fn shift_right(mut self) -> Self {
        self.instructions.push(Instruction::ShiftRight);
        self
    }

    // ── Scalar arithmetic ───────────────────────────────

    pub fn abs(mut self) -> Self {
        self.instructions.push(Instruction::Abs);
        self
    }
    pub fn eq(mut self) -> Self {
        self.instructions.push(Instruction::Eq);
        self
    }
    #[allow(clippy::should_implement_trait)]
    pub fn neg(mut self) -> Self {
        self.instructions.push(Instruction::Neg);
        self
    }
    pub fn add(mut self) -> Self {
        self.instructions.push(Instruction::Add);
        self
    }
    pub fn mul(mut self) -> Self {
        self.instructions.push(Instruction::Mul);
        self
    }
    pub fn divmod(mut self) -> Self {
        self.instructions.push(Instruction::DivMod);
        self
    }
    pub fn mod252(mut self) -> Self {
        self.instructions.push(Instruction::Mod252);
        self
    }
    #[allow(clippy::should_implement_trait)]
    pub fn not(mut self) -> Self {
        self.instructions.push(Instruction::Not);
        self
    }
    pub fn and(mut self) -> Self {
        self.instructions.push(Instruction::And);
        self
    }
    pub fn or(mut self) -> Self {
        self.instructions.push(Instruction::Or);
        self
    }
    pub fn size(mut self) -> Self {
        self.instructions.push(Instruction::Size);
        self
    }

    // ── CS opcodes ─────────────────────────────────────

    /// `alloc` (0x5c) — allocates a low-level R1CS variable. `witness`
    /// = `Some(int)` on the prover side (fills the cleartext value
    /// the constraint system uses), `None` on the verifier side.
    pub fn alloc(mut self, witness: Option<Scalar>) -> Self {
        self.instructions.push(Instruction::Alloc(witness));
        self
    }

    /// `expr` (0x5d).
    pub fn expr(mut self) -> Self {
        self.instructions.push(Instruction::Expr);
        self
    }

    /// `range` (0x64) — `x n → x`. Checks `0 ≤ x < 2^n`, with `n` in
    /// `[1, 64]`. Raw scalars work in every context; R1CS expressions
    /// are supported only in external execution. Preserves the value's type.
    pub fn range(mut self) -> Self {
        self.instructions.push(Instruction::Range);
        self
    }

    /// `scalar` (0x5a) — `string → expr`. Lifts a 32-byte String
    /// (parsed as `Scalar`) into a constant Expression.
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

    pub fn dict(mut self) -> Self {
        self.instructions.push(Instruction::Dict);
        self
    }
    pub fn put(mut self) -> Self {
        self.instructions.push(Instruction::Put);
        self
    }
    pub fn replace(mut self) -> Self {
        self.instructions.push(Instruction::Replace);
        self
    }
    pub fn get(mut self) -> Self {
        self.instructions.push(Instruction::Get);
        self
    }
    pub fn get_opt(mut self) -> Self {
        self.instructions.push(Instruction::GetOpt);
        self
    }
    pub fn get_dup(mut self) -> Self {
        self.instructions.push(Instruction::GetDup);
        self
    }
    pub fn first(mut self) -> Self {
        self.instructions.push(Instruction::First);
        self
    }
    pub fn last(mut self) -> Self {
        self.instructions.push(Instruction::Last);
        self
    }
    pub fn next(mut self) -> Self {
        self.instructions.push(Instruction::Next);
        self
    }

    // ── Cryptography ────────────────────────────────────

    /// `transcript` (0x80) — open a fresh Merlin transcript.
    pub fn transcript(mut self) -> Self {
        self.instructions.push(Instruction::Transcript);
        self
    }
    /// `twrite` (0x81) — absorb `(label, data)` into the transcript.
    pub fn twrite(mut self) -> Self {
        self.instructions.push(Instruction::TWrite);
        self
    }
    /// `tread` (0x82) — squeeze `n` challenge bytes under `label`.
    pub fn tread(mut self) -> Self {
        self.instructions.push(Instruction::TRead);
        self
    }
    pub fn sha256(mut self) -> Self {
        self.instructions.push(Instruction::Sha256);
        self
    }
    pub fn sha512(mut self) -> Self {
        self.instructions.push(Instruction::Sha512);
        self
    }
    pub fn sha3(mut self) -> Self {
        self.instructions.push(Instruction::Sha3);
        self
    }
    pub fn keccak256(mut self) -> Self {
        self.instructions.push(Instruction::Keccak256);
        self
    }

    /// `log` (0x87) — see [`Instruction::Log`].
    pub fn log(mut self) -> Self {
        self.instructions.push(Instruction::Log);
        self
    }

    // ── Tokens ──────────────────────────────────────────

    pub fn amount(mut self) -> Self {
        self.instructions.push(Instruction::Amount);
        self
    }
    /// `issuepriv` (0x91) — confidential mint under the enclosing
    /// predicate. See [`Instruction::IssuePriv`].
    pub fn issuepriv(mut self) -> Self {
        self.instructions.push(Instruction::IssuePriv);
        self
    }
    /// `issueprivflv` (0x92) — consumer-side flavor helper for
    /// `issuepriv`. See [`Instruction::IssuePrivFlv`].
    pub fn issueprivflv(mut self) -> Self {
        self.instructions.push(Instruction::IssuePrivFlv);
        self
    }
    /// `issuepub` (0x93) — cleartext mint under the enclosing actor.
    /// See [`Instruction::IssuePub`].
    pub fn issuepub(mut self) -> Self {
        self.instructions.push(Instruction::IssuePub);
        self
    }
    /// `issuepubflv` (0x94) — consumer-side flavor helper for
    /// `issuepub` (renamed from `issueflv`). See [`Instruction::IssuePubFlv`].
    pub fn issuepubflv(mut self) -> Self {
        self.instructions.push(Instruction::IssuePubFlv);
        self
    }
    pub fn retire(mut self) -> Self {
        self.instructions.push(Instruction::Retire);
        self
    }
    pub fn borrow(mut self) -> Self {
        self.instructions.push(Instruction::Borrow);
        self
    }
    pub fn merge(mut self) -> Self {
        self.instructions.push(Instruction::Merge);
        self
    }
    pub fn split(mut self) -> Self {
        self.instructions.push(Instruction::Split);
        self
    }

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

    pub fn verify(mut self) -> Self {
        self.instructions.push(Instruction::Verify);
        self
    }

    /// `fee` (0x9b) — external-only.
    pub fn fee(mut self) -> Self {
        self.instructions.push(Instruction::Fee);
        self
    }

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

    /// `jumpif:n` (0xa3) — pop an Scalar; jump to label `n` iff non-zero.
    pub fn jumpif(mut self, n: u32) -> Self {
        self.instructions.push(Instruction::JumpIf(n));
        self
    }

    /// `return` (0xa4). Method named `return_` because `return` is a Rust keyword.
    pub fn return_(mut self) -> Self {
        self.instructions.push(Instruction::Return);
        self
    }

    pub fn type_(mut self) -> Self {
        self.instructions.push(Instruction::Type);
        self
    }

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
    /// calls left on the stack (Scalar; non-zero = true).
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
        self.loop_scopes.push(LoopScope {
            top,
            end_jumps: Vec::new(),
        });
        self = cond(self);
        let j_body = self.push_jump_placeholder(true);
        let j_end = self.push_jump_placeholder(false);
        self.loop_scopes
            .last_mut()
            .expect("while scope")
            .end_jumps
            .push(j_end);
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
        self.loop_scopes.push(LoopScope {
            top,
            end_jumps: Vec::new(),
        });
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
        let top = self
            .loop_scopes
            .last()
            .expect("build_continue outside a loop")
            .top;
        self.jump(top)
    }

    // ── Contract + I/O ───────────────────────────────────

    /// `input`. Witness data (open commitments on Token
    /// payloads) rides on the pushed String value — call
    /// `push_str(String::contract(c))` before this on the prover side;
    /// verifiers push `String::Opaque(contract.id().to_vec())` and resolve
    /// the body from the transaction's public witness BoC.
    pub fn input(mut self) -> Self {
        self.instructions.push(Instruction::Input);
        self
    }

    /// Pushes `internal_key root_id index` and embeds the selected public Cell
    /// path into this program. For a tree made with `PredicateTree::from_scripts`,
    /// also retains that program's assignments and nested witnesses. No caller
    /// assembly of a separate witness bag is needed before `build_tx`.
    ///
    /// `program_index` selects a program in logical input order, not a blinded
    /// Trie position. Push the gas grant, arguments, and count before `open`.
    pub fn push_taproot_proof(
        mut self,
        tree: &PredicateTree,
        program_index: usize,
    ) -> Result<Self, VMError> {
        let (proof, cells) = tree.witness_for(program_index)?;
        self.cells.push(cells);
        if let Some(script) = tree.script_witness(program_index) {
            self = self.with_script_witness(script.clone())?;
        }
        Ok(self
            .push_point(*proof.internal_key.as_bytes())
            .push_str(String::from(proof.root.to_vec()))
            .push_int(proof.index))
    }
    pub fn contract(mut self) -> Self {
        self.instructions.push(Instruction::Contract);
        self
    }
    pub fn output(mut self) -> Self {
        self.instructions.push(Instruction::Output);
        self
    }
    pub fn open(mut self) -> Self {
        self.instructions.push(Instruction::Open);
        self
    }
    pub fn signtx(mut self) -> Self {
        self.instructions.push(Instruction::Signtx);
        self
    }
    pub fn signcall(mut self) -> Self {
        self.instructions.push(Instruction::Signcall);
        self
    }

    // ── Actor invocation + state ─────────

    pub fn send(mut self) -> Self {
        self.instructions.push(Instruction::Send);
        self
    }
    pub fn call(mut self) -> Self {
        self.instructions.push(Instruction::Call);
        self
    }
    pub fn load(mut self) -> Self {
        self.instructions.push(Instruction::Load);
        self
    }
    pub fn save(mut self) -> Self {
        self.instructions.push(Instruction::Save);
        self
    }
    pub fn setcode(mut self) -> Self {
        self.instructions.push(Instruction::Setcode);
        self
    }
    pub fn addstorage(mut self) -> Self {
        self.instructions.push(Instruction::AddStorage);
        self
    }
    pub fn quotestorage(mut self) -> Self {
        self.instructions.push(Instruction::QuoteStorage);
        self
    }

    // ── Tx-level & frame introspection ───

    pub fn timelock(mut self) -> Self {
        self.instructions.push(Instruction::Timelock);
        self
    }
    pub fn version(mut self) -> Self {
        self.instructions.push(Instruction::Version);
        self
    }
    pub fn selfid(mut self) -> Self {
        self.instructions.push(Instruction::Selfid);
        self
    }
    pub fn anchor(mut self) -> Self {
        self.instructions.push(Instruction::Anchor);
        self
    }
    pub fn gas(mut self) -> Self {
        self.instructions.push(Instruction::Gas);
        self
    }
    pub fn usage(mut self) -> Self {
        self.instructions.push(Instruction::Usage);
        self
    }
    pub fn callerid(mut self) -> Self {
        self.instructions.push(Instruction::Callerid);
        self
    }
    pub fn gaslimit(mut self) -> Self {
        self.instructions.push(Instruction::Gaslimit);
        self
    }
    pub fn capacity(mut self) -> Self {
        self.instructions.push(Instruction::Capacity);
        self
    }
    pub fn height(mut self) -> Self {
        self.instructions.push(Instruction::Height);
        self
    }
}

// ── Script ──────────────────────────────────────────────────────────

/// A compiled script — the immutable value/exec form a [`ScriptBuilder`]
/// produces and a [`CallFrame`](crate::vm) runs. Two representations of
/// the same bytecode (todo #1–3 — unifies the former `Code` and
/// `ProgramItem`):
///
/// - `Transparent(Vec<Instruction>)` — prover's view; carries
///   witness-bearing instructions, ready to execute without re-decoding.
/// - `Opaque(Vec<u8>)` — verifier / actor view; raw bytecode decoded one
///   instruction at a time, never materializing a `Vec<Instruction>`.
///
/// Both yield identical canonical bytecode via [`to_bytecode`](Self::to_bytecode).
#[derive(Clone, Debug)]
pub enum Script {
    /// Prover's pre-decoded, witness-bearing instructions.
    Transparent(Vec<Instruction>),
    /// Verifier's raw bytecode, decoded on demand.
    Opaque(Vec<u8>),
}

impl Script {
    /// Canonical bytecode. Allocates only for the `Transparent` case.
    pub fn to_bytecode(&self) -> Vec<u8> {
        match self {
            Script::Opaque(b) => b.clone(),
            Script::Transparent(instrs) => {
                let mut out = Vec::new();
                for instr in instrs {
                    instr.encode(&mut out);
                }
                out
            }
        }
    }

    /// Decodes to the instruction list (re-parsing the `Opaque` case,
    /// dropping any witnesses it never carried).
    pub fn into_instructions(self) -> Result<Vec<Instruction>, VMError> {
        match self {
            Script::Transparent(instrs) => Ok(instrs),
            Script::Opaque(b) => Ok(ScriptBuilder::parse(&b)?.into_instructions()),
        }
    }
}

impl CellEncode for Script {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder.store_snake(&self.to_bytecode())?;
        Ok(())
    }
}

impl CellDecode for Script {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        cells: &mut R,
    ) -> Result<Self, CellError> {
        Ok(Self::Opaque(slice.load_snake(cells, u32::MAX as usize)?))
    }
}

/// Canonical code identity shared by scripts and prover overlays.
pub(crate) fn script_cell(bytecode: &[u8]) -> Result<cells::Cell, CellError> {
    let mut builder = CellBuilder::new();
    builder.store_snake(bytecode)?;
    Ok(builder.build())
}

#[cfg(test)]
mod witness_tests {
    use super::*;
    use crate::{vm::Anchor, Predicate, Value};

    #[test]
    fn nested_witnesses_are_collected_separately_from_bytecode() {
        let contract = Contract::new(
            Predicate::opaque(Predicate::unspendable_key()),
            Anchor([5; 32]),
            Value::Scalar(Scalar::ONE),
        )
        .unwrap();
        let id = contract.id();
        let reference = String::contract(contract);
        assert_eq!(reference.to_bytes_vec(), id);
        assert_eq!(reference.len(), 32);
        assert!(String::from(id.to_vec()).to_contract().is_err());

        let branch = ScriptBuilder::new()
            .push_str(reference)
            .input()
            .alloc(Some(Scalar::from(9u64)));
        let branch_id = script_cell(&branch.to_bytecode()).unwrap().id();
        let program = ScriptBuilder::new()
            .push_script(branch.clone())
            .with_script_witness(branch)
            .unwrap();
        let bag = program.cell_witnesses().unwrap();
        assert!(bag.contains(&id));
        assert!(
            !bag.contains(&branch_id),
            "inline bytecode needs no duplicate code Cell"
        );
        assert_eq!(program.contract_witnesses()[&id].id(), id);
        let scripts = program.script_witnesses().unwrap();
        assert!(
            matches!(scripts[&branch_id].last(), Some(Instruction::Alloc(Some(value))) if *value == Scalar::from(9u64))
        );
        let bytecode = program.to_bytecode();
        let public = ScriptBuilder::parse(&bytecode).unwrap();
        assert_eq!(public.to_bytecode(), bytecode);
        assert!(public.contract_witnesses().is_empty());
        assert!(public.script_witnesses().unwrap().is_empty());
    }
}
