# FlameVM implementation plan

The canonical execution plan for the FlameVM crate. Self-sufficient;
read this and `flamevm/spec.md` together to know what the VM is and
what's left to build.

---

## Status overview

| #  | Phase                                                              | Status |
|----|--------------------------------------------------------------------|--------|
| 1  | Skeleton — VM core, CallFrame, Run, dispatch loop, `nop`           | ✅      |
| 2  | Stack literals & manipulation                                      | ✅      |
| 3  | Control flow & explicit return                                     | ✅      |
| 4  | Int253 arithmetic, logic, size                                     | ✅      |
| 5  | String ops                                                         | ✅      |
| 6  | Dict ops                                                           | ✅      |
| 7  | Hash & Merlin                                                      | ✅      |
| 8  | Cells + open + signtx + signrun                                    | ✅      |
| 9  | Inputs (stateless VM) + Cell wire encoding                         | ✅      |
| 10 | Tokens (port from zkvm) + clear-only opcodes                       | ✅      |
| 11 | CS bootstrap (real Prover/Verifier, Instruction enum, Program)     | ✅      |
| 12 | Range proofs + Constraint composition                              | ✅      |
| 13 | Rich `String` + `scalar` / `commit` / `decrypt`                    | ✅      |
| 14 | Encrypted `borrow` + `mix` cloak gadget                            | ✅      |
| 15 | Batch verifier + `Explicit` deferred sigs                          | ✅      |
| 16 | TxID merkle root + `log` opcode                                    | ✅      |
| 17 | Hygiene sweep + ADR housekeeping                                   | ✅      |
| 18 | `TxEntry::Header` + TxID transcript binding                        | ✅      |
| 19 | `op_fee` + `CheckedFee` accumulator                                | ✅      |
| 20 | TxBound multi-sig batch verification                               | ✅      |
| 21 | `TxResult` shape + finalize return values                          | ✅      |
| 22 | Gas-cost table + per-op charging                                   | ⏳      |
| 23 | Block resource pools (B_par : B_ser)                               | ⏳      |
| 24 | `ActorState` + `ActorRegistry` (real, not stub)                    | ⏳      |
| 25 | `op_load` + `op_save`                                              | ⏳      |
| 26 | `op_call` + frame creation                                         | ⏳      |
| 27 | Re-entrancy guard + memory-cap allocator                           | ⏳      |
| 28 | Actor lifecycle: grace + freeze + maturity                         | ⏳      |
| 29 | Introspection: header + resources (7 opcodes)                      | ⏳      |
| 30 | Introspection: identity (4 opcodes)                                | ⏳      |
| 31 | Chain info (6 opcodes)                                             | ⏳      |
| 32 | `op_send` + `TxEntry::Send` + send queue                           | ⏳      |
| 33 | Encrypted `issue` (after ADR)                                      | ⏳      |
| 34 | Fuzz targets + canonicality sweeps                                 | ⏳      |
| 35 | spec.md + design.md sync + ADR backfill                            | ⏳      |
| 36 | End-to-end integration tests                                       | ⏳      |

**21 of 36 complete (58 %).** Total test count: 417 passing; build
clean; 4 leftover compiler warnings, all targeted by Phases 24 / 26 /
27 / 30.

---

# Section 1 — Status review

## Phases complete

