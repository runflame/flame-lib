# FlameVM implementation plan — rev 4 (`plan2.md`)

This document is the canonical execution plan, **fully replacing
`plan.md` rev 3**. It is split into:

- **Section 1 — Status review**: what's done, what's done but
  partial, what's documented but unimplemented, and the open
  architectural decisions.
- **Section 2 — Phase ladder (dependency-ordered)**: every phase,
  done or pending, in linear execution order. Old phase numbers
  appear as "(was: …)" annotations so progress is traceable.
- **Section 3 — Design-doc audit**: every architectural commitment
  in `design.md`, traced to the phase that implements (or already
  implemented) it.

The phase ladder targets **~1 session of focused work per phase**.
Where the original plan had a fat phase (e.g. old Phase 17 — fee +
finalization + gas + block pools all in one), it is split. Where
old work was a sliver, it merges with neighbours.

---

# Section 1 — Status review

## 1.1 Phases complete (16 of 36)

| New # | Old # | Phase | Tests | Notes |
|---|---|---|---|---|
| 1 | 0 | Skeleton | smoke | `VM`, `CallFrame`, `Run`, `CallKind`, dispatch loop, `0x1d nop`. |
| 2 | 1 | Stack literals & manipulation | 24 | push:k / pushint{8,16,64,128,full} / pushstr / pushpoint / pushtoken / drop / dup / dup:k / roll / roll:k. |
| 3 | 2 | Control flow & explicit return | 22 | verify / run / loop / switch / return / break:k / type. Clean-stack rule enforced. |
| 4 | 3 | Int253 arithmetic, logic, size | 30 | abs / eq / neg / add / mul / divmod / mod252 / not / and / or / size. |
| 5 | 4 | String ops | 24 | readbits / readint / readstr / readpoint / writebits / writeint / append / writezeros / bitnot / bitor / bitand / bitxor / shiftleft / shiftright / keccak256. |
| 6 | 5 | Dict ops | 24 | dict / put / replace / get / getopt / getdup / first / last / next. Sticky copy/portability flags. |
| 7 | 6 | Hash & Merlin | 11 | merlin / merlinwrite / merlinread / sha256 / sha512 / sha3. |
| 8 | 9 | Cells: output / open / signtx / signrun | 11 | Run-level cell-open, Taproot PredicateTree with blinded sibling leaves and NUMS internal key. |
| 9 | 10a | Inputs (stateless VM) + Cell wire encoding | 14 | `input` opcode decodes wire-cell from String. Cell encode/decode canonical. No Utreexo trait inside VM. |
| 10 | 8 | Tokens: port `Token`/`WideToken` from zkvm + clear-only opcodes | 35 | amount / issue (cleartext) / retire / borrow (cleartext) / merge / split / issueflv. `flavor_from_actor`. |
| 11 | 11 | CS bootstrap (real `Prover` / `Verifier`) | 9 | `Delegate` trait, `Instruction` enum (60 variants), `Program` builder, `ProgramItem`, full VM-loop refactor to dispatch on `Instruction`. End-to-end `alloc(7) + alloc(3) == alloc(10)` proves+verifies. |
| 12 | 12 | Range proofs & constraint composition | 12 | `range` opcode (dynamic n ∈ [1, 64]). `not` / `and` / `or` Constraint overloads. |
| 13 | 13 | Rich `String` + `scalar` / `commit` / `decrypt` | 13 | `String` becomes enum (Opaque + Commitment + Scalar + Predicate). End-to-end prove+verify with witness-bearing stack values. |
| 14 | 13.5 | Encrypted `borrow` + `mix` cloak gadget | 3 | `op_borrow_encrypted` mirrors zkvm. `op_mix` invokes `spacesuit::cloak`. `WideToken` now constructible. |
| 15 | 14 | Batch verifier + `Explicit` deferred sigs | 2 | `Delegate::BatchVerifier`, `musig::BatchVerifier<ThreadRng>` on both sides. `Verifier::verify` batch-verifies Explicit sigs at finalize. `MultiscalarMul` deleted. |
| 16 | 17a | TxID merkle root + `log` opcode | 6 | `TxID::from_log` over txlog, domain `flamevm.txid.v1`. `MerkleItem for TxEntry`. `0x6f log` opcode matches zkvm. |

