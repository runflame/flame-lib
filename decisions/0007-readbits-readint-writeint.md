# ADR 0007 — `readbits` / `readint` / `writeint` opcode set

- **Status:** accepted
- **Date:** 2026-05-22
- **Proposers:** Architect (in dialogue with VM Engineer), prompted by VM Auditor's 2026-05-22 surface sweep Finding 6 (`op_read_uint` / `op_read_int` failure-mode clarification).
- **Deciders:** Architect
- **Supersedes / related:** ADR 0006 (little-endian everywhere — pins LSB-first bit order); informs ADR 0006's follow-up bake-into-spec item.

## Context

The original spec carried three String-ops opcodes for moving integers in
and out of byte strings:

- `readuint(s, n)` at 0x40 — read n ≤ 32 bytes as a non-negative LE integer.
- `readint(s, n)` at 0x41 — read n ≤ 32 bytes as a signed integer, sign at
  the high bit of the high byte (sign-magnitude form matching `Int253`).
- `writeint(s, x)` at 0x45 — write all 32 bytes of an `Int253`.

Three problems came out of the VM Auditor's 2026-05-22 surface sweep and
the subsequent design discussion:

1. **`readuint` is redundant.** The only realistic primitive for parsing
   variable-width unsigned bit streams is symmetric to `writebits` at
   0x44. A bit-aligned reader covers byte-aligned reads as a special case
   (n ∈ {8, 16, 24, ...}). The separate byte-aligned unsigned reader pays
   an opcode slot for no expressive gain.
2. **`readint`'s width parameter is dead weight.** External protocols
   that carry signed integers almost universally use two's-complement,
   which `readint` does not produce. The opcode's actual use case is
   parsing Flame-native canonical `Int253` substrings, which are always
   32 bytes. The parameterized width is a footgun without a use case.
3. **The failure mode for non-canonical magnitude was undocumented**
   (Audit Finding 6). The spec showed an optional-shape stack diagram
   (`s' x 1 | s 0`) but did not state which inputs land in which branch
   versus which abort the script outright.

We have separately committed to little-endian everywhere (ADR 0006),
which pins the bit order question that previously rode along with these
opcodes.

## Options considered

For the slot 0x40 question (drop `readuint` vs replace it):

1. **Drop `readuint` and reclaim the slot.** Other 0x4x slots shift up.
   - Cons: shifts every opcode in column 0x4_ from 0x41 down by one,
     touching every other String-ops entry. Disturbs a dense, otherwise
     stable section of the opcode space.
2. **Drop `readuint` and leave a hole at 0x40.**
   - Cons: violates the dense-by-design encoding principle (we have
     reserved slots elsewhere and they cost real complexity).
3. **Replace `readuint` with `readbits` at 0x40** (chosen).
   - Pros: symmetric to `writebits(s, x, n)` at 0x44. One bit-stream
     primitive in each direction. Subsumes the byte-aligned unsigned
     reader. Zero layout shift. The sign-aware semantics (sign at fixed
     bit position 255) is a natural extension because every Flame
     integer is `Int253`.
   - Cons: the new opcode is slightly more nuanced than `readuint` was;
     authors using `readbits` for pure unsigned parsing must understand
     that the bit at position 255 is *not* read unless n = 256.

For the 0x41 `readint` width-parameter question:

1. **Keep variable width with sign-magnitude semantics.**
   - Cons: the only realistic call site reads the full canonical 32 bytes.
     The parameter occupies a stack slot and decodes a flag for nothing.
2. **Change semantics to two's-complement** (separate from the rename).
   - Cons: would diverge from `Int253`'s in-memory sign-magnitude form
     and require a conversion at the parser boundary. Loses the
     `readbits(s, 256)` equivalence.
3. **Drop the width parameter; fix at 32 bytes** (chosen). Equivalent
   to `readbits(s, 256)`.
   - Pros: matches the only real use case; removes a stack argument;
     trivially equivalent to the bit-stream reader at full width;
     stays in sign-magnitude form (matches `Int253`).
   - Cons: callers wanting partial signed parsing must compose
     `readbits` themselves. We do not have a known site for this.

For naming:

1. **Rename to `readint253` / `writeint253` for type-explicit names.**
   - Pros: opcode name tells you the target type.
   - Cons: longer, repeats the type-name suffix on opcodes that already
     live in a typed instruction set.
2. **Keep short `readint` / `writeint`; make docs Int253-explicit**
   (chosen).
   - Pros: compact spec table; the opcode set is `Int253`-native
     throughout, and the docs name the target type once.
   - Cons: a casual reader who skims only the opcode name might assume
     a two's-complement read.

