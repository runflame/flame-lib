//! Shared test fixtures for the vm.rs test modules.
//!
//! Everything that was previously inline in `vm.rs::tests` but
//! isn't a `#[test]` function lives here. Test submodules pull
//! these in via `use super::test_helpers::*;`.

#![allow(dead_code, unused_imports)]

// Tests are a descendant of `vm`, so `super::super::*` also
// pulls in everything vm.rs imports privately
// (CompressedRistretto, Scalar, Transcript, Cell, Commitment,
// etc.). Don't re-import any of those below or you'll get
// "defined multiple times".
pub use super::super::*;

pub(crate) use super::mem_registry::MemRegistry;

pub use crate::{
    CheckedFee, CommitmentWitness, Constraint, Expression, Instruction, PredicateTree, Prover,
    Script, ScriptBuilder, SecretConstraint, Token, Value, Variable, Verifier, WideToken,
    FLAME_FLAVOR, MAX_FEE,
};
pub use bulletproofs::PedersenGens;
pub use curve25519_dalek::constants::RISTRETTO_BASEPOINT_TABLE;

/// Registry whose `load_code` always returns the same script.
pub(crate) struct StubRegistry {
    pub script: Vec<u8>,
}

impl ActorRegistry for StubRegistry {
    fn load_code(&self, _actor: &ActorID) -> Result<Vec<u8>, VMError> {
        Ok(self.script.clone())
    }

    fn actor_code_bytes(&self, _actor: &ActorID) -> Result<u64, VMError> {
        Ok(self.script.len() as u64)
    }

    fn actor_state_bytes(&self, _actor: &ActorID) -> Result<u64, VMError> {
        Ok(0)
    }

    fn set_code(&mut self, _actor: &ActorID, _code: Vec<u8>) -> Result<(), VMError> {
        unimplemented!("StubRegistry::set_code — use MemRegistry")
    }

    fn actor_usage(&self, _actor: &ActorID) -> Result<u64, VMError> {
        Ok(0)
    }

    fn actor_capacity(&self, _actor: &ActorID, _height: u64) -> Result<u64, VMError> {
        Ok(0)
    }

    fn quote_storage(
        &self,
        _actor: &ActorID,
        _bytes: u64,
        _height: u64,
    ) -> Result<Option<crate::actor::StoragePurchase>, VMError> {
        Ok(None)
    }

    fn purchase_storage(
        &mut self,
        _actor: &ActorID,
        _bytes: u64,
        _height: u64,
    ) -> Result<Option<crate::actor::StoragePurchase>, VMError> {
        Ok(None)
    }

    fn validate_actor_storage(&self, _actor: &ActorID, _height: u64) -> Result<(), VMError> {
        Ok(())
    }

    fn exists(&self, _actor: &ActorID) -> bool {
        true
    }

    // The remaining trait methods aren't exercised by tests that
    // pin this stub — once Units 5–8 land the affected tests
    // construct a `MemRegistry` instead. Surface a clear panic
    // so a future test that strays here gets an obvious error.
    fn load_state(&mut self, _id: &ActorID) -> Result<Value, VMError> {
        unimplemented!("StubRegistry::load_state — use MemRegistry for state-touching tests")
    }
    fn save_state(&mut self, _id: &ActorID, _state: Value) -> Result<(), VMError> {
        unimplemented!("StubRegistry::save_state — use MemRegistry for state-touching tests")
    }
    fn push_checkpoint(&mut self) {
        // StubRegistry has no mutable state worth snapshotting.
    }
    fn pop_checkpoint_commit(&mut self) {}
    fn pop_checkpoint_rollback(&mut self) {}

    fn commit_tx_destructions(&mut self) -> Vec<ActorID> {
        Vec::new()
    }
    fn deploy(&mut self, _id: ActorID, _code: Vec<u8>, _state: Value) -> Result<(), VMError> {
        unimplemented!("StubRegistry::deploy — use MemRegistry")
    }
}

pub(crate) fn dummy_header() -> TxHeader {
    TxHeader {
        version: 1,
        locktime: 0,
    }
}

pub(crate) fn dummy_message(gas: u64) -> Message {
    Message::new(
        ActorID::Hash([0u8; 32]),
        None,
        Anchor([0u8; 32]),
        Vec::new(),
        gas,
        // Test fixture: NUMS-unspendable predicate as the refund
        // sink. No real bounce path exercised by the tests that
        // call `dummy_message`; this just satisfies the field.
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("message payload is portable")
}

/// Builds a VM running `script` as the entry Run of an InternalRoot.
pub(crate) fn vm_with_script(script: Vec<u8>) -> VM {
    let kind = CallKind::InternalRoot {
        actor: ActorID::Hash([0u8; 32]),
        caller: None,
    };
    VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::parse(&script)
                .expect("script parses")
                .into_instructions(),
            kind,
            1_000_000,
        )
        .with_anchor(Anchor([0u8; 32])),
    )
}

/// Runs steps until the current Run is exhausted, *without* invoking
/// finish_call (so the test can inspect the leftover stack).
pub(crate) fn run_to_end(vm: &mut VM) -> Result<(), VMError> {
    while !vm.current_call.is_finished() {
        vm.step_internal()?;
    }
    Ok(())
}

