# Architectural questions — Phases 1–6

Open items surfaced while implementing opcodes. Each item is a question for Architect with my current implementation choice, the rationale, and a placeholder for the canonical answer. Once an answer lands, the corresponding code/spec/test gets revisited.

Format per item:

> **Question / decision needed**
> Implementation today: …
> Stakes: …
>
> _Architect response:_ _(empty until filled in)_

---

## Phase 1 — Stack literals

### 1.1 `pushtoken` variant ✅ **resolved**

Spec (`0x1b pushtoken`): "Pushes 0-qty token of any flavor, reading next 32 bytes for flavor."

> The spec doesn't say which token variant is produced (`Token`, `ClearToken`, `WideToken`).
>
> Implementation today: `ClearToken { qty: 0, flv: Int253(scalar) }`. The 32 bytes are parsed via `Scalar::from_canonical_bytes` (positive Int253). Non-canonical bytes hard-error.
>
> Stakes: choice changes the variant on the stack (affects subsequent ops that pattern-match), the flavor representation (Int253 vs Point), and whether the bytes are validated as a scalar or accepted raw.
>
> _Architect response:_ ClearToken is a good choice. We lack ability to dynamically choose flavor. Let's change this into taking int253 argument as a flavor. Update the code and the spec.md with stack diagrams accordingly.
>
> **Applied** — `0x1b pushtoken` now reads `flv → token` (pops an Int253 from the stack instead of reading 32 inline bytes); spec.md line 319 updated; `op_pushtoken` rewritten; 7 tests retargeted, `pushtoken_rejects_noncanonical_flavor` removed (no longer reachable), `pushtoken_requires_int_flavor` + `pushtoken_full_flavor_via_pushint_full` added. 236 tests green.

### 1.2 `pushint{16,64,128}` byte order ✅ **resolved**

Spec only states "big-endian" explicitly for `pushint16` (`0x12/0x13`). The other widths inherit no explicit endian.

> Implementation today: BE for all of `pushint16/64/128` (consistency).
>
> Stakes: changes the encoded literal layout; consensus-critical for scripts that embed multi-byte ints.
>
> _Architect response:_ use LE everywhere.
>
> **Applied** — `Run::read_be_uint` renamed to `read_le_uint`; spec.md rows 0x12-0x18 updated to say "little-endian"; tests renamed `pushint{16,64,128}_be_decoding` → `_le_decoding` and re-encoded with `to_le_bytes()`; `shift_too_large_errors` and `push_small_uint` helper switched to LE. 254 tests green.

---

## Phase 2 — Control flow

### 2.1 Type codes for stack-only types ✅ **resolved**

Spec table maps wire-encodable types to a tag base (Int253 → 0, String → 68, Dict → 128, Point → 248, …, Merlin → 253). It does not assign codes for stack-only types (`Variable`, `Expression`, `Constraint`, `MultiscalarMul`).

> Implementation today: `Value::type_code()` returns `0xc0` for Variable, `0xc1` for Expression, `0xc2` for Constraint. These are arbitrary, chosen to avoid wire-tag collisions (254 reserved, 255 extension).
>
> Stakes: any script using the `type` opcode (`0x7f`) on a stack-only value sees these codes. They become observable behavior and need a canonical assignment if scripts are to be portable across implementations.
>
> _Architect response:_ wire encoding is also observable behavior and will be stuck forever. In principle, we don't have to conflate typecodes with wire prefixes, but if all our types fit among the codes, we might as well reuse them. It does not mean, though, that all types are encodeable - we will not serialize Variables, Expressios, MultiscalarMul, WideTokens and some others.
>
> **Applied** — kept current `type_code()` assignments: encodable types reuse their wire-tag base (Int253=0, String=68, Dict=128, Point=248, Token=249, ClearToken=250, WideToken=251, Object=252, Merlin=253); non-encodable stack-only types use the higher private range (Variable=0xc0, Expression=0xc1, Constraint=0xc2). The split between "has a wire tag" and "is actually serialized" will be reconciled in encoding work (revisit which of Token/WideToken/Object/Merlin actually round-trip through the wire format).

### 2.2 End-of-script without explicit `return` ✅ **resolved**

Spec describes `return` and `break:0` but does not state whether a script *must* end with one or whether falling off the end is also a clean exit.

> Implementation today: falling off the end of the entry Run of a call is equivalent to break:0 (the call exits cleanly iff the stack is empty). No mandate to use `return 0`.
>
> Stakes: stricter interpretation would catch a missing return statement at validation time; looser is forgiving but allows silent script truncation.
>
> _Architect response:_ no need to do explicit return to save space on compact scripts. We need to be protected against script truncation by correct cryptographic commitment (under signature, taproot hash etc.) If we do not return explicitly, we return 0 items and need to keep the stack clean as well.
>
> **Applied** — no code change. Current `finish_call` semantics already match: end-of-entry-Run + empty stack = clean exit; end-of-entry-Run + non-empty stack = `StackNotClean`. Script-truncation protection is the caller's responsibility (signature / Taproot binding on the script bytes).

