# ADR 0006 — Little-endian everywhere

- **Status:** accepted
- **Date:** 2026-05-22
- **Proposers:** Architect (prompted by VM Auditor's 2026-05-22 surface sweep)
- **Deciders:** Architect
- **Supersedes / related:** —

## Context

Flame's wire format, on-disk format, hash-transcript framing, and VM data
types all encode multi-byte integers. Until now, byte order has been a
matter of convention rather than a documented commitment, and each
implementation site has chosen for itself. That is exactly the
environment in which canonicality bugs breed: a value encoded one way at
one layer and another way at another layer either disagrees at a hash
boundary or admits two encodings of the same logical value.

A new piece of evidence makes this concrete. The VM Auditor's
2026-05-22 initial surface sweep
(`audits/vm/2026-05-22-initial-surface-sweep.md`, Finding 1, High)
found a canonicality break in the U64 sub-varint branch of
`flamevm/src/encoding.rs`: a payload chosen so that
`SUBVAR_U64_BASE + payload` wraps modulo 2^64 yields a value already
representable in a shorter width, so a single logical value has two
distinct wire encodings. That bug is fixable in isolation, but the
class of bug — "multiple ways to write the same integer" — is the
class we want the protocol's encoding rules to make impossible by
construction. A single, system-wide endianness commitment is one of the
two structural moves that close off this class (the other being
strict-shortest-encoding canonicality, which we already require).

Several local facts already pull us toward little-endian:

- The cryptographic substrate is little-endian by definition.
  Ristretto255 / curve25519 scalars are 32-byte little-endian encodings
  of integers mod ℓ. Flame's `Int253` lives in the same algebraic
  neighborhood and naturally encodes the same way.
- The varint family we already use (the varint plus sub-varint
  encoding in `flamevm/src/encoding.rs`) is inherently
  low-order-byte-first by construction. Fixed-width little-endian
  integers preserve the same mental model: bytes 0..k of the encoding
  carry the low-order k bytes of the value.
- The primitives Flame integrates against (BLAKE2/3, ChaCha20,
  curve25519-dalek, Ristretto255, Bulletproofs) are little-endian
  natively. Mixing endianness at the boundary between our framing and
  these primitives would require byte-swapping at every transcript
  append, which is both a performance footgun and a bug magnet.
- The remaining big-endian conventions in the surrounding ecosystem
  (SHA-256's length field, certain legacy MAC constructions, network
  byte order in Bitcoin's pre-SegWit script encoding) live *inside*
  primitives or in foreign protocols. They do not leak into our wire
  format unless we consume their byte outputs and re-encode — and even
  then, the prescription is "consume opaque, re-encode at the
  boundary," not "match their endianness."

The cost of the decision is low. The system has no implemented BE
encoder today, so this commitment ratifies the existing direction of
travel rather than reversing any code. The benefit is high: every
future decoder reviewer can reach for a single rule rather than
re-deriving the choice per type, and a whole class of
"oh-this-one-is-different" canonicality bugs is closed off at the
architectural level.

## Options considered

1. **Little-endian everywhere** (chosen).
   - Pros: matches the cryptographic substrate; matches the varint
     family's inherent byte order; one rule across the whole protocol;
     no byte-swap shims at primitive boundaries; eliminates the
     "which endian here?" question from every decoder review.
   - Cons: diverges from "network byte order" tradition in some
     non-cryptographic legacy protocols. Not actually a cost — Flame
     does not interoperate with those protocols at the wire-format
     layer.
2. **Big-endian everywhere.**
   - Pros: matches some legacy network-protocol tradition.
   - Cons: forces byte-swaps at every interaction with the
     cryptographic substrate; clashes with the natural byte order of
     the varint family (varints are unambiguously low-order-first);
     no positive interop benefit since Flame's external interfaces are
     either cryptographic (LE-native) or our own (we choose).
3. **Mixed: BE for "framing" fields, LE for "cryptographic" fields.**
   - Pros: none specific to Flame.
   - Cons: this is precisely the configuration in which canonicality
     bugs at the boundary between framing and payload become
     impossible to reason about systematically. Every decoder review
     becomes a per-field endian audit. This is the option the ADR
     exists to foreclose.
4. **Defer.**
   - Pros: minimal current code to migrate.
   - Cons: deferral is itself a decision in favor of option 3 by
     default — each implementer continues to pick locally. The audit
     finding above is the first concrete instance of the cost of that
     default.

## Decision

Flame is **little-endian everywhere**.

Concretely:

- Every multi-byte fixed-width integer in serialized form — wire
  format, on-disk format, hash-transcript inputs, VM data types — is
  encoded in little-endian byte order. This includes `u8`/`u16`/`u32`/
  `u64`/`u128` width fields, `Int253` magnitudes, and any other
  fixed-width integer the system serializes now or in the future.
- Length prefixes, domain tags, and integer parameters appended to
  cryptographic transcripts (Merlin, hash-based transcripts, signature
  challenge inputs) are little-endian.
- The varint and sub-varint families remain low-order-byte-first
  (which is little-endian by construction); no change to existing
  encoders.
- Cryptographic primitives whose outputs are conventionally rendered
  big-endian (e.g., the SHA-256 length field, certain legacy MAC
  bit-orderings) are either consumed as opaque byte strings or
  re-encoded at the boundary so that no Flame-level field is BE.

## Consequences

- Positive: a single, system-wide rule replaces per-type endianness
  reasoning. Decoder reviews check canonicality against one
  prescription rather than auditing the choice per field.
- Positive: cryptographic substrate (Ristretto255 scalars, Int253,
  Merlin/transcript inputs, ChaCha20/BLAKE2/3 framing) needs no
  byte-swap shim layer.
- Positive: a class of boundary bugs ("BE here, LE there, hash
  disagrees") is foreclosed at the architectural level.
- Positive: aligns with the existing varint family without exception,
  so no per-type "wait, this width is BE" footnote ever needs to
  appear in the spec.
- Negative: re-iterates that Flame does not follow the legacy
  "network byte order" tradition. Engineers porting code from
  protocols that use BE on the wire (some Bitcoin script artifacts,
  some IETF protocols) must explicitly byte-swap at the import
  boundary.
- Follow-up: VM Engineer to bake the LE commitment into
  `flamevm/spec.md` (Encoding section, plus a one-line statement at
  the top of the Data types section). VM Engineer to add the
  decode-encode-bit-compare canonicality test to every new decoder as
  a standing review item. VM Auditor's existing per-type canonicality
  fuzz target requirement (per `audits/vm/CLAUDE.md` guardrails) is
  reinforced.
- Affected artifacts: `design.md` §Data types / Wire format,
  `flamevm/spec.md` §Encoding, and any future ADR that introduces a
  new serialized integer type.

## References

- `audits/vm/2026-05-22-initial-surface-sweep.md` Finding 1
  (non-canonical U64 sub-varint) — the concrete bug whose class this
  ADR closes off.
- `flamevm/src/encoding.rs` — existing varint / sub-varint
  implementation; already low-order-first.
- Ristretto255 specification (Hamburg et al.) — 32-byte little-endian
  scalar encoding mod ℓ.
- `audits/vm/CLAUDE.md` — "Every encodable type needs a
  canonical-encoding fuzz target."