pub(crate) fn assert_int(v: &Value, expected: Int253) {
    match v {
        Value::Int253(i) => assert_eq!(*i, expected, "expected {:?}, got {:?}", expected, i),
        other => panic!("expected Int253, got {:?}", value_kind(other)),
    }
}

pub(crate) fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Int253(_) => "Int253",
        Value::String(_) => "String",
        Value::Dict(_) => "Dict",
        Value::Point(_) => "Point",
        Value::Token(_) => "Token",
        Value::WideToken(_) => "WideToken",
        Value::ClearToken(_) => "ClearToken",
        Value::Cell(_) => "Cell",
        Value::Merlin(_) => "Merlin",
        Value::Variable(_) => "Variable",
        Value::Expression(_) => "Expression",
        Value::Constraint(_) => "Constraint",
        Value::MultiscalarMul(_) => "MultiscalarMul",
    }
}

/// Runs `step_internal` until it reports the tx is done (Ok(false)).
/// Used to exercise full programs including call-frame exit.
pub(crate) fn run_until_tx_done(vm: &mut VM) -> Result<(), VMError> {
    while vm.step_internal()? {}
    Ok(())
}

// ── Self-checking opcode harness (Bucket B) ──────────────────────────
//
// These let opcode tests assert their result *inside the VM* via
// `eq; verify`, so test bodies never reach into `vm.current_call.stack`
// or build `CallFrame`/`CallKind`. The VM is the door; the script
// checks itself.

/// Asserts the **entire** post-`builder` stack equals `expected_top_first`
/// (top element first), checked in-VM and consuming the whole stack: for
/// each expected value it appends `push:v; eq; verify; drop; drop`
/// (`eq` is non-consuming, so its two int operands are dropped after the
/// boolean is `verify`'d). The tx must finish cleanly, so this also
/// asserts there is no extra residue. Use when every stack slot is an
/// `Int253`. A mismatch fails `verify`.
pub(crate) fn assert_stack(mut builder: ScriptBuilder, expected_top_first: &[Int253]) {
    for &v in expected_top_first {
        builder = builder.push_int(v).eq().verify().drop_().drop_();
    }
    let mut vm = vm_with_script(builder.to_bytecode());
    run_until_tx_done(&mut vm).expect("opcode self-check (eq; verify) passed");
}

/// `i64` convenience over [`assert_stack`].
pub(crate) fn assert_stack_ints(builder: ScriptBuilder, expected_top_first: &[i64]) {
    let v: Vec<Int253> = expected_top_first
        .iter()
        .map(|&x| Int253::from(x))
        .collect();
    assert_stack(builder, &v);
}

/// Asserts only the **top** of the post-`builder` stack equals
/// `expected`, via `push:expected; eq; verify`. Tolerant of residue
/// below the top (e.g. an opcode's non-int operands), so use it when the
/// full stack isn't all ints. A mismatch fails `verify`.
pub(crate) fn assert_top(builder: ScriptBuilder, expected: impl Into<Int253>) {
    let mut vm = vm_with_script(builder.push_int(expected).eq().verify().to_bytecode());
    run_to_end(&mut vm).expect("top-of-stack self-check (eq; verify) passed");
}

/// Runs `builder` and returns the hard-failure error — for tests that
/// assert a specific `VMError` via `matches!` without touching the VM.
pub(crate) fn run_err(builder: ScriptBuilder) -> VMError {
    let mut vm = vm_with_script(builder.to_bytecode());
    run_until_tx_done(&mut vm).expect_err("expected a hard failure")
}

// ── Integration delivery harness (Bucket C) ──────────────────────────

/// Delivers `msg` to `reg` via `VM::execute_internal` and returns the
/// resulting effect log (the public observable). Test bodies assert on
/// `TxEntry`s, not on internal frame/stack state.
pub(crate) fn deliver(reg: &mut MemRegistry, msg: Message) -> Vec<TxEntry> {
    let block = BlockContext { height: 0 };
    VM::execute_internal(dummy_header(), msg, reg, &block)
        .expect("internal delivery ok")
        .txlog
}

/// Like [`deliver`] but expects the delivery to hard-fail, returning the
/// error.
pub(crate) fn deliver_err(reg: &mut MemRegistry, msg: Message) -> VMError {
    let block = BlockContext { height: 0 };
    VM::execute_internal(dummy_header(), msg, reg, &block).expect_err("expected delivery failure")
}

