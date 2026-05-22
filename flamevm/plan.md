# VM instruction implementation plan

Ordered so each phase delivers a coherent, testable slice and unlocks the next. Most opcodes work in both contexts; the few that don't are tagged **[E]** (external-only) or **[I]** (internal-only).

Convention per phase: **Goal**, **Reuses** (already in `flame-lib`), **New** (to add), **Opcodes**, **Tests**.

---

### ✅ Phase 0 — Skeleton (done)
- [x] `VM`, `CallFrame`, `Run`, `CallKind`, `Delegate`, dispatch loop
- [x] `finish_run`, `finish_call` (strict empty-stack rule)
- [x] `0x1d nop`

---

### ✅ Phase 1 — Stack literals & manipulation (done)

**Goal**: scripts can place every primitive on the stack and shuffle items. Unblocks every later phase.

**Reuses**: `Int253::from_bytes`, `Int253::from_parts`, `Value` variants, `String`, `Point`, `Scalar::from_canonical_bytes`.

**Added**:
- `Run::read_u8`, `read_bytes(n)`, `read_be_uint(n)`, `read_sub_varint()`, `is_finished()`.
- `VM::push_value`, `pop_value`, `pop_int253`, `int253_to_stack_index`.
- `Value::try_clone()` (errors `TypeNotCopyable` for linear types), `is_droppable()` (true for plain data + zero-qty `ClearToken`).
- `Point::from_bytes(32)` constructor; `#[derive(Clone, Copy, Debug)]`.
- `String`: `#[derive(Clone, Debug)]`.
- `ClearToken { qty: Int253, flv: Int253 }` with `new`, `qty`, `flv`, `is_zero_qty`.
- `VMError::StackUnderflow`, `TypeNotCopyable`, `TypeNotDroppable`, `UnexpectedEndOfScript`, `TypeNotInt253`, `InvalidInt253Encoding`, `IndexOutOfRange`.

**Opcodes**:
- [x] `0x00..=0x0f` `push:k`
- [x] `0x10..=0x18` `pushint8/16/64/128/full` (big-endian magnitude; sign from opcode pair)
- [x] `0x19` `pushstr` (sub-varint length + bytes)
- [x] `0x1a` `pushpoint`
- [x] `0x1b` `pushtoken` (zero-qty `ClearToken` with canonical-scalar flavor)
- [x] `0x1c` `drop` (refuses non-droppable; preserves the value on error)
- [x] `0x1e` `dup`, `0x20..=0x2f` `dup:k`
- [x] `0x1f` `roll`, `0x30..=0x3f` `roll:k`

**Tests landed** (24 new, all green):
- `push_immediate_k_roundtrips_0_to_15`
- `pushint8_positive_and_negative`, `pushint16/64/128_be_decoding`, `pushint_full_roundtrip`
- `pushint_full_rejects_negative_zero`, `pushint8_at_end_of_script_errors`
- `pushstr_immediate_length`, `pushstr_short_input_errors`
- `pushpoint_roundtrip`
- `pushtoken_zero_qty_with_flavor`, `pushtoken_rejects_noncanonical_flavor`
- `drop_droppable_int`, `drop_underflow_errors`
- `dup_immediate_zero_copies_top`, `dup_immediate_k_picks_kth_from_top`, `dup_dynamic_pops_index`, `dup_out_of_range_errors`, `dup_noncopyable_errors`
- `roll_immediate_moves_kth_to_top`, `roll_zero_is_noop`, `roll_dynamic_pops_index`, `roll_out_of_range_errors`

**Deferred to later phases**:
- `Dict` copyability flag propagation → Phase 5.
- `drop` on linear types that aren't trivially constructible in Phase 1 (Variable/Expression/Constraint) tested only after Phase 11.
- Non-zero-qty `ClearToken` non-droppable case retested once `merge`/`split`/`issue` land in Phase 8.

---

### ✅ Phase 2 — Control flow & explicit return (done)

**Goal**: scripts can branch, loop, and cross call boundaries explicitly. **Strict cross-frame value transfer lives only here.**

**Reuses**: phase-1 stack helpers (`pop_value`, `pop_int253`, `push_value`).

