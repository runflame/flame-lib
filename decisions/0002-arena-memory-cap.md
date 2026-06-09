# ADR 0002 — Transient memory cap = 4× persistent vbytes

- **Status:** accepted
- **Date:** 2026-05-22
- **Proposers:** Architect (promoted from existing design discussion)
- **Deciders:** Architect
- **Supersedes / related:** ADR 0004 (arbitrary vbyte sizing)

## Context

During an actor method invocation, the script needs working memory beyond its
persistent state: it must `load` the actor's state, mutate or rebuild it, and
`save` the new version. It may also build intermediate dicts, lists, or
constraint structures that exist only for the duration of the call.

A naive design exposes a separate "memory grant" parameter on every message send
and method call, with its own pricing, accounting, and forwarding rules. That
multiplies the dimensions a script author must reason about, and creates two
classes of pathological behavior we want to avoid:

1. Actors begging for memory grants from senders they cannot influence.
2. Senders sizing memory grants by guess, then overspending defensively.

We also want a deterministic, per-actor upper bound on transient memory at
allocation time, so that a node can refuse an allocation immediately rather
than discover an overrun mid-execution.

The persistent vbyte balance of an actor is already known and recorded. It is
the only resource the actor controls directly, and it already serves as the
"how much state should I be allowed to have" signal. Tying the transient cap
to the persistent size collapses two resources into one and removes the need
for any per-call memory parameter.

## Options considered

1. **Per-call memory grant** — every message send and method call carries a
   memory allotment, priced separately from gas.
   - Pros: senders control the cap precisely.
   - Cons: adds a third resource lane (after gas and vbytes); requires
     forwarding semantics; senders must guess actor needs; pathological
     "memory begging" patterns emerge.
2. **Fixed protocol-wide cap** — every actor has the same transient memory cap
   regardless of state size.
   - Pros: trivial; no per-actor state to track.
   - Cons: small actors get unnecessary headroom; large actors cannot expand
     their state in place because the cap does not scale. Forces awkward
     streaming patterns.
3. **k× persistent size, single fixed k** (chosen, k = 4).
   - Pros: zero parameters at call sites; scales with the actor; deterministic
     at allocation time; matches the natural pattern of "load, mutate, save"
     where the mutation may double-buffer the state.
   - Cons: an actor wanting more transient memory must purchase more vbytes
     (which it must hold persistently). This is intentional: the cost of
     transient memory is reflected in the cost of persistent state.
4. **k× persistent size, configurable k per actor** — actor declares its k.
   - Pros: flexibility.
   - Cons: another parameter to abuse; opens optimization games; does not
     materially help unless k is very large, at which point we should just
     raise the protocol-wide constant.

## Decision

Transient memory available to an actor during a call is capped at **4× the
actor's current persistent state size in vbytes**. The opcode `memlimit`
returns this cap. There is no per-call memory grant, no per-actor `k`,
and no way to borrow memory from another actor. Allocations that would push
live memory over the cap fail the call deterministically.

The 4× factor is chosen because it is the smallest integer that comfortably
supports the canonical load-mutate-save pattern: 1× for the loaded state,
1× for the new-state being built, and 2× of headroom for intermediate
structures (dict rebuilds, constraint accumulation, working stacks).

## Consequences

- Positive: zero new parameters at call sites; no forwarding rules; no
  memory-grant market; no memory-begging anti-pattern.
- Positive: an actor's transient memory cost is paid through its persistent
  vbyte balance — a single resource governs both lanes, and the existing
  vbyte fee market governs both.
- Positive: deterministic check at allocation time; nodes can reject an
  over-cap allocation without speculation.
- Negative: an actor that briefly needs much more transient memory than
  its steady-state size must over-allocate vbytes permanently. This is the
  intended pressure: transient memory is not free, and the cost is paid
  through the resource that the actor already controls.
- Negative: the fixed 4× constant is a magic number. Raising it requires an
  ADR (and probably soft-fork governance via the existing supermajority
  process used for other protocol parameters).
- Follow-up: VM Engineer must enforce the cap at allocation time in the
  execution loop, with a fuzz target verifying that the cap is checked at
  alloc (not at use). VM Auditor's `threats/vm.md` §2.2 tracks this.
- Affected artifacts: `design.md` §Resources, `flamevm/design.md`
  "Transient memory" subsection, `flamevm/spec.md` (`memlimit` opcode).

## References

- `flamevm/design.md` — Resources / Transient memory subsection.
- `threats/vm.md` §2.2 — memory exhaustion above the 4× cap.
- `flamevm/CLAUDE.md` Guardrails — "Memory cap is 4× current persistent
  vbyte size. Enforced at allocation time. No per-call grants."