---

## Phase 3 — Int253 arithmetic, logic, size

### 3.1 `divmod` rounding convention ✅ **resolved**

Spec (`0x55 divmod`): "Computes the quotient and the remainder for int." No definition of rounding behavior for negative operands.

> Implementation today: truncated division (toward zero). `sign(q) = sign(x) XOR sign(z)`, `sign(r) = sign(x)`. Matches Rust's `i64` convention. Example: `-13 divmod 5` → `q = -2, r = -3`.
>
> Stakes: divmod is the only signed-division op; rounding convention is consensus-critical for scripts that do signed arithmetic.
>
> _Architect response:_ let's use the simplest option that makes the smallest code. If it matches Rust convention, even better.
>
> **Applied** — no code change. Current `Int253::div_rem` already implements truncated division matching Rust's `i64`. Tests `divmod_basic` + `divmod_negative_dividend` confirm.

### 3.2 `divmod` for magnitudes exceeding `u64::MAX` ✅ **resolved**

> Implementation today: returns `MagnitudeTooLarge` error for any input whose magnitude doesn't fit `u64`. Rationale: bigint division isn't trivial; deferred until demand.
>
> Stakes: the spec doesn't restrict magnitudes, so this is a "spec hole" the implementation chose to fill conservatively. Any script using divmod on > 64-bit operands fails.
>
> _Architect response:_ implement a full-sized divmod with rust-like i64 semantics.
>
> **Applied** — `Int253::div_rem` rewritten to use a 4×u64-limb unsigned divmod (`divmod_u256` in `int253.rs`); operands up to ℓ are supported. `MagnitudeTooLarge` removed from `VMError`. The Phase-3 test `divmod_magnitude_too_large` was replaced with `divmod_full_width_magnitude_succeeds` (`2^128 / 1`).

### 3.3 `eq` semantics for linear types ✅ **resolved**

Spec (`0x51 eq`): "Checks equality of two types." Silent on what equality means for tokens, transcripts, variables, expressions.

> Implementation today: same-variant comparisons for `Int253`, `String`, `Point`, `Dict` use value/byte equality. Cross-variant returns `0` (false). Same-variant linear types (`Token`, `ClearToken`, `WideToken`, `Object`, `Merlin`, `Variable`, `Expression`, `Constraint`) hard-error with `TypeNotComparable`.
>
> Stakes: scripts that try to compare linear values get a hard fail rather than a meaningful boolean. Alternatives: always return 0, or define identity equality (pointer-style — but linear types have no stable identity).
>
> _Architect response:_ we don't have references semantics, so identity-equality is impossible. Tokens are confusing to compare since encrypted token can in theory be equal to unencrypted one, but that requires extra checks and computation. Lets enable `eq` only for obvious primitives: only ints, string, points. dicts require recursion and non-trivial gas computation, so lets avoid it.
>
> **Applied** — `Value::try_eq` Dict branch removed; Dict same-variant now falls into the `TypeNotComparable` catch-all. Cross-variant still returns `Ok(false)`. New test `eq_two_dicts_is_not_comparable` asserts.

### 3.4 `mod252` upper-bound semantics ✅ **resolved**

Spec (`0x56 mod252`): "Interprets 0..64-byte as a little-endian unsigned integer and mod-reduces to R255 group order."

> "0..64-byte" is ambiguous between half-open `[0, 64)` and closed `[0, 64]`.
>
> Implementation today: inclusive — strings of length 0 through 64 accepted, 65+ rejected (`StringTooLongForModReduction`).
>
> Stakes: edge-case behavior for 64-byte inputs.
>
> _Architect response:_ Accept up to 64 bits inclusive.
>
> **Applied** — no code change. Implementation already accepts 0..=64 *bytes* (the response says "bits" but context makes it clear 64-byte upper bound was intended; the inclusive bound matches). Test `mod252_64_bytes_reduces` confirms the 64-byte case.

---

## Phase 4 — String ops

### 4.1 `writebits` for non-byte-aligned `n` ✅ **resolved**

Spec (`0x44 writebits`): "Appends low n bits of an int."

> Strings are byte-aligned per spec.md, so writing a non-multiple-of-8 number of bits has no defined behavior. Three plausible interpretations: error, round up with zero pad, pack into the trailing byte.
>
> Implementation today: requires `n % 8 == 0` and `n ≤ 256`; otherwise errors `BitCountOutOfRange`.
>
> Stakes: spec compliance and script expressiveness.
>
> _Architect response:_ require alignment: n % 8 == 0
>
> **Applied** — `op_write_bits` re-tightened to require `n % 8 == 0`; spec.md row 0x44 updated to say so; test `write_bits_partial_byte_high_bits_zeroed` replaced by `write_bits_non_aligned_hard_fails` asserting `BitCountOutOfRange`. Roundtrip tests (`write_then_read_bits_roundtrip_nonneg`) now skip non-aligned `n` cases. Asymmetric with `readbits` (which still accepts sub-byte `n` with high-bit masking) — this matches the architect's response which targeted writebits specifically.

