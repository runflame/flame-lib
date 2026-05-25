# Audit 2026-05-22 — Initial surface sweep of FlameVM

Scope:
- `flamevm/spec.md` (full)
- `flamevm/src/` (all files): `lib.rs`, `int253.rs`, `encoding.rs`, `dict.rs`,
  `string.rs`, `value.rs`, `token.rs`, `object.rs`, `crypto.rs`,
  `constraints.rs`, `errors.rs`, `tx.rs`, `vm.rs`
- `design.md` (root) — Data types, Cells, Actors, Authorization,
  Confidentiality, Resources
- ADRs 0001–0005 (re-named from prior taxonomy; per architect feedback
  2026-05-22)
- `feedback/2026-05-22-architect-on-vm-auditor.md`

Methodology:
- First-pass code review of every file in `flamevm/src/`, focusing on
  encoder/decoder canonicality, opcode dispatch, linear-type discipline,
  resource accounting hooks, and call/run scope semantics.
- Spec vs implementation cross-check for every opcode currently
  implemented (Phases 1–4 per `flamevm/CLAUDE.md`).
- Walk of `threats/vm.md` against the actual code.
- No prior audit reports exist; entire current surface is in scope.

## Summary

The current implementation covers value types (Int253, String, Dict,
Point; Token/ClearToken/WideToken/Object stubs), the canonical wire
encoding for Int253 / String / Dict / Point, and Phases 1–4 opcodes
(stack literals, control flow, Int253 arithmetic, string ops). Many
critical surfaces are unimplemented (call/send/load/save, constraint
system opcodes, gas accounting, memory accounting, sigverify, dict
opcodes, token opcodes). The threat model entries for those areas are
correctly marked Open; this audit focuses on what is implemented.

Findings:
- 2 High
- 3 Medium
- 4 Low
- 5 Informational

No Critical findings in the current code (the most dangerous surfaces —
linear-type opcodes, send/call/load/save, constraint system — are not
yet wired up).

---

## Findings

### Finding 1: Non-canonical U64 sub-varint silently accepted (encoding & VM) [High]

**Scenario.** A peer encodes a value `n < SUBVAR_U64_BASE` (i.e., any
value normally encoded with the U8 / U16 / U32 sub-varint widths) using
the U64 sub-varint tag (`subvarint.tag == 3`). The decoder reconstructs
the original value via `SUBVAR_U64_BASE + payload`, but the payload may
be chosen so that the addition wraps around modulo 2^64 and yields a
value already representable in a shorter width.

**Prerequisites.** A peer that controls a wire-encoded
String/List/Dict prefix (i.e., any external transaction or actor state
blob). The bug is in two places:

- `flamevm/src/encoding.rs` line 171 (`read_subvarint`): the addition is
  written as `SUBVAR_U64_BASE + r.read_u64()?`. In debug builds this
  panics on overflow; in release builds it wraps silently. Either way,
  values that fall in the U8 / U16 / U32 sub-ranges are not rejected.
- `flamevm/src/vm.rs` line 218 (`Run::read_sub_varint`, used by
  `pushstr`): the addition is `SUBVAR_U64_BASE.wrapping_add(...)` —
  explicit wrap. Non-canonical sub-varints in `pushstr` operands are
  silently accepted.

**Impact.**
- *Encoding canonicality break.* `threats/vm.md` §3.4 currently says
  "offset-based ranges leave no canonicality choice — Mitigated by
  construction." That is false for the U64 width: the upper end of the
  U64 range wraps into the U8 range.
- A given logical value can have two distinct wire encodings. Anything
  that takes the SHA-256 of the wire bytes (TxID, cell hashing, actor
  state hashing, predicate compression) will see distinct hashes for
  semantically equal values. Adversarial peer can mint duplicate inputs
  with different content-addressed identities.
- A panic in debug builds is a quality-of-test issue: every fuzz target
  that exercises sub-varints will trip the assertion on a small subset
  of inputs that are nonetheless validly typed inputs.

**Evidence.** Concrete payload exhibiting the wrap:
- Tag byte: `0x03` (subvarint tag = 3, U64 width).
- Payload (8 LE bytes): `2^64 - SUBVAR_U64_BASE = 2^64 - 4_295_033_088`.
- Result via release-mode `wrapping_add`: `0`. So a value of 0 (whose
  canonical encoding is `0x00 0x00` — tag U8 + byte 0) is also
  accepted via a 9-byte alternative encoding.

This can be added directly to a test using `read_subvarint` and to a
fuzz target for `read_value` to confirm the wrap.

