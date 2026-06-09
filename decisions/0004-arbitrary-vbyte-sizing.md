# ADR 0004 — Arbitrary per-vbyte actor sizing (no power-of-two arenas)

- **Status:** accepted
- **Date:** 2026-05-22
- **Proposers:** Architect (promoted from existing design discussion)
- **Deciders:** Architect
- **Supersedes / related:** ADR 0002 (4× transient memory cap depends on this).

## Context

Many resource-managed runtimes quantize allocations into a small set of
arena sizes — typically powers of two (1 KiB, 2 KiB, 4 KiB, ...). The
advantages are operational: fewer distinct sizes simplifies the allocator,
amortizes per-allocation metadata, and (in OS kernels) maps cleanly to
page sizes.

The disadvantage is that the resource the user buys is not the resource
the user pays for. An actor that needs 1100 bytes of state pays for 2048;
one that needs 4100 pays for 8192. The fee market then prices arena
slots, not bytes, and authors are nudged into pathological behaviors:

- **Slot-fitting** — designing data layouts to barely fit one slot,
  sacrificing maintainability.
- **Slot-jumping** — over-sizing on initial allocation to avoid a
  later resize that doubles the cost.
- **Fee distortion** — the marginal cost of one more byte is zero
  until the slot boundary, then it doubles. This produces a non-monotonic
  fee gradient and breaks intuitive fee estimation.

Quantization also interacts badly with ADR 0002's "transient memory =
4× persistent". With power-of-two arenas, an actor sitting in the
middle of a 4 KiB slot has 16 KiB of transient memory regardless of
how much of that slot it actually uses. The pricing signal that ADR 0002
relies on — "your transient cap is proportional to what you actually
paid for" — is corrupted.

The right unit is the byte. Storage cost is denominated in protocol-
defined virtual bytes (vbytes), which already abstract away on-disk
representation differences across implementations. Pricing per vbyte
makes the fee gradient monotonic and the transient memory cap honest.

## Options considered

1. **Power-of-two arenas (1, 2, 4, 8, ... KiB).**
   - Pros: allocator simplicity; canonical pattern from kernel and
     language runtime literature.
   - Cons: non-monotonic fee gradient; corrupts the 4× transient cap;
     incentivizes slot-fitting and slot-jumping; prices what the
     implementation does, not what the user wants.
2. **A small fixed set of non-power-of-two slot sizes** (e.g., 256, 1024,
   4096, 16384).
   - Pros: marginal allocator simplification.
   - Cons: all the cons of (1), just with different cliffs.
3. **Per-vbyte allocation; no slots** (chosen).
   - Pros: fee gradient is monotonic and matches actual cost; transient
     memory cap from ADR 0002 is honest; no slot-fitting games.
   - Cons: implementations cannot pre-bucket allocations as cheaply.
     This is purely an implementation concern, not a protocol one:
     vbytes are a protocol abstraction; on-disk representation is free
     to bucket as it wishes, provided vbyte accounting matches the
     protocol formula.

## Decision

Actor persistent state is metered in **vbytes** with no quantization.
An actor occupying N vbytes pays for N vbytes per block of decrement.
There are no slot sizes, no per-slot fees, no jumps. The vbyte formula
is defined at the protocol level so that all implementations agree on
the cost of a given state regardless of how the implementation chooses
to lay it out on disk.

Vbyte purchase, transfer, decrement, and recycling all operate at
per-vbyte granularity. Future protocol parameter changes to vbyte
introduction rate, grace period (ADR 0005), or transient memory factor
(ADR 0002) inherit this granularity.

## Consequences

- Positive: fee gradient for state size is monotonic and predictable.
  Authors can reason about marginal storage cost without slot maps.
- Positive: ADR 0002's transient memory cap is exactly proportional
  to the vbytes the actor actually holds, preserving the pricing signal.
- Positive: the supermajority parameter-adjustment mechanism applies
  uniformly to vbyte introduction rate without slot-recalibration.
- Negative: implementations cannot cheaply bucket allocations by a
  small set of arena sizes. They must either accept the heap cost or
  bucket internally while accounting per vbyte. The protocol does
  not specify how — only that vbyte accounting must match.
- Negative: the vbyte formula (how the abstract count is derived from
  the concrete state encoding) must be specified precisely and
  invariantly across implementations. The Consensus Engineer and VM
  Engineer share responsibility for keeping the formula deterministic
  and unambiguous.
- Follow-up: precise vbyte formula must be specified in `flamevm/spec.md`
  (current spec sketches it; refine through normal feedback loop).
  Consensus Engineer's deterministic-replay test vectors must include
  vbyte accounting cases.
- Affected artifacts: `design.md` §Resources / Storage, `flamevm/spec.md`,
  `consensus/` test vectors.

## References

- `flamevm/design.md` — Resources / Storage subsection.
- ADR 0002 (transient memory cap, depends on per-vbyte accounting).
- ADR 0005 (grace period, denominated in active blocks of vbyte-paying life).