### 4.2 `writebits` ignores sign ✅ **resolved**

Spec: "Appends low n bits of an int."

> Implementation today: writes the low `n` bits of `abs(x)` — the sign bit is discarded. The magnitude is the 32-byte LE form returned by `Int253::abs().to_bytes()`.
>
> Stakes: scripts that round-trip integers via `writebits`/`readint` would lose sign information unless they encode it separately. Alternative: write a sign-magnitude bit pattern matching `pushint`'s full form.
>
> _Architect response:_ keep it simple - interpret bits of the int as just bits. If the sign is included into the requested window - it gets written.
>
> **Applied** — `op_write_bits` now uses `x.to_bytes()` (raw 32-byte sign-magnitude LE form) instead of `x.abs().to_bytes()`. The sign bit lives at bit 7 of byte 31 and is included iff `n = 256`. Doc-comment on `op_write_bits` updated.

### 4.3 `readuint`/`readint` for values ≥ ℓ ✅ **resolved**

Both opcodes' soft-failure path is "string too short". The spec is silent on what happens if the 32-byte value isn't a canonical Ristretto scalar.

> Implementation today: reading 32 bytes that don't form a canonical scalar (value ≥ ℓ, or negative-zero pattern for `readint`) produces hard `InvalidInt253Encoding`. Soft-fail is reserved for the "string too short" case.
>
> Stakes: a careless script that reads from attacker-controlled bytes can hit a hard error mid-execution. Alternative: also soft-fail (push original + 0) on canonical violations.
>
> _Architect response:_ int must be valid and canonical. if we read from arbitrary source, we could use mod252 operation to mod-reduce any bit-pattern as a LE integer. Also, what's the difference between readuint and readint really? Maybe we need just one of those.
>
> **Applied** — the opcodes have been unified into `0x40 readbits` (replacing `readuint`) and `0x41 readint = readbits(s, 256)`. Both soft-fail on canonical violations (magnitude ≥ ℓ, negative zero, insufficient bytes) — the architect's "must be valid and canonical" requirement is encoded as: invalid bytes → soft-fail with the original string preserved, so the script can branch on the `0` flag and fall back to `mod252` for arbitrary-source reduction. Tests `read_bits_magnitude_at_ell_soft_fails`, `read_bits_magnitude_above_ell_soft_fails`, `read_bits_negative_zero_soft_fails` confirm.

### 4.4 `shiftleft`/`shiftright` for `n > 256` ✅ **resolved**

Spec: "Shifts bits left by n≤256 bits." Says nothing about what happens for `n` exceeding 256.

> Implementation today: errors `IndexOutOfRange` for `n > 256`.
>
> Stakes: consistency check; some chains allow arbitrarily large shifts (return all-zero result), some error.
>
> _Architect response:_ yes, explicit error for all out-of-range situations in all instructions.
>
> **Applied** — no code change. Implementation already errors with `IndexOutOfRange` for `n > 256`. Test `shift_too_large_errors` confirms.

---

## Phase 5 — Dict ops

### 5.1 `replace` operand order ✅ **resolved**

Spec (`0x62 replace`): `dict v k → dict' {prev 1 | 0}`.

Stack diagram has `k` on top, `v` below. This differs from `put`'s `dict k v` (v on top). Possibly a spec typo — both ops conceptually need (dict, k, v) and consistency would have them in the same order.

> Implementation today: follows the spec literally. `replace` pops `k` first, then `v`, then `dict`.
>
> Stakes: trivial implementation flip if the spec is corrected; significant footgun if scripts use both opcodes.
>
> _Architect response:_ let's flip to "k v" for consistency.
>
> **Applied** — `op_replace` now pops `v` first (top), then `k`. Spec.md row 0x62 updated to `dict k v → dict' {prev 1 | 0}`. Tests `replace_existing_returns_prev` + `replace_absent_returns_zero` retargeted (push k before v).

### 5.2 `dict` opcode duplicate-key handling ✅ **resolved**

Spec (`0x60 dict`): "Creates a new dict with 2*n items as key-value pairs."

> No statement on duplicates.
>
> Implementation today: errors `DictKeyOccupied` on duplicates.
>
> Stakes: alternative is "last wins" (BTreeMap default), which is permissive but allows non-canonical input.
>
> _Architect response:_ duplicates are forbidden.
>
> **Applied** — no code change. Implementation already errors `DictKeyOccupied`. Test `dict_construction_duplicate_keys_errors` confirms.

