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
| 22 | Input-cell witness via `String::Cell` (zkvm-parity refactor)       | ✅      |
| 23 | Confidential N→M end-to-end test harness                           | ✅      |
| 24 | `ActorState` + `ActorRegistry` + `Address` + `send.rs`             | ✅      |
| 25 | `op_load` + `op_save` (incl. Q6 self-destruct)                     | ✅      |
| 26 | `op_call` + frame creation + `TxEntry::Call`                       | ✅      |
| 27 | Re-entrancy guard                                                  | ✅      |
| 28 | Actor lifecycle: grace + freeze + maturity (VM-side)               | ✅      |
| 29 | Introspection: identity (4 opcodes)                                | ✅      |
| 30 | Introspection: tx header (`timelock`, `version`)                   | ⏳      |
| 31 | Chain info (6 opcodes)                                             | ⏳      |
| 32 | `op_send` + `TxEntry::Send` + send queue                           | ✅      |
| 33 | Encrypted `issue` (after ADR)                                      | ⏳      |
| 34 | Gas-cost table + per-op charging                                   | ⏳      |
| 35 | Memory-cap allocator + resource introspection (5 opcodes)          | ⏳      |
| 36 | Block resource pools (B_par : B_ser)                               | ⏳      |
| 37 | Fuzz targets + canonicality sweeps                                 | ⏳      |
| 38 | spec.md + design.md sync + ADR backfill                            | ⏳      |
| 39 | End-to-end integration tests                                       | ⏳      |
| 40 | Isolated calls: `open`/`signcall`/`call` unified frames (ADR 0013) | ⏳      |

**31 of 40 complete (78 %).** Total test count: 536 passing; build
clean; remaining compiler warnings target Phases 34 / 35 (gas + mem
accounting) and Phase 40 (frame creation paths for `open`/`signcall`).

### Actor build (Phases 24–29, 32) — landed in 8 units

The actor sub-project shipped as 8 logically-distinct units rather
than the original 7-phase decomposition; the units roll up into the
phase numbers above. See commits `d8b7d32`..`3046843` for the
land sequence, and the architect-side question dialogue captured
in the conversation log for the design decisions (referred to
below as Q1–Q6, awaiting ADR backfill `0010` / `0011` / `0012`).

| Unit | Maps to plan phase | Highlights |
|---|---|---|
| 1 — actor.rs data model | 24 | `ActorID` (Hash + Constructor enum), `MethodKey(Int253)`, `ActorState`, `Actor`, `vbyte_size` (Q2: wire_len + 32 overhead), `b"flamevm.actorid"` domain (Q1). |
| 2 — `address.rs` | 24 | `Address::Predicate` / `MessageTarget` enum + canonical wire encoding; not a stack `Value` variant. |
| 3 — Registry trait + `MemRegistry` + `VbytePool` | 24 + 28 | `ActorRegistry` trait (load/save/resolve/mark/deploy/credit_vbytes/tick_block); `VbytePool` (5000/block introduction + 100-block maturity); per-block ACTIVE↔FROZEN↔CLEARED state machine. |
| 4 — Move Message/ActorID out of vm.rs; add `send.rs` | (sub-task of 32) | `Message` gains `refund_predicate` (Q3); new `SendID` newtype (Q5 — SendID == Message.anchor). |
| 5 — `op_load` + `op_save` | 25 | Per-frame `loaded` flag; cross-frame `mark_for_destruction` as re-entry lock; tx-end `commit_tx_destructions` hook = Q6 self-destruct. **`LoadWithoutSave` is not an error** — it's the destroy path. |
| 6 — `op_call` + re-entrancy + `TxEntry::Call` | 26 + 27 | `ActorCall` frame with caller + anchor; `iter_actor_ids_on_stack` walks the live frame chain for the guard; `TxEntry::Call` records callee + pre_state_root + callee_anchor so Internal TxID binds to the exact actor states observed (Q5). |
| 7 — Identity opcodes | 29 | `actorid` / `anchor` / `callerid` / `method`. `CallKind::ActorCall` extended with `anchor`. All four hard-fail `OpcodeRequiresActorContext` from external root. |
| 8 — `op_send` + `TxEntry::Send` + queue | 32 | Anchor ratchet right before emitting Send (Q5 timing); `payload_hash` keeps Send entry fixed-size; `refund_predicate` operand (Q3); `NonPortableInSend` guards args. |

