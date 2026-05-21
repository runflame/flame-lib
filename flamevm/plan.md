# VM instruction implementation plan

Ordered so each phase delivers a coherent, testable slice and unlocks the next. Most opcodes work in both contexts; the few that don't are tagged **[E]** (external-only) or **[I]** (internal-only).

Convention per phase: **Goal**, **Reuses** (already in `flame-lib`), **New** (to add), **Opcodes**, **Tests**.

---

### ✅ Phase 0 — Skeleton (done)
- [x] `VM`, `CallFrame`, `Run`, `CallKind`, `Delegate`, dispatch loop
- [x] `finish_run`, `finish_call` (strict empty-stack rule)
- [x] `0x1d nop`

---

### Phase 1 — Stack literals & manipulation

**Goal**: scripts can place every primitive on the stack and shuffle items. Unblocks every later phase.

**Reuses**: `Int253::from_bytes`, `Value` variants, `string::String`, `crypto::Point`, `Token`.

**New**:
- `VM::push_value`, `pop_value`, `pop_int253`, `pop_string`, `peek(k)`, `roll(k)`.
- `Run::read_u8`, `read_le(n)`, `read_bytes(n)` — inline-bytes reader for `pushint*`/`pushstr`.
- `Value::is_copyable()`, `is_droppable()` (per spec.md §Types).
- `VMError::StackUnderflow`, `TypeNotCopyable`, `TypeNotDroppable`, `UnexpectedEndOfScript`.

**Opcodes**:
- [ ] `0x00..=0x0f` `push:k`
- [ ] `0x10..=0x18` `pushint8/16/64/128/full`
- [ ] `0x19` `pushstr` (uses `encoding::sub_varint`)
- [ ] `0x1a` `pushpoint`
- [ ] `0x1b` `pushtoken` (zero-qty)
- [ ] `0x1c` `drop`
- [ ] `0x1e` `dup`, `0x20..=0x2f` `dup:k`
- [ ] `0x1f` `roll`, `0x30..=0x3f` `roll:k`

**Tests**: round-trip each literal type; `dup` on `Token` errors; `roll` past stack depth errors.

---

### Phase 2 — Control flow & explicit return

**Goal**: scripts can branch, loop, and cross call boundaries explicitly. **Strict cross-frame value transfer lives only here.**

**Reuses**: phase-1 stack helpers.

**New**:
- `VM::push_run(script)` (pushes current onto `run_stack`).
- `VM::break_runs(k)` — errors with `BreakOutOfCall` if `k > run_stack.len()`.
- `VM::return_from_call(k)` — pop `k` items + count, **assert remaining stack empty**, pop call frame, push items onto parent. One atomic step.
- `Value::type_code() -> u8` per the encoding-tag table in spec.md §Types.
- `VMError::VerifyFailed`, `BreakOutOfCall`, `BadReturnArity`.

**Opcodes**:
- [ ] `0x79` `verify`
- [ ] `0x7b` `run`, `0x7c` `loop`, `0x7d` `switch`
- [ ] `0x7e` `return k` — the **only** way values cross a call boundary
- [ ] `0x80..=0x8f` `break:k`
- [ ] `0x7f` `type`

**Tests**: `return` with leftover stack → `StackNotClean`; `break:k` past run_stack → `BreakOutOfCall`; nested `run`/`loop` terminates only on `break`/`return`.

---

### Phase 3 — Int253 arithmetic, logic, size

**Goal**: numeric/logical operations. Wires the existing `Int253` ops through dispatch.

**Reuses**: `Int253` arithmetic, ordering, `Scalar::from_bytes_mod_order_wide`.

**New**:
- `Int253::div_rem` (currently missing).
- `Int253::abs_with_sign()` (split).
- `Int253::from_string_mod_l(bytes)` for `mod252`.
- `Value` equality across compatible variants.

**Opcodes**:
- [ ] `0x50` `abs`, `0x51` `eq`, `0x52` `neg`
- [ ] `0x53` `add`, `0x54` `mul` (Int253 path only; Expression overload in Phase 11)
- [ ] `0x55` `divmod`, `0x56` `mod252`
- [ ] `0x57` `not`, `0x58` `and`, `0x59` `or` (Constraint overload in Phase 12)
- [ ] `0x5f` `size` (string len / dict count)