### 5.3 Portability flag for `Token` ✅ **resolved**

`Token` (encrypted, in-range) is per spec "portable: yes". The struct is still empty (Phase 13 adds fields).

> Implementation today: `Value::is_portable()` returns `true` for `Token` unconditionally. WideToken: `false`. ClearToken: `true` iff `qty ≥ 0`. These are correctness-preserving placeholders until the structs gain real fields.
>
> Stakes: once Token has Point commitments, `is_portable` should likely still be true (range-proof attests non-negativity). But the *check* may want to be: "Token is portable iff its range proof is valid", which only the constraint system knows. Currently flagging as a Phase-13 confirmation point.
>
> _Architect response:_ Token is always portable. WideToken is never portable. ClearToken is conditionally portable based on qty. Rangeproof is added behind the scenes by `mix` gadget (cloak protocol) when it creates Token instances on stack.
>
> **Applied** — no code change. Current `Value::is_portable` already matches: Token=true, WideToken=false, ClearToken=qty≥0. Phase 13 will wire the cloak/mix gadget that produces Token instances; that flow inherits this policy.

---

## Phase 6 — Hash & Merlin

### 6.1 Merlin constructor label channel ✅ **resolved** (feedback filed)

`merlin::Transcript::new(label: &'static [u8])` requires a 'static label, but user-supplied labels are runtime-only.

> Implementation today: every `Merlin` opens with a fixed domain separator `flamevm::merlin` passed to `Transcript::new`; the user label is appended immediately as the first message under a fixed tag `b"label"`. Net effect: two transcripts with different user labels diverge from byte 0. Protocol security preserved.
>
> Stakes: the literal byte stream of the transcript is not the same as a hypothetical reference implementation that managed to feed the user label directly into `Transcript::new`. Consensus-critical if `merlin*` opcodes are ever observable on-chain.
>
> _Architect response:_ we need to feed labels as-is from the smart contracts to implement the same protocols you'd have in the offchain code. Merlin's API is limiting due to use of static - add suggestion to improve the API. We might need to fork Merlin with extension API.
>
> **Applied** — feedback note filed at `feedback/2026-05-22-vm-engineer-on-merlin-api.md` proposing three resolution options (document wrapper as canonical / vendor `merlin-flame` shim / fork merlin). Code unchanged pending architect's choice. Phase 14 (sigverify/signtx) is the deadline — those bind to transcripts and need the canonical bytes locked.

### 6.2 SHA-3 variant ✅ **resolved**

Spec (`0x6e sha3`): "Returns a 256-bit string with sha3-256 digest of an input."

> Implementation today: SHA3-256 per FIPS-202 (padding `0x06`). Not Keccak-256 (Ethereum's variant, padding `0x01`).
>
> Stakes: a one-byte padding difference produces entirely different digests. Wrong choice breaks consensus.
>
> _Architect response:_ add `keccak256` opcode for ethereum compatibility, keep sha3 fips-202 compliant.
>
> **Applied** — new opcode `0x4e keccak256` added (slot was empty in the string-ops range; thematic placement aside, it's the only consecutive free slot). Implementation in `op_keccak256` uses `sha3::Keccak256`. Spec.md updated. Tests: `keccak256_empty`, `keccak256_abc` against Ethereum reference vectors, and `keccak256_differs_from_sha3` confirms the FIPS-202 vs Keccak distinction at the byte level.

---

## Suggested processing order

Items most likely to surface in early integration tests (consensus-critical hashing, encoding, and dispatch semantics):

1. **6.2** SHA-3 variant — single trivial yes/no, blocks any hash-using consumer.
2. **1.2** `pushint{16,64,128}` endianness — fixed by a sentence in spec.md.
3. **5.1** `replace` operand order — fix spec or fix code, one-line change.
4. **3.1** `divmod` rounding — choose a convention.
5. **1.1** `pushtoken` variant — affects token-touching code in later phases.
6. **2.1** Type codes for stack-only types — needs canonical assignment.
7. **3.3** `eq` for linear types — semantics decision.
8. **4.1** `writebits` non-aligned `n` — semantics decision.
9. **4.2** `writebits` sign treatment — semantics decision.
10. **4.3** `read{uint,int}` non-canonical values — error vs soft-fail.
11. **6.1** Merlin label channel — investigate whether direct injection is possible/desirable.
12. **2.2** End-of-script semantics — strictness choice.
13. **3.2** `divmod` for large magnitudes — implement bigint or document the limit.
14. **3.4** `mod252` inclusive vs exclusive 64.
15. **4.4** `shiftleft/right` for `n > 256`.
16. **5.2** `dict` duplicate-key handling.
17. **5.3** Token portability flag (revisit in Phase 13).

---

## Phase 9 — Cells, opens, signatures

### 9.1 `signtx` deferred-sig message — what does it commit to? ✅ **resolved**

Spec (`0x98 signtx`): "Defers external transaction signature verification."

> Implementation today: the deferred sig's `message` is a Merlin transcript over the cell's `id()` only:
> ```
> transcript = Transcript::new(b"flamevm.signtx")
> transcript.append(b"cell_id", cell.id())
> message = transcript.challenge_bytes(b"msg", 32)
> ```
> No binding to the transaction (no TxID, no input/output set, no fee, nothing).
>
> Stakes: a `signtx` signature is portable across **any** transaction that consumes a cell with the same id. The intent ("Defers *external transaction* signature verification") strongly implies the sig should bind to the TxID, but TxID isn't known until finalize.
>
> Three candidates:
> - **a)** Message = TxID. Bitcoin-`SIGHASH_ALL` style. Signer authorizes the whole tx; cell identity falls out of the input set being part of TxID.
> - **b)** Message = TxID || cell_id. Belt-and-suspenders.
> - **c)** Merlin transcript over (TxID, cell-id, optional tags). Most flexible; can extend.
>
> Decision is blocking Phase 14 (sigverify + delegate finalize): finalize must construct the same message the signer used.
>
> _Architect response:_ see zkvm implementation: signtx defers verification entirely and opens up payload immediately. After external tx is finalized, txid is known and VM forms a complete message over multi-key musig protocol with the txid-bound message. 
>
> **Applied** — `DeferredSig` refactored from a single struct into a tagged enum with `TxBound { verification_key }` and `Explicit { verification_key, message, signature }` variants. `signtx` pushes a `TxBound` record — no message is built at op-time; finalize will construct the TxID-bound message and aggregate via MuSig. `signrun` pushes `Explicit`. Test `signtx_pours_payload_and_records_txbound_sig` confirms.