/// Builds a minimal internal `Message` targeting `actor` with no
/// payload — the common Bucket-C delivery fixture.
pub(crate) fn msg_to(actor: ActorID) -> Message {
    Message::new(
        actor,
        None,
        Anchor([0u8; 32]),
        Vec::new(),
        1_000_000,
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("message payload is portable")
}

/// Like [`msg_to`] but carries a single top-of-stack selector arg
/// (ADR 0020 dispatch convention).
pub(crate) fn msg_with_sel(actor: ActorID, sel: u64) -> Message {
    Message::new(
        actor,
        None,
        Anchor([0u8; 32]),
        vec![Value::Int253(Int253::from(sel))],
        1_000_000,
        Predicate::opaque(Predicate::unspendable_key()),
    )
    .expect("message payload is portable")
}

/// Deploys an actor whose code is `recv` (id derived from the code, a
/// stand-in for the real constructor) and returns its id. The common
/// Bucket-C fixture: deploy, then `deliver` and read the effect log.
pub(crate) fn deploy_actor(reg: &mut MemRegistry, recv: Vec<u8>) -> ActorID {
    let id = ActorID::Hash(ActorID::Constructor(recv.clone()).to_hash());
    reg.deploy(id.clone(), recv, empty_state(), 1_000_000)
        .expect("deploy");
    id
}

/// A `call` to `target` with `k=0` args (plain single-action callee).
/// Returns the builder *after* the `call` so the caller chains the
/// result handling (`.drop_().drop_()` for the `[count, success]`).
pub(crate) fn call_to(target: &ActorID) -> ScriptBuilder {
    ScriptBuilder::new()
        .push_int(0u64) // k = 0
        .push_int(50_000u64) // gas
        .push_str(String::from(target.to_hash().to_vec()))
        .call()
}

/// A `call` to `target` passing `sel` as the single (topmost) arg — the
/// dispatch-selector convention for `dispatch_code` callees.
pub(crate) fn call_with_sel(target: &ActorID, sel: u64) -> ScriptBuilder {
    ScriptBuilder::new()
        .push_int(sel) // selector arg (top)
        .push_int(1u64) // k = 1
        .push_int(50_000u64) // gas
        .push_str(String::from(target.to_hash().to_vec()))
        .call()
}

/// Builds an inline subprogram string-payload as a script that
/// `pushstr`s the subprogram's bytes. Returns the prefix bytes
/// (`0x19` + sub-varint length + payload).
pub(crate) fn pushstr_bytes(payload: &[u8]) -> Vec<u8> {
    let mut s = vec![0x19, 0x00, payload.len() as u8];
    s.extend_from_slice(payload);
    s
}

/// Builds an actor code blob that dispatches on the **top-of-stack
/// selector** (ADR 0020 convention) to one of `arms`
/// (selector → handler bytecode). Callers pass the selector as the
/// last (= topmost) argument. Handlers must be label-free (the
/// dispatch owns labels `0..=arms.len()`).
pub(crate) fn dispatch_code(arms: &[(u64, Vec<u8>)]) -> Vec<u8> {
    // `eq` is non-consuming in the cleartext branch (`a b → a b {0|1}`,
    // spec §eq), so each compare leaves the selector `S` and the arm
    // value `sel`; we drop them explicitly. A matched `jumpif` lands at
    // `label i` with `[S sel]` on the stack → the handler drops both.
    let n = arms.len() as u32;
    let mut p = ScriptBuilder::new(); // selector S already on the stack
    for (i, (sel, _)) in arms.iter().enumerate() {
        // [S] → push sel [S sel] → eq [S sel b] → jumpif (pops b);
        // no-match falls through to drop the arm value → [S].
        p = p.push_int(*sel).eq().jumpif(i as u32).drop_();
    }
    p = p.drop_().push_int(0u64).verify(); // no match: drop S, revert
    for (i, (_, handler)) in arms.iter().enumerate() {
        p = p
            .label(i as u32)
            .drop_()
            .drop_() // drop arm value + selector
            .raw_bytes(handler)
            .expect("handler parses")
            .jump(n);
    }
    p.label(n).to_bytecode()
}

/// Helper: builds a VM with a child CellOpen frame as `current_call`
/// and a placeholder ExternalRoot on `call_stack`. Used by the arity
/// / clean-stack `return` tests which need a non-root frame to
/// exercise the inner checks (root frame would short-circuit with
/// `ReturnAtRoot`).
pub(crate) fn vm_with_nested_child_script(script: Vec<u8>) -> VM {
    let parent = CallFrame::new(Vec::new(), CallKind::ExternalRoot, 500);
    let child_kind = CallKind::CellOpen {
        predicate: Predicate::opaque(CompressedRistretto([0u8; 32])),
        external_context: true,
        caller_id: None,
    };
    let child = CallFrame::new(
        ScriptBuilder::parse(&script)
            .expect("script parses")
            .into_instructions(),
        child_kind,
        500,
    );
    let mut vm = VM::new(dummy_header(), parent);
    let p = mem::replace(&mut vm.current_call, child);
    vm.call_stack.push(p);
    vm
}

pub(crate) fn assert_str(v: &Value, expected: &[u8]) {
    match v {
        Value::String(s) => assert_eq!(s.as_opaque().unwrap(), expected),
        other => panic!("expected String, got {}", value_kind(other)),
    }
}

/// Helper: encodes `value` (non-negative `Int253`) as a low-`n_bits`
/// LSB-first byte sequence (writebits-compatible).
pub(crate) fn writebits_bytes(value: &Int253, n_bits: usize) -> Vec<u8> {
    assert!(n_bits <= 256);
    let int_bytes = value.to_bytes();
    let n_bytes = n_bits.div_ceil(8);
    let mut out = int_bytes[..n_bytes].to_vec();
    let tail = n_bits % 8;
    if tail != 0 && n_bytes > 0 {
        let mask = (1u8 << tail) - 1;
        out[n_bytes - 1] &= mask;
    }
    out
}

pub(crate) fn assert_dict_keys(v: &Value, expected: &[Int253]) {
    match v {
        Value::Dict(d) => {
            let keys: Vec<Int253> = d.entries().map(|(k, _)| *k).collect();
            assert_eq!(keys, expected);
        }
        other => panic!("expected Dict, got {}", value_kind(other)),
    }
}

pub(crate) fn hex_to_bytes(h: &str) -> Vec<u8> {
    let h: std::string::String = h.chars().filter(|c| !c.is_whitespace()).collect();
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

/// Fixed blinding-key seed for tests — keeps tree construction
/// deterministic across runs.
pub(crate) const TEST_BLINDING_KEY: [u8; 32] = [0u8; 32];

/// Helper: builds a single-leaf `PredicateTree` from a program and a
/// known internal-key scalar, plus the `TaprootProof` that opens it.
pub(crate) fn build_predicate_with_program(
    program: &[u8],
    internal_secret: u64,
) -> (PredicateTree, TaprootProof) {
    let secret = Scalar::from(internal_secret);
    let x_point = RISTRETTO_BASEPOINT_TABLE * &secret;
    let internal_key = x_point.compress();
    let tree = PredicateTree::new(
        Some(internal_key),
        vec![program.to_vec()],
        TEST_BLINDING_KEY,
    )
    .unwrap();
    let cp = tree.taproot_proof_for(0).unwrap();
    (tree, cp)
}

/// Helper: builds a `PredicateTree` with multiple programs and the
/// `TaprootProof` that opens the `program_index`-th one.
pub(crate) fn build_multi_leaf_predicate(
    programs: Vec<Vec<u8>>,
    program_index: usize,
    internal_secret: u64,
) -> (PredicateTree, TaprootProof) {
    let secret = Scalar::from(internal_secret);
    let x_point = RISTRETTO_BASEPOINT_TABLE * &secret;
    let internal_key = x_point.compress();
    let tree = PredicateTree::new(Some(internal_key), programs, TEST_BLINDING_KEY).unwrap();
    let cp = tree.taproot_proof_for(program_index).unwrap();
    (tree, cp)
}

pub use crate::token::flavor_from_actor as test_flavor_from_actor;

/// Convenience: builds a Token via the cleartext constructor for tests.
pub(crate) fn make_cleartext_token(qty: u64, flv: u64) -> Token {
    Token::cleartext(Int253::from(qty), Int253::from(flv)).expect("u64 quantity is in range")
}

/// Builds a VM running `script` under InternalRoot with a specific
/// actor identity (so `op_issue` has actor context).
pub(crate) fn vm_internal_with_actor(script: Vec<u8>, actor: ActorID) -> VM {
    let kind = CallKind::InternalRoot {
        actor,
        caller: None,
    };
    VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::parse(&script)
                .expect("script parses")
                .into_instructions(),
            kind,
            1_000_000,
        )
        .with_anchor(Anchor([0u8; 32])),
    )
}