**Tests**: wraparound matches Int253 unit tests; `divmod` rejects zero divisor; `mod252` on 64-byte input matches Dalek reference.

---

### Phase 4 — String ops

**Goal**: parse and assemble byte buffers (for cells, signature messages, custom protocols).

**Reuses**: `crate::String`.

**New**:
- Destructive readers: `String::read_uint_le(n)`, `read_int_le(n)`, `read_substr(n)`, `read_point`.
- Appenders: `write_bits_le(int, n)`, `write_int_full(int)`, `write_zeros(n)`, `bitwise(op, other)`, `shift_left/right(n)`.
- `VMError::StringTooShort`, `BitwiseSizeMismatch`, `ShiftTooLarge`.

**Opcodes**:
- [ ] `0x40..=0x43` `read{uint,int,str,point}`
- [ ] `0x44..=0x47` `write{bits,int,zeros}`, `append`
- [ ] `0x48..=0x4b` `bit{not,or,and,xor}`
- [ ] `0x4c..=0x4d` `shift{left,right}`

**Tests**: short-input rejection; bitwise on mismatched sizes errors.

---

### Phase 5 — Dict ops

**Goal**: build/query/iterate dicts (lists, structs, enum variants).

**Reuses**: `Dict` (BTreeMap-backed, sorted).

**New**:
- `Dict::insert_strict` (fails on occupied key).
- `Dict::range_after(k)` for `next`.
- Portable / copyable flag propagation when items are inserted.

**Opcodes**:
- [ ] `0x60` `dict`, `0x61` `put`, `0x62` `replace`
- [ ] `0x63` `get`, `0x64` `getopt`, `0x65` `getdup`
- [ ] `0x66` `first`, `0x67` `last`, `0x68` `next`

**Tests**: `put` on occupied key fails; `getdup` of non-copyable errors; non-portable item poisons dict's portable flag.

---

### Phase 6 — Hash & Merlin

**Goal**: cryptographic primitives that don't touch the CS.

**Reuses**: `crypto::Merlin`.

**New**:
- `sha2`, `sha3` crate deps.
- `VMError::MerlinLabelTooLong`.

**Opcodes**:
- [ ] `0x69..=0x6b` `merlin`, `merlinwrite`, `merlinread`
- [ ] `0x6c..=0x6e` `sha256`, `sha512`, `sha3`

**Tests**: known-vector roundtrips for each hash; transcript roundtrip.

---

### Phase 7 — Introspection (header, resources, identity)

**Goal**: scripts can read their own running context.

**Reuses**: `CallKind.actor()` (already in phase 0).

**New**:
- `CallKind::caller_id()`, `method_key()`, `anchor()` (return `Option`).
- `VMError::OpcodeRequiresActorContext`.

**Opcodes**:
- [ ] `0x9a` `timelock`, `0x9b` `version`
- [ ] `0x9c` `actorid`, `0xa0` `callerid`, `0xa1` `method`, `0x9d` `anchor`
- [ ] `0x9e` `gas`, `0xa2` `gaslimit`, `0x9f` `bytes`, `0xa3` `memlimit`, `0xa4` `newbytes`

**Tests**: `actorid` from `ExternalRoot` errors; `gas` reflects remaining budget once gas table lands.

---

### Phase 8 — Clear tokens & issuance

**Goal**: non-confidential token arithmetic + flavor binding to actor ID.

**Reuses**: `ClearToken`, `Token`, `WideToken`.

**New**:
- `ClearToken::merge`, `split`, `borrow`.
- `flavor_from_actor(actor, tag) -> Int253`.
- Issuance / retirement effects into `TxLog` (introduce `TxLog` if not present).

**Opcodes**:
- [ ] `0x70` `amount`, `0x78` `issueflv`
- [ ] `0x71` `issue` (clear path), `0x72` `retire` (clear path), `0x73` `borrow` (clear path)
- [ ] `0x74` `merge`, `0x75` `split`

