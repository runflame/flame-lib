# FlameVM CS-bootstrap audit — 2026-05-24

**Scope**: Phase-11 architecture (Prover/Verifier delegates, Instruction
layer, Program builder, VM loop refactor) compared against zkvm and
similar blockchain VMs with confidential transfers (Mimblewimble/Grin,
Solana's BPF, Cosmos's CosmWasm, Bitcoin Script, FVM/Lurk).

**Audit method**: walk each architectural concern from the Phase-11
plan, mark concordance with zkvm and divergences (intentional or
otherwise), then survey similar systems for design alternatives we
should track.

---

## 1. Instruction layer (one variant per opcode + witness slots)

### What we did
- `flamevm/src/ops.rs::Instruction` — full ~60-variant enum mirroring
  every opcode in the spec.
- `Alloc(Option<Int253>)` is the sole witness-bearing variant; `encode()`
  discards the witness (verifier sees `Alloc(None)`).
- `PushInt(Int253)` consolidates `push:k` / `pushint8/16/64/128/full`
  by picking the narrowest opcode at encode time.
- `Ext(u8)` catches unknown opcode bytes for forward-compat (so a
  future protocol revision can add opcodes without breaking older
  verifiers).

### How zkvm does it
- `zkvm/src/ops.rs::Instruction` — variant per opcode (zkvm has fewer:
  no Dict, no String ops, no Token quartet, no Cell repackaging
  guards). zkvm uses `Push(String)` with a rich `String` enum (see
  §3 below) rather than `PushInt(Int253)`.
- `Alloc(Option<ScalarWitness>)` — same pattern; witness discarded
  by encode, restored only on the prover side.
- `Ext(u8)` for extension opcodes — direct match.

### How similar systems do it
- **Mimblewimble (Grin / Beam)**: no opcodes at all. Transactions
  carry only Pedersen commitments, range proofs, and aggregated
  Schnorr signatures. Programs aren't a thing. Our Instruction
  layer is closer to Bitcoin Script / Cardano-Plutus's lineage.
- **Bitcoin Script**: opcodes encode `OP_X(...)` directly in
  bytecode; no separate "Instruction" enum at the prover level
  because Bitcoin has no zero-knowledge proofs to construct.
- **Solana BPF / Cosmos CosmWasm**: WASM bytecode is the
  serialization. No domain enum like ours. They rely on the WASM
  parser to walk instructions and a richer runtime to bind witness
  data (where applicable).
- **FVM (Filecoin VM) / Lurk**: Lurk programs are RISC-style
  bytecode for a SNARK prover. There's a parsing/lowering step that
  resembles our Instruction parse, but witness lives in a separate
  "private input vector" — closer to our pre-refactor side-channel
  than to our current in-Instruction witness.

### Verdict
✅ Matches zkvm. The `Ext(u8)` extension hatch is a known-good Bitcoin
pattern. `PushInt(Int253)` collapsing five push-width opcodes into one
typed variant is cleaner than zkvm (which exposes the width to
authoring) but functionally equivalent.

---

## 2. Program builder + ProgramItem

### What we did
- `Program(Vec<Instruction>)` — append-only builder with ~60 fluent
  methods.
- `ProgramItem { Bytecode(Vec<u8>), Program(Program) }` —
  verifier's view vs prover's view, identical wire encoding.
- `Program::to_bytecode()` + `to_witnesses()` derive both
  representations from the same source.