**Deferred to consensus-side** (the VM has nothing to do; trait
surface is in place):
- Transparent `Constructor` deploy at first message delivery (Q4).
- Bounce-Output emission on internal-tx failure (Q3) — consensus
  builds the cell under the message's `refund_predicate` and emits
  it as an Output effect directly.
- Per-block `tick_block` driver — consensus calls into the
  registry method after applying each block.

The confidential N→M transaction test harness (Phase 23) exercises
13 shapes end-to-end through `Prover::prove` → `Verifier::verify`:
- 1→1 / 1→2 / 1→3 / 2→1 / 2→2 / 2→3 / 3→1 / 3→2 / 3→3 (single flavor)
- 2→2 / 3→2 / 2→3 / 3→3 (two flavors)
- 2 negative tests: imbalance rejected, flavor mismatch rejected

### Gas + memory accounting is deferred (Phases 34–36)

`gas_used` and `vbytes_used` currently always read `0` in `TxResult`.
Every opcode is treated as costing **gas = 1, mem = 0** *implicitly*
— no charging, no limit enforcement, no introspection.

All prerequisites for the resource pipeline are now in place:

- ✅ real actor registry to size `mem_limit = 4 × vbytes(actor)` (Phase 24)
- ✅ working call stack with refund hook so `op_call` gas refund makes sense (Phase 26)
- ✅ re-entrancy guard so `mem_used` cleanup on frame exit is well-defined (Phase 27)