### 9.2 `signrun` deferred-sig message — same question ✅ **resolved**

Spec (`0x99 signrun`): "Executes a signed program on behalf of the actor. The program may check the actor ID or anchor, or any other value, to appropriately bind the signature."

> Implementation today:
> ```
> transcript = Transcript::new(b"flamevm.signrun")
> transcript.append(b"cell_id", cell.id())
> transcript.append(b"program", program)
> message = transcript.challenge_bytes(b"msg", 32)
> ```
>
> Binds to cell-id + program. Does **not** bind to the args being passed in, nor to the TxID.
>
> Replay risk: a `signrun` signature is portable to any context that opens the same cell with the same program, regardless of args / actor / tx. The spec's "program may check the actor ID or anchor" puts the burden on the program author to add binding — which is real defense-in-depth but a footgun if forgotten.
>
> Candidates:
> - **a)** Current (cell-id + program). Program author binds rest.
> - **b)** + args. Most binding, but args may include non-portable values that complicate hashing.
> - **c)** + TxID. Like 9.1 but for run.
>
> _Architect response:_ signrun can bind only to the program, then the program itself can be bound to the anchor or other data by exlicitly checking for it in its code. E.g. "anchor 123 eq verify" part of the program fails if the anchor does not match the hardcoded one and effectively binds the program and the signature to that exact value "123".
>
> **Applied** — `signrun_message(program)` now transcribes only the program bytes (cell-id removed from the binding). `op_signrun` builds the `DeferredSig::Explicit { message, ... }` from this. Test `signrun_message_binds_only_to_program_not_to_cell` confirms by running signrun against two different predicates and verifying message bytes match. Spec.md row 0x99 + Discussion section document the program-binds-context-explicitly policy.

### 9.3 `Cell::id` — should it bind payload bytes? ✅ **resolved**

> Implementation today: `Cell::id` is a Merlin transcript that absorbs predicate-point, anchor, payload length, and per-payload-item **type code only** — *not* the payload's canonical bytes:
> ```
> for v in &self.payload {
>     t.append_message(b"payload.tag", &[v.type_code()]);
> }
> ```
>
> Two cells with the same predicate, the same anchor, and the same payload type-shape but different values produce **the same `id()`**.
>
> The anchor ratchet between successive outputs makes this not bite in single-tx tests (anchors differ → ids differ). It bites at any boundary where two cells happen to share an anchor (impossible inside one tx, possible with inputs in Phase 10 or with malicious cell construction).
>
> Stakes: `Cell::id` is the deferred-sig message anchor (9.1, 9.2), the next-anchor seed (`Cell::to_anchor`), and the txlog Output commitment. All consensus-critical.
>
> Mitigation: bind payload via canonical wire-encoding of each value. Blocked on full Value encoding for Token/ClearToken/WideToken (the plain-data types already encode canonically).
>
> _Architect response:_ do like in zkvm. Entire payload must be hashed via current encoding API. Anchor provides uniqueness, there must be no two cells with two identical anchors - that's enforced partly in VM, partly via Utreexo logic and actor queue.
>
> **Applied** — `Cell::id` rewritten: each payload value is encoded via `encoding::write_value` into a buffer and the bytes are absorbed into the Merlin transcript under tag `b"payload.item"`. Test `cell_id_changes_when_payload_value_changes` confirms two cells with same predicate + same anchor + different `Int253` values now have distinct ids. Phase-9 limitation: only payload values with canonical encoders (Int253, String, Dict, Point today) are supported — Token/ClearToken/WideToken/Merlin/etc. will panic in `Cell::id` until Phase 13 wires their encoders. Anchor-uniqueness invariant (no two cells with the same anchor) is the responsibility of the input/ratchet/Utreexo chain, not validated here.