- `Program::parse(bytes)` parses bytecode back into Instructions
  with `None` witnesses (verifier's reconstruction).

### How zkvm does it
- `zkvm::program::Program` — same shape, fluent builder with
  per-opcode methods.
- `ProgramItem` — same `Bytecode / Program` split, used by `Delegate::new_run`
  to select prover vs verifier RunType.

### How similar systems do it
- **Bitcoin**: `bitcoin::Script` is `Vec<u8>` with `Builder` helpers;
  no separate prover-form because there's no proof to build.
- **Plutus (Cardano)**: programs are typed ASTs (PLC). The on-chain
  form is serialized CBOR. The chain executes the AST; the prover/
  verifier asymmetry doesn't apply since Plutus is deterministic
  and re-evaluated by validators.
- **Mina (snarkyjs)**: circuits are authored as code-generated
  R1CS. There's no programmable VM — the prover's program IS the
  circuit. Our Program is more flexible (one program, two views).

### Verdict
✅ Matches zkvm. The pattern is well-trodden and avoids forcing the
verifier to allocate the rich-witness types.

---

## 3. Witness flow — Instruction-embedded vs rich `String`

### What we did
- **Single witness path** so far: `Instruction::Alloc(Option<Int253>)`
  carries one scalar witness per alloc opcode.
- The flat `String` (just `Vec<u8>`) means `commit` / `scalar` /
  `unblind` opcodes — which in zkvm would consume a witness-bearing
  String off the stack — are deferred to Phase 13.

### How zkvm does it
- **Dual witness paths**:
  1. `Instruction::Alloc(Option<ScalarWitness>)` — same as ours.
  2. Rich `String` enum: `String::Commitment(Box<Commitment>)`,
     `String::Scalar(Box<ScalarWitness>)`, `String::Predicate(Box<Predicate>)`,
     `String::Output(Box<Contract>)`. Witnesses ride the stack as
     part of String values; `commit`/`unblind`/etc. downcast to
     extract them.
- The dual-path design is crucial for opcodes that consume witnesses
  from the stack after arbitrary `dup`/`roll` rearrangements — the
  witness travels with the value.

### Risk surface
**M1 (medium)**: Phase 11 supports only one CS opcode (`alloc`) for
witness flow. The `expr` opcode is wired but `op_expr` calls
`commit_variable` which returns `WitnessMissing`. So `expr` is
effectively a stub today. This is documented but easy to miss when
reviewing the public API.

**Recommendation**: doc-comment `expr`'s Phase-13 dependency at the
opcode definition site (already done in spec.md row 0x5d should
probably reference Phase 13).

### Verdict
⚠️ Partial vs zkvm. Acceptable as Phase 11 MVP; Phase 13 closes the
gap. No protocol-correctness implications today since the missing
opcodes have no production use.

---

## 4. VM loop architecture

### What we did
- `Run` is an enum:
  - `Bytecode { script, pc }` — verifier and internal contexts parse
    Instructions on the fly.
  - `Queue { instructions, index }` — prover walks pre-decoded
    Instructions with witnesses attached.
- Dispatch is **Instruction-driven**:
  - `step_external<D>` and `step_internal` both call
    `current_run.next_instruction()` and route through
    `dispatch_external(instr, delegate)` or `dispatch_internal(instr)`.
  - Expression / Constraint overloads of `add`/`mul`/`eq`/`neg`/`verify`
    are intercepted in `dispatch_external` by peeking at the top of
    stack.
- `loop` (rewind cursor) and `break:k` (jump to end) work
  identically for both Run variants via the trait-like methods
  `rewind()` and `jump_to_end()`.

### How zkvm does it
- `Delegate` is generic over a `RunType` associated type. ProverRun
  is `VecDeque<Instruction>`; VerifierRun is `(Vec<u8>, offset)`.
  Delegate provides `next_instruction(&mut Self::RunType)`.
- VM loop: `while let Some(instr) = delegate.next_instruction(...)`.

### Divergence from zkvm
- We avoid the generic-RunType pattern. Instead, `Run` is a concrete
  enum that knows both shapes. Pros: no generic propagation through
  `CallFrame`/`VM`; works for both external and internal contexts
  (zkvm has no internal context). Cons: `Run::Queue` is unused in
  internal context — slight code dead-weight.
- We don't have `loop` semantically; zkvm omits `loop` too (their
  `Call` opcode subsumes it). FlameVM's `loop` works correctly for
  both bytecode and queue runs.

### Risk surface
**L1 (low)**: A maliciously-crafted prover could construct a Queue
Run with an `Alloc(Some(...))` and a verifier walking the bytecode
sees `Alloc(None)`. The CS is constructed differently, but the proof
must commit to the resulting transcript — so the asymmetry doesn't
let the prover lie. This is the same invariant zkvm relies on.

**L2 (low)**: The new `Run::next_instruction` parses one Instruction
per step. Parsing failures (malformed bytecode) now propagate as
`VMError::UnexpectedEndOfScript` / `VMError::InvalidInt253Encoding`
rather than per-handler errors. We preserved the exact error codes
through `Instruction::parse` returning `VMError` directly. Cross-
checked against the `pushint_full_rejects_negative_zero` test that
exercises this path.

### Verdict
✅ Architecturally equivalent to zkvm, with a simpler concrete `Run`
enum in place of zkvm's generic RunType. The Expression-overload
peek-at-stack-top pattern in `dispatch_external` is novel (zkvm
dispatches purely by Instruction variant) — works because FlameVM
has separate add/mul/eq opcodes that share variants across Int253
and Expression. **Worth re-examining if peek-then-dispatch ever
behaves differently from intended on edge cases** (e.g., stack
underflow at peek time).