| #  | Phase                                              | Tests | Highlights |
|----|----------------------------------------------------|-------|------------|
| 1  | Skeleton                                           | smoke | `VM`, `CallFrame`, `Run`, `CallKind`, dispatch loop, `0x1d nop`. |
| 2  | Stack literals & manipulation                      | 24    | push:k / pushint{8,16,64,128,full} / pushstr / pushpoint / pushtoken / drop / dup / dup:k / roll / roll:k. |
| 3  | Control flow & explicit return                     | 22    | verify / run / loop / switch / return / break:k / type. Clean-stack rule enforced. |
| 4  | Int253 arithmetic, logic, size                     | 30    | abs / eq / neg / add / mul / divmod / mod252 / not / and / or / size. |
| 5  | String ops                                         | 24    | readbits / readint / readstr / readpoint / writebits / writeint / append / writezeros / bitnot / bitor / bitand / bitxor / shiftleft / shiftright / keccak256. |
| 6  | Dict ops                                           | 24    | dict / put / replace / get / getopt / getdup / first / last / next. Sticky copy/portability flags. |
| 7  | Hash & Merlin                                      | 11    | merlin / merlinwrite / merlinread / sha256 / sha512 / sha3. |
| 8  | Cells + open + signtx/signrun                      | 11    | Run-level cell-open, Taproot `PredicateTree` with blinded sibling leaves and NUMS internal key. |
| 9  | Inputs + Cell wire encoding                        | 14    | `input` opcode decodes wire-cell from String. Cell encode/decode canonical. No Utreexo trait inside VM. |
| 10 | Tokens (port) + clear-only opcodes                 | 35    | amount / issue (cleartext) / retire / borrow (cleartext) / merge / split / issueflv. `flavor_from_actor`. |
| 11 | CS bootstrap (Prover / Verifier)                   | 9     | `Delegate` trait, `Instruction` enum (60 variants), `Program` builder, `ProgramItem`, dispatch on `Instruction`. End-to-end `alloc(7) + alloc(3) == alloc(10)` proves+verifies. |
| 12 | Range proofs + Constraint composition              | 12    | `range` opcode (dynamic n ∈ [1, 64]). `not` / `and` / `or` Constraint overloads. |
| 13 | Rich `String` + scalar/commit/decrypt              | 13    | `String` becomes enum (Opaque + Commitment + Scalar + Predicate). End-to-end prove+verify with witness-bearing stack values. |
| 14 | Encrypted `borrow` + `mix` cloak gadget            | 3     | `op_borrow_encrypted`. `op_mix` invokes `spacesuit::cloak`. `WideToken` now constructible. |
| 15 | Batch verifier + `Explicit` deferred sigs          | 2     | `Delegate::BatchVerifier`, `musig::BatchVerifier<ThreadRng>` on both sides. `Verifier::verify` batch-verifies Explicit sigs at finalize. `MultiscalarMul` deleted. |
| 16 | TxID merkle root + `log` opcode                    | 6     | `TxID::from_log` over txlog, domain `flamevm.txid.v1`. `MerkleItem for TxEntry`. `0x6f log` opcode. |

## What's done but incomplete

Items that work but have known follow-ups, ordered by severity:

| Area | Gap | Severity | Targeted in phase |
|---|---|---|---|
| TxID transcript binding | `TxID::from_log` exists; not yet bound into R1CS transcript before `prove`/`verify`. **Proof currently doesn't commit to the effect list.** | High | 18 |
| TxBound deferred sigs | `signtx` records `DeferredSig::TxBound { vk }` but no aggregate signature is checked at finalize. | High | 20 |
| `TxEntry::Header` not in TxID | TxID merkle root currently excludes `(version, locktime)`. Different headers, same effect list → same TxID. | Medium | 18 |
| `op_mix` missing zero-count guard | `m == 0 \|\| n == 0` panics `spacesuit::mix::k_mix` (`0..k-1` underflow). | Medium | 17 |
| `BulletproofGens(1024, 1)` re-allocated per `Prover`/`Verifier` | ~16 KB per instantiation; should be a singleton. | Medium | 17 |
| Encrypted `issue` | Spec says `qty: Point → Token`. Current code errors `TokenRequiresCS`. Needs architect ADR on actor-context-vs-CS-context. | Medium | 33 |
| `String::as_bytes` panic on witness variants | Sharp edge — documented but no CI lint. New opcodes might trip it. | Low | 17 (doc) |
| Bit-op on witness-bearing `String` drops witness | Documented but a programmer gotcha. | Low | 17 (doc) |
| `MultiscalarMul` row stale in `spec.md` | Type table still lists it; code deleted. | Low | 17 |
| `op_log` not in `spec.md` | Wired in code (0x6f) but spec row missing. | Low | 17 |
| `decrypt` uses default `PedersenGens` only | Future multi-gens use would need parameterization. | Low | (deferred) |
| `BatchSignatureVerificationFailed` is opaque | Doesn't say which sig failed. | Low | (accepted) |
| Sub-varint U64 branch overflow regression test missing | Pre-existing finding. | Low | 34 |