/// Quick helper: builds a stub delegate for external-context tests
/// that need to step through `step_external` once. Defined as a
/// free function so the borrowing patterns in
/// `issue_at_external_root_errors_actor_context` work.
pub(crate) fn make_stub_delegate() -> StubDelegate {
    StubDelegate::new()
}

/// Drives `vm.step_external(delegate)` until done or error,
/// returning the error if any. Doesn't call finalize.
pub(crate) fn drive_external(vm: &mut VM, delegate: &mut StubDelegate) -> Result<(), VMError> {
    while vm.step_external(delegate)? {}
    Ok(())
}

/// Builds a VM running `script` under `ExternalRoot`. Mirror of
/// `vm_with_script` for the external-context opcode tests.
pub(crate) fn vm_external_with_script(script: Vec<u8>) -> VM {
    VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::parse(&script)
                .expect("script parses")
                .into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
        ),
    )
}

/// Builds a wire-encoded cell as a `Vec<u8>` so tests can feed it
/// to the `input` opcode (which pops a `String` and decodes it).
pub(crate) fn encode_cell_to_bytes(cell: &Cell) -> Vec<u8> {
    let mut buf = Vec::new();
    cell.encode(&mut buf).expect("cell encodes");
    buf
}

/// Builds a non-trivial test cell — opaque predicate, fixed anchor,
/// two-item portable payload. Used by both the round-trip and the
/// `input` opcode tests.
pub(crate) fn fixture_cell() -> Cell {
    let predicate = Predicate::opaque(CompressedRistretto([0xaa; 32]));
    let anchor = Anchor([0x42; 32]);
    let payload = vec![
        Value::Int253(Int253::from(7u64)),
        Value::String(String::from(b"hello".to_vec())),
    ];
    Cell::new(predicate, anchor, payload).expect("fixture payload is portable")
}

/// Helper: decode and discard a Cell so tests can use a simple
/// `Result<(), VMError>` assertion shape.
pub(crate) fn decode_cell_dropping_ok(bytes: &[u8]) -> Result<(), VMError> {
    let mut r: &[u8] = bytes;
    <Cell as readerwriter::Decodable>::decode(&mut r)
        .map_err(|_| VMError::MalformedCellEncoding)?;
    Ok(())
}

//
// The tests below assemble small but complete external-tx programs
// — input → authorize → output — and drive them through the full
// `step_external` dispatch loop plus `Delegate::finalize`. They are
// the first tests that exercise the VM's external-context API as a
// unit and serve as ground truth for the Phase-10 cell life-cycle.

/// Drives `script` through `step_external` to completion using a
/// `StubDelegate`, returns the resulting VM (so the test can inspect
/// txlog, deferred_sigs, last_anchor, etc.). Mirrors the body of
/// `VM::execute_external` minus the `into_result()` consumption.
pub(crate) fn run_external_workflow(script: Vec<u8>) -> VM {
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            ScriptBuilder::parse(&script)
                .expect("script parses")
                .into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
        ),
    );
    let mut delegate = StubDelegate::new();
    while vm.step_external(&mut delegate).expect("step_external ok") {}
    vm
}