**Added**:
- `VM::enter_run(script)` — pushes current Run onto the run-stack, swaps in a fresh Run. Used by both `run` and `switch`.
- `VM::op_return` — atomic frame-pop with strict empty-stack check after taking `k` return values. At the outermost frame, only `return 0` is permitted (no parent to receive values).
- `VM::op_break_k` — discards `k` to-be-resumed Runs, then advances the current Run's PC to its end. Dispatch loop's `finish_run` then resumes the next Run or invokes `finish_call`. `k > run_stack.len()` → `BreakOutOfCall`.
- `VM::pop_string` helper.
- `Value::type_code()` — uses wire-encoding base tags from spec.md §Types for encodable types; assigns provisional `0xc0..` codes to stack-only types (`Variable`/`Expression`/`Constraint`) pending Architect confirmation.
- `VMError::TypeNotString`, `VerifyFailed`, `BreakOutOfCall`, `BadReturnArity`.

**Opcodes**:
- [x] `0x79` `verify` — fails on zero; pops on success
- [x] `0x7b` `run` — pops a String, suspends current Run, switches to new
- [x] `0x7c` `loop` — resets current Run's PC to 0 (gas will eventually cap unbounded loops)
- [x] `0x7d` `switch` — pops `x a b`, runs `a` if `x` non-zero else `b`
- [x] `0x7e` `return k` — the only way values cross a call boundary; strict arity + clean-stack check
- [x] `0x80..=0x8f` `break:k` — stops current Run plus `k` more from the run-stack; `k` past run-stack depth is `BreakOutOfCall`
- [x] `0x7f` `type` — peeks the top, pushes its type code as `Int253`

**Tests landed** (22 new, all green):
- `verify_truthy_pops`, `verify_zero_fails`, `verify_requires_int`
- `run_creates_nested_run`, `run_resumes_outer_after_subprogram_finishes`, `run_requires_string`
- `loop_resets_pc_to_zero`
- `switch_picks_a_when_x_nonzero`, `switch_a_actually_runs_when_x_nonzero`, `switch_picks_b_when_x_zero`
- `return_zero_at_root_exits_cleanly`, `return_nonzero_at_root_errors`, `return_with_dirty_leftover_errors`, `return_too_few_items_errors`, `return_transfers_values_to_parent`
- `break_zero_ends_current_run_only`, `break_one_ends_subprog_and_outer`, `break_out_of_call_errors`, `break_zero_at_root_ends_cleanly`
- `type_pushes_int253_code`, `type_pushes_string_code`, `type_underflow_errors`

**Deferred to later phases**:
- `verify` accepting `Expression` (not just `Int253`) → Phase 11 once constraint dispatch lands.
- `return` exercised via real `call`/`open` boundary → Phase 15 / 9.
- Type codes for stack-only types are provisional `0xc0..`; surface to Architect for canonicalization.

**Note for Architect**: I extended `pushint16/64/128` BE consistency from spec.md's explicit "big-endian" wording on `pushint16`; worth a sentence to nail down for the other widths. Also: stack-only type codes (Variable/Expression/Constraint) need a canonical assignment — current codes are internal.

---

### ✅ Phase 3 — Int253 arithmetic, logic, size (done)

**Goal**: numeric/logical operations. Wires the existing `Int253` ops through dispatch, adds truncated `div_rem`, and bridges to Dalek for `mod252`.

**Reuses**: `Int253` arithmetic + ordering + sign-magnitude helpers, `Scalar::from_bytes_mod_order_wide`, `Dict::len`, `String::len`.

**Added**:
- `Int253::div_rem(other) -> Option<(Int253, Int253)>` — truncated division: `sign(q) = sign(self) ^ sign(other)`, `sign(r) = sign(self)`. Returns `None` for zero divisor or magnitudes exceeding `u64::MAX` (the latter is a Phase-3 limitation; big-int division comes later).
- `Value::try_eq(&other) -> Result<bool, VMError>` — cross-variant always false; same-variant for plain-data types compares value; `Dict` recursive; linear/constraint same-variant cases error `TypeNotComparable` until later phases.
- `VMError::DivByZero`, `MagnitudeTooLarge`, `StringTooLongForModReduction`, `TypeHasNoLength`, `TypeNotComparable`.