**Total: 385 tests; build clean; 4 leftover warnings (all map to pending Phases 17/24/26/30).**

## 1.2 What's done but incomplete

Items that work but have known follow-ups, ordered by severity:

| Area | Gap | Severity | Targeted in phase |
|---|---|---|---|
| **TxID transcript binding** | `TxID::from_log` exists; not yet bound into R1CS transcript before `prove`/`verify`. **Proof currently doesn't commit to the effect list.** | High | 18 |
| **TxBound deferred sigs** | `signtx` records `DeferredSig::TxBound { vk }` but no aggregate signature is checked at finalize. | High | 20 |
| **`TxEntry::Header` not in TxID** | TxID merkle root currently excludes `(version, locktime)`. Different headers, same effect list → same TxID. | Medium | 18 |
| **`op_mix` missing zero-count guard** | `m == 0 \|\| n == 0` panics `spacesuit::mix::k_mix` (`0..k-1` underflow). | Medium | 17 |
| **`BulletproofGens(1024, 1)` re-allocated per `Prover`/`Verifier`** | ~16 KB per instantiation; should be a singleton. | Medium | 17 |
| **Encrypted `issue`** | Spec says `qty: Point → Token`. Current code errors `TokenRequiresCS`. Needs architect ADR on actor-context-vs-CS-context. | Medium | 33 |
| **`String::as_bytes` panic on witness variants** | Sharp edge — documented but no CI lint. New opcodes might trip it. | Low | 17 (doc) |
| **Bit-op on witness-bearing String drops witness** | Documented but a programmer gotcha. | Low | 17 (doc) |
| **`MultiscalarMul` row stale in `spec.md`** | Type table still lists it; code deleted. | Low | 17 |
| **`op_log` not in `spec.md`** | Wired in code (0x6f) but spec row missing. | Low | 17 |
| **`decrypt` uses default `PedersenGens` only** | Phase 13 audit L2: future multi-gens use would need parameterization. | Low | (deferred) |
| **`BatchSignatureVerificationFailed` is opaque** | Doesn't say which sig failed (mirrors zkvm). | Low | (accepted) |
| **Sub-varint U64 branch overflow regression test missing** | Pre-existing Finding 1 from `audits/vm/2026-05-22-initial-surface-sweep.md`. | Low | 34 |

## 1.3 Documented but unimplemented

Items present in `spec.md` or `design.md` with no code yet:

| Opcode/feature | Source | Phase |
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

## 1.4 Architect ADR queue

Phases that need an ADR before implementation:

| Topic | Blocking phase | Notes |
|---|---|---|
| Encrypted `issue` semantics | 33 | Variable-only (zkvm), Predicate-as-issuer, internal-only, or explicit-cid? |
| `MultiscalarMul` removal from spec.md type table | 17 | Code already removed; spec needs ADR-backed update. |
| `op_log` (0x6f) addition to spec.md | 17 | Already wired; needs spec row + ADR pointer. |
| Extension tag (255) policy | 34 (or earlier) | Reject vs reserve for soft-fork. Currently rejects. |
| Refund predicate execution context | 32 (alongside send) | Fresh micro-VM vs recoverable sub-call? |
| `BulletproofGens` singleton | 17 | Implementation detail; low ADR weight. |

## 1.5 Audit findings status

Findings rolled up across all `audits/vm/*` reports:

| ID | Source | Severity | Status |
|---|---|---|---|
| Phase 11 M1 | `expr` is effectively a stub until rich-String lands | Medium | ✅ Resolved in Phase 13 |
| Phase 11 M2 | Prover/Verifier transcript label divergence risk | Medium | ✅ Covered transitively by prove+verify tests |
| Phase 11 M3 | `BulletproofGens` duplicated | Medium | ⏳ Phase 17 |
| Phase 12 M1 | `BulletproofGens` size bump amplifies | Medium | ⏳ Phase 17 |
| Phase 12 L1-L4 | Range proof / Constraint overload docs | Low | ⏳ Phase 17 (docs) |
| Phase 13 M1 | `String::as_bytes` panic on witness variants | Medium | ⏳ Phase 17 (lint or doc) |
| Phase 13 L1 | Bit-op witness loss | Low | ⏳ Phase 17 (docs) |
| Phase 13.5 M1 | `op_mix` zero-count panic | Medium | ⏳ Phase 17 |
| Phase 14 M2 | `BatchSignatureVerificationFailed` opaque | Medium | (accepted — mirrors zkvm) |
| Phase 14 M3 | TxID not transcript-bound | High | ⏳ Phase 18 |
| Phase 17 partial L2 | `TxEntry::Header` missing from TxID | Low → Medium | ⏳ Phase 18 |
| Initial-surface | Sub-varint U64 branch overflow | Medium | ⏳ Phase 34 |

