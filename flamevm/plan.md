# FlameVM implementation plan

The canonical execution plan for the FlameVM crate. Self-sufficient;
read this and `flamevm/spec.md` together to know what the VM is and
what's left to build.

---

## Status overview

### Completed phases (1–38)

| #  | Phase                                                              |
|----|--------------------------------------------------------------------|
| 1  | Skeleton — VM core, CallFrame, Run, dispatch loop, `nop`           |
| 2  | Stack literals & manipulation                                      |
| 3  | Control flow & explicit return                                     |
| 4  | Int253 arithmetic, logic, size                                     |
| 5  | String ops                                                         |
| 6  | Dict ops                                                           |
| 7  | Hash & Merlin                                                      |
| 8  | Cells + open + signtx + signcall                                   |
| 9  | Inputs (stateless VM) + Cell wire encoding                         |
| 10 | Tokens (port from zkvm) + clear-only opcodes                       |
| 11 | CS bootstrap (real Prover/Verifier, Instruction enum, Program)     |
| 12 | Range proofs + Constraint composition                              |
| 13 | Rich `String` + `scalar` / `commit` / `decrypt`                    |
| 14 | Encrypted `borrow` + `mix` cloak gadget                            |
| 15 | Batch verifier + `Explicit` deferred sigs                          |
| 16 | TxID merkle root + `log` opcode                                    |
| 17 | Hygiene sweep + ADR housekeeping                                   |
| 18 | `TxEntry::Header` + TxID transcript binding                        |
| 19 | `op_fee` + `CheckedFee` accumulator                                |
| 20 | TxBound multi-sig batch verification                               |
| 21 | `TxResult` shape + finalize return values                          |
| 22 | Input-cell witness via `String::Cell` (zkvm-parity refactor)       |
| 23 | Confidential N→M end-to-end test harness                           |
| 24 | `ActorState` + `ActorRegistry` + `Address` + `send.rs`             |
| 25 | `op_load` + `op_save` (incl. Q6 self-destruct)                     |
| 26 | `op_call` + frame creation                                         |
| 27 | Re-entrancy guard                                                  |
| 28 | Actor lifecycle: grace + freeze + maturity (VM-side)               |
| 29 | Introspection: identity (4 opcodes)                                |
| 30 | Introspection: tx header (`timelock`, `version`)                   |
| 31 | `op_send` + `TxEntry::Send` + send queue                           |
| 32 | Resource introspection opcodes (5 opcodes)                         |
| 33 | Isolated calls: `open`/`signcall`/`call` unified frames (ADR 0013) |
| 34 | `Point` enum + `String::Point` unification                         |
| 35 | `MultiscalarMul` for Sigma-protocol verification                   |
| 36 | TxLog records effects, not control flow (ADR 0014)                 |
| 37 | Batch rollback under call failure                                  |
| 38 | Issuance redesign (`issuepub` / `issuepriv` split; ADR-pending)    |

### Pending phases (39–41)

| #  | Phase                                                              |
|----|--------------------------------------------------------------------|
| 40 | Gas-cost estimation (incl. block resource pool consideration)      |
| 41 | Memory cap (allocator hooks + enforcement)                         |
| 42 | Integration tests (incl. fuzzing & canonicality sweeps)            |

**39 of 42 complete (~93 %).** Total test count: 579 passing; build
clean. Remaining compiler warnings target Phase 40 (gas charging) and
Phase 41 (mem-cap allocator).

### Actor build (Phases 24–29, 31) — landed in 8 units

The actor sub-project shipped as 8 logically-distinct units. The units
roll up into the phase numbers above. See commits `d8b7d32`..`3046843`
for the land sequence, and the architect-side question dialogue
captured in the conversation log for the design decisions (Q1–Q6,
awaiting ADR backfill `0010` / `0011` / `0012`).

| Unit | Maps to phase | Highlights |
|---|---|---|
| 1 — actor.rs data model | 24 | `ActorID` (Hash + Constructor), `MethodKey(Int253)`, `ActorState`, `Actor`, `vbyte_size` (Q2), `b"flamevm.actorid"` domain (Q1). |
| 2 — `address.rs` | 24 | `Address::Predicate` / `MessageTarget` enum + canonical wire encoding. |
| 3 — Registry trait + `MemRegistry` + `VbytePool` | 24 + 28 | `ActorRegistry` trait (load/save/resolve/mark/deploy/credit_vbytes/tick_block); `VbytePool` (5000/block introduction + 100-block maturity); per-block ACTIVE↔FROZEN↔CLEARED state machine. |
| 4 — Move Message/ActorID out of vm.rs; add `send.rs` | (sub-task of 31) | `Message` gains `refund_predicate` (Q3); new `SendID` newtype (Q5). Originally SendID == `Message.anchor`; later (Phase 39) redefined as the canonical hash of the whole Send (analogous to CellID for cells). |
| 5 — `op_load` + `op_save` | 25 | Per-frame `loaded` flag; cross-frame `mark_for_destruction` as re-entry lock; tx-end `commit_tx_destructions` hook = Q6 self-destruct. |
| 6 — `op_call` + re-entrancy | 26 + 27 | `ActorCall` frame with caller + anchor; `iter_actor_ids_on_stack` walks the live frame chain for the guard. |
| 7 — Identity opcodes | 29 | `actorid` / `anchor` / `callerid` / `method`. `CallKind::ActorCall` extended with `anchor`. All four hard-fail `OpcodeRequiresActorContext` from external root. |
| 8 — `op_send` + `TxEntry::Send` + queue | 31 | Anchor ratchet right before emitting Send (Q5 timing); `payload_hash` keeps Send entry fixed-size; `refund_predicate` operand (Q3); `NonPortableInSend` guards args. |