**Opcodes**:
- [x] `0x50` `abs` — pops Int253, pushes (magnitude, sign-bit) with sign-bit on top
- [x] `0x51` `eq` — peeks top two, pushes 1/0; operands stay on stack
- [x] `0x52` `neg` — Int253 negation (Expression overload deferred to Phase 11)
- [x] `0x53` `add`, `0x54` `mul` — Int253 wrap-mod-ℓ arithmetic
- [x] `0x55` `divmod` — truncated division, `DivByZero`/`MagnitudeTooLarge` on bad inputs
- [x] `0x56` `mod252` — String (0..=64 bytes) → LE unsigned → reduce mod ℓ
- [x] `0x57` `not`, `0x58` `and`, `0x59` `or` — Int253 logical (Constraint overloads deferred to Phase 12)
- [x] `0x5f` `size` — String byte count / Dict entry count; other types error `TypeHasNoLength`

**Tests landed** (30 new, all green):
- `abs_of_negative_pushes_magnitude_and_sign`, `abs_of_positive_pushes_sign_zero`, `abs_of_zero_is_sign_zero`
- `eq_pushes_one_for_equal_ints`, `eq_pushes_zero_for_distinct_ints`, `eq_cross_type_is_zero`, `eq_underflow_errors`, `eq_noncomparable_linear_type_errors`
- `neg_flips_sign`, `neg_of_zero_stays_positive`
- `add_basic`, `add_with_negative`, `mul_basic`, `mul_sign_xor`, `add_requires_int_operands`
- `divmod_basic`, `divmod_negative_dividend`, `divmod_by_zero_errors`, `divmod_magnitude_too_large`
- `mod252_empty_string_is_zero`, `mod252_short_string_is_le_value`, `mod252_64_bytes_reduces`, `mod252_too_long_errors`
- `not_zero_to_one`, `not_nonzero_to_zero`
- `and_truth_table` (all four cases), `or_truth_table` (three cases)
- `size_of_string`, `size_of_int_errors`, `size_underflow_errors`

**Deferred to later phases**:
- Expression overloads for `neg`/`add`/`mul`/`eq` → Phase 11.
- Constraint overloads for `not`/`and`/`or` → Phase 12.
- `divmod` for magnitudes exceeding `u64::MAX` (needs big-int division) → out of scope for the current spec; revisit if user demand arises.
- `size` of Dict in spec also covers "struct's number of entries" — the wire layout for non-Dict structs (if any) is folded into Dict for now.

---

### ✅ Phase 4 — String ops (done)

**Goal**: parse and assemble byte buffers (for cells, signature messages, custom protocols).

**Reuses**: `crate::String`, `Int253::abs/to_bytes/from_parts`, `Scalar::from_canonical_bytes`.

**Added**:
- `String::append(&other)`, `append_bytes(&[u8])`, `split_at(n) -> Option<(remainder, head)>`.
- `String::bit_not()`, `bit_or/and/xor(&other) -> Option<String>` (None on size mismatch).
- `String::shift_left(n)`, `shift_right(n)` — bit-level shifts with big-endian semantics; return `(shifted, removed)`. `shift_left` removed is zero-padded on the *left*; `shift_right` removed is zero-padded on the *right*. Implemented via bit-by-bit copy (clear and correct; can be optimized later).
- Internal helpers `bit_at`/`set_bit` (MSB-first numbering).
- `VM::pop_byte_count(max)` helper for the "pop an `Int253`, validate as `usize ≤ max`" pattern.
- `VM::push_read_failure(original)` — restores the original string under a `0` flag for the `read*` opcodes' failure path.
- `VMError::BitwiseSizeMismatch`, `BitCountOutOfRange`.

**Opcodes**:
- [x] `0x40` `readuint` — `s n → s' x 1 | s 0`. Reads `n ≤ 32` bytes LE as unsigned `Int253`.
- [x] `0x41` `readint` — same, but high bit of byte `n-1` is the sign bit.
- [x] `0x42` `readstr` — splits off `n` bytes as a new `String`.
- [x] `0x43` `readpoint` — splits off 32 bytes as a `Point`.
- [x] `0x44` `writebits` — appends low `n` bits of `Int253` magnitude; `n` must be a multiple of 8, `≤ 256`.
- [x] `0x45` `writeint` — appends the full 32-byte sign-magnitude form.
- [x] `0x46` `append` — `s || s'`.
- [x] `0x47` `writezeros` — appends `n` zero bytes.
- [x] `0x48..=0x4b` `bit{not,or,and,xor}` — bytewise; OR/AND/XOR require equal length.
- [x] `0x4c..=0x4d` `shift{left,right}` — bit-level, `n ≤ 256`. Result keeps original length; removed bits returned as a side string with padding side matching the shift direction (left→left-pad, right→right-pad).