---

# Section 2 — Phase ladder (dependency-ordered)

## Master status table

| New # | Status | Phase | Size | Old # |
|---|---|---|---|---|
| 1–7 | ✅ | Primitives: skeleton → hash | — | 0–6 |
| 8 | ✅ | Cells + open + signtx/signrun | — | 9 |
| 9 | ✅ | Inputs + Cell wire encoding | — | 10a |
| 10 | ✅ | Tokens (port) + clear-only opcodes | — | 8 |
| 11 | ✅ | CS bootstrap | — | 11 |
| 12 | ✅ | Range proofs + Constraint composition | — | 12 |
| 13 | ✅ | Rich String + scalar/commit/decrypt | — | 13 |
| 14 | ✅ | Encrypted borrow + mix gadget | — | 13.5 |
| 15 | ✅ | Batch verifier + Explicit sigs | — | 14 |
| 16 | ✅ | TxID + log opcode | — | 17a |
| **17** | ⏳ | **Hygiene sweep + ADR housekeeping** | small | (new) |
| 18 | ⏳ | TxEntry::Header + TxID transcript binding | small | 17b |
| 19 | ⏳ | `op_fee` + `CheckedFee` accumulator | small | 17c |
| 20 | ⏳ | TxBound multi-sig batch verification | medium | 17d |
| 21 | ⏳ | TxResult shape + finalize return values | small | 17e |
| 22 | ⏳ | Gas-cost table + per-op charging | medium | 17f |
| 23 | ⏳ | Block resource pools (B_par : B_ser) | small | 17g |
| 24 | ⏳ | ActorState + ActorRegistry (real) | medium | 15a |
| 25 | ⏳ | `op_load` / `op_save` | small | 15b |
| 26 | ⏳ | `op_call` + frame creation | medium | 15c |
| 27 | ⏳ | Re-entrancy guard + memory-cap allocator | medium | 15d |
| 28 | ⏳ | Actor lifecycle: grace + freeze + maturity | medium | 15e |
| 29 | ⏳ | Introspection A: header + resources (7 opcodes) | small | 7a |
| 30 | ⏳ | Introspection B: identity (4 opcodes) | small | 7b |
| 31 | ⏳ | Chain info (6 opcodes) | medium | 16 |
| 32 | ⏳ | `op_send` + TxEntry::Send + send queue | medium | 10b |
| 33 | ⏳ | Encrypted `issue` (after ADR) | small | 13.6 (new) |
| 34 | ⏳ | Fuzz targets + canonicality sweeps | medium | (new) |
| 35 | ⏳ | spec.md + design.md sync + ADR backfill | small | (new) |
| 36 | ⏳ | End-to-end integration tests | medium | (new) |

**Pending: 20 phases. Total session estimate: 20–25 sessions to feature-complete.**

---

## Section 2.1 — Pending phase details

### 17 — Hygiene sweep + ADR housekeeping

**Goal**: knock out all known low-effort gaps + reduce the ADR queue. No new opcodes.

**Items**:
- `op_mix` pre-check `m > 0 && n > 0` → `MixDegenerate` error. Avoids `spacesuit::mix::k_mix` underflow panic. (Phase 13.5 M1)
- `BulletproofGens` singleton via `once_cell::sync::Lazy` (or `std::sync::OnceLock`) in `prover.rs` / `verifier.rs`. Saves ~16 KB per Prover/Verifier instantiation. (Phase 11/12 M3/M1)
- `spec.md`: remove `MultiscalarMul` type-table row; add `0x6f log` row with full description.
- `spec.md`: refresh row 0x71 issue, 0x73 borrow with encrypted-branch notes (pointer to phase 14/33).
- `String::as_bytes` panic risk: add module-doc warning + an internal `String::ensure_opaque()` helper for new opcode authors.
- Doc-only: cross-link audit-findings list into plan2.md.