The remaining work is just the per-opcode tables + hooks (Phase 34
gas, Phase 35 mem-cap, Phase 36 block pools). The introspection
opcodes (`gas`, `bytes`, `gaslimit`, `memlimit`, `newbytes`) are
gated on Phases 34/35 since they read counters those phases
populate.

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
| 11 | CS bootstrap (Prover / Verifier)                   | 9     | `Delegate` trait, `Instruction` enum, `Program` builder, `ProgramItem`, dispatch on `Instruction`. End-to-end `alloc(7) + alloc(3) == alloc(10)` proves+verifies. |
| 12 | Range proofs + Constraint composition              | 12    | `range` opcode (dynamic n ∈ [1, 64]). `not` / `and` / `or` Constraint overloads. |
| 13 | Rich `String` + scalar/commit/decrypt              | 13    | `String` becomes enum (Opaque + Commitment + Scalar + Predicate). End-to-end prove+verify with witness-bearing stack values. |
| 14 | Encrypted `borrow` + `mix` cloak gadget            | 3     | `op_borrow_encrypted`. `op_mix` invokes `spacesuit::cloak`. `WideToken` constructible. |
| 15 | Batch verifier + `Explicit` deferred sigs          | 2     | `Delegate::BatchVerifier`, `musig::BatchVerifier<ThreadRng>` on both sides. `MultiscalarMul` deleted. |
| 16 | TxID merkle root + `log` opcode                    | 6     | `TxID::from_log` over txlog, domain `flamevm.txid.v1`. `MerkleItem for TxEntry`. `0x6f log` opcode. |
| 17 | Hygiene sweep                                      | 4     | `MixDegenerate` guard. `BulletproofGens` singleton. spec.md row sync (`log` / `MultiscalarMul`). |
| 18 | `TxEntry::Header` + TxID transcript binding        | 4     | Header at txlog[0]. `cs.transcript().append_message(b"flamevm.txid", &txid.0)` on both sides. |
| 19 | `op_fee` + `CheckedFee`                            | 14    | `0x7a fee` allocates WideToken debt; `MAX_FEE = 2²⁴` per-tx cap. `TxEntry::Fee(u64)`. |
| 20 | TxBound multi-sig batch verification               | 7     | `DeferredSig::TxBound { vk, cell_id }`. `verify_multi_batched` against `flamevm.signtx.v1` transcript bound to TxID. |
| 21 | `TxResult` shape                                   | 4     | Unified return: `{ txid, txlog, total_fee, gas_used, vbytes_used, bytecode, proof, deferred_sigs, sends }`. |
| 22 | Input-cell witness via `String::Cell` (zkvm-parity refactor) | 6     | `String::Cell(Arc<Cell>)` is the prover-side carrier; verifier pushes `String::Opaque(bytes)`. `to_cell()` handles both shapes. `Instruction::Input` is a unit variant (no operand). Drops the `attach_input_witnesses` walk and three witness-mismatch errors. |
| 23 | Confidential N→M test harness                      | 13    | Full input→open→mix→output prove/verify round-trip. Matrix: N∈{1,2,3} × M∈{1,2,3} × {1,2 flavors} + 2 negatives. |
| 24 | ActorState + Registry + Address                    | 51    | `ActorID` (enum), `MethodKey(Int253)`, `ActorState`, `Actor`, `vbyte_size`, `Address` enum, `ActorRegistry` trait, `MemRegistry`, `VbytePool` (sum of Units 1+2+3 in the actor build). |
| 25 | `op_load` + `op_save`                              | 11    | Per-frame `loaded` flag, cross-frame registry mark, tx-end `commit_tx_destructions` hook = Q6 self-destruct. |
| 26 | `op_call` + `TxEntry::Call`                        | 7     | `ActorCall` frame, parent-stack return via existing `op_return` machinery, `TxEntry::Call { callee, method, pre_state_root, callee_anchor }`. |
| 27 | Re-entrancy guard                                  | (incl. in 26) | `iter_actor_ids_on_stack` walks current + suspended frames; hard-fail `ReentrancyDetected` covers direct + indirect cycles. |
| 28 | Actor lifecycle (VM-side)                          | (incl. in 24) | `tick_block` + `VbytePool` queue/release; per-actor `frozen_since` / `active_blocks` / grace formula. Consensus-side per-block driver TBD by integrator. |
| 29 | Identity opcodes                                   | 10    | `actorid` / `anchor` / `callerid` / `method`. `CallKind` extended with `method()` / `caller()` / `anchor()` accessors. |
| 32 | `op_send` + `TxEntry::Send` + queue                | 7     | Anchor ratchet at send time (Q5); `Message` queue drained into `TxResult.sends`; `payload_hash` keeps Send entry fixed-size; `refund_predicate` operand for Q3 bounce path. |

## Known wiring gap

| Area | Gap | Severity | Targeted in phase |
|---|---|---|---|
| Encrypted `issue` | Spec says `qty: Point → Token`. Current code errors `TokenRequiresCS`. Needs architect ADR on actor-context-vs-CS-context. | Medium | 33 |
| `String::as_bytes` panic on witness variants | Sharp edge — documented but no CI lint. | Low | 38 (doc) |
| `decrypt` uses default `PedersenGens` only | Future multi-gens use would need parameterization. | Low | (deferred) |
| `BatchSignatureVerificationFailed` is opaque | Doesn't say which sig failed. | Low | (accepted) |
| Sub-varint U64 branch overflow regression test missing | Pre-existing finding. | Low | 37 |

## Documented but unimplemented