**Tests landed** (24 new, all green):
- `read_uint_success`, `read_uint_too_short_preserves_string`, `read_uint_n_too_large_errors`
- `read_int_positive`, `read_int_negative`
- `read_str_success`, `read_str_too_short_preserves`
- `read_point_success`, `read_point_too_short`
- `write_bits_basic`, `write_bits_non_multiple_of_8_errors`
- `write_int_appends_full_32_bytes`
- `append_concatenates`, `write_zeros_appends_n_zero_bytes`
- `bit_not_inverts`, `bit_or_basic`, `bit_and_basic`, `bit_xor_basic`, `bit_or_size_mismatch_errors`
- `shift_left_by_byte`, `shift_left_by_4_bits_left_pads_removed`, `shift_left_zero_is_noop`
- `shift_right_by_byte`, `shift_right_by_4_bits_right_pads_removed`
- `shift_too_large_errors`

**Deferred / open**:
- `writebits` accepting non-byte-aligned `n` — spec says "appends low n bits" without alignment constraint. Phase 4 enforces a multiple-of-8 to preserve byte-aligned strings. If sub-byte writes are wanted, spec must define how to handle the trailing partial byte.
- Reading values whose top byte makes them non-canonical (≥ ℓ) currently produces `InvalidInt253Encoding` rather than the soft-failure path. Confirm with Architect whether such inputs should soft-fail (push original + 0) or hard-error.
- Shift implementation is bit-by-bit; optimize to byte+bit memmove later if hot.

---

### ✅ Phase 5 — Dict ops (done)

**Goal**: build/query/iterate dicts (lists, structs, enum variants), with copyable/portable flag tracking so `dup`/`getdup` and (future) `output` enforce linear-type discipline correctly.

**Reuses**: `Dict` (BTreeMap-backed), `Int253` ordering.

**Added**:
- Dict carries two **sticky** flags: `copyable` and `portable`. Both start `true`; insertion of a non-copyable or non-portable value flips the corresponding flag false permanently. Removal does *not* unset; once poisoned, stays poisoned. This is a safe over-approximation matching the script's intuition.
- `Dict::insert_strict(k, v) -> Result<(), Value>` — strict insert, returns the rejected value on key conflict.
- `Dict::first_key()`, `last_key()`, `next_key_after(&k)` — ordered traversal via `BTreeMap::range`.
- `Dict::try_clone()` — deep clone gated on the sticky `copyable` flag; recursively clones every member.
- `Dict::is_copyable()`, `is_portable()` — accessors for the flags.
- `Dict::insert` now updates flags; `from_values`/`from_entries_unchecked` updated to absorb flags.
- `Value::is_copyable()`, `is_portable()` (parallel to `is_droppable`).
- `Value::try_clone` extended to handle Dict via `Dict::try_clone`.
- `Value::is_droppable` extended: empty dict is droppable.
- `VM::pop_dict` helper.
- `VMError::TypeNotDict`, `DictKeyOccupied`, `DictKeyNotFound`.

**Opcodes**:
- [x] `0x60` `dict` — pop `n`, then `n` key/value pairs (key on top of each pair); duplicates error.
- [x] `0x61` `put` — strict insert; conflict errors.
- [x] `0x62` `replace` — overwrite; returns prior value as optional `{prev 1 | 0}`. Operand order per spec: `dict v k`.
- [x] `0x63` `get` — remove and return `(dict', k, v)`; missing key hard-errors.
- [x] `0x64` `getopt` — remove and return `(dict', {v 1 | 0})`; missing key soft-fails.
- [x] `0x65` `getdup` — copy value at key; missing soft-fails, non-copyable hard-errors. Dict stays on stack unchanged.
- [x] `0x66` `first` — smallest key + flag, or `0` on empty.
- [x] `0x67` `last` — largest key + flag, or `0` on empty.
- [x] `0x68` `next` — smallest key strictly greater than the popped `k`, or `0` if none.

