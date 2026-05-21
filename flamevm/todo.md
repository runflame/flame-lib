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

### 1.2 `pushint{16,64,128}` byte order

Spec only states "big-endian" explicitly for `pushint16` (`0x12/0x13`). The other widths inherit no explicit endian.

> Implementation today: BE for all of `pushint16/64/128` (consistency).
>
> Stakes: changes the encoded literal layout; consensus-critical for scripts that embed multi-byte ints.
>
> _Architect response:_

---

## Phase 2 — Control flow

### 2.1 Type codes for stack-only types

Spec table maps wire-encodable types to a tag base (Int253 → 0, String → 68, Dict → 128, Point → 248, …, Merlin → 253). It does not assign codes for stack-only types (`Variable`, `Expression`, `Constraint`, `MultiscalarMul`).

> Implementation today: `Value::type_code()` returns `0xc0` for Variable, `0xc1` for Expression, `0xc2` for Constraint. These are arbitrary, chosen to avoid wire-tag collisions (254 reserved, 255 extension).
>
> Stakes: any script using the `type` opcode (`0x7f`) on a stack-only value sees these codes. They become observable behavior and need a canonical assignment if scripts are to be portable across implementations.
>
> _Architect response:_

### 2.2 End-of-script without explicit `return`

Spec describes `return` and `break:0` but does not state whether a script *must* end with one or whether falling off the end is also a clean exit.

> Implementation today: falling off the end of the entry Run of a call is equivalent to break:0 (the call exits cleanly iff the stack is empty). No mandate to use `return 0`.
>
> Stakes: stricter interpretation would catch a missing return statement at validation time; looser is forgiving but allows silent script truncation.
>
> _Architect response:_

---

## Phase 3 — Int253 arithmetic, logic, size

### 3.1 `divmod` rounding convention

Spec (`0x55 divmod`): "Computes the quotient and the remainder for int." No definition of rounding behavior for negative operands.

> Implementation today: truncated division (toward zero). `sign(q) = sign(x) XOR sign(z)`, `sign(r) = sign(x)`. Matches Rust's `i64` convention. Example: `-13 divmod 5` → `q = -2, r = -3`.
>
> Stakes: divmod is the only signed-division op; rounding convention is consensus-critical for scripts that do signed arithmetic.
>
> _Architect response:_

### 3.2 `divmod` for magnitudes exceeding `u64::MAX`

> Implementation today: returns `MagnitudeTooLarge` error for any input whose magnitude doesn't fit `u64`. Rationale: bigint division isn't trivial; deferred until demand.
>
> Stakes: the spec doesn't restrict magnitudes, so this is a "spec hole" the implementation chose to fill conservatively. Any script using divmod on > 64-bit operands fails.
>
> _Architect response:_

### 3.3 `eq` semantics for linear types

Spec (`0x51 eq`): "Checks equality of two types." Silent on what equality means for tokens, transcripts, variables, expressions.

> Implementation today: same-variant comparisons for `Int253`, `String`, `Point`, `Dict` use value/byte equality. Cross-variant returns `0` (false). Same-variant linear types (`Token`, `ClearToken`, `WideToken`, `Object`, `Merlin`, `Variable`, `Expression`, `Constraint`) hard-error with `TypeNotComparable`.
>
> Stakes: scripts that try to compare linear values get a hard fail rather than a meaningful boolean. Alternatives: always return 0, or define identity equality (pointer-style — but linear types have no stable identity).
>
> _Architect response:_

### 3.4 `mod252` upper-bound semantics

Spec (`0x56 mod252`): "Interprets 0..64-byte as a little-endian unsigned integer and mod-reduces to R255 group order."

> "0..64-byte" is ambiguous between half-open `[0, 64)` and closed `[0, 64]`.
>
> Implementation today: inclusive — strings of length 0 through 64 accepted, 65+ rejected (`StringTooLongForModReduction`).
>
> Stakes: edge-case behavior for 64-byte inputs.
>
> _Architect response:_

---

## Phase 4 — String ops

### 4.1 `writebits` for non-byte-aligned `n`

Spec (`0x44 writebits`): "Appends low n bits of an int."

> Strings are byte-aligned per spec.md, so writing a non-multiple-of-8 number of bits has no defined behavior. Three plausible interpretations: error, round up with zero pad, pack into the trailing byte.
>
> Implementation today: requires `n % 8 == 0` and `n ≤ 256`; otherwise errors `BitCountOutOfRange`.
>
> Stakes: spec compliance and script expressiveness.
>
> _Architect response:_

