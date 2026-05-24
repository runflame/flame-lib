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
| 22 | Input-cell witness re-attachment (`Instruction::Input(witness)`)   | ✅      |
| 23 | Confidential N→M end-to-end test harness                           | ✅      |
| 24 | `ActorState` + `ActorRegistry` (real, not stub)                    | ⏳      |
| 25 | `op_load` + `op_save`                                              | ⏳      |
| 26 | `op_call` + frame creation                                         | ⏳      |
| 27 | Re-entrancy guard                                                  | ⏳      |
| 28 | Actor lifecycle: grace + freeze + maturity                         | ⏳      |
| 29 | Introspection: identity (4 opcodes)                                | ⏳      |
| 30 | Introspection: tx header (`timelock`, `version`)                   | ⏳      |
| 31 | Chain info (6 opcodes)                                             | ⏳      |
| 32 | `op_send` + `TxEntry::Send` + send queue                           | ⏳      |
| 33 | Encrypted `issue` (after ADR)                                      | ⏳      |
| 34 | Gas-cost table + per-op charging                                   | ⏳      |
| 35 | Memory-cap allocator + resource introspection (5 opcodes)          | ⏳      |
| 36 | Block resource pools (B_par : B_ser)                               | ⏳      |
| 37 | Fuzz targets + canonicality sweeps                                 | ⏳      |
| 38 | spec.md + design.md sync + ADR backfill                            | ⏳      |
| 39 | End-to-end integration tests                                       | ⏳      |

**23 of 39 complete (59 %).** Total test count: 437 passing; build
clean; 4 leftover compiler warnings, all targeted by Phases 24 / 26
/ 27 / 35.

The confidential N→M transaction test harness (Phase 23) exercises
13 shapes end-to-end through `Prover::prove` → `Verifier::verify`:
- 1→1 / 1→2 / 1→3 / 2→1 / 2→2 / 2→3 / 3→1 / 3→2 / 3→3 (single flavor)
- 2→2 / 3→2 / 2→3 / 3→3 (two flavors)
- 2 negative tests: imbalance rejected, flavor mismatch rejected

### Gas + memory accounting is deferred

`gas_used` and `vbytes_used` currently always read `0` in `TxResult`.
Until the actor machinery (Phases 24–28) lands, every opcode is
treated as costing **gas = 1, mem = 0** *implicitly* — no charging,
no limit enforcement, no introspection. The resource pipeline lights
up in Phases 34–36 once we have:

- a real actor registry to size `mem_limit = 4 × vbytes(actor)` (Phase 24)
- a working call stack so refunds across `op_call` make sense (Phase 26)
- a re-entrancy guard so `mem_used` cleanup on frame exit is well-defined (Phase 27)

This ordering is load-bearing: there's no point implementing gas
charging before there are call frames to charge against, and no
point implementing `mem_limit` before there's an actor whose vbytes
size the cap.

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
| 22 | Input-cell witness re-attachment                   | 7     | `Instruction::Input(Option<Box<InputWitnesses>>)`. Prover-side re-attaches `Commitment::Open` post-decode; point-equality check guards prover bugs. |
| 23 | Confidential N→M test harness                      | 13    | Full input→open→mix→output prove/verify round-trip. Matrix: N∈{1,2,3} × M∈{1,2,3} × {1,2 flavors} + 2 negatives. |

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
| `0x94 send` | spec.md row | 32 |
| `0x95 call` | spec.md row | 26 |
| `0x96 load`, `0x97 save` | spec.md rows | 25 |
| `0x9a timelock`, `0x9b version` | spec.md rows | 30 |
| `0x9c actorid`, `0x9d anchor`, `0xa0 callerid`, `0xa1 method` | spec.md rows | 29 |
| `0x9e gas`, `0x9f bytes`, `0xa2 gaslimit`, `0xa3 memlimit`, `0xa4 newbytes` | spec.md rows | 35 |
| `0xa5..=0xaa` chain-info opcodes | spec.md rows | 31 |
| Memory cap `4× vbytes` enforcement | design.md ADR 0002 | 35 |
| Gas charging per opcode | design.md §Resources / Gas | 34 |
| Block resource pools (`B_par : B_ser = 4:1`) | design.md §Block resource pools | 36 |
| Actor grace + freeze + 100-block maturity | design.md ADR 0005 | 28 |
| Re-entrancy guard | design.md ADR 0003 | 27 |