**Tests landed** (24 new, all green):
- `dict_construction_zero_pairs`, `dict_construction_two_pairs`, `dict_construction_duplicate_keys_errors`
- `put_inserts_into_empty`, `put_on_occupied_key_errors`
- `replace_existing_returns_prev`, `replace_absent_returns_zero`
- `get_existing_returns_dict_k_v`, `get_missing_errors`
- `getopt_existing`, `getopt_missing`
- `getdup_copyable`, `getdup_missing_pushes_zero`, `getdup_noncopyable_errors`
- `first_of_empty_pushes_zero`, `first_returns_smallest_key`, `last_returns_largest_key`
- `next_finds_strictly_greater_key`, `next_past_last_pushes_zero`
- `dict_with_token_is_noncopyable`, `dup_of_copyable_dict_succeeds`, `dup_of_noncopyable_dict_errors`
- `empty_dict_is_droppable`, `nonempty_dict_is_not_droppable`

**Open / deferred**:
- The `replace` opcode's operand order (`dict v k`, with `k` on top) differs from `put`'s (`dict k v`). Confirm with Architect — this could be a spec typo, in which case I'll mirror to `dict k v` and re-test.
- `dict_with_token_is_noncopyable` only covers the zero-qty ClearToken case (the only token currently constructible). Re-verify non-portable flag propagation in Phase 8 when ClearTokens can hold non-zero / negative qty.
- Real wire-format encoding/decoding for dicts already exists in `encoding.rs` and is now exercised through `from_entries_unchecked`; flag tracking added via `absorb_flags`.

---

### ✅ Phase 6 — Hash & Merlin (done)

**Goal**: cryptographic primitives that don't touch the CS.

**Reuses**: `crypto::Merlin` wrapper, `merlin::Transcript`.

**Added**:
- `sha2 = "0.10"`, `sha3 = "0.10"` deps in `flamevm/Cargo.toml`.
- `Merlin::new(user_label)` — fresh transcript bound to a fixed domain separator (`flamevm::merlin.v1`) plus the user label, since `Transcript::new` requires `'static`.
- `Merlin::write_bytes(user_label, data)` — absorbs `(label, data)` into the transcript.
- `Merlin::read_bytes(user_label, n)` — squeezes `n` bytes of challenge under `label`.
- `VM::pop_merlin` helper.
- `VMError::TypeNotMerlin`.

**Opcodes**:
- [x] `0x69` `merlin` — pops label, pushes a fresh transcript.
- [x] `0x6a` `merlinwrite` — `merlin label str → merlin`. Stack order per spec (str on top).
- [x] `0x6b` `merlinread` — `merlin label n → merlin str`. Squeezes `n` bytes.
- [x] `0x6c` `sha256`, `0x6d` `sha512`, `0x6e` `sha3` (SHA3-256) — pop a String, push the digest.

**Tests landed** (11 new, all green):
- `merlin_creates_transcript`
- `merlin_is_noncopyable_and_nondroppable` — confirms linear-type discipline
- `merlin_write_then_read_produces_bytes`
- `merlin_read_is_deterministic` — same inputs → same challenge
- `merlin_read_diverges_on_different_label` — protocol-level separation
- `merlin_write_requires_merlin_on_bottom`
- `sha256_empty`, `sha256_abc` — NIST test vectors
- `sha512_empty` — NIST test vector
- `sha3_empty`, `sha3_abc` — NIST test vectors

**Design notes**:
- `Merlin::new` cannot pass a runtime label directly to `Transcript::new` (`'static` constraint). Workaround: fixed domain separator + first message is the user label under a fixed tag. Protocol property preserved (different user labels → divergent transcripts from byte 0).
- `Merlin` is **linear**: neither copyable nor droppable. Tests confirm both via `dup:0` and `drop` opcodes returning the right errors.
- No `MerlinLabelTooLong` introduced — strings can be any length per Phase 4 semantics. If DoS becomes a concern, gas metering (Phase 17) is the natural cap.
- SHA test vectors use NIST FIPS-180 / FIPS-202 published values; one positive vector and one empty-input vector per hash.

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

### ✅ Phase 9 — Cells, outputs, open, signtx, signrun (done)

**Goal**: cell life-cycle (construction → sealing → opening) with Taproot-style unlock; first opcodes that nest a new `Run` driven by an external program.