/// Test-only `Delegate` impl backed by `r1cs::Verifier`. None of its
/// methods are exercised by the Phase-10 tests; it exists purely so
/// `step_external` is satisfiable.
pub(crate) struct StubDelegate {
    pub cs: bulletproofs::r1cs::Verifier<merlin::Transcript>,
    pub batch: musig::BatchVerifier<rand::rngs::ThreadRng>,
}

impl StubDelegate {
    pub(crate) fn new() -> Self {
        Self {
            cs: bulletproofs::r1cs::Verifier::new(merlin::Transcript::new(b"flamevm.test.stub")),
            batch: musig::BatchVerifier::new(rand::thread_rng()),
        }
    }
}

impl Delegate for StubDelegate {
    type CS = bulletproofs::r1cs::Verifier<merlin::Transcript>;
    type BatchVerifier = musig::BatchVerifier<rand::rngs::ThreadRng>;

    fn cs(&mut self) -> &mut Self::CS {
        &mut self.cs
    }

    fn batch_verifier(&mut self) -> &mut Self::BatchVerifier {
        &mut self.batch
    }

    fn commit_variable(
        &mut self,
        _commitment: &Commitment,
    ) -> Result<(CompressedRistretto, bulletproofs::r1cs::Variable), VMError> {
        unreachable!("StubDelegate::commit_variable should not be called in Phase-10 tests");
    }
}

/// Helper: build a VM in external context with a witness-bearing
/// program (so the prover-side Alloc witnesses are intact), step
/// `n_steps` instructions against a Prover, then return the VM
/// (without finalizing — so a WideToken can sit on the stack).
/// Mirrors the encrypted-borrow test's pattern.
pub(crate) fn run_external_steps<'g>(
    pc_gens: &'g PedersenGens,
    program: ScriptBuilder,
    n_steps: usize,
) -> (VM, Prover<'g>) {
    let mut vm = VM::new(
        dummy_header(),
        CallFrame::new(
            program.into_instructions(),
            CallKind::ExternalRoot,
            1_000_000,
        ),
    );
    let mut prover = Prover::new(pc_gens);
    for _ in 0..n_steps {
        vm.step_external(&mut prover).expect("step ok");
    }
    (vm, prover)
}

/// Helper: turn a scalar secret into a `(CompressedRistretto, sk)`
/// pair. The CompressedRistretto is the verification key; the
/// scalar is the signing key.
pub(crate) fn signing_keypair(secret: u64) -> (CompressedRistretto, Scalar) {
    use curve25519_dalek::constants::RISTRETTO_BASEPOINT_TABLE;
    let sk = Scalar::from(secret);
    let vk = (RISTRETTO_BASEPOINT_TABLE * &sk).compress();
    (vk, sk)
}

/// Helper: build a script that consumes one cell via input+signtx
/// and then no-ops the popped payload + count. Returns the script
/// bytes and the consumed cell's id.
pub(crate) fn make_signtx_script_with_cell(vk: CompressedRistretto) -> (Vec<u8>, CellID) {
    let cell = Cell::new(
        Predicate::opaque(vk),
        Anchor([0x42; 32]),
        vec![Value::Int253(Int253::from(0u64))], // single Int253 payload
    )
    .expect("payload is portable");
    let cell_id = cell.id();
    let script = ScriptBuilder::new()
        .push_str(String::from(encode_cell_to_bytes(&cell)))
        .input()
        .signtx() // pushes 1 Int253 (payload) + count 1
        .drop_() // drop count
        .drop_() // drop payload Int253
        .to_bytecode();
    (script, cell_id)
}

/// Helper: build a Token with `Commitment::Open` quantity and
/// flavor (witness-bearing). The token can be embedded in a Cell
/// and pushed via `String::cell(c)` so witnesses survive the trip
/// through `op_input` and into the downstream `mix` gadget.
pub(crate) fn make_open_token(
    qty_value: u64,
    flv_value: u64,
    qty_blind: u64,
    flv_blind: u64,
) -> Token {
    let q = Commitment::blinded_with_factor(Int253::from(qty_value), Scalar::from(qty_blind));
    let f = Commitment::blinded_with_factor(Int253::from(flv_value), Scalar::from(flv_blind));
    Token::new(q, f)
}