### 9.4 `Cell::id` vs. wire-encoding hash — one or two identities? ✅ **resolved**

> Cells in Utreexo are stored as wire-encoded blobs. The natural "this cell's identity" is `H(wire-bytes)`. But `Cell::id` is currently a Merlin transcript over fields.
>
> Two functions for one concept ("identity of a cell") will drift apart. The choice:
> - **a)** `Cell::id` = `H(canonical-wire-encoding)`. Utreexo and protocol commitments agree.
> - **b)** `Cell::id` = Merlin transcript (current). Utreexo hashes the wire bytes separately. Two distinct ids.
>
> Architect's prior guidance: "all internal hashes via transcript". Suggests (a) with the transcript being the canonical bytes-hash. Concretely: `Cell::id` should be a transcript that absorbs the cell's wire bytes once we have the wire encoding.
>
> _Architect response:_ we do not hash "wire bytes". We use encoding API for two purposes: produce a blob of "wire bytes" for transmission and to recursively produce a hash via Merlin Transcript without preallocation of buffers.
>
> **Applied** — `Cell::id` is the single canonical identity, computed via a Merlin transcript that absorbs the encoded bytes of each payload value (currently via an intermediate `Vec<u8>` buffer; the "no preallocation" pattern — writing the encoder's output directly into the transcript via a Writer-impl wrapper — is a future optimization, tracked separately). No "wire-hash" alternative is introduced.

### 9.5 `CallProof` wire layout ✅ **resolved**

> Implementation today: a single `String` on the stack, decoded with a fixed-layout body:
> ```
> internal_key (32) || pos_len:u32_LE || position || n_count:u32_LE || neighbors × 32 || program
> ```
>
> Architect's prior guidance was to "keep call proof as strings: hashes of neighbours on stack, separate string for bit-pattern position, and other data - program, pubkey". That's four separate strings on the stack (or a list-style Dict containing them), not a packed bag of bytes.
>
> Stakes: spec compliance + integrator UX. The list-style Dict version reuses the existing Dict encoding/decoding machinery. The current packed version is a one-off encoder.
>
> _Architect response:_ lets use VM values verbatim and put these parameters as distinct values on stack. We'll avoid creating a new entity with its own encoding rules, and allow for easier runtime inspection/dynamic composition this way.
>
> **Applied** — `open`'s stack diagram changed from `cell args… k callproof → results…` to `cell internal_key neighbors position program args… k → results…`. `op_open` now pops four distinct values (Point internal_key, Dict neighbors, String position, String program) and reconstructs the `CallProof` struct in-VM via `callproof_from_stack_pieces`. The old packed-bytes `decode_callproof` helper is removed. Spec.md row 0x93 updated; tests retargeted with a `push_callproof_pieces` test helper. New test `open_passes_args_after_payload` exercises non-zero args.

### 9.6 `return` inside an opened cell program ✅ **resolved**

> With Run-level (not Call-level) cell-open, `return k` inside an opened cell's program pops the **outer call frame**, not the cell-run. A cell author can write a program that calls `return 0` and silently exits the outer script at the open-point.
>
> Two design choices:
> - **a)** Status quo — `return k` exits the enclosing call. Cell author has the same power as if their program were inlined. Composition footgun.
> - **b)** Cell-run swallows `return` — treat as `break:0` of the cell-run. The outer script always continues after `open` ran the program to completion.
>
> _Architect response:_ option (b) would be a weird special-case. Since we treat opens as runs, the program has full access to the surrounding context, that's fine.
>
> **Applied** — no code change. Current Run-level semantics already match: `return k` from inside the opened program exits the enclosing call frame; `break:k` past the run-stack errors `BreakOutOfCall`. Spec.md row 0x93 description updated to spell this out explicitly.

### 9.7 Position bit ordering in `CallProof` ✅ **resolved**

> Implementation today: `get_bit(bits, i)` reads bit `i` as `(bits[i/8] >> (i%8)) & 1` — LSB-first within byte, zero-extended past the end. The bit value `0` means "neighbor is on the right of running hash", `1` means "on the left".
>
> Consensus-critical: any verifier walking the same merkle path must use the same bit convention. Currently undocumented in `spec.md`.
>
> Confirm convention + add to spec.md row 0x93 description.
>
> _Architect response:_ document explicitly, use ZkVM call proof implementation as a reference.
>
> **Applied** — bit-ordering convention added to spec.md row 0x93: "Position bits are read LSB-first within byte, zero-extended past the end; bit value `0` = current hash on left / neighbor on right, `1` = swap." Confirmed against zkvm's `merkle::Directions` iterator which reads `(position & 1)` LSB-first and `Side::from_bit(0) = Left`. Matches.