**Deferred to consensus-side** (the VM has nothing to do; trait
surface is in place):

- Transparent `Constructor` deploy at first message delivery (Q4).
- Bounce-Output emission on internal-tx failure (Q3) — consensus
  builds the cell under the message's `refund_predicate`.
- Per-block `tick_block` driver — consensus calls into the
  registry method after applying each block.

The confidential N→M transaction test harness (Phase 23) exercises
13 shapes end-to-end through `Prover::prove` → `Verifier::verify`:
- 1→1 / 1→2 / 1→3 / 2→1 / 2→2 / 2→3 / 3→1 / 3→2 / 3→3 (single flavor)
- 2→2 / 3→2 / 2→3 / 3→3 (two flavors)
- 2 negative tests: imbalance rejected, flavor mismatch rejected

### Gas + memory accounting is partially in place (Phases 32, 39–40)

`gas_used` and `vbytes_used` are tracked on each CallFrame (set by
`CallFrame::new`, read by the introspection opcodes shipped in
Phase 32) but **no opcode currently charges them**. Every opcode is
treated as costing **gas = 0, mem = 0** — no enforcement, no
exhaustion error.

All prerequisites for the resource pipeline are in place:

- ✅ real actor registry to size `mem_limit = 4 × vbytes(actor)` (Phase 24)
- ✅ working call stack with refund hook so `op_call` gas refund makes sense (Phase 26)
- ✅ re-entrancy guard so `mem_used` cleanup on frame exit is well-defined (Phase 27)
- ✅ the 5 introspection opcodes that read the counters (Phase 32)

Remaining work: per-opcode gas charging (Phase 39), mem-cap
allocator hooks (Phase 40).

### Post-isolation refactors (Phases 33–37, all landed)

After the ADR 0013 isolation work shipped, follow-up refactors
unified the witness model, added a lazy MSM type, retired
`TxEntry::Call`, and closed the batch-rollback hole:

- **Phase 33 — Isolated `open`/`signcall`/`call` (ADR 0013).** All
  three opcodes create isolated call frames with explicit `gas`/`bytes`
  operands. `signrun` renamed to `signcall`; cell-script sandbox
  eliminates confused-deputy by construction.
- **Phase 34 — `Point` enum.** `Point` became `enum { Opaque,
  Commitment(Box<Commitment>), Predicate(Box<Predicate>) }`. The
  `String` enum collapsed its `Commitment` and `Predicate` variants
  into a single `String::Point(Point)`. `Instruction::PushPoint`
  now takes a `Point` operand (same 32 wire bytes), so the prover can
  attach witness data via `pushpoint`, not only `pushstr`. `op_cell` /
  `op_output` route through `Point::to_predicate` so a prover-side
  `PredicateTree` witness flows into cell construction.
- **Phase 35 — `MultiscalarMul`.** Resurrected as a first-class
  `Value` type for lazy Sigma-protocol verification. Lifts through
  `op_add` / `op_neg` / `op_mul`; `op_verify` queues the MSM into the
  same `BatchVerifier` lane as Schnorr / MuSig signatures as the
  statement *"sum == identity point"*. Audit
  `audits/vm/2026-05-26-msm-design.md` flagged two follow-ups
  (gas charge per term + per-frame size cap), both rolled into
  Phase 39 / 40.
- **Phase 36 — TxLog records effects, not control flow (ADR 0014).**
  Dropped `TxEntry::Call`; added `TxEntry::ActorSave { actor,
  post_state_root }` emitted by `op_save`. Internal TxID still binds
  to per-call actor state, now via the save entry instead of an
  always-on call entry.
- **Phase 37 — Batch rollback under call failure.**
  `starsig::BatchSnapshot` + `BatchCheckpoint` trait; `CallFrame`
  snapshots the global batch on parent-frame push; `fail_current_call`
  restores it. Failed callees can no longer pollute the caller's
  MSM / sig batch.

---

# Section 1 — Completed phase details

## Detailed table