## Documented but unimplemented

Items present in `spec.md` or `design.md` with no code yet:

| Opcode / feature | Source | Phase |
|---|---|---|
| `0x94 send` | spec.md row | 32 |
| `0x95 call` | spec.md row | 26 |
| `0x96 load`, `0x97 save` | spec.md rows | 25 |
| `0x7a fee` | spec.md row | 19 |
| `0x9a timelock`, `0x9b version` | spec.md rows | 29 |
| `0x9c actorid`, `0x9d anchor`, `0xa0 callerid`, `0xa1 method` | spec.md rows | 30 |
| `0x9e gas`, `0x9f bytes`, `0xa2 gaslimit`, `0xa3 memlimit`, `0xa4 newbytes` | spec.md rows | 29 |
| `0xa5..=0xaa` chain-info opcodes | spec.md rows | 31 |
| Memory cap `4× vbytes` enforcement | design.md ADR 0002 | 27 |
| Gas charging per opcode | design.md §Resources / Gas | 22 |
| Block resource pools (`B_par : B_ser = 4:1`) | design.md §Block resource pools | 23 |
| Actor grace + freeze + 100-block maturity | design.md ADR 0005 | 28 |
| Atomic-external-tx semantics (already partial) | design.md commitment | 21 |
| Re-entrancy guard | design.md ADR 0003 | 27 |

## Architect ADR queue

Phases that need an ADR before implementation:

| Topic | Blocking phase | Notes |
|---|---|---|
| Encrypted `issue` semantics | 33 | Variable-only, Predicate-as-issuer, internal-only, or explicit-cid? |
| `MultiscalarMul` removal from spec.md type table | 17 | Code already removed; spec needs ADR-backed update. |
| `op_log` (0x6f) addition to spec.md | 17 | Already wired; needs spec row + ADR pointer. |
| Extension tag (255) policy | 34 (or earlier) | Reject vs reserve for soft-fork. Currently rejects. |
| Refund predicate execution context | 32 | Fresh micro-VM vs recoverable sub-call? |
| `BulletproofGens` singleton | 17 | Implementation detail; low ADR weight. |

## Audit findings status

Findings rolled up across all `audits/vm/*` reports:

| Source | Severity | Status | Targeted in phase |
|---|---|---|---|
| `expr` was effectively a stub | Medium | ✅ Resolved | (closed by Phase 13) |
| Prover/Verifier transcript label divergence risk | Medium | ✅ Covered transitively by tests | — |
| `BulletproofGens` duplicated | Medium | ⏳ | 17 |
| `BulletproofGens` size bump amplifies | Medium | ⏳ | 17 |
| Range proof / Constraint overload doc gaps | Low | ⏳ | 17 |
| `String::as_bytes` panic on witness variants | Medium | ⏳ | 17 |
| Bit-op witness loss | Low | ⏳ | 17 |
| `op_mix` zero-count panic | Medium | ⏳ | 17 |
| `BatchSignatureVerificationFailed` opaque | Medium | (accepted) | — |
| TxID not transcript-bound | High | ⏳ | 18 |
| `TxEntry::Header` missing from TxID | Medium | ⏳ | 18 |
| Sub-varint U64 branch overflow | Medium | ⏳ | 34 |

---

# Section 2 — Pending phases

## Phase 17 — Hygiene sweep + ADR housekeeping

**Goal**: knock out all known low-effort gaps + reduce the ADR
queue. No new opcodes.