## Architect ADR queue

| Topic | Blocking phase | Notes |
|---|---|---|
| Input-cell witness encoding | — (resolved Phase 22) | Implemented as `Instruction::Input(Option<Box<InputWitnesses>>)`; verifier-side parses to `None`. ADR pending in Phase 38 housekeeping. |
| Encrypted `issue` semantics | 33 | Variable-only, Predicate-as-issuer, internal-only, or explicit-cid? |
| Extension tag (255) policy | 37 | Reject vs reserve for soft-fork. Currently rejects. |
| Refund predicate execution context | 32 | Fresh micro-VM vs recoverable sub-call? |

---

# Section 2 — Pending phases

## Phase 22 — Input-cell witness re-attachment

**Goal**: close the witness-loss bug for cell-bearing confidential
Tokens, so the full input → open → mix → output chain works for
encrypted inputs.

**Problem**: `Cell::encode` strips `Commitment::Open` → `Closed`
(only points reach the wire). `op_input → Cell::decode` reconstructs
the cell with `Closed` Tokens — the prover loses its own witnesses
on the round-trip. Then `op_mix → value_to_allocated →
commit_variable` errors `WitnessMissing` on the prover side because
`Commitment::Closed` has no `.witness()`. The verifier path works
fine (it only needs points), but no proof can be produced.

**Solution**: extend `Instruction::Input` with an optional
prover-side witness, following the same pattern as
`Instruction::Alloc(Option<Int253>)`. Verifier-side parsing yields
`Input(None)`; bytecode `encode()` writes only `0x90` (witnesses
never reach the wire).

**Items**:
- New type `InputWitnesses { tokens: Vec<TokenWitness> }` where
  `TokenWitness { qty: Commitment, flv: Commitment }` carries Open
  commitments. Lives in a fresh `flamevm/src/witness.rs` (or under
  `vm.rs`).
- `Instruction::Input(Option<Box<InputWitnesses>>)`. Box keeps the
  enum cheap when most variants don't carry witnesses.
- `Instruction::encode` writes `0x90` regardless of inner Option.
- `Instruction::parse` reads `0x90` → `Input(None)`.
- `Program::input_with_witnesses(InputWitnesses) -> Self` builder.
  Keep existing `Program::input()` → no-witness form.
- `op_input(witness: Option<&InputWitnesses>)` (signature change):
  after `Cell::decode`, if witness is `Some`, walk the payload in
  order; for each `Value::Token` entry pop the next `TokenWitness`
  and replace the Token's `qty` / `flv` commitments with the
  witness-bearing Open variants. **Assert** that each Open
  commitment's `to_point()` matches the decoded Closed point;
  mismatch → `WitnessPointMismatch` error.
- Witness queue length must match the count of `Token` entries in
  the cell payload; under-/over-supply → `WitnessCountMismatch`.
- `VMError::WitnessPointMismatch`, `WitnessCountMismatch`.
- Dispatch: `I::Input(w)` arm in `dispatch_external` threads
  `w.as_deref()` into `op_input`. Verifier always sees `None`.

**Tests**: ~6 new
- `input_with_witness_upgrades_closed_to_open` — single Token cell,
  prover gets Open commitments post-input.
- `input_witness_count_mismatch_rejects` — too many / too few.
- `input_witness_point_mismatch_rejects` — bogus witness.
- `input_no_witness_keeps_closed` — None branch unchanged.
- `input_encoded_byte_is_just_0x90` — wire form unaffected.
- `input_witness_for_non_token_payload_skipped` — Int253 / String
  payload entries are passed through without consuming the witness
  queue.