| #  | Phase                                                       | Tests | Highlights |
|----|-------------------------------------------------------------|-------|------------|
| 1  | Skeleton                                                    | smoke | `VM`, `CallFrame`, `Run`, `CallKind`, dispatch loop, `0x1d nop`. |
| 2  | Stack literals & manipulation                               | 24    | push:k / pushint{8,16,64,128,full} / pushstr / pushpoint / pushtoken / drop / dup / dup:k / roll / roll:k. |
| 3  | Control flow & explicit return                              | 22    | verify / run / loop / switch / return / break:k / type. Clean-stack rule enforced. |
| 4  | Int253 arithmetic, logic, size                              | 30    | abs / eq / neg / add / mul / divmod / mod252 / not / and / or / size. |
| 5  | String ops                                                  | 24    | readbits / readint / readstr / readpoint / writebits / writeint / append / writezeros / bitnot / bitor / bitand / bitxor / shiftleft / shiftright / keccak256. |
| 6  | Dict ops                                                    | 24    | dict / put / replace / get / getopt / getdup / first / last / next. Sticky copy/portability flags. |
| 7  | Hash & Merlin                                               | 11    | merlin / merlinwrite / merlinread / sha256 / sha512 / sha3. |
| 8  | Cells + open + signtx/signcall                              | 11    | Run-level cell-open, Taproot `PredicateTree` with blinded sibling leaves and NUMS internal key. (Frame-isolation arrived in Phase 33.) |
| 9  | Inputs + Cell wire encoding                                 | 14    | `input` opcode decodes wire-cell from String. Cell encode/decode canonical. No Utreexo trait inside VM. |
| 10 | Tokens (port) + clear-only opcodes                          | 35    | amount / issue (cleartext) / retire / borrow (cleartext) / merge / split / issueflv. `flavor_from_actor`. |
| 11 | CS bootstrap (Prover / Verifier)                            | 9     | `Delegate` trait, `Instruction` enum, `Program` builder, `ProgramItem`, dispatch on `Instruction`. End-to-end `alloc(7) + alloc(3) == alloc(10)` proves+verifies. |
| 12 | Range proofs + Constraint composition                       | 12    | `range` opcode (dynamic n ∈ [1, 64]). `not` / `and` / `or` Constraint overloads. |
| 13 | Rich `String` + scalar/commit/decrypt                       | 13    | `String` becomes enum (Opaque + Commitment + Scalar + Predicate). End-to-end prove+verify with witness-bearing stack values. |
| 14 | Encrypted `borrow` + `mix` cloak gadget                     | 3     | `op_borrow_encrypted`. `op_mix` invokes `spacesuit::cloak`. `WideToken` constructible. |
| 15 | Batch verifier + `Explicit` deferred sigs                   | 2     | `Delegate::BatchVerifier`, `musig::BatchVerifier<ThreadRng>` on both sides. `MultiscalarMul` deleted (resurrected in Phase 35). |
| 16 | TxID merkle root + `log` opcode                             | 6     | `TxID::from_log` over txlog, domain `flamevm.txid`. `MerkleItem for TxEntry`. `0x6f log` opcode. |
| 17 | Hygiene sweep                                               | 4     | `MixDegenerate` guard. `BulletproofGens` singleton. spec.md row sync. |
| 18 | `TxEntry::Header` + TxID transcript binding                 | 4     | Header at txlog[0]. `cs.transcript().append_message(b"flamevm.txid", &txid.0)` on both sides. |
| 19 | `op_fee` + `CheckedFee`                                     | 14    | `0x7a fee` allocates WideToken debt; `MAX_FEE = 2²⁴` per-tx cap. `TxEntry::Fee(u64)`. |
| 20 | TxBound multi-sig batch verification                        | 7     | `DeferredSig::TxBound { vk, cell_id }`. `verify_multi_batched` against `flamevm.signtx` transcript bound to TxID. |
| 21 | `TxResult` shape                                            | 4     | Unified return: `{ txid, txlog, total_fee, gas_used, vbytes_used, bytecode, proof, deferred_sigs, sends }`. |
| 22 | Input-cell witness via `String::Cell` (zkvm-parity)         | 6     | `String::Cell(Arc<Cell>)` is the prover-side carrier; verifier pushes `String::Opaque(bytes)`. `to_cell()` handles both shapes. `Instruction::Input` is a unit variant. |
| 23 | Confidential N→M test harness                               | 13    | Full input→open→mix→output prove/verify round-trip. Matrix: N∈{1,2,3} × M∈{1,2,3} × {1,2 flavors} + 2 negatives. |
| 24 | ActorState + Registry + Address                             | 51    | `ActorID` (enum), `MethodKey(Int253)`, `ActorState`, `Actor`, `vbyte_size`, `Address` enum, `ActorRegistry` trait, `MemRegistry`, `VbytePool` (sum of Units 1+2+3). |
| 25 | `op_load` + `op_save`                                       | 11    | Per-frame `loaded` flag, cross-frame registry mark, tx-end `commit_tx_destructions` hook = Q6 self-destruct. |
| 26 | `op_call` + frame creation                                  | 7     | `ActorCall` frame, parent-stack return via existing `op_return` machinery. |
| 27 | Re-entrancy guard                                           | (incl. in 26) | `iter_actor_ids_on_stack` walks current + suspended frames; hard-fail `ReentrancyDetected` covers direct + indirect cycles. |
| 28 | Actor lifecycle (VM-side)                                   | (incl. in 24) | `tick_block` + `VbytePool` queue/release; per-actor `frozen_since` / `active_blocks` / grace formula. |
| 29 | Identity opcodes                                            | 10    | `actorid` / `anchor` / `callerid` / `method`. `CallKind` extended with `method()` / `caller()` / `anchor()` accessors. |
| 30 | Tx-header introspection                                     | 4     | `0x9a timelock` (Bitcoin BIP-65 threshold, `LOCKTIME_TIMESTAMP_THRESHOLD = 500_000_000`) and `0x9b version`. |
| 31 | `op_send` + `TxEntry::Send` + queue                         | 7     | Anchor ratchet at send time (Q5); `Message` queue drained into `TxResult.sends`; `payload_hash` keeps Send entry fixed-size; `refund_predicate` operand for Q3 bounce path. |
| 32 | Resource introspection opcodes                              | 7     | `0x9e gas`, `0xa2 gaslimit`, `0xa3 memlimit`, `0xa4 newbytes` (all four "either context"); `0x9f bytes` (int.; requires actor + registry; routes through `ActorRegistry::actor_vbytes`). Counters read but not yet charged. |
| 33 | Isolated `open` / `signcall` / `call` (ADR 0013)            | 13    | `signrun` → `signcall` rename incl. transcript label. `op_open` and `op_signcall` create isolated `CellOpen` frames with explicit `gas`/`bytes` operands. `CallKind::CellOpen { anchor, predicate, external_context }` snapshots caller's CS context. `enter_cell_open_frame` + `pop_gas_bytes` helpers shared by both opcodes. Confused-deputy class structurally eliminated. |
| 34 | `Point` enum + `String::Point`                              | 0     | Refactor. `Point::{Opaque, Commitment(Box<Commitment>), Predicate(Box<Predicate>)}`. `String::Commitment` + `String::Predicate` collapse into `String::Point(Point)`. `Instruction::PushPoint(Point)` so `pushpoint` carries witnesses too. `op_cell`/`op_output` route through `Point::to_predicate`. No test count change (refactor is type-only). |
| 35 | `MultiscalarMul` for Sigma protocols                        | 13    | `MultiscalarMul` Value type (linear, portable, non-wire). Lazy `Vec<(Scalar, Point)>` accumulator; `op_add`/`neg`/`mul` lift point arithmetic; `op_verify` adds it to the existing `BatchVerifier` lane as `sum == identity`. |
| 36 | TxLog records effects, not control flow (ADR 0014)          | 1 (new) | Drop `TxEntry::Call`; add `TxEntry::ActorSave { actor, post_state_root }` emitted by `op_save`. MerkleItem domain tags retired/added. Tests reworked: `call_emits_…` → `call_does_not_emit_txlog_entry_by_itself`. |
| 37 | Batch rollback under call failure                           | 2 (new) | `starsig::BatchSnapshot` + `BatchCheckpoint` trait. `CallFrame::snap_batch` populated when child pushed; restored on failure. MSM + sig contributions from failed callees no longer pollute the caller's batch. |
| 38 | Issuance redesign + `TxEntry::Receive` + op_save audit fixes (F1, F2, F3) | 14 (new) | Two disjoint mint opcodes + two consumer-side flavor helpers (`issuepriv`/`issueprivflv` at `0x91`/`0x92`, `issuepub`/`issuepubflv` at `0x93`/`0x94`); distinct Merlin labels `flamevm.{issuepriv,issuepub}.flavor`; typed `TxEntry::IssuePub(Int253, Int253)` / `TxEntry::IssuePriv(point, point)`. `TxEntry::Receive([u8; 32])` emitted as the first effect after `Header` in `VM::execute_internal` so Internal TxID commits to its triggering SendID. **op_save audit fixes**: F1 — actor-state rollback on call failure via `ActorRegistry::{push_checkpoint, pop_checkpoint_commit, pop_checkpoint_rollback}` (snapshots on frame entry, restores on `fail_current_call` + tx-level rollback in `execute_internal`); closes the laundering-via-failed-subcall hole. F2 — `TxEntry::ActorSave { actor, state }` now carries the full state (symmetric with `Output(Cell)`); merkle leaf hashes `(actor.to_hash(), state.root())`. F3 — save failures propagate as call failures, rolled back via F1 (no more silent self-destruct on save error). |
| 39 | SendID content-addressing + `TxEntry::Send(Message)` unification | 13 (new) | (a) `TxEntry::Send` collapsed from an eight-field struct variant to a single-field tuple variant carrying `Message` — symmetric with `TxEntry::Output(Cell)`. Removes a duplicated field list, the `op_send` field-by-field copy, and the consensus-side reconstruction step. (b) `Message::encode(w)` defines the canonical wire form (`anchor` raw bytes → canonical `ActorID::encode` for target → option byte + canonical `ActorID::encode` for caller → `Int253` for method → 32-byte point for refund predicate → LE-u64 gas/vbytes → LE-u64 payload count + canonical `write_value` per item). `Message::id()` is `H(b"flamevm.send.id" ‖ encode())` — one wire encoding, one hash, one identity, mirroring `Cell::id()`. (c) `MerkleItem` for `TxEntry::Send` commits the 32-byte SendID under tag `b"send"`. (d) `VM::execute_internal` uses `message.id().as_bytes()` for `TxEntry::Receive`. New tests guard determinism + divergence on every field (incl. vbytes, caller, refund predicate) and canonical actor/caller form equivalence. Uniqueness still inherited from the embedded anchor. |