**Items**:
- `op_mix` pre-check `m > 0 && n > 0` → new `MixDegenerate` error.
  Avoids `spacesuit::mix::k_mix` underflow panic.
- `BulletproofGens` singleton via `once_cell::sync::Lazy` (or
  `std::sync::OnceLock`) in `prover.rs` / `verifier.rs`. Saves
  ~16 KB per `Prover` / `Verifier` instantiation.
- `spec.md`: remove `MultiscalarMul` type-table row; add `0x6f log`
  row with full description.
- `spec.md`: refresh row `0x71 issue`, `0x73 borrow` with
  encrypted-branch notes (pointer to Phase 14 / 33).
- `String::as_bytes` panic risk: module-doc warning + an internal
  `String::ensure_opaque()` helper for new opcode authors.
- Doc-only: cross-link audit-findings list into this plan.

**Tests**: ~5 new (mix m=0 / n=0 rejection; singleton bp_gens
identity; spec/doc unchanged).

---

## Phase 18 — `TxEntry::Header` + TxID transcript binding

**Goal**: close the highest-severity audit finding — TxID is not
bound into the proof, so the proof currently doesn't commit to the
effect list.

**Items**:
- `TxEntry::Header(TxHeader)` variant. `MerkleItem::commit` absorbs
  `version` and `locktime` (u32 LE).
- Emit `TxEntry::Header` at the start of `Prover::prove` and
  `Verifier::verify` (first txlog entry).
- After `VM::run_external_program` / `run_external` returns,
  compute `TxID::from_log(&vm.txlog)`. Bind into the R1CS transcript
  via `cs.transcript().append_message(b"flamevm.txid", &txid.0)`
  before `cs.prove` / `cs.verify`.
- Update `VM::run_external` / `run_external_program` to return
  `(TxResult, Vec<DeferredSig>, Vec<TxEntry>)` so the prover /
  verifier can read the txlog.
- Determinism + tampering tests: different header → different
  TxID; tampered txlog → R1CS proof rejected.

**Tests**: ~6 new (header-changes-txid, txlog-tamper-rejects-proof,
header-deterministic, …).

---

## Phase 19 — `op_fee` + `CheckedFee` accumulator

**Goal**: spec-mandated fee opcode + per-tx accumulator.

**Items**:
- `0x7a fee`: `qty flv → widetoken`. Pops `qty: Int253`,
  `flv: Int253`. Records `TxEntry::Fee(u64)` (cleartext only
  initially). Pushes a `WideToken` debt with `-qty` (matches
  `borrow` -T pattern).
- `TxEntry::Fee(u64)` variant.
- `VM::total_fee: CheckedFee` accumulator. Increments on each
  `op_fee`; overflow → `FeeTooHigh`.
- `CheckedFee::add(u64) -> Result<(), VMError>` helper.
- spec.md row `0x7a` kept; refresh description.

**Tests**: ~4 new (fee accumulates, overflow rejected, fee in txlog
hash, dispatch-internal rejects).

---

## Phase 20 — TxBound multi-sig batch verification

**Goal**: close the second highest-severity finding — TxBound sigs
are recorded but never checked.

**Items**:
- Tx-envelope shape: `ExternalTx { header, script, signature:
  musig::Signature, proof }`. Already exists in `tx.rs`; wire up.
- `Verifier::verify` walks `deferred_sigs`, collects
  `TxBound { verification_key }` items, calls
  `signature.verify_multi_batched(&mut signtx_transcript,
  verifier.signtx_items, &mut verifier.batch)`. `signtx_transcript`
  bound to TxID (from Phase 18).
- `Prover::prove` cannot produce the multi-sig from witness alone
  — introduce `UnsignedTx` shape that surfaces `signtx_items` to
  an external signer.
- Architect ADR pointer: signing protocol is external to the VM;
  the VM only verifies.