**Tests**: flavor-mismatch merge fails; split past quantity fails; retire emits the right log entry.

---

### Phase 9 — Outputs, objects, cell-open, signtx/signrun

**Goal**: external tx can seal portable values into cells; cell-opening creates a `CellOpen` call frame.

**Reuses**: `Predicate`, `Anchor`, `DeferredSig`.

**New**:
- `Cell` type per design.md §Cells (predicate + anchor + payload).
- `Anchor::ratchet()`.
- `VM::push_call_frame(CellOpen { ... }, script)` — pushes the cell-open isolation boundary.
- `signtx` / `signrun`: push a `DeferredSig`.
- `VMError::AnchorMissing`, `NonPortableInOutput`.

**Opcodes**:
- [ ] `0x91` `object`, `0x92` `output`
- [ ] `0x93` `open` — pushes `CallKind::CellOpen` frame
- [ ] `0x98` `signtx`, `0x99` `signrun`

**Tests**: `output` rejects non-portable values; `open` confirms caller's stack is invisible to the inner script; signtx pushes a DeferredSig with matching key/message.

---

### Phase 10 — Inputs (Utreexo) and send queue

**Goal**: external tx claims outputs; either context queues async messages.

**Reuses**: phase-9 cell decoding, `Message` type.

**New**:
- `Utreexo` trait (placeholder; real impl out of scope here).
- `VM.sends: Vec<Message>` collector.
- `TxResult.sends` field.
- `VMError::ExternalOnly`.

**Opcodes**:
- [ ] `0x90` `input` **[E]**
- [ ] `0x94` `send`

**Tests**: `input` from `InternalRoot` errors; `send` from `InternalRoot` records `caller` correctly.

---

### Phase 11 — Constraint system bootstrap (real Prover/Verifier)

**Goal**: stand up real Delegate impls and the first CS-touching opcodes. After this phase, external txs can actually be proven.

**Reuses**: `bulletproofs::r1cs::Prover`/`Verifier`, `constraints::{Variable, Expression, Constraint}`.

**New**:
- `flamevm::delegates::Prover` — wraps `r1cs::Prover`, stores witness scalars.
- `flamevm::delegates::Verifier` — wraps `r1cs::Verifier`, holds the proof bytes.
- Real `Delegate::commit_variable` impls for both.
- Dispatch overloads: `0x52 neg`, `0x53 add`, `0x54 mul`, `0x51 eq` accept `Expression` operands.

**Opcodes (all [E])**:
- [ ] `0x5a` `const`, `0x5b` `extvar`, `0x5c` `intvar`, `0x5d` `expr`

**Tests**: `const(7) + const(3) == const(10)` — prove + verify roundtrip.

---

### Phase 12 — Range proofs & constraint composition

**Goal**: bit-range constraints and constraint algebra.

**Reuses**: `spacesuit::range_proof`, `Constraint::{and, or, not, verify}`.

**New**: overloads for `0x57 not`, `0x58 and`, `0x59 or` on `Constraint`; range proof gadget invocation.

**Opcodes (all [E])**: 
- [ ] `0x5e` `range`

**Tests**: in-range witness verifies; out-of-range value fails verification.

---

### Phase 13 — Confidential tokens, mix, decrypt

**Goal**: encrypted-quantity / encrypted-flavor operations using spacesuit cloak.

**Reuses**: `spacesuit::cloak`, `Token`, `WideToken`.

**New**:
- Confidential paths of `0x71 issue`, `0x72 retire`, `0x73 borrow` (point-committed qty/flv).
- `Token::decrypt` (cleartext reveal with blinding).

**Opcodes (all [E])**:
- [ ] `0x76` `mix`
- [ ] `0x77` `decrypt`
- [ ] Confidential branches of `0x71/0x72/0x73` (extends phase 8)

**Tests**: 2-in/2-out mix balances qty/flv; decrypt fails on commitment mismatch.

---

### Phase 14 — Signatures (sigverify + delegate finalize)

**Goal**: sig checks accumulate during execution and are processed at the end. Verifier batches; prover signs.

**Reuses**: `musig::Signature`, `musig::BatchVerification`, `DeferredSig`.