### 9.8 Trust model in cell-open — explicit spec note? ✅ **resolved**

> Run-level cell-open means the cell's program inherits the caller's full authority: same stack, same gas, same actor identity (when called from inside an actor), same memory budget, can emit outputs/sends, etc.
>
> This is by design under "you accepted the predicate" — but the spec doesn't say it. A contract author opening a cell from an untrusted source (which they shouldn't but might) faces unbounded blast radius.
>
> Add explicit text to `spec.md` near `open` / `signrun` describing the trust model.
>
> _Architect response:_ the creator of external transaction is the same person owning the cell, so opening as a run is totally fine. This poses problems if cells are opened in internal context, but we can review that later.
>
> **Applied** — trust-model paragraph added to spec.md Discussion section: explains that the tx creator and the cell-opener are the same party (so granting full local authority to the cell's program is the same as inlining trusted code), and flags the internal-context cell-open case for future review.

### 9.9 `signrun` linearity — leaked values in caller scope ✅ **resolved**

> When `open` or `signrun` finishes, the outer script has whatever the cell program left on the stack. If the cell program creates a linear value (a `merlin`, a `Token`, etc.) and leaves it on the stack, the outer script must consume it. The caller may not know what to do with it — `StackNotClean` at frame exit.
>
> Status quo: caller's responsibility to consume whatever the cell program produces. Cell authors should leave a documented return shape.
>
> Alternative: gate the cell program's stack outputs to a declared portable shape only.
>
> _Architect response:_ caller's responsibility to consume whatever the cell program produces.
>
> **Applied** — no code change; status quo confirmed. Documented in spec.md (the trust-model paragraph notes that the cell author's program output shape is part of the contract a caller chose to accept).

### 9.10 Multi-leaf `PredicateTree` ✅ **resolved**

> `PredicateTree { internal_key, programs }` permits any number of programs at the type level. `merkle_root` and `callproof_for` `assert_eq!(programs.len(), 1)` and panic otherwise. The single-leaf restriction blocks any real predicate use (multiple unlock paths is the point of Taproot).
>
> Either:
> - **a)** Ship general balanced merklization now (small extension; ~30 LOC + tests).
> - **b)** Keep single-leaf for Phase 9, ship multi-leaf in a follow-up.
>
> Recommend (a) since "Taproot with one leaf" is essentially Schnorr-tweaked-key with no path-options — defeats the purpose.
>
> _Architect response:_ please do multi-leaf - see zkvm for reference and do like it's done there.
>
> **Applied** — `PredicateTree::merkle_root` now does general balanced merklization via `merkle_root_of_programs` (split at `next_power_of_two(n) / 2`, recurse, hash via existing `merkle_node_hash`/`merkle_leaf_hash` transcripts). `PredicateTree::callproof_for(index)` returns `Result<CallProof, VMError>`, building neighbors + position bits during a root-to-leaf descent and reversing to produce the leaf-to-root order `merkle_walk_up` expects. Single-leaf `assert!`s removed.
>
> Additional cleanup landed in the same pass (matched audit items 1.2-1.5 from the Phase-9 audit):
>
> - `PredicateTree::new(internal_key, programs)` introduced as the only validated constructor: errors `EmptyPredicateTree` on no programs, `InvalidPoint` on non-decompressable key. Fields demoted to `pub(crate)`; `internal_key()` / `programs()` are read-only accessors.
> - `PredicateTree::compute_point` no longer needs `.expect()` (key validated at construction).
> - `merkle_walk_up` now meaningfully returns `Result`: errors `MalformedCallProof` when position bits don't cover all neighbors.
> - New `VMError` variants: `EmptyPredicateTree`, `InvalidPoint`, `ProgramIndexOutOfRange`.
>
> Tests landed (4 new):
> - `predicate_tree_new_validates_inputs` — both error paths.
> - `multi_leaf_predicate_each_program_unlocks_via_its_path` — 3-leaf tree, each leaf opens correctly via its CallProof and cell-program runs to clean stack.
> - `multi_leaf_predicate_wrong_leaf_path_hard_fails` — forged callproof with mismatched program → `CallProofMismatch`.
> - `callproof_for_out_of_range_index_errors` — `ProgramIndexOutOfRange`.
>
> Test helper `push_callproof_pieces` generalized to build a non-empty neighbors `Dict` via the `dict` opcode with `(val_i, key_i, ..., n)` push pattern.

---