**Tests**: ~5 new tests (mix m=0 / n=0 rejection; singleton bp_gens identity; spec/doc unchanged).

---

### 18 — `TxEntry::Header` + TxID transcript binding

**Goal**: close the highest-severity audit finding (TxID not bound into proof).

**Items**:
- `TxEntry::Header(TxHeader)` variant. `MerkleItem::commit` absorbs `version` and `locktime` (u32 LE).
- Emit `TxEntry::Header` at start of `Prover::prove` and `Verifier::verify` (first txlog entry).
- After `VM::run_external_program` / `run_external` returns, compute `TxID::from_log(&vm.txlog)`. Bind into the R1CS transcript via `cs.transcript().append_message(b"flamevm.txid", &txid.0)` before `cs.prove` / `cs.verify`.
- Update `VM::run_external` / `run_external_program` to return `(TxResult, Vec<DeferredSig>, Vec<TxEntry>)` so prover/verifier can read the txlog.
- Determinism + tampering tests: different header → different TxID; tampered txlog → R1CS proof rejected.

**Tests**: ~6 new (header-changes-txid, txlog-tamper-rejects-proof, header-deterministic, etc.).

---

### 19 — `op_fee` + `CheckedFee` accumulator

**Goal**: spec-mandated fee opcode + per-tx accumulator.

**Items**:
- `0x7a fee`: `qty flv → widetoken`. Pops `qty: Int253`, `flv: Int253`. Records `TxEntry::Fee(u64)` (cleartext-only initially). Pushes a `WideToken` debt with `-qty` (matches `borrow` -T pattern).
- `TxEntry::Fee(u64)` variant.
- `VM::total_fee: CheckedFee` accumulator. Increments on each `op_fee`; overflow → `FeeTooHigh`.
- `CheckedFee::add(u64) -> Result<(), VMError>` helper.
- spec.md row 0x7a kept; refresh description.

**Tests**: ~4 new (fee accumulates, overflow rejected, fee in txlog hash, dispatch-internal rejects).

---

### 20 — TxBound multi-sig batch verification

**Goal**: close the second highest-severity finding (TxBound sigs unchecked).

**Items**:
- Tx-envelope shape: `ExternalTx { header, script, signature: musig::Signature, proof }`. Already exists in `tx.rs`; wire up.
- `Verifier::verify` walks `deferred_sigs`, collects `TxBound { verification_key }` items, calls `signature.verify_multi_batched(&mut signtx_transcript, verifier.signtx_items, &mut verifier.batch)`. signtx_transcript bound to TxID (from Phase 18).
- `Prover::prove` cannot produce the multi-sig from witness alone — `UnsignedTx` shape that surfaces `signtx_items` to an external signer. New struct.
- Architect ADR pointer: signing protocol is external to VM; VM only verifies.

**Tests**: ~5 new (multi-sig batches multiple TxBound sigs, tampered key rejected, missing-sig rejected, single-TxBound case, no-TxBound case).

---

### 21 — `TxResult` shape + finalize return values

**Goal**: stable public API for downstream consumers.

**Items**:
- `TxResult { txid: TxID, txlog: Vec<TxEntry>, total_fee: u64, gas_used: u64, vbytes_used: u64, proof: Option<R1CSProof>, sends: Vec<Message> }`. (`Message` placeholder until Phase 32.)
- `Prover::prove` returns `TxResult` (proof = Some(...)); `Verifier::verify` returns `TxResult` (proof = None — already verified).
- Drop the multi-return tuple shape; everything in TxResult.
- Backwards-compat: `VM::execute_external` keeps its current signature.

**Tests**: ~3 (TxResult populated correctly for trivial program, TxResult.txid matches between prover/verifier, TxResult.txlog contents).

---

### 22 — Gas-cost table + per-op charging

**Goal**: spec-mandated gas metering.

**Items**:
- `gas_cost(instruction: &Instruction) -> u64` table per design.md §Resources / Gas (specific costs TBD; start with uniform = 1, refine after benchmarks).
- `VM::charge_gas(amount: u64) -> Result<(), VMError>`: increments `current_call.gas_used`; errors `GasExhausted` if `gas_used > gas_limit`.
- Call `charge_gas` in dispatch loop before each opcode handler.
- `VMError::GasExhausted`.