//
// The whole point of the VM: take N input cells whose Token
// payloads are confidential (Pedersen-committed qty + flv), run
// them through `mix` to balance against M new output cells
// (also confidential), and produce a single ZK proof that the
// verifier accepts.
//
// The script shape:
//
//   ┌── per input cell i ───────────────────────────────────┐
//   │  pushstr <cell_bytes_i>                                │
//   │  input  (with InputWitnesses_i, prover-side)           │
//   │  pushpoint <internal_key_i>                            │
//   │  <neighbors dict — empty for single-leaf trees>        │
//   │  pushstr <position_i>                                  │
//   │  pushstr <program_i (= empty)>                         │
//   │  push:0  (k = 0 args)                                  │
//   │  open   (unlocked Run is empty → payload stays)        │
//   └────────────────────────────────────────────────────────┘
//   ↓  stack now: [Token_0, Token_1, …, Token_{N-1}]
//
//   For each output j:
//   ┌── push the (qty, flv) commitment Strings ─────────────┐
//   │  pushstr <qty_open_j>                                  │
//   │  pushstr <flv_open_j>                                  │
//   └────────────────────────────────────────────────────────┘
//   ↓  stack: [Token_0, …, Token_{N-1},
//              qty_0, flv_0, qty_1, flv_1, … qty_{M-1}, flv_{M-1}]
//
//   push:M  push:N  mix
//   ↓  stack: [out_Token_0, out_Token_1, …, out_Token_{M-1}]
//
//   For each output j (in stack order):
//   ┌── wrap into output cell ──────────────────────────────┐
//   │  push:1            (k = 1, the Token below is payload) │
//   │  pushpoint <pred_j>                                    │
//   │  output            (emits TxEntry::Output, pops Token  │
//   │                     + count + pred)                    │
//   └────────────────────────────────────────────────────────┘
//   ↓  stack: empty → finish_call accepts.

/// Description of one input to the test harness.
#[derive(Clone)]
pub(crate) struct NMInputSpec {
    pub qty: u64,
    pub flv: u64,
    /// Blinding factor for the qty commitment.
    pub qty_blind: u64,
    /// Blinding factor for the flv commitment.
    pub flv_blind: u64,
    /// Anchor bytes for the input cell — must be unique per
    /// cell so cell-ids don't collide. Cell identity also
    /// commits the predicate and payload, but anchor uniqueness
    /// is the simplest way to keep distinct cells distinct.
    pub anchor: [u8; 32],
}

/// Description of one output to the test harness.
#[derive(Clone)]
pub(crate) struct NMOutputSpec {
    pub qty: u64,
    pub flv: u64,
    /// Blinding factor for the new qty commitment.
    pub qty_blind: u64,
    /// Blinding factor for the new flv commitment.
    pub flv_blind: u64,
    /// Caller-chosen distinguisher for the output predicate.
    /// `output_predicate_point(tag)` derives a real Ristretto
    /// point via `PredicateTree::scripts_only` blinded by the
    /// tag — so the output predicate is a valid point that a
    /// future "consume Alice's output as input" test could
    /// open. Arbitrary 32-byte arrays (e.g. `[0xb1; 32]`) don't
    /// decompress to valid Ristretto and would break any such
    /// chained test, even though `Predicate::Opaque` accepts
    /// them by value.
    pub predicate_tag: u8,
}

/// Derive a real Ristretto predicate point for an output spec
/// — a single-empty-leaf scripts-only tree, blinded by the
/// caller-chosen `tag`. The tag picks the encoding distinct
/// from other outputs; the resulting compressed point IS a
/// valid Ristretto encoding (unlike a bare `[0xbN; 32]` array).
pub(crate) fn output_predicate_point(tag: u8) -> CompressedRistretto {
    let mut blinding = TEST_BLINDING_KEY;
    blinding[0] = tag;
    PredicateTree::scripts_only(vec![Vec::new()], blinding)
        .expect("scripts_only tree builds")
        .point
}

/// Builds the prover-side `ScriptBuilder` for the N→M script. The
/// `inputs` and `outputs` specs must already balance per flavor
/// (`mix` will fail at CS solve time otherwise — exercised
/// separately by the negative tests).
///
/// Returns the prover ScriptBuilder (witnesses inline) — the canonical
/// bytecode is recovered via `program.to_bytecode()` on the
/// verifier side.
pub(crate) fn build_confidential_nm_program(
    inputs: &[NMInputSpec],
    outputs: &[NMOutputSpec],
) -> ScriptBuilder {
    let mut program = ScriptBuilder::new();

    //                    taproot_proof pieces + push:0 + open. ──
    for inp in inputs {
        let (cell, cp) = build_input_cell(inp);
        // pushstr String::cell(c) — prover-side witness carrier
        // (Token's open commitments ride along into op_input).
        // The verifier-side equivalent is `String::from(cell.to_bytes())`.
        program = program.push_str(String::cell(cell));
        program = program.input();
        // taproot_proof pieces.
        program = push_taproot_proof_to_program(program, &cp);
        // A generous gas operand, then push:0 args, open.
        // After open, the parent stack has [Token, k=1, success=1]:
        // verify pops the success marker (hard-fail if 0), drop
        // discards the count, leaving just the Token for mix.
        program = program
            .push_int(1024u64)
            .push_int(0u64)
            .open()
            .verify()
            .drop_();
    }

    //                    commit String (witness-bearing). ──
    for out in outputs {
        let (q_open, f_open) = open_commitments_for_output(out);
        program = program
            .push_str(String::commitment(q_open))
            .push_str(String::commitment(f_open));
    }

    //
    // `op_mix` pops `n` first (top of stack), then `m`. The spec
    // notation `m n → values` reads bottom-to-top, so the
    // canonical push order is m then n.
    program = program
        .push_int(inputs.len() as u64) // m — input count
        .push_int(outputs.len() as u64) // n — output count (top)
        .mix();

    //
    // After `mix`, the stack is [O_0, O_1, …, O_{M-1}] (bottom →
    // top, matching the `outputs[]` spec order). `op_output` pops
    // its k=1 payload from the TOP — which would naively pair
    // `outputs[0].predicate` with `O_{M-1}`'s commitments,
    // reversing every multi-output transfer.
    //
    // Fix: for each `i` in spec order, roll `outputs[i]`'s token
    // to the top first, then emit. The token at position
    // `(M-1-i)` from the top is the one whose qty/flv
    // commitments were the i-th pair pushed before `mix` — i.e.
    // outputs[i]. `roll:0` is a no-op for the last iteration.
    for i in 0..outputs.len() {
        let k = outputs.len() - 1 - i;
        if k > 0 {
            program = program.roll_k(k as u8);
        }
        program = program
            .push_int(1u64)
            .push_point(*output_predicate_point(outputs[i].predicate_tag).as_bytes())
            .output();
    }
    program
}

