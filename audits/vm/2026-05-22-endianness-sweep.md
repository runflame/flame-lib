# Audit 2026-05-22 — Endianness sweep (ADR 0006 readiness)

Scope:
- `flamevm/src/encoding.rs` (wire format)
- `flamevm/src/vm.rs` (opcode dispatch, in-script literal reads)
- `flamevm/src/int253.rs` (canonical scalar / sign-magnitude)
- `flamevm/src/string.rs` (bit-shift framing)
- `flamevm/src/crypto.rs` (Merlin wrapper; no integer serialization)
- `flamevm/src/tx.rs`, `value.rs`, `token.rs`, `object.rs`, `dict.rs`,
  `constraints.rs`, `errors.rs`, `lib.rs`
- `flamevm/spec.md` (full text)
- `readerwriter/src/{writer.rs,reader.rs}` (the byte-IO traits used by
  the wire codec; confirmed LE)
- Tree-wide scan for `to_be_bytes` / `from_be_bytes` / `BigEndian::`
  / `byteorder` outside `flamevm/`

Methodology:
- Tree-wide ripgrep for BE primitives and prose, scoped to the
  Flame-owned crates (the legacy `zkvm`/`token`/`blockchain`/`accounts`/
  `p2p` Cargo deps on `byteorder` are out of scope for this audit; they
  are not on the Flame consensus surface).
- Manual walk of every site that writes or reads a multi-byte integer.
- Read-back of the spec to enumerate the prose pinning endianness either
  way.