**Tests**: ~5 new (multi-sig batches multiple TxBound sigs,
tampered key rejected, missing-sig rejected, single-TxBound case,
no-TxBound case).

---

## Phase 21 — `TxResult` shape + finalize return values

**Goal**: stable public API for downstream consumers.

**Items**:
- `TxResult { txid: TxID, txlog: Vec<TxEntry>, total_fee: u64,
  gas_used: u64, vbytes_used: u64, proof: Option<R1CSProof>,
  sends: Vec<Message> }`. (`Message` placeholder until Phase 32.)
- `Prover::prove` returns `TxResult` with `proof = Some(...)`;
  `Verifier::verify` returns `TxResult` with `proof = None` (already
  verified).
- Drop the multi-return tuple shape; everything in `TxResult`.
- Backwards-compat: `VM::execute_external` keeps its current
  signature.

**Tests**: ~3 (`TxResult` populated correctly for trivial program,
`TxResult.txid` matches between prover/verifier, `TxResult.txlog`
contents).

---

## Phase 22 — Gas-cost table + per-op charging

**Goal**: spec-mandated gas metering.

**Items**:
- `gas_cost(instruction: &Instruction) -> u64` table per design.md
  §Resources / Gas (specific costs TBD; start with uniform = 1,
  refine after benchmarks).
- `VM::charge_gas(amount: u64) -> Result<(), VMError>`: increments
  `current_call.gas_used`; errors `GasExhausted` if
  `gas_used > gas_limit`.
- Call `charge_gas` in dispatch loop before each opcode handler.
- `VMError::GasExhausted`.

**Tests**: ~6 (gas charged per op, exhaustion errors, refund on
clean call exit, gas across nested calls, no-charge on `Ext`,
leftover gas to parent).

---

## Phase 23 — Block resource pools (B_par : B_ser)

**Goal**: implement design.md §Block resource pools.

**Items**:
- `BlockResourcePools { par_gas_remaining: u64,
  ser_gas_remaining: u64 }` — initial ratio 4:1.
- VM exposes a hook for the consensus crate to deduct per-tx gas
  from the correct pool.
- VM doesn't enforce the pool itself (that's consensus); just
  provides accurate per-tx gas via `TxResult.gas_used`.
- design.md cross-reference noted as a consensus-VM seam.

**Tests**: ~3 (pool deduction shape, tx exceeding pool rejected
at consensus seam, ratio enforcement).

---

## Phase 24 — `ActorState` + `ActorRegistry` (real, not stub)

**Goal**: actor storage layer.

**Items**:
- `ActorState` type per design.md (`public: Dict`, `private: Dict`).
- `ActorRegistry` trait extended: `load_actor(actor_id) ->
  Result<ActorState>`, `save_actor(actor_id, state) -> Result<()>`,
  `mark_for_destruction(actor_id)`,
  `unmark_for_destruction(actor_id)`,
  `is_marked_for_destruction(actor_id) -> bool`.
- In-memory impl for tests; real impl is integrator territory.
- `ActorID` computation from constructor script per design.md.

**Tests**: ~5 (load/save round-trip, mark/unmark idempotent,
unknown actor errors).

---

## Phase 25 — `op_load` + `op_save`

**Goal**: spec opcodes `0x96` / `0x97`.

**Items**:
- `0x96 load`: `ø → dict`. Calls `registry.load_actor(current_actor)`
  + `registry.mark_for_destruction(current_actor)`. Pushes
  `Value::Dict(state)`.
- `0x97 save`: `dict → ø`. Pops Dict, calls
  `registry.save_actor(current_actor, dict)` +
  `registry.unmark_for_destruction(current_actor)`.
- Pair invariant: `load` without subsequent `save` errors
  `LoadWithoutSave` at call exit. Tracked via per-frame flag.
- `VMError::LoadWithoutSave`.

**Tests**: ~6 (load+save pair, load-without-save errors at exit,
double-load errors, save-without-load errors).

---

## Phase 26 — `op_call` + frame creation