**Simplification adopted**: cell-opening is **Run-level, not Call-level**. `open`/`signrun` push a new Run onto `run_stack` and pour the cell's payload + caller's args onto the current call's stack. No isolation. Any failure inside is a hard call-level failure (= whole external tx). Full transactional isolation (Model B) is deferred to Phase 15 for `call`.

**Added**:
- `Anchor::ratchet()` — domain-separated next-anchor derivation via Merlin transcript `flamevm.anchor.ratchet.v1`.
- `cell.rs` module — `Predicate { Opaque, Tree }` enum, `PredicateTree` (single-leaf for Phase 9), `CallProof { internal_key, neighbors, position, program }`, `Cell { predicate, anchor, payload }`. Taproot helpers (`taproot_tweak`, `merkle_leaf_hash`, `merkle_node_hash`, `merkle_walk_up`) all via Merlin per architect.
- `Value::Cell(Cell)` — renamed from `Value::Object(Object)`; the old `object.rs` was renamed to `cell.rs`.
- `TxEntry::Output(Cell)` variant; `VM.txlog` field. (Note: TxEntry no longer derives Clone/Debug/Serde since Cell isn't.)
- `VM::pop_point`, `pop_cell`, `pop_n_values`, `pop_n_portable` helpers.
- `VM::decode_callproof` with a fixed-layout wire form (internal_key || pos_len:u32 || position || n_count:u32 || neighbors × 32 || program).
- `VM::signtx_message` / `signrun_message` — Merlin transcripts binding the deferred sigs to cell-id / cell-id+program.
- `VMError`: `TypeNotCell`, `CallProofMismatch`, `AnchorMissing`, `NonPortableInOutput`, `BadSignatureBytes`, `TypeNotPoint`, `MalformedCallProof`.

**Opcodes**:
- [x] `0x91` `cell` — `args… k pred → cell`. Builds a transient Cell; consumes `last_anchor`, ratchets.
- [x] `0x92` `output` — same construction, emits `TxEntry::Output` instead of pushing.
- [x] `0x93` `open` — `cell args… k callproof → results…`. Verifies callproof (hard-fail), pours payload + args onto stack, enters Run with unlocked program.
- [x] `0x98` `signtx` — `cell → items… k`. Pours payload + count onto stack; records DeferredSig (signature resolved at finalize).
- [x] `0x99` `signrun` — `cell prog sig args… m → items… k`. Records DeferredSig over (cell-id, program); pours payload + args; enters Run with `prog`.

**Tests landed** (11 new, all green):
- `anchor_ratchet_changes_value_and_is_deterministic`
- `cell_opcode_requires_seeded_anchor`
- `cell_opcode_builds_a_cell_and_ratchets_anchor`
- `cell_opcode_rejects_non_portable_payload`
- `cell_is_noncopyable_and_nondroppable`
- `output_opcode_emits_to_txlog_without_pushing`
- `open_with_valid_callproof_runs_program`
- `open_with_wrong_program_hard_fails`
- `signtx_pours_payload_and_records_deferred_sig`
- `signrun_runs_signed_program`
- `signrun_rejects_wrong_signature_length`

**Deferred to later phases**:
- Full canonical wire encoding of Cell (list-style Dict) — Phase 17 territory; current `Cell::id` uses a Merlin-transcript hash over predicate+anchor+payload type-codes (sufficient for distinguishing cells but not yet bytewise-canonical with payload contents).
- Multi-leaf `PredicateTree` (general balanced merkle); Phase 9 ships single-leaf only.
- Batched Schnorr verification of `DeferredSig` (Phase 14).
- Anchor seeded via `input` opcode (Phase 10); for Phase 9 tests seed `VM.last_anchor` directly.
- `Model B` (full transactional rollback at call boundaries) for Phase 15 `call`.

**Surfaced for Architect**:
- The `Cell::id` Merlin transcript currently absorbs payload via type-code only. Full encoding-driven payload binding lands when the cell wire format is finalized.
- `decode_callproof` uses a minimal fixed-layout wire form. The architect-suggested "list-style Dict with strings" encoding (neighbors as a Dict of strings, position as a String, program as a String, internal_key as a String) is more idiomatic but adds Dict-decoding plumbing. Worth a follow-up to align with the rest of the wire formats.

---

### Phase 9 — Outputs, objects, cell-open, signtx/signrun (superseded by ✅ Phase 9 above)

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