---

## Phase 23 — Confidential N→M end-to-end test harness

**Goal**: complete external-only confidential transaction tests
matching the canonical use case: N inputs, M outputs, K asset
flavors. Single round-trip through `Prover::prove` → `Verifier::verify`.

**Scope**: tests only (and the helpers they need). No new opcodes.

**Test fixture helpers** (in `flamevm/src/vm.rs::tests` or a new
`flamevm/tests/confidential_nm.rs` integration file):

- `make_confidential_token(qty, flv, qty_blind, flv_blind) -> (Token, TokenWitness)`
  — returns the witness-bearing Token (prover-side) and the matching
  `TokenWitness` to feed into `Program::input_with_witnesses`.
- `make_confidential_input_cell(token, predicate, anchor) -> (Cell, InputWitnesses, CallProof)`
  — packages a Token into a cell under a scripts-only predicate
  (`PredicateTree::scripts_only(vec![drop_program_for_payload], ...)`)
  whose unlocked program simply leaves the payload tokens on the
  stack for downstream `mix` to consume. Returns the cell, its
  prover-side witnesses, and a callproof for `open`.
- `assemble_nm_script(inputs, outputs, witnesses) -> Program` —
  builds the canonical N→M script:
    1. For each input cell: `pushstr <cell_bytes>` → `input` (with
       witness) → callproof pieces → `push:0` → `open` (unlocked
       program leaves Tokens on stack).
    2. For each output: `pushstr <qty_open_commitment>` (witness-bearing,
       prover) → `pushstr <flv_open_commitment>` (same).
    3. `push:M push:N mix` → pops 2M output Strings + N input
       Tokens, balances per flavor, pushes M output Tokens.
    4. For each output (deepest first): `push:1` (k=1) → `pushpoint
       <output_predicate>` → `output`.

**Test matrix** (`flamevm/tests/confidential_nm.rs` or inline; one
test fn per shape):
- `confidential_1_to_1_single_flavor`
- `confidential_1_to_2_single_flavor` (split: 10 → [4, 6])
- `confidential_2_to_1_single_flavor` (merge: [3, 7] → 10)
- `confidential_2_to_2_single_flavor` (4-way shuffle)
- `confidential_2_to_2_two_flavors` (gold+silver, balanced
  per-flavor)
- `confidential_3_to_3_two_flavors` (mixed split/merge across
  flavors)
- `confidential_3_to_2_two_flavors`
- `confidential_2_to_3_two_flavors`

Each test:
1. Constructs N input cells with random blinding factors.
2. Builds the script + witness queues.
3. Calls `Prover::prove` → asserts result has `proof: Some`.
4. Calls `Verifier::verify` → asserts accepts.
5. Asserts `result.txlog` contains exactly `Header + N×Input + M×Output`.
6. Asserts `result.txid` matches between prover and verifier.

**Negative tests** (~3):
- `confidential_unbalanced_inputs_rejected` — qty sum mismatch →
  CS fails → `InvalidR1CSProof`.
- `confidential_flavor_mismatch_rejected` — output flavor not
  in input set → CS fails.
- `confidential_range_overflow_rejected` — output qty exceeds
  2⁶⁴ → range proof fails.

**Tests**: ~11 new (8 positive matrix + 3 negative).

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
  `gas_limit = popped_gas` (no charging yet — see Phase 34),
  `newbytes = popped_bytes`.
- Pushes args onto the new frame's stack.
- On clean exit: parent's stack receives results via the existing
  `finish_call` machinery; no gas refund yet (Phase 34).

**Tests**: ~6 (call A → B with args, A → B → C clean exit,
`ArgCount` mismatch errors, unknown method errors).

---

## Phase 27 — Re-entrancy guard

**Goal**: design.md ADR 0003 (no re-entrancy).