**Mitigation.**
1. Use `checked_add` and return `ReadError::InvalidFormat` /
   `VMError::UnexpectedEndOfScript` on overflow.
2. Even when the addition doesn't overflow, reject when the
   reconstructed value is below `SUBVAR_U64_BASE` — that means it
   should have used the U8/U16/U32 width and the encoding is
   non-canonical. Same applies to the U16 and U32 branches: after the
   add, verify the result is at or above the width's base.
3. Add a `subvarint_rejects_noncanonical_u64` test plus a fuzz target
   that round-trips arbitrary u64s through encode/decode and rejects
   any alternative encoding.

**References.**
- `flamevm/src/encoding.rs`:138, :171
- `flamevm/src/vm.rs`:198, :218
- `threats/vm.md` §3.4

---

### Finding 2: Wrong-type pop silently destroys linear values (forward-looking) [High]

**Scenario.** `pop_string`, `pop_int253`, and `pop_byte_count` all
implement type-checking by first popping the top value off the stack
and then matching on its variant; the wrong-variant arm discards the
value. For a linear-type value (Token, ClearToken non-zero,
WideToken, Object, …), this is an implicit drop — no `retire` is
performed and no error specifically signals the linearity violation.

```rust
fn pop_string(&mut self) -> Result<String, VMError> {
    match self.pop_value()? {                 // pops first
        Value::String(s) => Ok(s),
        _ => Err(VMError::TypeNotString),     // wrong-type variant is dropped
    }
}
```

**Prerequisites.** A script that pushes a non-copyable linear value and
then invokes any opcode that pops with type-checking via these helpers.
Today, the VM treats any `VMError` as fatal for the whole tx and
external-tx atomicity says "no effects on failure" — so the leak is
contained to the failed tx. The risk lands the moment any opcode is
introduced that catches a sub-error and continues, or any sub-VM micro-
execution path (refund predicate evaluation, dict-poisoning checks, …)
is added.

**Impact (forward-looking).**
- If error recovery is ever added (refund predicates per spec §Sends,
  for example, are explicit "in a fresh micro-VM" candidates), this
  pattern silently launders linear-typed values. A token would be
  destroyed without a `retire` effect, breaking conservation of supply
  and breaking the no-implicit-drop invariant.
- Inconsistent with `op_drop`'s explicit "restore on failure" pattern
  at `vm.rs`:786–794 — that one carefully pushes the value back if it
  is non-droppable.

**Mitigation.**
1. Introduce a peek-then-pop helper:
   ```rust
   fn pop_if<T>(...) -> Result<T, VMError>
   ```
   that inspects the top variant before removing it; only removes on
   match. Use it across every typed-pop helper.
2. Or: in the wrong-variant arm, push the value back before returning
   the error (the pattern used in `op_drop`).
3. Add a test that runs `pushtoken X, op_run` and (after the fix)
   asserts the ClearToken is still on the stack after the error.

**Status.** This is High severity because it is a structural design
choice that becomes immediately exploitable as soon as any error path
becomes recoverable. The audit prefers fixing now while the surface is
small.

**References.**
- `flamevm/src/vm.rs`:786 (good: `op_drop` restores), :693, :699,
  :841, :1310 (bad: typed-pop helpers drop on mismatch)
- `threats/vm.md` §5.1, §5.2 (token duplication / drop)
- ADR 0001 (linear-type discipline)

---

### Finding 3: `op_loop` is an unconditional infinite-loop DoS until gas metering lands [Medium]

**Scenario.** Script `[0x7c]` (`loop`) resets PC to 0 and re-executes
itself; without `break`/`return`/gas the VM never terminates.

**Prerequisites.** Any path that runs adversarial bytecode under the
current implementation — internal-tx dispatch (via the test stub or any
future ActorRegistry) or external script. The unit test
`internal_unknown_opcode_errors` is not parameterized; nothing in the
test suite asserts that `[0x7c]` alone terminates.

**Impact.** A single one-byte adversarial script wedges the VM until
the host process is killed. Validators that dispatch transactions
without an external timeout / gas budget will hang.

**Mitigation.**
- Wire opcode gas accounting (acknowledged TODO per `flamevm/CLAUDE.md`
  "Memory cap enforcement (4× vbyte rule)" — same status for gas).
- Until then, a hard per-tx step limit (`MAX_OPCODES = 1_000_000` or
  similar) as a strictly defensive cap. Even after gas lands, keeping a
  step ceiling is cheap defense in depth.