/// Build the Open `(qty, flv)` commitments for an input spec.
pub(crate) fn open_commitments(inp: &NMInputSpec) -> (Commitment, Commitment) {
    let q = Commitment::blinded_with_factor(Int253::from(inp.qty), Scalar::from(inp.qty_blind));
    let f = Commitment::blinded_with_factor(Int253::from(inp.flv), Scalar::from(inp.flv_blind));
    (q, f)
}

/// Build the Open `(qty, flv)` commitments for an output spec.
pub(crate) fn open_commitments_for_output(out: &NMOutputSpec) -> (Commitment, Commitment) {
    let q = Commitment::blinded_with_factor(Int253::from(out.qty), Scalar::from(out.qty_blind));
    let f = Commitment::blinded_with_factor(Int253::from(out.flv), Scalar::from(out.flv_blind));
    (q, f)
}

/// Like `push_taproot_proof_pieces` but emits Instructions into a
/// ScriptBuilder (so the prover keeps witness-bearing variants).
pub(crate) fn push_taproot_proof_to_program(
    mut program: ScriptBuilder,
    cp: &TaprootProof,
) -> ScriptBuilder {
    program = program.push_point(*cp.internal_key.as_bytes());
    // Neighbors as a list-style Dict: for each neighbor, push
    // (val, key); then push n, dict.
    for (i, h) in cp.neighbors.iter().enumerate() {
        program = program
            .push_str(String::from(h.to_vec()))
            .push_int(i as u64);
    }
    program = program.push_int(cp.neighbors.len() as u64).dict();
    program = program
        .push_str(String::from(cp.position.clone()))
        .push_str(String::from(cp.program.clone()));
    program
}

/// Build the input cell + the matching `TaprootProof` for an input
/// spec. Used by both `build_confidential_nm_program` (to
/// produce the cell bytes pushed onto the stack) and
/// `assert_nm_txlog` (to compute the expected `cell_id` for the
/// `TxEntry::Input` assertion).
///
/// Uses `PredicateTree::scripts_only` (NUMS-unspendable
/// internal key) so the cell is openable only by the empty
/// script leaf — never key-path. We derive the tree's blinding
/// key from `inp.anchor`, so every input cell carries a
/// *distinct* predicate point. Without per-cell variation the
/// harness would only exercise the "all inputs locked under
/// the same predicate" shape, which doesn't match real
/// transactions where each input comes from its own keypair.
pub(crate) fn build_input_cell(inp: &NMInputSpec) -> (Cell, TaprootProof) {
    let (q_open, f_open) = open_commitments(inp);
    let token = Token::new(q_open, f_open);
    // Leaf script `push:1, return` — under ADR 0013 the opened cell
    // runs in an isolated frame, so the leaf must explicitly return
    // its single-Token payload to the caller.
    let leaf = vec![0x01, 0xa4];
    let tree = PredicateTree::scripts_only(vec![leaf], input_blinding_for(inp))
        .expect("scripts_only tree builds");
    let cp = tree.taproot_proof_for(0).expect("taproot_proof for leaf 0");
    let pred_point = tree.point;
    let cell = Cell::new(
        Predicate::opaque(pred_point),
        Anchor(inp.anchor),
        vec![Value::Token(token)],
    )
    .expect("payload is portable");
    (cell, cp)
}

/// Derive a per-input PredicateTree blinding key from
/// `inp.anchor`. Just XOR-ing the anchor into
/// `TEST_BLINDING_KEY` gives every distinct input spec its own
/// tree without exposing a new field on `NMInputSpec`. The
/// blinding key is consensus-irrelevant to the test (it only
/// affects predicate-point determinism); we just need *some*
/// per-cell variation.
pub(crate) fn input_blinding_for(inp: &NMInputSpec) -> [u8; 32] {
    let mut k = TEST_BLINDING_KEY;
    for (i, byte) in k.iter_mut().enumerate() {
        *byte ^= inp.anchor[i];
    }
    k
}