---

## 5. Prover / Verifier API

### What we did
- `Prover<'g>` wraps `r1cs::Prover<'g, Transcript>` + `BulletproofGens(64, 16)`.
  Public entry: `Prover::prove(pc_gens, program, header, gas, mem) ->
  (bytecode, proof, result, sigs)`. Lifecycle:
  1. Build the prover.
  2. Run the program via `VM::run_external_program` (Run::Queue).
  3. `into_proof()` calls `r1cs::Prover::prove(&bp_gens)` and returns
     the `R1CSProof`.
- `Verifier` wraps `r1cs::Verifier<Transcript>` + the same generators.
  Public entry: `Verifier::verify(pc_gens, bytecode, proof, header,
  gas, mem) -> (result, sigs)`. Lifecycle:
  1. Build the verifier.
  2. Run the bytecode via `VM::run_external` (Run::Bytecode).
  3. `verify_proof(proof, pc_gens)` calls `r1cs::Verifier::verify(...)`.
- Transcript label `flamevm.r1cs.v1` is consensus-fixed.

### How zkvm does it
- `Prover::build_tx(program, header, bp_gens) -> UnsignedTx` — same
  pattern, returns serialized bytecode + proof bundled into an
  `UnsignedTx` that's then signed externally.
- `Verifier::precompute(tx) -> PrecomputedTx` followed by
  `verify_tx(precomputed, bp_gens) -> VerifiedTx` — split into two
  phases for batched signature verification.

### Divergence from zkvm
- We don't yet have an `UnsignedTx` / `PrecomputedTx` envelope.
  Phase 14 (signatures) and Phase 17 (txlog → TxID → tx assembly)
  will introduce them.
- `bp_gens` is owned by the Prover/Verifier rather than passed in
  per-call. zkvm shares a singleton `bp_gens` across the program.
  Phase 11 doesn't optimize this; if proof construction becomes a
  hot path, pass `bp_gens` in.

### Risk surface
**M2 (medium)**: Transcript label divergence between Prover and
Verifier would silently invalidate every proof. Cross-check: both
files use `Transcript::new(b"flamevm.r1cs.v1")`. **No automated test
covers this invariant** — recommend a test that fails if the labels
diverge (e.g., `assert_eq!` on the literal byte strings between
prover.rs and verifier.rs at compile time, or a positive prove+verify
test for every CS phase).

The current `prove_then_verify_*` tests transitively guard this — any
label divergence would break them. So acceptable for Phase 11.

**M3 (medium)**: `BulletproofGens::new(64, 16)` is duplicated in
both files. If one grows and the other doesn't, proofs silently
fail. Same recommendation: lift to a shared constant or pass in.

### Verdict
✅ Architecturally clean. The duplication of generators-and-label is
a code-quality issue, not a correctness issue (it's caught by
end-to-end tests). Track for Phase 14 refactor.

---

## 6. Cross-VM comparison: confidential-value handling

| System | Quantity representation | Range proof? | Equality enforced via |
|---|---|---|---|
| FlameVM (us) | Pedersen commitment in `Token { qty, flv }` | yes (Phase 12 `range`) | `eq` Expression-overload → R1CS constraint |
| zkvm | `Value { qty: Commitment, flv: Commitment }` | yes (their `range` op) | `eq` Expression → Constraint |
| Mimblewimble | Pedersen commitment (no flavor) | yes (Bulletproofs) | excess signature over sum-of-commits |
| Monero (RingCT) | Pedersen commitment | yes (Bulletproofs) | balance proof + range proofs |
| Mina | Snark over an AST | (whole circuit) | algebraic |
| Solana SPL Token Confidential | Pedersen + ElGamal hybrid | yes | ZK proof of equality |
| Bitcoin (no confidential) | u64 cleartext | n/a | none — txns are public |

**Observations**:

1. We're in the Pedersen + Bulletproofs lineage, same as zkvm and
   Grin. The cleartext-vs-encrypted ambiguity (a Token whose
   commitments are `Open(unblinded)` is effectively cleartext but
   syntactically encrypted) is **the same as zkvm's** and is
   well-precedented.

2. The Token/ClearToken/WideToken trichotomy is a FlameVM-specific
   refinement of zkvm's `Value/ClearValue/WideValue`. We made
   ClearToken non-portable when qty < 0 (matching design.md's
   debt-token semantics), and WideToken non-constructible in Phase 11
   (deferred to Phase 12 `borrow` and Phase 13 `cloak`).