| Opcode / feature | Source | Phase |
|---|---|---|
| `0x9a timelock`, `0x9b version` | spec.md rows | 30 |
| `0x9e gas`, `0x9f bytes`, `0xa2 gaslimit`, `0xa3 memlimit`, `0xa4 newbytes` | spec.md rows | 35 |
| `0xa5..=0xaa` chain-info opcodes | spec.md rows | 31 |
| Encrypted `issue` (Point → Token branch) | spec.md row, design.md | 33 |
| Memory cap `4× vbytes` enforcement | design.md ADR 0002 | 35 |
| Gas charging per opcode | design.md §Resources / Gas | 34 |
| Block resource pools (`B_par : B_ser = 4:1`) | design.md §Block resource pools | 36 |
| Per-block consensus-side `tick_block` driver | design.md ADR 0005 | (consensus / integrator) |
| Transparent Constructor deploy at delivery | Q4 | (consensus / integrator) |
| Bounce-Output emission on internal-tx failure | Q3 | (consensus / integrator) |

## Architect ADR queue

| Topic | Blocking phase | Notes |
|---|---|---|
| Input-cell witness encoding | — (resolved; refactored to zkvm parity) | Carrier is `String::Cell(Arc<Cell>)` — same pattern as zkvm's `String::Output`. Prover pushes `String::Cell(c)` with open commitments; verifier pushes `String::Opaque(bytes)`. `Instruction::Input` is a unit variant; no separate witness queue, no re-attachment step. ADR pending in Phase 38 housekeeping. |
| Actor data model (Q1, Q2, Q4, Q6) | — (resolved during actor build) | Q1: `b"flamevm.actorid"` domain. Q2: vbyte = wire_len(state) + 32. Q4: Constructor-form id deploys transparently at first delivery. Q6: load-without-save is the destroy path. ADR `0010-actor-data-model` queued for Phase 38. |
| Send-ID + Internal TxID (Q3, Q5) | — (resolved during actor build) | Q5: three IDs (External TxID, SendID = Send.anchor, Internal TxID); anchor ratcheted before emitting `TxEntry::Send`; Internal TxID binds to per-call `pre_state_root` via `TxEntry::Call`. Q3: send-failure bounce is a consensus-emitted Output, not a fresh sub-VM. ADR `0011-send-id-and-internal-txid` queued for Phase 38. |
| Load/save re-entry lock (Q6) | — (resolved during actor build) | `mark_for_destruction` as the cross-frame lock; per-frame `loaded` flag layered on top; tx-end sweep destroys still-marked actors. ADR `0012-load-save-reentry-lock` queued for Phase 38. |
| Encrypted `issue` semantics | 33 | Variable-only, Predicate-as-issuer, internal-only, or explicit-cid? |
| Extension tag (255) policy | 37 | Reject vs reserve for soft-fork. Currently rejects. |

---

# Section 2 — Pending phases

## Phases 22–23 — Witness re-attachment + N→M test harness (landed)

Detailed specs for both phases lived in this section while they
were in flight; both are now done (see "Phases complete" table
above for headlines, commit log for the implementation).

Recap:
- **Phase 22** originally added `Instruction::Input(Option<Box<InputWitnesses>>)`
  so the prover could re-attach `Commitment::Open` after the
  Cell::decode round-trip strips them to `Closed`. Verifier
  always parsed to `Input(None)`; bytecode was the bare `0x90`.
  **Later refactored** to match zkvm: the witness now rides on a
  `String::Cell(Arc<Cell>)` pushed before the bare `Instruction::Input`
  unit variant. Same wire bytes; no side-channel queue; the
  `attach_input_witnesses` walk and the three `WitnessCount/Point/NotOpen`
  errors are deleted.
- **Phase 23** built the confidential-N→M test harness (13
  passing shapes including 2 negative-balance tests). The
  helpers `make_confidential_token` /
  `make_confidential_input_cell` / `assemble_nm_script` live
  in `flamevm/src/tests/test_confidential_nm.rs`.

---

## Phases 24–29 + 32 — Actor build (landed)