/// Strong txlog assertion: every `TxEntry::Input` matches the
/// corresponding input cell's `id()`, every `TxEntry::Output`
/// has the predicate point + Token qty/flv commitment points
/// the spec asked for. Catches the predicate/token-pairing
/// inversion that a naive `txlog.len()` check would miss.
pub(crate) fn assert_nm_txlog(result: &TxResult, inputs: &[NMInputSpec], outputs: &[NMOutputSpec]) {
    // Length check first — keeps the messages short on shape
    // bugs (wrong count) before walking entry-by-entry.
    assert_eq!(
        result.txlog.len(),
        1 + inputs.len() + outputs.len(),
        "txlog length must be Header + N inputs + M outputs"
    );
    // Header at index 0.
    assert!(matches!(result.txlog[0], TxEntry::Header(_)));
    // Inputs at [1..=N], in spec order. The cell_id check is
    // load-bearing — it pins down predicate + anchor + payload
    // bytes all at once.
    for (i, inp) in inputs.iter().enumerate() {
        let expected_id = build_input_cell(inp).0.id();
        match &result.txlog[1 + i] {
            TxEntry::Input(id) => {
                assert_eq!(*id, expected_id, "txlog[{}] input cell_id mismatch", 1 + i)
            }
            _ => panic!("txlog[{}] must be Input", 1 + i),
        }
    }
    // Outputs at [1+N .. 1+N+M], in spec order. We verify:
    //
    //   - predicate point             (catches pairing inversion)
    //   - Token qty/flv commitment points
    //   - cell anchor                 (catches anchor-chain bugs)
    //
    // The anchor chain seeds at the LAST input's ratcheted
    // anchor (op_input overwrites `last_anchor` on each input,
    // so after the N-th input it equals `inputs[N-1].to_anchor()`).
    // Each output's anchor is the *previous cell*'s ratcheted
    // anchor; we walk forward as we go.
    //
    // The cell_id is hash(predicate, anchor, payload), so
    // asserting all three pins down the cell_id without
    // recomputing it.
    //
    // Anchor chain (split-at-every-call design): after each input's
    // `open`, the parent's anchor is split — the right half stays in
    // the parent. So after the last input's `input` then `open`, the
    // running anchor is `Anchor(last_input.id()).split().1`. Each
    // output then splits it again: left → output.anchor, right →
    // next iteration's parent.
    let last_input_id = build_input_cell(inputs.last().expect("at least one input"))
        .0
        .id();
    let (_, mut expected_anchor) = Anchor(last_input_id).split();
    for (j, out) in outputs.iter().enumerate() {
        let expected_pred = output_predicate_point(out.predicate_tag);
        let (q_open, f_open) = open_commitments_for_output(out);
        let idx = 1 + inputs.len() + j;
        match &result.txlog[idx] {
            TxEntry::Output(c) => {
                assert_eq!(
                    c.predicate.to_point(),
                    expected_pred,
                    "output[{}] predicate mismatch (idx {})",
                    j,
                    idx
                );
                // Split the running parent anchor; the left half is
                // what this output's `anchor` field commits to, the
                // right half becomes the next iteration's parent.
                let (split_left, split_right) = expected_anchor.split();
                assert_eq!(
                    c.anchor.0, split_left.0,
                    "output[{}] anchor mismatch (idx {})",
                    j, idx
                );
                expected_anchor = split_right;
                assert_eq!(
                    c.payload().len(),
                    1,
                    "output[{}] payload must contain exactly 1 Token",
                    j
                );
                match &c.payload()[0] {
                    Value::Token(t) => {
                        assert_eq!(
                            t.qty.to_point(),
                            q_open.to_point(),
                            "output[{}] qty commitment point mismatch",
                            j
                        );
                        assert_eq!(
                            t.flv.to_point(),
                            f_open.to_point(),
                            "output[{}] flv commitment point mismatch",
                            j
                        );
                    }
                    _ => panic!("output[{}] payload[0] must be Token", j),
                }
            }
            _ => panic!("txlog[{}] must be Output", idx),
        }
    }
    // No `signtx` / `signcall` in the N→M matrix → deferred_sigs
    // empty on both sides. No `send` yet → sends empty.
    // Pinning these guards against future opcode misroutes
    // silently emitting spurious deferred records or send queue
    // entries during the harness's prove/verify round-trip.
    assert!(
        result.deferred_sigs.is_empty(),
        "confidential N→M matrix tests must not emit deferred sigs"
    );
    assert!(
        !result.txlog.iter().any(|e| matches!(e, TxEntry::Send(_))),
        "confidential N→M matrix tests must not emit sends"
    );
}

/// Drive the full prove-then-verify round trip for an N→M
/// confidential transaction and assert the full txlog shape +
/// per-cell contents. Single entry point for every positive
/// matrix test.
///
/// `mem_limit = 0` keeps this test helper unmetered. Production zero-limit
/// and nested-frame semantics are tracked separately in plan item 6.
pub(crate) fn run_confidential_nm(inputs: &[NMInputSpec], outputs: &[NMOutputSpec]) -> TxResult {
    let pc_gens = PedersenGens::default();
    let program = build_confidential_nm_program(inputs, outputs);
    let prover_result =
        Prover::prove(&pc_gens, program, dummy_header(), 1_000_000).expect("prove ok");
    let txid_p = prover_result.txid;
    // The prover-side TxResult already exposes the full txlog —
    // assert against it so any divergence between prover and
    // verifier views is independently visible.
    assert_nm_txlog(&prover_result, inputs, outputs);
    let TxResult {
        bytecode, proof, ..
    } = prover_result;
    let proof = proof.expect("proof set");
    let pc_gens_v = PedersenGens::default();
    let result = Verifier::verify(
        &pc_gens_v,
        bytecode,
        &proof,
        dummy_header(),
        1_000_000,
        None,
    )
    .expect("verify ok");
    assert_eq!(result.txid, txid_p, "prover/verifier TxID agree");
    // Verifier-side txlog must match exactly already
    // covers TxID determinism, but this catches any future
    // divergence in the txlog content itself.
    assert_nm_txlog(&result, inputs, outputs);
    result
}