**Tests**: ~6 (gas charged per op, exhaustion errors, refund on clean call exit, gas across nested calls, no-charge on Ext, leftover gas to parent).

---

### 23 — Block resource pools (B_par : B_ser)

**Goal**: design.md §Block resource pools commitment.

**Items**:
- `BlockResourcePools { par_gas_remaining: u64, ser_gas_remaining: u64 }` — initial ratio 4:1.
- VM exposes a hook for the consensus crate to deduct per-tx gas from the correct pool.
- VM doesn't enforce the pool itself (that's consensus); just provides accurate per-tx gas via TxResult.gas_used.
- design.md cross-reference noted as a consensus-VM seam.

**Tests**: ~3 (pool deduction shape, tx exceeding pool rejected at consensus seam, ratio enforcement).

---

### 24 — `ActorState` + `ActorRegistry` (real, not stub)

**Goal**: actor storage layer.

**Items**:
- `ActorState` type per design.md (`public: Dict`, `private: Dict`).
- `ActorRegistry` trait extended: `load_actor(actor_id) -> Result<ActorState>`, `save_actor(actor_id, state) -> Result<()>`, `mark_for_destruction(actor_id)`, `unmark_for_destruction(actor_id)`, `is_marked_for_destruction(actor_id) -> bool`.
- In-memory impl for tests; real impl is integrator territory.
- ActorID computation from constructor script per design.md.

**Tests**: ~5 (load/save round-trip, mark/unmark idempotent, unknown actor errors).

---

### 25 — `op_load` + `op_save`

**Goal**: spec opcodes 0x96 / 0x97.

**Items**:
- `0x96 load`: `ø → dict`. Calls `registry.load_actor(current_actor)?` + `registry.mark_for_destruction(current_actor)`. Pushes `Value::Dict(state)`.
- `0x97 save`: `dict → ø`. Pops Dict, calls `registry.save_actor(current_actor, dict)?` + `registry.unmark_for_destruction(current_actor)`.
- Pair invariant: `load` without subsequent `save` errors `LoadWithoutSave` at call-exit. Tracked via per-frame flag.
- `VMError::LoadWithoutSave`.

**Tests**: ~6 (load+save pair, load-without-save errors at exit, double-load errors, save-without-load errors).

---

### 26 — `op_call` + frame creation

**Goal**: synchronous actor-to-actor call.

**Items**:
- `0x95 call`: `args… k gas bytes method addr → results… k'`. Pops k, gas, bytes, method, addr (actor_id String).
- Looks up bytecode via `registry.resolve_method(actor, method)?`.
- Creates a new CallFrame with `CallKind::ActorCall { actor, method, caller: current_actor }`, `gas_limit = popped_gas`, `mem_limit = registry.actor_vbytes(actor) * 4`, `newbytes = popped_bytes`.
- Pushes args onto new frame's stack.
- On clean exit: refunds leftover gas via existing `finish_call` machinery.

**Tests**: ~6 (call A → B with args, A→B→C clean exit, gas refund, ArgCount mismatch errors, unknown method errors).

---

### 27 — Re-entrancy guard + memory-cap allocator

**Goal**: design.md ADR 0002 (memory cap) + ADR 0003 (no re-entrancy).

**Items**:
- `VM::check_no_reentry(target_actor) -> Result<()>` walks `iter::once(&current_call).chain(call_stack.iter())`; errors `ReentrancyDetected` if `actor()` matches.
- Called from `op_call` before frame creation.
- `VM::charge_mem(vbytes: u64) -> Result<(), VMError>` increments `current_call.mem_used`; errors `MemoryCapExceeded` if `mem_used > mem_limit`.
- Hooks in every allocator: `String::append`, `String::append_bytes`, `Dict::insert`, `Cell::new`, `Token::new`, `WideToken` paths.
- Per-call `mem_used` is transient (discarded on call exit).
- `VMError::ReentrancyDetected`, `MemoryCapExceeded`.

**Tests**: ~8 (A→B→A direct cycle → ReentrancyDetected; A→B→C→A indirect cycle; recursion within one method allowed; memcap on String.append; memcap on Dict; per-call reset).

---

### 28 — Actor lifecycle: grace + freeze + 100-block maturity