The actor sub-project (data model → registry → lifecycle →
load/save → call + re-entrancy → identity opcodes → send) is in.
See the "Actor build (Phases 24–29, 32)" section at the top of
this file for the 8-unit roll-up and the commit list, plus the
"Phases complete" table for per-phase test counts.

Material design decisions made during the build and now blocked
into the codebase (all queued for ADR backfill in Phase 38 —
`0010-actor-data-model`, `0011-send-id-and-internal-txid`,
`0012-load-save-reentry-lock`):

- **Q1**: actor-id hash domain = `b"flamevm.actorid"`. The id is
  the hash of the *constructor script*; both `ActorID::Hash(h)`
  and `ActorID::Constructor(bytes)` are two views of the same
  identity (`h == H(bytes)`), and the registry canonicalizes on
  the 32-byte hash so callers can pass either form.
- **Q2**: vbyte size = `wire_len(state) + 32` (the 32 covers the
  lifecycle counters).
- **Q3**: send-failure bounce path = consensus emits an Output
  effect directly under `Message.refund_predicate`, no fresh
  sub-VM. The VM records the refund predicate and is done.
- **Q4**: `ActorID::Constructor(bytes)` inlines the actor's code
  on the wire for transparent first-delivery deployment.
  Consensus runs the constructor to produce the initial state
  and registers under the canonical hash (which it already
  knows: it's `H(constructor_bytes)`, computable without
  running anything).
- **Q5**: three IDs — External TxID (external txlog merkle root,
  known at broadcast), SendID = `Send.anchor` (deterministic at
  broadcast, identifies the future internal tx), Internal TxID
  (binds to actual execution including per-call `pre_state_root`).
  Anchor ratcheted at send time *before* the `TxEntry::Send` lands
  so External TxID covers all SendIDs.
- **Q6**: `op_load` without a matching `op_save` is **not** an
  error — it's the self-destruct path. Tx-end commit hook
  (`commit_tx_destructions`) drops still-marked actors and
  recycles their vbytes through the 100-block maturity queue.

Deferred to consensus-side (the VM has nothing to do; trait
surface is in place):