## Known wiring gap

| Area | Gap | Severity | Targeted in phase |
|---|---|---|---|
| `String::as_bytes` panic on witness variants | Sharp edge — documented but no CI lint. | Low | 42 (lint) |
| `decrypt` uses default `PedersenGens` only | Future multi-gens use would need parameterization. | Low | (deferred) |
| `BatchSignatureVerificationFailed` is opaque | Doesn't say which sig failed. | Low | (accepted) |
| Sub-varint U64 branch overflow regression test missing | Pre-existing finding. | Low | 42 |

## Documented but unimplemented

| Opcode / feature | Source | Phase |
|---|---|---|
| Gas charging per opcode | design.md §Resources / Gas | 40 |
| Block resource pools (`B_par : B_ser = 4:1`) | design.md §Block resource pools | 40 (consideration) |
| Memory cap `4× vbytes` enforcement (allocator hooks) | design.md ADR 0002 | 41 |
| Chain-state introspection opcodes (`height`, `blockhash`, `blockburn`, `blockweight`, `blockrate`, `chainstate`) | design.md §Chain-state introspection (forward-looking) | deferred |
| Per-block consensus-side `tick_block` driver | design.md ADR 0005 | (consensus / integrator) |
| Transparent Constructor deploy at delivery | Q4 | (consensus / integrator) |
| Bounce-Output emission on internal-tx failure | Q3 | (consensus / integrator) |