**Goal**: synchronous actor-to-actor call.

**Items**:
- `0x95 call`: `args… k gas bytes method addr → results… k'`. Pops
  `k`, `gas`, `bytes`, `method`, `addr` (actor_id String).
- Looks up bytecode via `registry.resolve_method(actor, method)`.
- Creates a new `CallFrame` with
  `CallKind::ActorCall { actor, method, caller: current_actor }`,
  `gas_limit = popped_gas`,
  `mem_limit = registry.actor_vbytes(actor) * 4`,
  `newbytes = popped_bytes`.
- Pushes args onto the new frame's stack.
- On clean exit: refunds leftover gas via the existing
  `finish_call` machinery.

**Tests**: ~6 (call A → B with args, A → B → C clean exit, gas
refund, `ArgCount` mismatch errors, unknown method errors).

---

## Phase 27 — Re-entrancy guard + memory-cap allocator

**Goal**: design.md ADR 0002 (memory cap) + ADR 0003 (no
re-entrancy).

**Items**:
- `VM::check_no_reentry(target_actor) -> Result<()>` walks
  `iter::once(&current_call).chain(call_stack.iter())`; errors
  `ReentrancyDetected` if `actor()` matches.
- Called from `op_call` before frame creation.
- `VM::charge_mem(vbytes: u64) -> Result<(), VMError>` increments
  `current_call.mem_used`; errors `MemoryCapExceeded` if
  `mem_used > mem_limit`.
- Hooks in every allocator: `String::append`,
  `String::append_bytes`, `Dict::insert`, `Cell::new`,
  `Token::new`, `WideToken` paths.
- Per-call `mem_used` is transient (discarded on call exit).
- `VMError::ReentrancyDetected`, `MemoryCapExceeded`.

**Tests**: ~8 (A → B → A direct cycle → `ReentrancyDetected`;
A → B → C → A indirect cycle; recursion within one method
allowed; memcap on `String.append`; memcap on `Dict`; per-call
reset).

---

## Phase 28 — Actor lifecycle: grace + freeze + maturity

**Goal**: design.md ADR 0005.

**Items**:
- Per-block vbyte deduction (called by consensus crate after
  applying block).
- Freeze on `vbytes == 0`: subsequent `op_call` errors
  `ActorFrozen`. State preserved.
- Grace window = `min(active_blocks / 4, blocks_per_6_months)`.
  Top-up resets the counter.
- Elapse without top-up: state cleared, vbytes return to pool
  after 100 blocks (maturity).
- Pool re-introduction: 5000 vbytes / block (design.md
  commitment), adjustable up to 2× by supermajority.
- `VMError::ActorFrozen`.

**Tests**: ~6 (freeze on zero, grace counts down, top-up unfreezes,
expiry clears state, maturity delay, supermajority adjustment).

---

## Phase 29 — Introspection: header + resources (7 opcodes)

**Goal**: spec-mandated runtime introspection of tx header and
per-call resources.

**Items**:
- `0x9a timelock` (`ø → n {0|1}` — header.locktime + flag for
  height/timestamp).
- `0x9b version` (`ø → n` — header.version).
- `0x9e gas` (`ø → int` — `gas_limit - gas_used`).
- `0xa2 gaslimit` (`ø → int` — `gas_limit`).
- `0x9f bytes` (`ø → int` — actor's persistent vbytes; only
  meaningful in internal context).
- `0xa3 memlimit` (`ø → int` — `mem_limit`).
- `0xa4 newbytes` (`ø → int` — newbytes delivered with this call).
- All ~5 LOC each + 2 tests each.

**Tests**: ~14 (one positive + one negative per opcode).

---

## Phase 30 — Introspection: identity (4 opcodes)

**Goal**: actor identity / call context.

**Items**:
- `0x9c actorid` (`ø → string` — `current_call.kind.actor()`).
- `0x9d anchor` (`ø → string` — `current_call.kind.anchor()` for
  `InternalRoot` / `CellOpen`).