- Transparent `Constructor` deploy at first message delivery
  (registry's `deploy` is callable; consensus orchestrates).
- Bounce-Output emission on internal-tx failure (consensus
  builds the cell under `Message.refund_predicate`).
- Per-block `tick_block` driver (registry method exists;
  consensus calls it after applying each block).

---

## Phase 30 — Introspection: tx header (`timelock`, `version`)

**Goal**: 2 opcodes that read `TxHeader`.

**Items**:
- `0x9a timelock` (`ø → n {0|1}` — header.locktime + flag for
  height/timestamp).
- `0x9b version` (`ø → n` — header.version).

**Tests**: ~4 (positive each, encoding round-trip each).

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

## Phase 32 — `op_send` + `TxEntry::Send` + send queue (landed)

Shipped as part of the actor build. See the "Actor build" roll-up
at the top of this file. Stack shape was extended with a
`refund_predicate` operand right before `addr` (Flame extension
on top of the spec's bare `args… k gas bytes method addr → ø`
diagram) to make Q3 — the sender-chosen bounce path — explicit
on the wire.

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

## Phase 34 — Gas-cost table + per-op charging

**Goal**: spec-mandated gas metering. Switches the implicit
"gas = 1 per op, no enforcement" placeholder to real charging.

**Items**:
- `gas_cost(instruction: &Instruction) -> u64` table per design.md
  §Resources / Gas (start with uniform = 1, refine after benchmarks).
- `VM::charge_gas(amount: u64) -> Result<(), VMError>`: increments
  `current_call.gas_used`; errors `GasExhausted` if
  `gas_used > gas_limit`.
- Call `charge_gas` in dispatch loop before each opcode handler.
- `op_call` debits gas + refunds leftover on clean exit (depends
  on Phase 26 already shipping the call machinery).
- `op_send` debits sent gas from the caller frame.
- `VMError::GasExhausted`.

**Tests**: ~8 (gas charged per op, exhaustion errors, refund on
clean call exit, gas across nested calls, no-charge on `Ext`,
leftover gas to parent, send-debits-caller, call-args-out-of-budget).

---

## Phase 35 — Memory-cap allocator + resource introspection

**Goal**: design.md ADR 0002 (memory cap) + 5 spec opcodes that
report runtime resources.

**Items**:
- `VM::charge_mem(vbytes: u64) -> Result<(), VMError>` increments
  `current_call.mem_used`; errors `MemoryCapExceeded` if
  `mem_used > mem_limit` (where `mem_limit = 4 × vbytes(actor)`).
- Hooks in every allocator: `String::append`,
  `String::append_bytes`, `Dict::insert`, `Cell::new`,
  `Token::new`, `WideToken` paths.
- Per-call `mem_used` is transient (discarded on call exit).
- Resource introspection opcodes (depend on Phase 34 gas counter
  + this phase's mem counter being live):
  - `0x9e gas` (`ø → int` — `gas_limit - gas_used`).
  - `0xa2 gaslimit` (`ø → int` — `gas_limit`).
  - `0x9f bytes` (`ø → int` — actor's persistent vbytes; only
    meaningful in internal context).
  - `0xa3 memlimit` (`ø → int` — `mem_limit`).
  - `0xa4 newbytes` (`ø → int` — newbytes delivered with this call).
- `VMError::MemoryCapExceeded`.

**Tests**: ~10 (memcap on String/Dict/Cell/Token allocators;
per-call reset; each intro opcode positive + negative; mem
charged on send/output).

---

## Phase 36 — Block resource pools (B_par : B_ser)

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

## Phase 37 — Fuzz targets + canonicality sweeps

**Goal**: harden against parser / encoder bugs.

**Items**:
- Fuzz target: `Instruction::parse` round-trip canonicality. Any
  bytes that parse-then-encode produce identical bytes.
- Fuzz target: `Cell::decode` accepts only canonical wire-cells
  (no second valid encoding).
- Fuzz target: sub-varint U64 branch overflow.
- Fuzz target: `read_value` on arbitrary bytes never panics; only
  `Ok` / `Err`.
- Property test: `Program::to_bytecode → Program::parse` identity.
- Property test: `Cell::id` collision resistance on random
  fixtures.

**Tests**: ~5 fuzz targets + 4 property tests under `flamevm/fuzz/`.

---

## Phase 38 — spec.md + design.md sync + ADR backfill

**Goal**: every artifact reflects the current code.

**Items**:
- Walk every opcode row in `spec.md`; cross-check against current
  handler. Update wording, error codes, edge cases.
- Bump ADR index in `design.md`:
  - `0010-actor-data-model` — Q1 hash domain, Q2 vbyte sizing,
    Q4 Constructor deploy, Q6 load-without-save = destroy.
  - `0011-send-id-and-internal-txid` — Q5 three-IDs scheme,
    anchor ratchet timing, Internal TxID binding via
    `TxEntry::Call.pre_state_root`; Q3 refund-path = consensus
    Output emission.
  - `0012-load-save-reentry-lock` — `mark_for_destruction` as
    runtime enforcement of the load/save lock; per-frame
    `loaded` flag; tx-end commit sweep.
  - `0013-predicate-call-isolation` — `open` / `signcall` / `call`
    all create isolated call frames; `signrun` renamed to
    `signcall`; cell-script sandbox eliminates confused-deputy
    in actor context.
  - `MultiscalarMul` removal.
  - `op_log` opcode addition.
  - Encrypted `issue` semantics (recording the Phase-33 decision).
  - `BulletproofGens` singleton.
  - Input-cell witness re-attachment (Phase 22 design rationale).
  - Extension tag (255) policy (optional).
- Update `flamevm/design.md` (if it has stale content).
- Update `status/vm-engineer.md`.
- Resolve `threats/vm.md` "Open structural questions" against
  current state.

**Tests**: no code tests; quality gate is "no engineer reading
spec.md is misled".

---

## Phase 39 — End-to-end integration tests

**Goal**: prove the VM works as a system, not just per-opcode.

**Items**:
- Multi-actor scenario: actor A `issue`s a token, `send`s it to
  actor B. B `retire`s it.
- Parallel external + serial internal: 3 external txs each
  spawning 2 internal txs. Verify the consensus-side ordering.
- Cell life-cycle: `input` → `open` → repackage payload via
  `output`. Anchor chain advances correctly across multiple
  inputs.
- End-to-end constructor deploy: send to `ActorID::Constructor(...)`,
  consensus deploys, second send to canonical id hits the same
  actor (Q4 acceptance test).
- End-to-end self-destruct: load-without-save in a script,
  tx commits, actor removed, vbytes mature in pool (Q6).
- End-to-end refund: send to an actor whose method intentionally
  fails, Output emitted under the refund predicate, original args
  recoverable (Q3).
- Memory cap trigger end-to-end (after Phase 35).
- Gas exhaustion trigger end-to-end (after Phase 34).
- TxID determinism across distinct prover runs.
- Internal TxID changes when callee state changes between two
  otherwise-identical internal txs (Q5 acceptance test).

**Note**: the confidential-transfer life-cycle (alice + bob
`issue`, transfer via `mix`, `decrypt`) is already covered by
Phase 23. Phase 39 focuses on the actor-side flows.

**Tests**: ~10 integration tests under `flamevm/tests/`.

---

## Phase 40 — Isolated calls: unify `open` / `signcall` / `call`

**Goal**: implement ADR 0013 (design.md §Calls and isolation).
Predicate-bound execution always creates a new call frame; the
three call-creating opcodes share one mechanism.

**Status quo**: `op_open` and `op_signrun` use `enter_run` (Run-level,
shared frame). `op_call` already creates a `CallKind::ActorCall`
frame.

**Items**:
- **Rename** `signrun` → `signcall` everywhere: opcode constant
  `OP_SIGNRUN` → `OP_SIGNCALL` (byte stays `0x99`),
  `Instruction::Signrun` → `Signcall`, `signrun_message` →
  `signcall_message`, transcript label `flamevm.signrun.v1` →
  `flamevm.signcall.v1` (consensus-fixed; documented in spec).
- **Reshape `op_open`**:
  - Pop additional `gas` + `bytes` operands (Int253) like `op_call`.
    Spec stack: `cell ik nbrs pos script gas bytes args… k`.
  - Verify call-proof as today.
  - Build a `CallKind::CellOpen { anchor, predicate }` frame with
    its own stack (payload + args), gas, mem_limit. No actor identity.
  - Replace current Run-level enter; push parent onto `call_stack`.
- **Reshape `op_signcall`**: same as `op_open`, authenticator
  changes from merkle-path to signature.
- **`CallKind::CellOpen`**:
  - `actor()` returns `None` (no actor identity in cell scope).
  - `method()` returns `None`.
  - `caller()` returns `None`.
  - `anchor()` returns the stored cell anchor.
  - `op_load` / `op_save` / `op_call` / `op_send` all error
    `OpcodeRequiresActorContext` from inside a CellOpen frame.
- **Result protocol**: cell-script must `return k'` to exit; results
  pour onto the parent's stack. `break:k` cascades past frames as
  with actor calls. Stack must be empty at clean exit (the existing
  `finish_call` invariant).
- **External-context `is_external` semantics**: `CallKind::CellOpen`
  nested under `ExternalRoot` returns `true` from `is_external`
  (CS opcodes available); nested under `InternalRoot`/`ActorCall`
  returns `false` (no CS, no actor authority). This preserves the
  existing rule that external scripts can use the CS and internal
  scripts can't.
- **Tests**: ~12 new
  - `open_creates_isolated_frame_with_own_stack_and_gas`
  - `open_clears_remaining_gas_refunds_to_parent`
  - `open_script_op_load_errors_no_actor`
  - `open_script_op_call_errors_no_actor`
  - `open_script_op_send_errors_no_actor`
  - `open_external_ctx_allows_cs_opcodes`
  - `open_internal_ctx_blocks_cs_opcodes`
  - `signcall_isolation_matches_open`
  - `signcall_message_label_v1`
  - `open_return_arity_pours_to_parent`
  - `open_break_cascades_past_frame`
  - Existing `open_*` tests in `test_cells.rs` re-targeted to push
    `gas` + `bytes` operands and consume returns.

**ADR**: `decisions/0013-predicate-call-isolation.md` formalizes
the choice. Reserved alongside the actor-build backfill
(0010 / 0011 / 0012) — see Phase 38.

**Risk**: substantial. Existing scripts using cell-open and
`signrun` need updated stack shapes. Audit every existing
`open` / `signcall` test in `test_cells.rs` and
`test_authorization.rs` to add `gas`/`bytes` and `return k`.

---

# Section 3 — Design-doc audit

Walk every architectural commitment in `design.md` and trace to
its phase:

| design.md commitment | Status | Phase |
|---|---|---|
| Linear types non-copyable / non-droppable | ✅ Done | 2, 5, 8, 10, 13 |
| No re-entrancy (ADR 0003) | ✅ Done | 27 |
| Transient memory cap = 4× vbytes (ADR 0002) | ⏳ Pending | 35 |
| Single external-tx fee | ⏳ Partial — opcode wired | 19 + 34 |
| Per-vbyte persistent storage (ADR 0004) | ✅ Done (VM-side) | 24 + 28 |
| Wire format LE everywhere (ADR 0006) | ✅ Done | All wire-format phases |
| Cell + Actor naming (ADR 0001) | ✅ Done | 8 + 24 |
| Taproot predicates (ADR 0008) | ✅ Done | 8 |
| Atomic external-tx effects | ✅ Done | 18 + 21 |
| Grace + freeze + maturity (ADR 0005) | ✅ Done (VM-side) | 28 |
| TxID binding (signatures + ZK bind to TxID) | ✅ Done | 18 + 20 |
| Internal TxID binds to touched-actor state (Q5) | ✅ Done | 26 (`TxEntry::Call.pre_state_root`) |
| Concurrency (external parallel, internal serial) | ⏳ Consensus crate; VM hooks in 36 | 36 |
| Block resource pools (4:1) | ⏳ Pending | 36 |
| Bitcoin coupling (chain-info opcodes) | ⏳ Pending | 31 |
| Confidential N→M transfers | ✅ Done | 22 + 23 |
| Actor data model + identity (Q1, Q2, Q4) | ✅ Done | 24 |
| Send-id semantics + refund predicate (Q3, Q5) | ✅ Done | 32 |
| Load/save as re-entry lock + self-destruct (Q6) | ✅ Done | 25 |
| Isolated calls for `open` / `signcall` / `call` (ADR 0013) | ⏳ Pending | 40 |

**Open structural questions** from design.md:
- BFT family / stake / finality / validator rotation — consensus
  crate, not the VM.
- Extension tag (255) policy — VM; Phase 37 (with ADR).
- Refund predicate execution context — **resolved** (Q3): consensus
  emits an Output effect directly under `Message.refund_predicate`.
  No fresh sub-VM. ADR `0011-send-id-and-internal-txid` queued.
- Soft 8× internal-gas multiplier — VM hook in Phase 34, but the
  multiplier policy is consensus.
- Frontend framework — UI crate.

All VM-side commitments and open questions are mapped to a phase
(or marked resolved).

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