**Items**:
- `VM::check_no_reentry(target_actor) -> Result<()>` walks
  `iter::once(&current_call).chain(call_stack.iter())`; errors
  `ReentrancyDetected` if `actor()` matches.
- Called from `op_call` before frame creation.
- `VMError::ReentrancyDetected`.

**Tests**: ~4 (A → B → A direct cycle → `ReentrancyDetected`;
A → B → C → A indirect cycle; recursion within one method
allowed; sibling calls allowed).

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

## Phase 29 — Introspection: identity (4 opcodes)

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

## Phase 32 — `op_send` + `TxEntry::Send` + send queue

**Goal**: async message-send to actors.

**Items**:
- `0x94 send`: `args… k gas bytes method addr → ø`. Pops operands;
  no per-tx gas debit yet (Phase 34 wires charging).
- Builds `Message { target, method, caller, anchor, payload, gas,
  vbytes }` (`caller = current_actor` or `None` for external).
- `TxEntry::Send(MessageRef)` variant — opaque handle into
  `VM.sends`.
- `VM.sends: Vec<Message>` collector. `TxResult.sends` returned.
- Refund predicate: per spec, message has a bounce predicate for
  failures. Architect ADR required ("Open structural question —
  refund predicate context").

**Tests**: ~8 (send from External → `caller = None`, send from
Internal → `caller = Some`, payload preserved, refund-predicate-shape).

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
  - `MultiscalarMul` removal.
  - `op_log` opcode addition.
  - Encrypted `issue` semantics (recording the Phase-33 decision).
  - `BulletproofGens` singleton.
  - Input-cell witness re-attachment (Phase 22 design rationale).
  - Extension tag (255) policy (optional).
  - Refund predicate execution context (optional).
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
- Re-entrancy attempt rejected end-to-end.
- Memory cap trigger end-to-end.
- Gas exhaustion trigger end-to-end.
- TxID determinism across distinct prover runs.

**Note**: the confidential-transfer life-cycle (alice + bob
`issue`, transfer via `mix`, `decrypt`) is already covered by
Phase 23. Phase 39 focuses on the actor-side flows.

**Tests**: ~10 integration tests under `flamevm/tests/`.

---

# Section 3 — Design-doc audit

Walk every architectural commitment in `design.md` and trace to
its phase:

| design.md commitment | Status | Phase |
|---|---|---|
| Linear types non-copyable / non-droppable | ✅ Done | 2, 5, 8, 10, 13 |
| No re-entrancy (ADR 0003) | ⏳ Pending | 27 |
| Transient memory cap = 4× vbytes (ADR 0002) | ⏳ Pending | 35 |
| Single external-tx fee | ⏳ Partial — opcode wired | 19 + 34 |
| Per-vbyte persistent storage (ADR 0004) | ⏳ Pending | 28 |
| Wire format LE everywhere (ADR 0006) | ✅ Done | All wire-format phases |
| Cell + Actor naming (ADR 0001) | ✅ Done | 8 + 24 |
| Taproot predicates (ADR 0008) | ✅ Done | 8 |
| Atomic external-tx effects | ✅ Done | 18 + 21 |
| Grace + freeze + maturity (ADR 0005) | ⏳ Pending | 28 |
| TxID binding (signatures + ZK bind to TxID) | ✅ Done | 18 + 20 |
| Concurrency (external parallel, internal serial) | ⏳ Consensus crate; VM hooks in 36 | 36 |
| Block resource pools (4:1) | ⏳ Pending | 36 |
| Bitcoin coupling (chain-info opcodes) | ⏳ Pending | 31 |
| Confidential N→M transfers | ✅ Done | 22 + 23 |

**Open structural questions** from design.md:
- BFT family / stake / finality / validator rotation — consensus
  crate, not the VM.
- Extension tag (255) policy — VM; Phase 37 (with ADR).
- Refund predicate execution context — VM; Phase 32 (with ADR).
- Soft 8× internal-gas multiplier — VM hook in Phase 34, but the
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