- `0xa0 callerid` (`ø → string` — caller actor id; all-zero if
  external).
- `0xa1 method` (`ø → int` — `current_call.kind.method()`).
- `CallKind` accessor methods (`anchor`, `method`, `caller`,
  `predicate`).
- All `OpcodeRequiresActorContext` from `ExternalRoot`.

**Tests**: ~8 (positive in valid context, ExternalOnly error from
wrong context).

---

## Phase 31 — Chain info (6 opcodes)

**Goal**: design.md Bitcoin coupling + spec rows `0xa5..=0xaa`.

**Items**:
- `BlockContext` populated from consensus crate (height,
  blockhash, blockburn, blockweight, blockrate, chainstate).
- `0xa5 height`, `0xa6 blockhash`, `0xa7 blockburn`,
  `0xa8 blockweight`, `0xa9 blockrate`, `0xaa chainstate`.
- 100-block maturity guard on all height-parameterized opcodes
  per design.md.
- `VMError::BlockHeightImmature` for queries on `h > current - 100`.
- `chainstate` returns a Dict with block stats.

**Tests**: ~8 (each opcode positive, maturity guard, chainstate
Dict shape).

---

## Phase 32 — `op_send` + `TxEntry::Send` + send queue

**Goal**: async message-send to actors.

**Items**:
- `0x94 send`: `args… k gas bytes method addr → ø`. Pops operands,
  debits gas/bytes from the caller frame.
- Builds `Message { target, method, caller, anchor, payload, gas,
  vbytes }` (`caller = current_actor` or `None` for external).
- `TxEntry::Send(MessageRef)` variant — opaque handle into
  `VM.sends`.