Context:
This audit pre-checks ADR 0006 (working name: "Little-endian
everywhere"). The goal is to identify any BE leak in the FlameVM wire
format, in-script literal format, in hash/transcript framing, or in
third-party boundaries that would conflict with the proposed invariant
that all multi-byte integers in wire format, on-disk format, transcript
inputs, and VM data types are LE.

## Summary

The wire format produced by `encoding.rs` is **substantially LE**:
every numeric payload (sub-varint U16/U32/U64, Int253 U32/U64 widths,
Int253 FULL 32-byte sign-magnitude scalar) is serialized little-endian.
The `readerwriter::Writer::write_u32`/`write_u64` helpers (the byte-IO
primitives used by the codec) are LE by definition. `Int253` itself is
LE by construction (sign in bit 255 of byte 31, magnitude bytes 0..30
little-endian).

The wire-format codec is **already all-LE**. No `to_be_bytes` /
`from_be_bytes` / `byteorder::BE` calls appear in the encoder/decoder
path. ADR 0006 does not require any change to `encoding.rs`.

However, **two surfaces outside the wire format are big-endian**:

1. **`pushint{8,16,64,128}` opcode literal operands (`0x10..=0x17`)**
   are read as big-endian magnitudes (`Run::read_be_uint`, called by
   `op_pushint_magnitude`). The spec explicitly says "big-endian" for
   `pushint16`. This is in **active production code**, not just tests,
   and is the single load-bearing BE site in the VM today.
2. **`String` bit-shift opcodes (`0x4c shiftleft`, `0x4d shiftright`)**
   treat the string as a big-endian bigint (byte 0 = most significant).
   This affects only `shiftleft`/`shiftright` semantics; it does not
   produce a serialized integer. Not a wire-format issue per se but
   worth recording because the comment uses the phrase "big-endian
   bigint" and a future reader might assume that to mean wire BE.

Findings by severity: **1 Medium, 1 Low, 3 Informational**. No High or
Critical: the existing wire format is already what ADR 0006 mandates,
and the BE leak is confined to the opcode literal layer where the spec
*currently* pins BE.

Counts of BE usages by location (tree-wide ripgrep, excluding tests):

| Location | BE call sites | Notes |
|---|---|---|
| `flamevm/src/encoding.rs` (encoder/decoder, the wire format) | **0** | All-LE, confirmed |
| `flamevm/src/vm.rs::Run::read_be_uint` | 1 production site | Called by `pushint{8,16,64,128}` |
| `flamevm/src/vm.rs` tests | 3 `to_be_bytes` | Tests cover the BE pushint opcodes; will need to flip to LE if ADR 0006 redefines those opcodes |
| `flamevm/src/string.rs` (shift_left/right comment) | "big-endian" prose | Affects `shiftleft`/`shiftright` semantics only; no integer serialization |
| `flamevm/spec.md` | 1 explicit "big-endian" on `pushint16`; the other widths inherit no explicit endian | The spec text is **inconsistent** today; `flamevm/plan.md` line 96 already flags this gap |
| `flamevm/src/int253.rs` | 0 BE | All LE: `to_u64` uses `from_le_bytes`; magnitude comparison walks from byte 31 down (correct for LE storage) |
| `flamevm/src/crypto.rs` (Merlin wrapper) | 0 | Defers length framing to the `merlin` crate; transcript framing is opaque to FlameVM and out of scope for the project's wire format |
| `flamevm/src/tx.rs` | 0 | No manual integer serialization yet; `serde(Serialize)` will need an explicit derive contract once TxID hashing lands |
| `consensus/`, `node/` | 0 | (No code yet; clean slate for ADR 0006) |
| Legacy `zkvm/`, `token/`, `accounts/`, `blockchain/`, `p2p/` | depend on `byteorder` | Out of scope for FlameVM; flagged for follow-up at the integration boundary |

The codebase is **already substantially LE** in the surface that ADR
0006 most cares about (the wire format). The remaining BE in
`pushint{8,16,64,128}` is the one place where the **spec and ADR 0006
will collide** — and it's a spec-text fix plus a one-function fix in
`vm.rs`, not a structural problem.

---

## Findings

### Finding 1: `pushint{8,16,64,128}` opcodes read magnitude as big-endian; conflicts with ADR 0006 [Medium]

**Location.**
- `flamevm/src/vm.rs::Run::read_be_uint` (line 172–182): the BE reader.
- `flamevm/src/vm.rs::op_pushint_magnitude` (line 785–795): single
  production caller; copies the BE-decoded `u128` low-to-high into a
  32-byte Scalar (the `magnitude.to_le_bytes()` at line 789 is correct
  for the scalar buffer — magnitude has already been computed as a
  u128). The BE happens at the *script byte → u128* boundary, not at
  the u128 → scalar boundary.
- `flamevm/spec.md` line 313: pins BE for `pushint16` explicitly.
- `flamevm/src/vm.rs` lines 1828, 1838, 3019 (tests): exercise the BE
  reading with `to_be_bytes` inputs. Will need flipping to LE if the
  opcode endianness changes.
- `flamevm/plan.md` line 96: VM engineer already flagged this as
  "extended BE consistency from spec.md's explicit BE wording on
  pushint16; worth a sentence to nail down for the other widths."

**Current endianness.** Big-endian for the magnitude payload (the
sign comes from the opcode pair, not from a sign-byte). One read,
inside one helper, called by one wrapper that handles every
`pushint{8,16,64,128}` width.

**Why it conflicts with ADR 0006.** If ADR 0006 states "all multi-byte
integers in wire format, on-disk format, hash transcript inputs, and
VM data types are LE," then the script-byte magnitude of
`pushint{8,16,64,128}` is a multi-byte integer in the wire format of
the executable program (the script itself). It must be LE under that
invariant.

A subtle counter-argument: scripts are byte sequences interpreted by
the VM, not transcript inputs to a hash. So one *could* claim "the
script is opaque bytes" and exempt the in-script literal layer from
ADR 0006. The auditor recommends rejecting that counter-argument:

1. The script is hashed (it becomes part of TxID via the merkle root
   over the effects list, per `design.md` §TxID binding). Two scripts
   with semantically identical literals but different endianness
   produce different TxIDs; this means the same source program
   compiled to BE vs LE bytes is not the same on-chain object. That
   collision is OK only if the spec pins one of them — and pinning LE
   matches ADR 0006.
2. Other in-script literals are already LE: `pushint` (0x18) reads
   the 32-byte canonical Int253 sign-magnitude, which is LE in the
   lower 255 bits per the Int253 spec. `pushpoint` (0x1a) reads 32
   bytes of compressed Ristretto, which is already LE per
   curve25519-dalek. The pushtoken/pushstr operand uses the
   sub-varint, which is LE. So `pushint{8,16,64,128}` is the only
   in-script literal class that swims against the LE tide.
3. The VM engineer's own `plan.md` already calls out this asymmetry
   as needing architect input.

**Impact.** Without a fix, two implementations could plausibly
disagree about `pushint{8,16,64,128}` byte order if one reads the
spec strictly ("only pushint16 is BE; the others default to LE") and
the other follows the existing implementation. This would be a
consensus break across implementations.

ADR 0006 is the natural forcing function for resolution: pin
`pushint{8,16,64,128}` to LE; update spec.md text; update
`read_be_uint` → `read_le_uint`; flip the three test inputs.

**Mitigation.**
1. In `spec.md` line 313, change the description for `pushint16` (and
   add explicit text for the other widths) to: "Reads 2/8/16 more
   bytes as little-endian unsigned magnitude, sets the sign to `s`."
2. In `flamevm/src/vm.rs::Run::read_be_uint`, replace the for-loop
   with a `u128::from_le_bytes`-equivalent (or rename to
   `read_le_uint` and adjust the for-loop to start at the high index
   and OR in shifted bytes — or simply zero-extend the slice into a
   16-byte array and call `u128::from_le_bytes`).
3. Flip the three test sites at `vm.rs` lines 1828, 1838, 3019.
4. The non-test caller `op_pushint_magnitude` itself does not need
   to change: it just takes a `u128` and copies its LE bytes into
   the low 16 bytes of the scalar — that step is already LE-correct
   and independent of how the `u128` was decoded from the script.

**References.**
- `flamevm/src/vm.rs` lines 172–182, 785–795.
- `flamevm/spec.md` line 313.
- `flamevm/plan.md` line 96 (engineer's own flag).
- ADR 0006 (proposed; not yet in `decisions/`).
- `threats/vm.md` §3 (encoding canonicality) — will be refined this
  cycle to record the LE invariant once ADR 0006 lands.

---

### Finding 2: `String::shift_left`/`shift_right` use big-endian bigint framing [Low]

**Location.**
- `flamevm/src/string.rs` lines 98–129 (`shift_left`) and 137–159
  (`shift_right`). The doc comment explicitly says "treating the string
  as a big-endian bigint: byte 0 holds the most significant bits."
- Bit-numbering helpers `bit_at` / `set_bit` at lines 170–191 use
  MSB-first within each byte. This is a separate, finer-grained
  convention from the byte-order one.

**Current endianness.** Big-endian "bigint view" of the string. A
shift-left moves bits toward byte 0; a shift-right moves bits away
from byte 0. The result interprets the string as a number with byte 0
holding the most-significant bits.

**Why it (mostly) doesn't conflict with ADR 0006.** This is *not* a
serialized integer. The string is byte-oriented data; the shift
opcodes choose a direction convention to define what "shift left"
means semantically. It is internally consistent and there is no other
opcode that reads or writes a multi-byte integer to or from this view.

The only soft conflict with ADR 0006 is **terminological**: the doc
comment uses the phrase "big-endian." A future reader of the spec
trying to enforce "everything is LE" might either (a) miss this site
and falsely report compliance, or (b) "fix" it by flipping the shift
direction, breaking the opcode's behavior.

The auditor recommends keeping the current shift semantics and
**rewording the comment** to use the term "MSB-first / byte 0 is most
significant" rather than "big-endian." This removes the terminological
collision without changing behavior. The rationale: bit-shift
direction is *not* an endianness choice in the usual sense; it is a
choice of which byte is "the start" of the conceptual number, which
maps to whether `shiftleft` reduces or grows the integer value.

**Impact.** Cosmetic / documentation. No on-chain behavior change.

**Mitigation.**
1. In `flamevm/src/string.rs` lines 98 and elsewhere in the file, drop
   the phrase "big-endian" and substitute "byte 0 is the most
   significant byte" or "MSB-first within and across bytes."
2. Document in ADR 0006 (or in a follow-up clarification) that the
   "all-LE" invariant applies to integer *serializations* — not to
   the geometric convention chosen by a bit-shift opcode.

**References.**
- `flamevm/src/string.rs` lines 98–129, 137–159, 168–191.
- ADR 0006 (proposed).
- `flamevm/spec.md` `shiftleft` / `shiftright` rows.

---

### Finding 3: Spec text inconsistency — only `pushint16` mentions endianness [Informational]

**Location.**
- `flamevm/spec.md` line 313 (the only line in the spec that pins an
  endianness for an in-script literal).

**Why it matters.** Even before ADR 0006 lands, the spec's stack-op
table calls out BE only for `pushint16`. The reader is left to infer
the byte order for `pushint8`/`pushint64`/`pushint128`. The
implementation extrapolates the BE convention from line 313 to all
widths, but the spec does not say so. This is a strict-reading
ambiguity that would be a divergent-implementation hazard
independently of ADR 0006.

**Mitigation.** Pin every `pushint{8,16,64,128}` row to a single
explicit endianness. ADR 0006 makes that endianness LE. If for some
reason ADR 0006 chooses to exempt in-script literals from the LE rule,
the spec must still pin every width to a single endianness.

**References.** `flamevm/spec.md` line 313; `flamevm/plan.md` line 96
(engineer's own flag); Finding 1 above.

---

### Finding 4: Merlin transcript wrapper hides length framing — confirm it's not a wire-format concern [Informational]

**Location.**
- `flamevm/src/crypto.rs` lines 41–68 (the `Merlin` type).

**Why it matters.** The Merlin transcript protocol internally uses
length-prefix framing for each `append_message` call, and the
underlying STROBE/Keccak construction has its own byte-order choices.
However:

1. FlameVM **never serializes a Merlin transcript** to the wire; it is
   a stack-only, non-portable type (per `spec.md` table line 253:
   `Merlin` tag exists but the spec entry just says "Reserved"-style
   "Instance of a Merlin transcript" — there is no spec-text for how
   to encode it).
2. Merlin's internal framing is consumed only by the `merlin` /
   `strobe` crates and only affects hash outputs, not anything that
   crosses an inter-implementation byte boundary directly.
3. ADR 0006 is about Flame's own choices; it does not constrain
   upstream cryptographic library framing.

**What to confirm.** When the `Merlin` type becomes portable (or when
the `merlinread`/`merlinwrite` opcodes are exposed to a path that
hashes their inputs into TxID via a non-Merlin transcript), revisit
this entry. Currently: no concern.

**Mitigation.** None today. Flag in `threats/vm.md` §14.2 (Merlin
transcript misuse) as a "verify when transcript framing crosses a
spec boundary."

**References.**
- `flamevm/src/crypto.rs` lines 41–68.
- `flamevm/spec.md` `merlin` / `merlinwrite` / `merlinread` rows.
- `threats/vm.md` §14.2.

---

### Finding 5: Legacy crates depend on `byteorder` — confirm they don't leak into FlameVM types [Informational]

**Location.** Tree-wide search for `byteorder` finds:
- `zkvm/Cargo.toml`, `token/Cargo.toml`, `blockchain/Cargo.toml`,
  `accounts/Cargo.toml`, `p2p/Cargo.toml`.
- `p2p/src/cybershake.rs` uses `byteorder::LittleEndian` (good — LE).

None of these crates are imported by `flamevm`. The only `flamevm`
dependency from this family that ports old code is `spacesuit`
(`SignedInteger`) via `int253.rs::From<SignedInteger>`. That
conversion goes through `to_u64()` / `from(u64)`, not through a
byte-level serialization, so it is endianness-agnostic.

**Why it matters.** ADR 0006 should explicitly state whether the
legacy crates (`zkvm` / `token` / `blockchain` / `accounts` / `p2p`)
are in scope. The auditor's reading is that they are *not* — they are
pre-Flame artifacts living in the same workspace and will be removed
or replaced as the new design lands. But if any of them ever serializes
a value that becomes part of a Flame consensus surface (a TxID input,
an actor state hash input, a sigverify message), the BE-vs-LE choice
in that crate becomes a Flame concern.

**Mitigation.**
1. ADR 0006 should state its scope explicitly: e.g., "applies to all
   crates whose output reaches a Flame consensus boundary." That
   matches the spirit of "everywhere" without pretending the legacy
   crates have been audited for it.
2. When the consensus crate or the node binary boundary lands,
   re-audit any byte-level serialization that crosses the FlameVM ↔
   consensus boundary.

**References.**
- `zkvm/Cargo.toml:15`, `token/Cargo.toml:9`,
  `blockchain/Cargo.toml:15`, `accounts/Cargo.toml:9`,
  `p2p/Cargo.toml:11`.
- `p2p/src/cybershake.rs:34`.

---

## Recap of changes since last audit

This is the second VM audit (after `2026-05-22-initial-surface-sweep.md`).
Diff vs that audit:

- No code changes have landed in `flamevm/src/` since the prior audit
  (Findings 1 and 2 from the surface-sweep audit are still open, per
  the engineer's status).
- This audit is a focused endianness sweep prompted by the architect's
  intent to publish ADR 0006. It supplements but does not overlap
  surface-sweep findings.

## Threat model refresh

`threats/vm.md` §3 (encoding canonicality) gains a new entry §3.8
recording the LE invariant once ADR 0006 lands. The auditor adds it
this cycle as **Mitigated by construction (for the wire format)** with
a forward-looking note that the `pushint{8,16,64,128}` literal layer
needs an explicit-LE update (Finding 1 of this audit).

§14.2 (Merlin transcript misuse) gets a one-line update noting Merlin
is stack-only and out of scope for ADR 0006 until its encoding is
exposed.

## Asks

To VM Engineer (separate feedback note `feedback/2026-05-22-vm-auditor-
on-pushint-endianness.md`):

- Acknowledge Finding 1 (`pushint{8,16,64,128}` BE). When ADR 0006
  lands, flip `Run::read_be_uint` to LE and update `spec.md` line 313
  to specify LE for all `pushint{8,16,64,128}` widths.
- Acknowledge Finding 2 (terminology cleanup in `string.rs` shift docs).

To Architect (cc'd via the same feedback note):

- ADR 0006's text should explicitly cover the in-script literal layer
  (it is not "just" wire format — scripts are hashed into TxID).
- ADR 0006 should state its scope: does "everywhere" include the
  legacy crates (`zkvm`, `token`, …)? Auditor recommends scoping to
  "everything that reaches a Flame consensus surface."
- ADR 0006 may want a sentence carving out STROBE/Merlin internal
  framing (out of scope) and bit-shift opcode "byte 0 is MSB"
  conventions (not a byte-order claim).

## Verdict

**The wire format is already all-LE.** The codebase is substantially
LE; ADR 0006 mostly *ratifies* existing practice. The single
load-bearing change needed for compliance is flipping `pushint{8,16,
64,128}` from BE to LE — one production helper, one spec line, three
test inputs. That change is straightforward and the engineer's
`plan.md` already anticipates it.