## Suggested processing order (Phase 9 items)

Blocking for downstream phases:
- **9.1 + 9.2** — sig messages. Phase 14 (sigverify + delegate finalize) needs these locked.
- **9.3 + 9.4** — `Cell::id` semantics. Phase 10 (inputs) needs the input → Utreexo round-trip pinned.

Spec-only fixes (no design tension):
- **9.5** — CallProof wire layout. Re-encode as Dict-of-strings or keep packed.
- **9.7** — position bit-ordering in spec.md.
- **9.8** — trust-model note in spec.md.

Design calls with implementation impact:
- **9.6** — `return` inside cell-open.
- **9.9** — linearity-leak through cell-open.
- **9.10** — multi-leaf predicate trees.

---

## Cross-cutting — Gas calibration

Open questions surfaced while drafting the opcode-pricing strategy. All four block construction of the first calibration harness; nothing downstream of "first measurable gas value" can land until they're settled.

> **Proposed resolution:** ADR 0009 — "Gas calibration strategy" (status: proposed). Picks one answer per G.1–G.4 plus the cross-cutting methodology (criterion + iai-callgrind, marginal-difference programs, two-metric take-worse rule, single lane for v0, per-PR iai gate, per-release criterion recalibration on a containerized AWS `c7i.large` reference). Architect-response slots below remain empty until the ADR is accepted (or modified) by the human Architect.

### G.1 Reference machine for calibration

> Calibration produces wall-clock numbers; those numbers are meaningless without a fixed reference machine. Every gas value is relative to that one box, and the choice locks in the absolute throughput of the network.
>
> Implementation today: none — no calibration harness exists yet.
>
> Stakes: too powerful a reference and weak nodes can't keep up; too weak and the chain is throttled below what production nodes can deliver. Cloud reference is reproducible but binds us to a vendor's hardware roadmap; bare-metal is durable but harder to share.
>
> Candidates:
> - **a)** Pinned AWS instance (e.g. `c7i.large`) with documented kernel/governor settings. Reproducible by anyone with an AWS account.
> - **b)** Specific bare-metal box (CPU model + freq + kernel + governor). Most durable; hardest to reproduce.
> - **c)** "Synthetic reference" — declare a fixed wall-clock budget per block; each node calibrates locally and prices its own gas, with a consensus floor.
>
> _Architect response:_ 

### G.2 Gas unit anchor

> What does "1 gas" mean in absolute terms on the reference machine?
>
> Implementation today: none.
>
> Stakes: too coarse (1 gas = 1 μs) and cheap opcodes like `dup`/`pop` can't be priced below 1 gas, so fee structure skews toward overcharging trivial ops. Too fine (1 gas = 1 ns) and numbers grow unwieldy, risking overflow at block-budget scale and noisy regression signals at the per-opcode scale.
>
> Default proposal: **1 gas = 100 ns** on the reference machine. Puts `dup`/`pop` at ~1–2 gas, typical arithmetic at 1–20 gas, heavy ZK ops at 10k–100k gas, with comfortable headroom against `u64` block budgets.
>
> _Architect response:_ _(empty until filled in)_

### G.3 One lane (compute-gas) or two (compute + bytes-touched)?

> Cell load and string-touching opcodes are I/O-dominant, not compute-dominant. Pricing them on the same axis as `add` either over- or under-charges, depending on which extreme dominates calibration.
>
> Implementation today: none.
>
> Stakes: single lane is simpler and matches Ethereum's gas model — every cost folds into one number, one block budget. Two lanes (compute-gas + bytes-touched) gives a more honest model but adds protocol surface (two block budgets, two fee axes, more places for adversarial skew).
>
> The 4× arena cap already bounds *transient memory size*; this question is about pricing the *cost of touching* memory and cells, not whether the allocation fits.
>
> _Architect response:_ _(empty until filled in)_

### G.4 Block compute budget

> Calibration is only meaningful relative to a per-block compute budget. The budget sets target throughput and bounds worst-case validation time on the reference machine.
>
> Implementation today: none.
>
> Stakes: too high and slow nodes fall behind / get partitioned; too low and the chain underuses available hardware. Needs to leave room for networking, signature aggregation, Utreexo updates, and slack for cache-cold paths.
>
> Default proposal: target **~200 ms of reference-CPU time per block** for opcode execution, with the remaining block interval reserved for everything else (gossip, finalization, persistence, slow-node margin).
>
> _Architect response:_ _(empty until filled in)_

---

## Suggested processing order (Gas calibration items)

All four are blocking; the natural order is:

1. **G.1** Reference machine — nothing else is measurable without it.
2. **G.4** Block budget — the second anchor; defines what "expensive" means.
3. **G.2** Gas unit — derives from (G.1, G.4) and per-opcode measurements.
4. **G.3** Lanes — can defer briefly by pricing everything single-lane and revisiting once cell-load numbers are in hand.