- Test target: assert that `step_internal` on `[0x7c]` does not loop
  more than `MAX_OPCODES` times.

**References.**
- `flamevm/src/vm.rs`:1204–1207
- `threats/vm.md` §2.1, §2.4
- ADR 0002

---

### Finding 4: `op_write_zeros` / `op_run` allow unbounded transient allocation [Medium]

**Scenario.** `op_write_zeros` pops a count `n` via `pop_byte_count(usize::MAX)`
then allocates `vec![0u8; n]`. With no `mem_used`/`mem_limit` enforcement
yet, a single script can request an allocation of up to `usize::MAX`.
Similarly, `op_run` clones the current `String`'s bytes into a fresh
`Run` script; combined with `op_loop` plus string-construction opcodes
(`append`, `writezeros`), a script can blow up RAM.

**Prerequisites.** Any context that admits adversarial bytecode. Same
exposure surface as Finding 3.

**Impact.** Host-process OOM. On a validator that doesn't isolate the
VM, a single tx can crash the node.

**Mitigation.**
- Wire 4× memory cap (ADR 0002). Every allocation site checks
  `mem_used + n ≤ mem_limit` before allocating. Sites to gate (current
  scan):
  - `op_write_zeros` (vm.rs:983)
  - `op_run`, `op_switch` (vm.rs:1195, :1212) — Run script clone
  - `op_append` (vm.rs:975) — String concat
  - `op_read_str` (vm.rs:913) — `s.split_at(n)` produces a new owned
    String of `n` bytes
  - `pushstr` (vm.rs:748) — bounded by script length, so transitively
    safe if script size is bounded, but explicit metering is still
    desirable
- Until the cap lands, a defensive `MAX_ALLOC` ceiling per op.
- Auditor will add fuzz target
  `flamevm/fuzz/auditor/memory_amplification.rs` that exercises
  `loop`+`append`+`writezeros` and asserts a bound is hit.

**References.**
- `flamevm/src/vm.rs`:983 (`op_write_zeros`), :1195 (`op_run`),
  :1319 (`enter_run`)
- ADR 0002
- `threats/vm.md` §2.2, §2.5

---

### Finding 5: `op_pushtoken` semantics inconsistent with spec ambiguity [Medium]

**Scenario.** Spec `0x1b pushtoken ø → token` says "Pushes 0-qty token
of any flavor, reading next 32 bytes for flavor." The spec types
chapter says `ClearToken` flavor is an Int253 and `Token`/`WideToken`
flavor is a Point. The opcode does not pin the variant.

The current implementation creates a `ClearToken` whose flavor is
constructed by `Scalar::from_canonical_bytes`. Two consequences:

1. If a caller expects `pushtoken` to produce a `Token` (encrypted
   variant) — e.g., to feed into a `mix` / `decrypt` later — they get
   a `ClearToken` instead, and downstream type-checks fail
   unpredictably.
2. The `Scalar::from_canonical_bytes` check rejects 32-byte flavors
   whose value is ≥ ℓ. Per the spec ("any flavor"), no such check is
   warranted. For `ClearToken` whose flavor is an `Int253`, the check
   makes sense (Int253 invariant requires canonical magnitude). But it
   means certain pre-shared flavor identifiers (e.g., uniformly random
   32-byte tags, e.g., truncated SHA-256) are silently rejected ~1
   time in 2^120 — astronomically rare, but adversarially constructible.

**Prerequisites.** Application code that constructs a flavor via
non-scalar means (e.g., concatenated identifiers, multi-source hash)
and feeds it through `pushtoken`.

**Impact.** Defensible behavior, but spec ambiguity creates a divergent-
implementation hazard. Two implementations could plausibly disagree on
the variant produced (Token vs ClearToken) or on the canonicality
check for the flavor bytes.