## Architect ADR queue

| Topic | Blocking phase | Notes |
|---|---|---|
| Predicate-call isolation | — (landed as `decisions/0013-predicate-call-isolation.md`, Phase 33) | `open` / `signcall` / `call` all create isolated frames with explicit `gas`/`bytes`. `CellOpen` has no actor identity. Confused-deputy class eliminated by construction. |
| Input-cell witness encoding | — (resolved Phase 22) | Carrier is `String::Cell(Arc<Cell>)` — same pattern as zkvm's `String::Output`. `Instruction::Input` is a unit variant. ADR pending. |
| Actor data model (Q1, Q2, Q4, Q6) | — (resolved during actor build) | Q1: `b"flamevm.actorid"` domain. Q2: vbyte = wire_len(state) + 32. Q4: Constructor-form id deploys transparently at first delivery. Q6: load-without-save is the destroy path. ADR `0010-actor-data-model` queued. |
| Send-ID + Internal TxID (Q3, Q5) | — (resolved during actor build) | Three IDs (External TxID, SendID, Internal TxID). Anchor ratcheted before emitting `TxEntry::Send(Message)`. `Message::id() = H(b"flamevm.send.id" ‖ Message.encode())` (Phase 39) — one canonical wire form, one hash, one identity. Uniqueness still inherited from the embedded anchor; the id commits to every parameter the delivery will carry, mirroring `Cell::id()`. Internal TxID binds to per-call state via `TxEntry::ActorSave` (Phase 36) and to its triggering Send via `TxEntry::Receive(send_id)` emitted as the first effect after Header (Phase 38). ADR `0011-send-id-and-internal-txid` queued. |
| Load/save re-entry lock (Q6) | — (resolved during actor build) | `mark_for_destruction` as the cross-frame lock; per-frame `loaded` flag; tx-end commit sweep. ADR `0012-load-save-reentry-lock` queued. |
| TxLog records effects, not control flow | — (landed as `decisions/0014-txlog-records-effects-not-control-flow.md`, Phase 36) | Dropped `TxEntry::Call`; added `TxEntry::ActorSave`. |
| `Point` enum + `String::Point` unification | — (landed Phase 34) | ADR `0015-point-enum-and-string-point` queued. |
| `MultiscalarMul` as first-class type | — (landed Phase 35) | ADR `0016-multiscalarmul-deferred-verification` queued. Audit `2026-05-26-msm-design.md` flagged size cap + gas-per-term as Phase 39/40 follow-ups. |
| Issuance redesign | — (landed Phase 38) | Two-opcode split: `issuepub` (cleartext, actor frames) + `issuepriv` (confidential, CellOpen frames). `flavor_from_predicate` helper added alongside `flavor_from_actor`. ADR `00XX-issuance-redesign` queued. |
| Extension tag (255) policy | 41 | Reject vs reserve for soft-fork. Currently rejects. |