**Goal**: design.md ADR 0005.

**Items**:
- Per-block vbyte deduction (called by consensus crate after applying block).
- Freeze on `vbytes == 0`: subsequent `op_call` errors `ActorFrozen`. State preserved.
- Grace window = `min(active_blocks / 4, blocks_per_6_months)`. Top-up resets the counter.
- Elapse without top-up: state cleared, vbytes return to pool after 100 blocks (maturity).
- Pool re-introduction: 5000 vbytes/block (design.md commitment), adjustable up to 2× by supermajority.
- `VMError::ActorFrozen`.

**Tests**: ~6 (freeze on zero, grace counts down, top-up unfreezes, expiry clears state, maturity delay, supermajority adjustment).

---

### 29 — Introspection block A: header + resources (7 opcodes)

**Goal**: spec-mandated runtime introspection of tx header and per-call resources.

**Items**:
- `0x9a timelock` (`ø → n {0|1}` — header.locktime + flag for height/timestamp).
- `0x9b version` (`ø → n` — header.version).
- `0x9e gas` (`ø → int` — gas_limit - gas_used).
- `0xa2 gaslimit` (`ø → int` — gas_limit).
- `0x9f bytes` (`ø → int` — actor's persistent vbytes; only meaningful in internal context).
- `0xa3 memlimit` (`ø → int` — mem_limit).
- `0xa4 newbytes` (`ø → int` — newbytes delivered with this call).
- All ~5 LOC each + 2 tests each.

**Tests**: ~14 (one positive + one negative per opcode).

---

### 30 — Introspection block B: identity (4 opcodes)

**Goal**: actor identity / call context.

**Items**:
- `0x9c actorid` (`ø → string` — current_call.kind.actor()).
- `0x9d anchor` (`ø → string` — current_call.kind.anchor() for InternalRoot / CellOpen).
- `0xa0 callerid` (`ø → string` — caller actor id; all-zero if external).
- `0xa1 method` (`ø → int` — current_call.kind.method()).
- `CallKind` accessor methods (anchor, method, caller, predicate).
- All `OpcodeRequiresActorContext` from `ExternalRoot`.

**Tests**: ~8 (positive in valid context, ExternalOnly error from wrong context).

---

### 31 — Chain info (6 opcodes)

**Goal**: design.md Bitcoin coupling + spec rows 0xa5..=0xaa.

**Items**:
- `BlockContext` populated from consensus crate (height, blockhash, blockburn, blockweight, blockrate, chainstate).
- `0xa5 height`, `0xa6 blockhash`, `0xa7 blockburn`, `0xa8 blockweight`, `0xa9 blockrate`, `0xaa chainstate`.
- 100-block maturity guard on all height-parameterized opcodes per design.md.
- `VMError::BlockHeightImmature` for queries on `h > current - 100`.
- `chainstate` returns a Dict with block stats.

**Tests**: ~8 (each opcode positive, maturity guard, chainstate Dict shape).

---

### 32 — `op_send` + `TxEntry::Send` + send queue

**Goal**: unpause Phase 10b (was paused until Phase 15 actor surface ready).

**Items**:
- `0x94 send`: `args… k gas bytes method addr → ø`. Pops operands, debits gas/bytes from caller frame.
- Builds `Message { target, method, caller, anchor, payload, gas, vbytes }` (`caller = current_actor` or None for external).
- `TxEntry::Send(MessageRef)` variant — opaque handle into VM.sends.
- `VM.sends: Vec<Message>` collector. `TxResult.sends` returned.
- Refund predicate: per spec, message has a bounce predicate for failures. Phase 32 may need an architect ADR (Open structural question — refund predicate context).

**Tests**: ~8 (send from External → caller=None, send from Internal → caller=Some, gas debited, bytes debited, gas-exceeds-budget rejects, payload preserved, refund-predicate-shape).

---

### 33 — Encrypted `issue` (after ADR)

**Goal**: close the last Token opcode gap.

**Architect ADR prerequisite**: resolve "actor context vs CS context" question. Three options:
1. Take Variable (zkvm-exact, drops actor-context dependency).
2. Take explicit Predicate-as-issuer (zkvm-style).
3. Make encrypted `issue` internal-only (so actor context exists).
4. Take explicit cid String + Variable.

**Items (post-ADR)**:
- `dispatch_external` peek for Variable operand → `op_issue_encrypted`.
- Implementation: commits qty Variable via delegate.commit_variable; flv via flavor_from_actor (or explicit per ADR); range-proves qty (64-bit); emits TxEntry::Issue.
- Push Token { qty, flv }.

**Tests**: ~5 (encrypted issue prove+verify, range-overflow rejected, unblinded equivalent of cleartext).

---

### 34 — Fuzz targets + canonicality sweeps

**Goal**: harden against parser / encoder bugs.

**Items**:
- Fuzz target: `Instruction::parse` round-trip canonicality. Any bytes that parse-then-encode produce identical bytes.
- Fuzz target: `Cell::decode` accepts only canonical wire-cells (no second valid encoding).
- Fuzz target: sub-varint U64 branch overflow (closes Initial-surface Finding 1).
- Fuzz target: `read_value` on arbitrary bytes never panics; only Ok / Err.
- Property test: `Program::to_bytecode → Program::parse` identity.
- Property test: `Cell::id` collision resistance on random fixtures.

**Tests**: ~5 fuzz targets + 4 property tests under `flamevm/fuzz/`.

---

### 35 — spec.md + design.md sync + ADR backfill

**Goal**: every artifact reflects the current code.

**Items**:
- Walk every opcode row in spec.md; cross-check against current handler. Update wording, error codes, edge cases.
- Bump ADR index in design.md:
  - ADR 0009: MultiscalarMul removal.
  - ADR 0010: op_log opcode addition.
  - ADR 0011: Encrypted issue semantics (recording the Phase-33 decision).
  - ADR 0012: BulletproofGens singleton.
  - ADR 0013 (optional): Extension tag (255) policy.
  - ADR 0014 (optional): Refund predicate execution context.
- Update `flamevm/design.md` (if it has stale content).
- Update `status/vm-engineer.md` to reflect plan2.md.
- Resolve threats/vm.md "Open structural questions" against current state.

**Tests**: no code tests; quality-gate is "no engineer reading spec.md is misled".

---

### 36 — End-to-end integration tests

**Goal**: prove the VM works as a system, not just per-opcode.

**Items**:
- Multi-actor scenario: actor A `issue`s a token, `send`s it to actor B. B `retire`s it.
- Parallel external + serial internal: 3 external txs each spawning 2 internal txs. Verify the consensus-side ordering.
- Cell life-cycle: `input` → `open` → repackage payload via `output`. Anchor chain advances correctly across multiple inputs.
- Confidential life-cycle: alice + bob each `issue` confidential Token; transfer via `mix`; bob `decrypt`s.
- Re-entrancy attempt rejected end-to-end.
- Memory cap trigger end-to-end.
- Gas exhaustion trigger end-to-end.
- TxID determinism across distinct prover runs.

**Tests**: ~10 integration tests under `flamevm/tests/`.

---

# Section 3 — Design-doc audit

Walk every architectural commitment in `design.md` and trace to its phase:

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
| Concurrency (external parallel, internal serial) | ⏳ Consensus crate; VM hooks in | 23 |
| Block resource pools (4:1) | ⏳ Pending | 23 |
| Bitcoin coupling (chain-info opcodes) | ⏳ Pending | 31 |

**Open structural questions** from design.md:
- **BFT family / stake / finality / validator rotation** — consensus crate, not VM.
- **Extension tag (255) policy** — VM; Phase 34 (with ADR).
- **Refund predicate execution context** — VM; Phase 32 (with ADR).
- **Soft 8× internal-gas multiplier** — VM hook in Phase 22, but multiplier policy is consensus.
- **Frontend framework** — UI crate.

All VM-side commitments and open questions are now mapped to a phase.

---

# Section 4 — Quality gates (carried from plan.md rev 3)

For each phase, the merge gate is:
1. `cargo test -p flamevm` is green, no new warnings beyond the documented Category-C placeholders.
2. Every opcode in the phase has at least one positive and one negative test.
3. No phase adds new public API beyond what the opcodes need — internal helpers stay `pub(crate)`.
4. Dispatch updates are explicit (no opcode silently routes through a wrong context).
5. Any new architectural decision goes through an ADR before code lands (per `CLAUDE.md` guardrails).