### 4.2 `writebits` ignores sign

Spec: "Appends low n bits of an int."

> Implementation today: writes the low `n` bits of `abs(x)` — the sign bit is discarded. The magnitude is the 32-byte LE form returned by `Int253::abs().to_bytes()`.
>
> Stakes: scripts that round-trip integers via `writebits`/`readint` would lose sign information unless they encode it separately. Alternative: write a sign-magnitude bit pattern matching `pushint`'s full form.
>
> _Architect response:_

### 4.3 `readuint`/`readint` for values ≥ ℓ

Both opcodes' soft-failure path is "string too short". The spec is silent on what happens if the 32-byte value isn't a canonical Ristretto scalar.

> Implementation today: reading 32 bytes that don't form a canonical scalar (value ≥ ℓ, or negative-zero pattern for `readint`) produces hard `InvalidInt253Encoding`. Soft-fail is reserved for the "string too short" case.
>
> Stakes: a careless script that reads from attacker-controlled bytes can hit a hard error mid-execution. Alternative: also soft-fail (push original + 0) on canonical violations.
>
> _Architect response:_

### 4.4 `shiftleft`/`shiftright` for `n > 256`

Spec: "Shifts bits left by n≤256 bits." Says nothing about what happens for `n` exceeding 256.

> Implementation today: errors `IndexOutOfRange` for `n > 256`.
>
> Stakes: consistency check; some chains allow arbitrarily large shifts (return all-zero result), some error.
>
> _Architect response:_

---

## Phase 5 — Dict ops

### 5.1 `replace` operand order

Spec (`0x62 replace`): `dict v k → dict' {prev 1 | 0}`.

Stack diagram has `k` on top, `v` below. This differs from `put`'s `dict k v` (v on top). Possibly a spec typo — both ops conceptually need (dict, k, v) and consistency would have them in the same order.

> Implementation today: follows the spec literally. `replace` pops `k` first, then `v`, then `dict`.
>
> Stakes: trivial implementation flip if the spec is corrected; significant footgun if scripts use both opcodes.
>
> _Architect response:_

### 5.2 `dict` opcode duplicate-key handling

Spec (`0x60 dict`): "Creates a new dict with 2*n items as key-value pairs."

> No statement on duplicates.
>
> Implementation today: errors `DictKeyOccupied` on duplicates.
>
> Stakes: alternative is "last wins" (BTreeMap default), which is permissive but allows non-canonical input.
>
> _Architect response:_

### 5.3 Portability flag for `Token`

`Token` (encrypted, in-range) is per spec "portable: yes". The struct is still empty (Phase 13 adds fields).

> Implementation today: `Value::is_portable()` returns `true` for `Token` unconditionally. WideToken: `false`. ClearToken: `true` iff `qty ≥ 0`. These are correctness-preserving placeholders until the structs gain real fields.
>
> Stakes: once Token has Point commitments, `is_portable` should likely still be true (range-proof attests non-negativity). But the *check* may want to be: "Token is portable iff its range proof is valid", which only the constraint system knows. Currently flagging as a Phase-13 confirmation point.
>
> _Architect response:_

---

## Phase 6 — Hash & Merlin

### 6.1 Merlin constructor label channel

`merlin::Transcript::new(label: &'static [u8])` requires a 'static label, but user-supplied labels are runtime-only.

> Implementation today: every `Merlin` opens with a fixed domain separator `flamevm::merlin.v1` passed to `Transcript::new`; the user label is appended immediately as the first message under a fixed tag `b"label"`. Net effect: two transcripts with different user labels diverge from byte 0. Protocol security preserved.
>
> Stakes: the literal byte stream of the transcript is not the same as a hypothetical reference implementation that managed to feed the user label directly into `Transcript::new`. Consensus-critical if `merlin*` opcodes are ever observable on-chain.
>
> _Architect response:_

### 6.2 SHA-3 variant

Spec (`0x6e sha3`): "Returns a 256-bit string with sha3-256 digest of an input."

> Implementation today: SHA3-256 per FIPS-202 (padding `0x06`). Not Keccak-256 (Ethereum's variant, padding `0x01`).
>
> Stakes: a one-byte padding difference produces entirely different digests. Wrong choice breaks consensus.
>
> _Architect response:_

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