**Mitigation.** File a clarification request to the Architect (via
this audit's feedback note to vm-engineer cc architect):
- Pin `pushtoken` to either ClearToken (with Int253 flavor) or Token
  (with Point flavor).
- If ClearToken: confirm the canonical-scalar check for the flavor
  bytes is intended (and update the spec text "any flavor" to "any
  canonical Int253 flavor").
- If Token: re-implement to construct a Token with the bytes as a
  CompressedRistretto flavor.

**References.**
- `flamevm/spec.md` lines 319, 404 (Token vs ClearToken flavor types)
- `flamevm/src/vm.rs`:774–783
- `threats/vm.md` §11.x (flavor binding / grinding)

---

### Finding 6: `op_read_uint` / `op_read_int` hard-fail on 32-byte non-canonical magnitudes [Low]

**Scenario.** Both opcodes have a spec stack diagram with two outcomes:
success `s n → s' x 1` or short-input `s → s 0`. The implementation
adds a third outcome: when `n = 32` and the lower 32 bytes (after
sign-bit masking for `readint`) encode a magnitude ≥ ℓ, the opcode
returns `Err(VMError::InvalidInt253Encoding)`, killing the whole
transaction.

**Prerequisites.** A script that reads 32 bytes (or, for `readint`, 32
bytes whose high-7-bits of byte 31 are set such that the masked
magnitude lands in [ℓ, 2^255)) from an attacker-controlled String.

**Impact.** A script that should produce a clean `0`-tag failure
("invalid input, try a smaller read") instead aborts the entire
transaction. For external transactions this is denial of service to
the script writer rather than a soundness issue; for application code
that wraps `readuint`/`readint` defensively (expecting only the
"success or short-input" diagram), this is a surprise.

**Mitigation.** Either:
- Push the `0`-tag failure branch (string restored, `0` pushed) when
  the magnitude is non-canonical — matches the spec's stated stack
  diagram and behaves uniformly with short-input.
- Or: update the spec to document the third "non-canonical magnitude"
  failure mode explicitly.

**References.**
- `flamevm/src/vm.rs`:861–909
- `flamevm/spec.md` §String operations (`readuint`, `readint`)

---

### Finding 7: `op_drop` rejects empty Dict despite spec wording [Low]

**Scenario.** Spec `0x1c drop`: "Drops any droppable item, including
empty structs and zero-tokens." Implementation only marks
Int253/String/Point as universally droppable, and ClearToken when
zero-qty. Empty Dicts hit the `_ => false` arm and fail with
`TypeNotDroppable`.

**Prerequisites.** Script that pushes an empty Dict and tries to drop
it.

**Impact.** Application-code surprise; not a security issue. The
linear-type discipline for Dicts requires the dict-member-flag work
(per `Value::try_clone`'s comment "Dict becomes copyable in Phase 5
once member-flag propagation lands"), so the gap is acknowledged.

**Mitigation.** Mark empty Dict as droppable now; defer non-empty
"dict is droppable iff all members are droppable" to Phase 5.

**References.**
- `flamevm/src/value.rs`:58
- `flamevm/spec.md` §Stack operations `drop`

---

### Finding 8: `read_value` returns `Ok(None)` for unimplemented tags, leaving reader at unpredictable offset [Low]

**Scenario.** `read_value` returns `Ok(None)` for the
Token/ClearToken/WideToken/Object/Merlin tags, signalling
"not-yet-implemented" without erroring. Inside a nested list/dict, the
inner failure propagates `Ok(None)` up the stack, but the reader has
already advanced past the partially-consumed prefix.

**Prerequisites.** Caller that uses `read_value` and continues to read
from the same reader after receiving `Ok(None)`. No such caller exists
today (only tests).

**Impact.** Parser confusion: a caller that doesn't treat `Ok(None)`
as fatal will read from the middle of a payload the decoder
ineffectively half-consumed.

**Mitigation.** Replace `Ok(None)` with `Err(ReadError::InvalidFormat)`
once the relevant types are encodable (Phase 8+). Until then,
document at the call site that `Ok(None)` is unrecoverable: the
reader's position is meaningless after such a return.

**References.** `flamevm/src/encoding.rs`:434, :513

---

### Finding 9: `pop_byte_count(usize::MAX)` invocations [Low]

**Scenario.** `op_read_str` and `op_write_zeros` allow `n` up to
`usize::MAX`. `op_read_str` is bounded by the source String's length
(safe today), but `op_write_zeros` is unbounded — see Finding 4. Even
when memory metering lands, the explicit `usize::MAX` ceiling sets the
wrong invariant.

**Mitigation.** Pass a meaningful upper bound to `pop_byte_count`. For
`writezeros` this is whatever the per-call memory limit allows. For
`readstr`, the bound is the source String's length.

**References.** `flamevm/src/vm.rs`:914, :984

---

### Finding 10: `op_pushpoint` / `op_read_point` accept any 32 bytes [Informational]

**Scenario.** Both opcodes wrap arbitrary 32-byte input as a Point
without checking that it decompresses to a valid Ristretto255 group
element. This is intentional (Ristretto255 decompression is
expensive), but downstream opcodes (`add` on Points, `sigverify`, …)
must check decompressability or operate on the compressed bytes.

**Impact.** None today. Soundness depends on every downstream opcode
that performs group operations actually doing the decompress check.
This is a property to maintain when those opcodes are added.

**Mitigation.** Standing requirement on Phase 11+ opcodes: any opcode
that does point arithmetic decompresses first and errors on invalid
encoding. Add as an explicit checklist item to the opcode
implementation review.

**References.**
- `flamevm/src/vm.rs`:759, :930
- `flamevm/src/crypto.rs`:16–25
- `threats/vm.md` §14.3

---

### Finding 11: Gas accounting unwired; deferred sigs unwired; constraint opcodes unwired [Informational]

**Scenario.** Multiple sub-systems are stubbed:
- `gas_used` is never incremented by any opcode; `gas_limit` is only
  consulted by `finish_call`/`op_return` for the "refund leftover to
  caller" pattern. Both pieces of arithmetic look correct once gas
  metering lands.
- `mem_used` / `mem_limit` are present in `CallFrame` but never
  consulted by any allocation site.
- `deferred_sigs` is never pushed to (no `sigverify` opcode).
- No constraint-system opcode (`const`, `extvar`, `intvar`, `expr`,
  `range`) is implemented.
- No `load`/`save`, no `call`/`send`/`open`, no `actorid`/`anchor`.

**Impact.** These are TODOs; not findings against existing code, just
inventory for future audits.

**Mitigation.** Each will be audited as it lands. The auditor flags
the priority order for the engineer:
1. Memory cap enforcement (ADR 0002) — needed before any test
   touches large allocations.
2. Re-entrancy guard at `call` (ADR 0003) — concrete attack surface.
3. Bulletproofs constraint accumulation in *external context only*
   (per ADR / `flamevm/CLAUDE.md` guardrail).
4. `load`/`save` mutual exclusion as the re-entry lock.

**References.**
- `flamevm/CLAUDE.md` status checklist
- ADR 0002 (memory), ADR 0003 (re-entrancy)
- `threats/vm.md` §1, §2.2, §7.3

---

### Finding 12: `op_read_uint` / `op_read_int` leak partial stack on hard failure [Informational]

**Scenario.** When `Scalar::from_canonical_bytes` returns None for a
32-byte read, the opcode errors but has already popped `s` and `n`
without restoring. Cf. Finding 2 (pattern is widespread); cf.
`op_drop` (vm.rs:786) which restores correctly.

**Impact.** None today (errors are fatal). Forward-looking concern
folded into Finding 2.

---

### Finding 13: Spec encoding table omits Token/ClearToken/WideToken/Object/Merlin payloads [Informational]

**Scenario.** The spec encoding table at `spec.md` line 100+ lists
tags 249–253 with descriptions like "Linear type … possibly
encrypted" but provides no payload format. The encoding code at
`encoding.rs`:513 treats those tags as not-yet-implemented.

**Impact.** Cannot encode/decode tokens, objects, or transcripts
today. Threat model entries §5.x rely on these formats existing
before they can be evaluated.

**Mitigation.** Engineer's checklist item. Architect's clarification
will likely flow through VM Engineer per the cross-role protocol.

---

## Recap of changes since last audit

This is the first audit; no diff. Subsequent audits should compare
against this report's coverage.

## Threat model refresh

`threats/vm.md` updates (separate edit in this cycle):
- §3.4 (sub-varint canonicality) moved from "Mitigated by construction"
  to **Open** with reference to Finding 1.
- §5.1 / §5.2 / §5.4 / §5.5 (linear-type drop/duplication) refined
  with reference to Finding 2's typed-pop anti-pattern.
- §2.1 / §2.2 / §2.4 / §2.5 cross-referenced with Findings 3 and 4.
- §15.x (determinism) — no change; Dict iteration is `BTreeMap`-backed
  which is deterministic.
- §11.x (flavor binding) — cross-referenced with Finding 5.

## Asks

To VM Engineer (separate feedback note):
- Fix Finding 1 (sub-varint canonicality) and Finding 2 (typed-pop
  linearity leak) in the next implementation pass.
- Apply defensive caps for Findings 3 and 4 until gas / memory
  metering lands.

To Architect (cc'd on VM Engineer feedback):
- Clarify `pushtoken` variant (Finding 5) — ClearToken vs Token, and
  whether the canonical-scalar check on flavor bytes is intended.
- Clarify `op_read_uint` / `op_read_int` failure modes (Finding 6) —
  hard-fail vs `0`-tag, per spec.