**New**:
- `Prover::finalize`: walks `deferred_sigs`, signs missing entries with stored keys.
- `Verifier::finalize`: walks `deferred_sigs`, batches into one MSM check.
- `VMError::SignatureFailed`.

**Opcodes**:
- [ ] `0x6f` `sigverify`

**Tests**: prover→verifier roundtrip on a known message; bad sig in batch fails the whole batch.

---

### Phase 15 — Internal calls, load, save (the actor heart)

**Goal**: synchronous actor-to-actor calls with full isolation, re-entrancy guard, state persistence.

**Reuses**: `CallKind::ActorCall`, `ActorRegistry` trait (extended).

**New**:
- `ActorRegistry::load_actor`, `save_actor` (real, not the phase-0 stub).
- `ActorState` type per design.md §Actor structure.
- `VM::check_no_reentry(target)` — walks `iter::once(&current_call).chain(call_stack.iter())`, errors if target's `actor()` matches.
- `VMError::ReentrancyDetected`, `ActorFrozen`, `LoadWithoutSave`.

**Opcodes (all [I])**:
- [ ] `0x95` `call`
- [ ] `0x96` `load`, `0x97` `save`

**Tests**: A → B → A direct cycle → `ReentrancyDetected`; recursion within one method allowed; save persists across nested call boundaries.

---

### Phase 16 — Chain info

**Goal**: scripts read block-level facts.

**Reuses**: phase-0 `BlockContext` stub.

**New**: populate `BlockContext` with `height`, `blockhash(h)`, `blockburn(h)`, `blockweight(h)`, `blockrate(h)`, `chainstate(h)`; enforce 100-block maturity.

**Opcodes (all [I])**:
- [ ] `0xa5..=0xaa` `height`, `blockhash`, `blockburn`, `blockweight`, `blockrate`, `chainstate`

**Tests**: chain-info from `ExternalRoot` errors; `blockburn` past maturity errors.

---

### Phase 17 — Fee, finalization, full tx assembly

**Goal**: tie everything together — gas-table metering, fee debt, txlog, txid, proof emission.

**Reuses**: all prior phases.

**New**:
- Static gas-cost table (`gas_cost(op) -> u64`); `VM::charge_gas` in dispatch.
- `CheckedFee` accumulator on VM; `WideToken` debt construction.
- `TxLog` populated from effects; `TxID` as merkle root.
- `TxResult` final shape: `{ txid, log, total_fee, sends, gas_used, vbytes_used, proof: Option<R1CSProof> }`.
- Block-level Phase-3 parallel/serial accounting (per design.md §Block limits).

**Opcodes**:
- [ ] `0x7a` `fee`

**Tests**: external tx round-trip (script → prover bytes → verifier ok); fee total matches sum of `fee` opcodes; serial-pool accounting holds.

---

## Notes on dependencies & sequencing

```
Phase 1 ── 2 ── 3 ── 4 ── 5 ── 6
                ↓
                7 ── 8 ── 9 ── 10
                          ↓
                          11 ── 12 ── 13
                                       ↓
                                14 ──┘    
                                ↓
                                15
                                ↓
                                16 ── 17
```

- **Phases 1–7** are context-agnostic and need no proof machinery — implement and test purely through `execute_internal` against a stub registry.
- **Phase 11** is the first phase that needs real `Prover`/`Verifier` implementations; everything before runs without them.
- **Phase 15** is the only phase that exercises non-trivial `ActorRegistry` semantics; until then the stub registry from phase 0 is fine.
- **Phase 17** is where the block-level rules from design.md §Gas land in code; everything before just records effects without enforcing block budgets.

## Quality control gates (per phase)

For each phase, the merge gate is:
1. `cargo test -p flamevm` is green, no new warnings beyond the pre-existing dead-code ones.
2. Every opcode in the phase has at least one positive and one negative test.
3. No phase adds new public API beyond what the opcodes need — internal helpers stay `pub(crate)`.
4. The opcode dispatch in `step_external` / `step_internal` is updated; no opcode silently routes via `try_common` if it has context-restricted semantics.