## Decision

The String-ops integer/bit set is fixed as follows.

- **0x40 `readbits`** — `s n → s' x 1 | s 0`.
  Reads `n ≤ 256` bits from string `s`, LSB-first within byte, into
  bits 0..n-1 of a new `Int253`. The sign bit lives at the fixed
  position 255 of the `Int253` representation: it is set only when
  `n = 256` and the input's bit at position 255 is 1. Soft-fails
  (returns the `s 0` branch with the original string unchanged) on
  insufficient bytes in `s`, magnitude ≥ ℓ (only possible when
  `n ≥ 253`), or negative zero (only possible when `n = 256` and
  magnitude is zero with sign bit set). Hard-fails the script when
  `n > 256` — an author-controlled bound violation.

- **0x41 `readint`** — `s → s' x 1 | s 0`.
  Reads the canonical 32-byte `Int253` from the front of `s`.
  Equivalent to `readbits(s, 256)`. Inherits the same soft-fail
  conditions. Has no width parameter.

- **0x44 `writebits`** — `s x n → s'`. Semantics unchanged. Appends
  the low `n` bits of `x`'s canonical 32-byte `Int253` representation,
  LSB-first within byte. The final byte is high-padded with zeros to
  keep the result byte-aligned. Hard-fails when `n > 256`.

- **0x45 `writeint`** — `s x → s'`. Appends the canonical 32-byte
  `Int253` representation of `x`. Equivalent to `writebits(s, x, 256)`.
  Has no width parameter.

Underlying these four opcodes is a uniform failure principle:

- **External data violations soft-fail.** Anything that depends on the
  contents of a byte string parsed by the script (insufficient length,
  non-canonical magnitude, negative-zero encoding) returns the
  `0`-branch of the optional and leaves the input string unmutated.
  Scripts can recover and try a different path.
- **Author-controlled bound violations hard-fail.** Anything the author
  fully controls when they wrote the script (`n > 256` is the only such
  condition today) aborts the script. The author wrote a bug; we will
  not paper over it.

## Consequences

- Positive: one bit-stream primitive in each direction (`readbits` /
  `writebits`); one canonical-Int253 primitive in each direction
  (`readint` / `writeint`). No redundant opcode.
- Positive: zero opcode-layout shift. Other String-ops slots unchanged.
- Positive: `readbits` and `writebits` round-trip the same bits.
  Round-trip at the value level is exact for non-negative values with
  `n ≤ 256` (the sign is never written or read for `n < 256`) and for
  any `Int253` when `n = 256`. Negative values with `n < 256` lose
  their sign on write because the sign bit at position 255 is outside
  the n bits being written — this is documented, not surprising.
- Positive: the soft/hard-fail principle is now spec-level prose, not
  per-opcode discretion. Future opcodes inherit the rule.
- Positive: closes VM Auditor's 2026-05-22 Finding 6.
- Negative: `readbits` is more nuanced than `readuint` was. The
  "n < 256 → sign always 0" detail is a quiet footgun for authors who
  reach for `readbits` expecting two's-complement semantics. The
  docstring states the rule explicitly; the audit will verify the
  test coverage.
- Negative: short names `readint`/`writeint` rely on docs (and on the
  spec's Int253-native context) to convey the target type. A reader
  hopping into the opcode table without context could mis-read.
- Follow-up: VM Auditor should refresh fuzz coverage for `readbits`
  (per the new entries in `threats/vm.md` §3.9–3.11) once the next
  audit cycle begins. The existing implementation in `flamevm/src/vm.rs`
  and the tests in the same file already cover the matrix.
- Affected artifacts: `flamevm/spec.md` §String operations (already
  updated this cycle); `flamevm/src/vm.rs` (`op_read_bits`,
  `op_read_int`, `op_write_bits`, `op_write_int` already implemented);
  `threats/vm.md` §3.9–3.11.

## References

- `audits/vm/2026-05-22-initial-surface-sweep.md` Finding 6 — the
  audit observation that prompted the failure-mode clarification.
- ADR 0006 — Little-endian everywhere (pins the LSB-first bit order
  used by `readbits` and `writebits`).
- `flamevm/spec.md` §String operations — current opcode table.
- `flamevm/src/vm.rs` `op_read_bits` / `op_read_int` /
  `op_write_bits` / `op_write_int` — implementation.
- `threats/vm.md` §3.9–3.11 — auditor's surface entries.