3. The flavor scalar (per-asset identity) is a Mimblewimble
   innovation extended in zkvm — Grin doesn't have multi-asset
   support natively. Our `flavor_from_actor(actor, tag)` Merlin
   transcript binds a flavor to an actor-id × tag pair, which is
   tighter than zkvm's predicate-based binding.

---

## 7. Spec adherence

Spec rows for token / CS opcodes were updated in earlier phases
(Phase 8 + Phase 11 MVP). After this Phase-11 refactor:

- **No spec changes**: dispatcher behavior on every opcode is
  semantically identical to before. The refactor is internal — the
  bytecode wire format and the resulting stack effects are
  unchanged.
- **One subtle change**: errors from malformed bytecode now flow
  through `Instruction::parse`'s `Result<Instruction, VMError>`.
  Verified that `InvalidInt253Encoding` still surfaces correctly
  via the `pushint_full_rejects_negative_zero` test.
- **Untested in this audit**: a malicious bytecode with a truncated
  string after `pushstr` opcode — does the parser correctly surface
  `UnexpectedEndOfScript`? Spot-checked one case
  (`parse_pushstr_with_long_payload`) but no negative
  truncation test for strings. **Recommend** adding
  `parse_pushstr_truncated_errors` in Phase 12.

---

## 8. Pending vulnerability surface

### Confirmed safe (Phase 11 hardening)

- ✅ Bytecode → Instruction parser canonicalizes int widths (no
  ambiguity between two equivalent encodings).
- ✅ Verifier walks bytecode independently — cannot be tricked by a
  prover-side rich Instruction stream.
- ✅ R1CS transcript labels match between prover and verifier
  (covered transitively by every `prove_then_verify_*` test).
- ✅ Negative tests confirm tampered proofs are rejected
  (`prove_succeeds_but_verify_fails_on_tampered_proof`) and
  unsatisfiable constraints are rejected
  (`prove_fails_for_unsatisfiable_equality`).

### Open items for future audits

| Item | Phase | Impact | Notes |
|---|---|---|---|
| `commit_variable` returns `WitnessMissing` from Phase 11 prover/verifier | 13 | M | Wire when rich `String` lands |
| `BulletproofGens(64, 16)` duplicated between prover/verifier | 14 | L | DRY refactor |
| Run::Queue + Cell::open program substitution | 13–15 | M | Cell-open programs are bytecode today; prover-side witnesses for cell programs would need rich `String::Program` variant |
| Sub-varint U64 branch overflow (pre-existing Finding 1) | 14 | M | Per audit `2026-05-22-initial-surface-sweep.md` |
| Deferred-sig batch verification | 14 | H | The `DeferredSig::TxBound` / `Explicit` records accumulate but aren't checked at finalize yet |
| TxID computation | 17 | H | Txlog → TxID hash → transcript binding pending |
| Memory cap (`4× vbytes`) enforcement | 15+ | M | `mem_limit` plumbed but `mem_used` never increments |
| Gas metering | 17 | M | `gas_limit` plumbed but never charged |

---

## 9. Recommendations

1. **Phase 12 must include**: rich `String` enum design discussion
   (architect ADR), even if not yet implemented. The Phase-11
   `commit_variable` stub commits us to that direction.

2. **Test coverage**: add a single "label-match" smoke test that
   constructs Prover and Verifier independently and asserts they
   accept each other's empty-program proof. Cheap canary against
   accidental label drift.

3. **Wire format documentation**: spec.md should note that
   `Instruction::parse` is canonical — any bytecode that round-trips
   through parse+encode must produce identical bytes (this property
   is tested in `ops.rs::tests::pushint_picks_smallest_width` for
   PushInt; should be extended to a comprehensive fuzz target).

4. **Style alignment with zkvm**: consider renaming
   `dispatch_common` → `dispatch` and `dispatch_external` →
   `dispatch_external` (already named that), since zkvm has a single
   `dispatch` per its single context. Bikeshed — current names are
   clear.

5. **Audit follow-up**: schedule a re-audit after Phase 14
   (signatures) lands, since `DeferredSig::TxBound` semantics
   interact with both `signtx` (Phase 9) and the transcript label
   chosen here.

---

## 10. Sign-off

Phase 11 CS bootstrap is architecturally sound and consistent with
zkvm's design for the parts that are present. Deferred items
(`scalar` / `commit` opcodes + rich `String`) are correctly scoped
to Phase 13. No high-severity findings in scope. Two medium findings
(M1: `expr` is effectively a stub; M3: generators/label duplication)
recommended for Phase-14 follow-up.

— vm-engineer (self-audit, 2026-05-24)