- `VM.sends: Vec<Message>` collector. `TxResult.sends` returned.
- Refund predicate: per spec, message has a bounce predicate for
  failures. Architect ADR required ("Open structural question —
  refund predicate context").

**Tests**: ~8 (send from External → `caller = None`, send from
Internal → `caller = Some`, gas debited, bytes debited,
gas-exceeds-budget rejects, payload preserved,
refund-predicate-shape).

---

## Phase 33 — Encrypted `issue` (after ADR)

**Goal**: close the last Token opcode gap.

**Architect ADR prerequisite**: resolve "actor context vs CS
context" question. Options:
1. Take `Variable` (drops the actor-context dependency).
2. Take explicit Predicate-as-issuer.
3. Make encrypted `issue` internal-only (actor context exists).
4. Take explicit `cid` String + Variable.

**Items (post-ADR)**:
- `dispatch_external` peek for Variable operand →
  `op_issue_encrypted`.
- Implementation: commits qty Variable via
  `delegate.commit_variable`; flv via `flavor_from_actor` (or
  explicit per ADR); range-proves qty (64-bit); emits
  `TxEntry::Issue`.
- Push `Token { qty, flv }`.

**Tests**: ~5 (encrypted issue prove+verify, range-overflow
rejected, unblinded equivalent of cleartext).

---

## Phase 34 — Fuzz targets + canonicality sweeps

**Goal**: harden against parser / encoder bugs.

**Items**:
- Fuzz target: `Instruction::parse` round-trip canonicality. Any
  bytes that parse-then-encode produce identical bytes.
- Fuzz target: `Cell::decode` accepts only canonical wire-cells
  (no second valid encoding).
- Fuzz target: sub-varint U64 branch overflow (closes the
  pre-existing finding).
- Fuzz target: `read_value` on arbitrary bytes never panics; only
  `Ok` / `Err`.
- Property test: `Program::to_bytecode → Program::parse` identity.
- Property test: `Cell::id` collision resistance on random
  fixtures.

**Tests**: ~5 fuzz targets + 4 property tests under `flamevm/fuzz/`.

---

## Phase 35 — spec.md + design.md sync + ADR backfill

**Goal**: every artifact reflects the current code.

**Items**:
- Walk every opcode row in `spec.md`; cross-check against current
  handler. Update wording, error codes, edge cases.
- Bump ADR index in `design.md`:
  - `MultiscalarMul` removal.
  - `op_log` opcode addition.
  - Encrypted `issue` semantics (recording the Phase-33 decision).
  - `BulletproofGens` singleton.
  - Extension tag (255) policy (optional).
  - Refund predicate execution context (optional).
- Update `flamevm/design.md` (if it has stale content).
- Update `status/vm-engineer.md`.
- Resolve `threats/vm.md` "Open structural questions" against
  current state.

**Tests**: no code tests; quality gate is "no engineer reading
spec.md is misled".

---

## Phase 36 — End-to-end integration tests

**Goal**: prove the VM works as a system, not just per-opcode.

**Items**:
- Multi-actor scenario: actor A `issue`s a token, `send`s it to
  actor B. B `retire`s it.
- Parallel external + serial internal: 3 external txs each
  spawning 2 internal txs. Verify the consensus-side ordering.
- Cell life-cycle: `input` → `open` → repackage payload via
  `output`. Anchor chain advances correctly across multiple
  inputs.
- Confidential life-cycle: alice + bob each `issue` confidential
  Token; transfer via `mix`; bob `decrypt`s.
- Re-entrancy attempt rejected end-to-end.
- Memory cap trigger end-to-end.
- Gas exhaustion trigger end-to-end.
- TxID determinism across distinct prover runs.

**Tests**: ~10 integration tests under `flamevm/tests/`.

---

# Section 3 — Design-doc audit

Walk every architectural commitment in `design.md` and trace to
its phase:

| design.md commitment | Status | Phase |
|---|---|---|
| Linear types non-copyable / non-droppable | ✅ Done | 2, 5, 8, 10, 13 |
| No re-entrancy (ADR 0003) | ⏳ Pending | 27 |
| Transient memory cap = 4× vbytes (ADR 0002) | ⏳ Pending | 27 |
| Single external-tx fee | ⏳ Partial | 19 + 22 |
| Per-vbyte persistent storage (ADR 0004) | ⏳ Pending | 28 |
| Wire format LE everywhere (ADR 0006) | ✅ Done | All wire-format phases |
| Cell + Actor naming (ADR 0001) | ✅ Done | 8 + 24 |
| Taproot predicates (ADR 0008) | ✅ Done | 8 |
| Atomic external-tx effects | ⏳ Partial — error-on-finalize works; TxID binding (Phase 18) closes the rest | 18 + 21 |
| Grace + freeze + maturity (ADR 0005) | ⏳ Pending | 28 |
| TxID binding (signatures + ZK bind to TxID) | ⏳ Pending | 18 + 20 |
| Concurrency (external parallel, internal serial) | ⏳ Consensus crate; VM hooks in 23 | 23 |
| Block resource pools (4:1) | ⏳ Pending | 23 |
| Bitcoin coupling (chain-info opcodes) | ⏳ Pending | 31 |

**Open structural questions** from design.md:
- BFT family / stake / finality / validator rotation — consensus
  crate, not the VM.
- Extension tag (255) policy — VM; Phase 34 (with ADR).
- Refund predicate execution context — VM; Phase 32 (with ADR).
- Soft 8× internal-gas multiplier — VM hook in Phase 22, but the
  multiplier policy is consensus.
- Frontend framework — UI crate.

All VM-side commitments and open questions are mapped to a phase.

---

# Section 4 — Quality gates

For each phase, the merge gate is:

1. `cargo test -p flamevm` is green, no new warnings beyond the
   documented placeholders.
2. Every opcode in the phase has at least one positive and one
   negative test.
3. No phase adds new public API beyond what the opcodes need —
   internal helpers stay `pub(crate)`.
4. Dispatch updates are explicit (no opcode silently routes through
   a wrong context).
5. Any new architectural decision goes through an ADR before code
   lands (per `CLAUDE.md` guardrails).