---

# Section 2 — Pending phases

## Phase 38 — Issuance redesign (landed)

Implemented per the two-opcode design (see spec.md §issuepriv /
§issuepub). The dispatch-peek / branched-`issue` of the previous
draft was rejected in favour of disjoint opcodes:

- **`issuepub`** (`0x92`, internal-only) — cleartext mint under the
  enclosing actor's identity. `qty:Int253 tag → ClearToken`. Same
  semantics as the old `op_issue` cleartext path, just renamed and
  type-tightened (Point-qty → `TypeNotInt253`).
- **`issuepriv`** (`0x91`, external-only) — confidential mint under
  the enclosing predicate's identity. `qty:Variable tag → Token`.
  Allocates a 64-bit range proof on the qty commitment; emits
  `TxEntry::IssuePriv(qty_point, unblinded_flv_point)`. Requires a
  `CallKind::CellOpen` frame (errors `OpcodeRequiresPredicateContext`
  outside) AND external context (errors `ExternalOnly` in internal).

**Flavor derivation.** New helper `flavor_from_predicate` alongside
`flavor_from_actor`. Same transcript label (`flamevm.token.flavor`,
consensus-fixed); the two are domain-separated by their first message
(`b"predicate"` vs `b"actor"`), so a predicate point that happens to
be byte-identical to an actor id still produces a *different* flavor.

**Structural justification**: privacy lives where CS lives. Predicate
frames run in external context (R1CS + batch verifier present) → the
confidential opcode lives here. Actor frames run in internal context
(no CS lane) → the cleartext opcode lives there. The opcode-vs-context
split is mechanical, not policy.

**Tests**: 6 new (in `test_tokens.rs`):
- `issuepriv_emits_token_with_predicate_bound_flavor` — positive
  unit test (CellOpen + external, manual VM setup to preserve
  prover-side commitment witness).
- `issuepriv_disjoint_flavor_from_issuepub` — domain-separation
  invariant.
- `issuepriv_at_external_root_errors_predicate_context` — wrong-frame
  rejection at root.
- `issuepriv_in_internal_context_yields_failure_marker` — CellOpen
  with `external_context: false` → `ExternalOnly` (swallowed into
  failure marker on parent's stack).
- `issuepriv_with_int_qty_yields_failure_marker` — `TypeNotVariable`
  rejection (failure-marker pattern).
- `issuepriv_prove_then_verify_end_to_end` — full Prover→Verifier
  roundtrip with an outer external script opening a cell whose leaf
  runs `commit ; issuepriv ; retire`; verifier reproduces the same
  txlog with matching `Issue` and `Retire` entries bound to the
  predicate-derived flavor.

Plus 3 `issuepub` tests carried over from the previous Phase 38
draft (renamed from the old `op_issue` tests; one negative case
flipped from `TokenRequiresCS` to `TypeNotInt253`).

**ADR backfill (pending)** — `decisions/00XX-issuance-redesign.md`
to record the two-opcode design, the domain-separation invariant,
and the "privacy lives where CS lives" structural argument.

**Companion change — `TxEntry::Receive` landed in the same batch.**
Closes the long-standing gap where `design.md` lists "Receive" as an
internal-tx effect but the code never emitted it. The new variant
`TxEntry::Receive([u8; 32])` carries the originating Send's anchor
(== SendID). `VM::execute_internal` pushes it as the first effect
after `Header`, so the Internal TxID merkle root commits to the
triggering Send — symmetric with `op_input` for external txs. Three
new tests in `test_actor_state.rs` confirm: (a) `Receive` lands at
txlog[1] with the correct SendID; (b) different anchors → different
Internal TxIDs; (c) same anchor → identical Internal TxIDs.

**Companion change — op_save audit fixes (F1, F2, F3).** Closes a
soundness gap where a failed sub-call's `op_save` left registry
state mutated while truncating the corresponding `TxEntry::ActorSave`
from the txlog — a smuggling channel past the call-boundary
rollback.

- **F1 (Critical)**: actor-state rollback on call failure.
  `ActorRegistry` trait gains `push_checkpoint`,
  `pop_checkpoint_commit`, `pop_checkpoint_rollback`. `MemRegistry`
  implements via an internal stack of `(actors, marks)` deep clones.
  VM's `step` pushes on frame-entry, commits on clean-exit,
  rollbacks on `fail_current_call` via a new `registry` param.
  `execute_internal` adds a tx-level checkpoint so root-frame
  failures also roll back. StubRegistry no-ops the methods.

- **F2 (Medium)**: `TxEntry::ActorSave` carries the full
  `ActorState`, not just its 32-byte root — symmetric with
  `TxEntry::Output(Cell)` carrying the full Cell. The merkle leaf
  still commits to the root only via a new infallible
  `ActorState::root()`; the actor id field now hashes through
  `to_hash()` (variant-canonical) rather than `to_bytes()`
  (variant-tagged), so two transactions issuing the same logical
  save via Hash- vs Constructor-form ids produce identical merkle
  leaves.

- **F3 (Low)**: `op_save` failure modes (`MalformedActorState`,
  `TypeNotDict`, `ActorNotFound`) now propagate as call failures,
  rolled back via F1's machinery. No more silent self-destruct on
  a save error.

Three new regression tests in `test_actor_call.rs`:
`f1_failed_subcall_load_does_not_destroy_actor` (the unmatched-load
rollback path), `f1_failed_subcall_save_rolls_back_state_mutation`
(the laundering scenario from the audit, with state-root +
no-ActorSave-leak assertions), `f3_save_failure_rolls_back_and_preserves_actor`
(save-time error no longer destroys the actor).

---

## Phase 39 — Gas-cost estimation

**Goal**: spec-mandated gas metering. Switches the implicit
"gas = 0 per op, no enforcement" placeholder to real charging, with
the seam the consensus crate needs for block-level resource pooling.

**Items**:

- `gas_cost(instruction: &Instruction) -> u64` table per design.md
  §Resources / Gas. Start uniform (= 1 op-byte for most ops, larger
  charges for CS-ops, MSM appends, range proofs, hashing). Refine
  after benchmarks.
- `VM::charge_gas(amount: u64) -> Result<(), VMError>`: increments
  `current_call.gas_used`; errors `GasExhausted` if
  `gas_used > gas_limit`.
- Call `charge_gas` in dispatch loop before each opcode handler.
- `op_call` / `op_open` / `op_signcall` debit gas + refund leftover
  on clean exit (the call machinery already exists; just needs the
  accounting).
- `op_send` debits sent gas from the caller frame.
- `VMError::GasExhausted`.
- **Per-MSM-term charge.** Audit `2026-05-26-msm-design.md`
  recommendation: charge per `(scalar, point)` term appended so a
  contract can't append unbounded terms for free at op_add cost.

**Block resource pool consideration** (design.md §Block resource
pools): the VM doesn't enforce per-block pools — that's consensus.
But Phase 39 must shape its public surface so consensus can plug in:

- `TxResult.gas_used` already exposes per-tx cost cleanly. Confirm
  it's the source of truth for consensus' B_par/B_ser deduction
  (4:1 ratio per design.md).
- Internal-tx sub-sends consume the originator's already-allotted
  budget — VM tracks `gas_used` accurately across the send chain;
  consensus does the bookkeeping.
- No new VM-side pool data structure needed in this phase; the
  hook is the existing `TxResult`.

**Tests**: ~10 — gas charged per op, exhaustion errors, refund on
clean call exit, gas across nested calls, leftover gas to parent,
send-debits-caller, call-args-out-of-budget, MSM-per-term charge,
`TxResult.gas_used` accuracy, gas exhaustion mid-call rolls back
parent-side state.

---

## Phase 40 — Memory cap

**Goal**: enforce the design.md ADR-0002 cap `mem_limit = 4 ×
vbytes(actor)` on transient memory.

**Items**:

- `VM::charge_mem(vbytes: u64) -> Result<(), VMError>` increments
  `current_call.mem_used`; errors `MemoryCapExceeded` if
  `mem_used > mem_limit`.
- Hooks in every allocator:
  - `String::append` / `String::append_bytes`
  - `Dict::insert`
  - `Cell::new`
  - `Token::new` + `WideToken` paths
  - `MultiscalarMul::push_term` (audit follow-up: per-frame size cap
    to bound verifier memory).
- Per-call `mem_used` is transient (discarded on call exit).
- `mem_limit` already on `CallFrame` — set by `CallFrame::new` based
  on actor vbytes; cell-open frames take their cap from the `bytes`
  operand.
- `VMError::MemoryCapExceeded`.

**Tests**: ~12 — memcap triggered on each allocator
(String / Dict / Cell / Token / MSM); per-call reset; mem charged on
send / output payloads; MSM size cap; mem cap not bled across
isolated call frames; mem cap correctly inherited into actor calls
(`4 × vbytes(actor)`); mem cap correctly inherited into cell-open
frames (literal `bytes` operand).

---

## Phase 41 — Integration tests (+ fuzzing & canonicality)

**Goal**: prove the VM works as a system end-to-end, and harden
against parser / encoder bugs. Combined into one phase because the
two test surfaces share fixtures and benefit from coordinated work.

**Integration tests** (under `flamevm/tests/`):

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
- Memory cap trigger end-to-end (after Phase 40).
- Gas exhaustion trigger end-to-end (after Phase 39).
- TxID determinism across distinct prover runs.
- Internal TxID changes when callee state changes between two
  otherwise-identical internal txs (Q5 acceptance test).

The confidential-transfer life-cycle (alice + bob `issue`, transfer
via `mix`, `decrypt`) is already covered by Phase 23. Phase 41
focuses on the actor-side flows and the resource caps.

**Fuzz targets** (under `flamevm/fuzz/`):

- `Instruction::parse` round-trip canonicality — any bytes that
  parse-then-encode produce identical bytes.
- `Cell::decode` accepts only canonical wire-cells (no second valid
  encoding).
- Sub-varint U64 branch overflow regression.
- `read_value` on arbitrary bytes never panics; only `Ok` / `Err`.

**Property tests** (inline `#[cfg(test)]`):

- `Program::to_bytecode → Program::parse` identity.
- `Cell::id` collision resistance on random fixtures.
- Extension tag (255) policy regression (per Architect ADR).

**Tests**: ~10 integration tests + 4 fuzz targets + 3 property
tests.

---

# Section 3 — Design-doc audit

Walk every architectural commitment in `design.md` and trace to
its phase:

| design.md commitment | Status | Phase |
|---|---|---|
| Linear types non-copyable / non-droppable | ✅ Done | 2, 5, 8, 10, 13 |
| No re-entrancy (ADR 0003) | ✅ Done | 27 |
| Transient memory cap = 4× vbytes (ADR 0002) | ⏳ Pending | 40 |
| Single external-tx fee | ⏳ Partial — opcode wired, charging pending | 19 + 39 |
| Per-vbyte persistent storage (ADR 0004) | ✅ Done (VM-side) | 24 + 28 |
| Wire format LE everywhere (ADR 0006) | ✅ Done | All wire-format phases |
| Cell + Actor naming (ADR 0001) | ✅ Done | 8 + 24 |
| Taproot predicates (ADR 0008) | ✅ Done | 8 |
| Atomic external-tx effects | ✅ Done | 18 + 21 |
| Grace + freeze + maturity (ADR 0005) | ✅ Done (VM-side) | 28 |
| TxID binding (signatures + ZK bind to TxID) | ✅ Done | 18 + 20 |
| Internal TxID binds to touched-actor state (Q5) | ✅ Done | 36 (`TxEntry::ActorSave { actor, state }`, merkle leaf hashes `state.root()`) |
| Concurrency (external parallel, internal serial) | ⏳ Consensus crate; VM hooks in 39 | 39 |
| Block resource pools (4:1) | ⏳ Pending | 39 (consideration) |
| Bitcoin coupling / chain-state introspection | ⏳ Deferred | (forward-looking in design.md) |
| Confidential N→M transfers | ✅ Done | 22 + 23 |
| Actor data model + identity (Q1, Q2, Q4) | ✅ Done | 24 |
| Send-id semantics + refund predicate (Q3, Q5) | ✅ Done | 31 |
| Load/save as re-entry lock + self-destruct (Q6) | ✅ Done | 25 |
| Isolated calls for `open` / `signcall` / `call` (ADR 0013) | ✅ Done | 33 |
| Witness types unified under `Point` / `String::Point` | ✅ Done | 34 |
| MultiscalarMul for Sigma-protocol verification | ✅ Done | 35 |
| TxLog records effects, not control flow (ADR 0014) | ✅ Done | 36 |
| Batch rollback under call failure | ✅ Done | 37 |
| Issuance in external + internal context, encrypted qty | ✅ Done (`issuepub` / `issuepriv` split) | 38 |

**Open structural questions** from design.md:

- BFT family / stake / finality / validator rotation — consensus
  crate, not the VM.
- Extension tag (255) policy — VM; Phase 41.
- Refund predicate execution context — **resolved** (Q3): consensus
  emits an Output effect directly under `Message.refund_predicate`.
  No fresh sub-VM. ADR `0011-send-id-and-internal-txid` queued.
- Soft 8× internal-gas multiplier — VM hook in Phase 39, but the
  multiplier policy is consensus.
- Chain-state introspection opcode set + maturity guard — design
  space sketched in design.md §Chain-state introspection (forward-
  looking); deferred until the dual-node surface stabilises.
- Frontend framework — UI crate.

All VM-side commitments and open questions are mapped to a phase
(or marked resolved / deferred).

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
