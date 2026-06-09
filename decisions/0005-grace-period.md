# ADR 0005 — Grace period = active_blocks / 4, capped at six months

- **Status:** accepted
- **Date:** 2026-05-22
- **Proposers:** Architect (promoted from existing design discussion)
- **Deciders:** Architect
- **Supersedes / related:** ADR 0004 (per-vbyte sizing).

## Context

An actor's persistent state is paid for in vbytes that decrement each block.
When the balance reaches zero, the protocol must decide between two extremes:

1. **Immediate eviction** — clear the actor the moment the balance hits zero.
   This minimizes wasted space but is unforgiving: a maintenance lapse,
   a notification miss, or a single missed top-up loses the actor's state
   permanently. Long-lived applications (an AMM running for years) lose
   their entire history to a one-block oversight.
2. **Indefinite preservation** — keep the actor's state forever once paid
   for, even if depleted. This is a free squat: an attacker pre-pays a
   single vbyte's worth of life on millions of garbage actors and the
   network pays storage forever.

Both extremes are wrong. We want a grace period: a finite preservation
window after depletion during which a top-up restores the actor. The
length of the window should reflect how invested the network already is
in this actor's state — long-lived actors get more grace, short-lived
actors get less.

The proportional rule "grace ∝ active life" matches that intuition.
An actor that has lived for a year has demonstrably been useful and
deserves operators time to notice and refill; an actor that lived for
two blocks is more likely to be a misallocation or attack.

Two parameters need to be set:
- The proportionality constant (grace as a fraction of active life).
- A hard cap (so that an actor running for ten years does not earn
  two and a half years of free grace).

## Options considered

1. **Fixed grace period (e.g., 100 blocks for everyone).**
   - Pros: trivial.
   - Cons: punishes long-lived actors (100 blocks is nothing for a year-
     old actor); rewards spam actors (100 blocks of free squat per
     spawned actor regardless of how short its useful life was).
2. **Grace = active_life × k, no cap.**
   - Pros: scales naturally.
   - Cons: an attacker who runs an actor for a long time then withdraws
     vbytes gets an enormous tail of free squat. Worst case: keep an
     actor alive cheaply for years specifically to bank grace, then
     squat the state for the full proportional grace period.
3. **Grace = active_life / k, capped at C months** (chosen, k = 4,
   C ≈ 6 months of blocks).
   - Pros: scales for honest long-lived actors up to a sensible bound;
     short-lived actors get little grace, neutering the squat attack;
     the cap bounds worst-case storage liability.
   - Cons: two parameters to remember. The k = 4 and 6-month numbers
     are calibrated, not derived from first principles — future
     measurement may justify revision.
4. **Per-actor declared grace, paid at deployment.**
   - Pros: explicit pricing.
   - Cons: another parameter at every deployment; opens optimization
     games; complicates the actor model.

## Decision

When an actor's vbyte balance reaches zero, it enters a **frozen** state:
calls are rejected, but state is preserved. The grace period during which
a top-up restores the actor is:

```
grace_blocks = min( active_blocks / 4, blocks_per_6_months )
```

where `active_blocks` is the number of blocks since the actor's most recent
activation (initial deployment, or last top-up from zero), and
`blocks_per_6_months` is the protocol's convention for six months of
blocks (a derived constant; same governance as other protocol params).

If a top-up arrives during the grace period, the actor is unfrozen and its
`active_blocks` counter resets to start counting from the unfreezing block.
If the grace period elapses without a top-up, the actor's state is cleared
and its vbytes rejoin the global pool subject to the standard 100-block
maturity (per design.md / `flamevm/design.md`).

The 4× divisor is symmetric with ADR 0002's 4× transient memory factor by
coincidence, not by causation. The choice falls out of: short-lived actors
should earn negligible grace (a 4-block actor earns 1 block of grace);
day-old actors should earn hours (~6 hours for a 24-hour-old actor at a
typical block cadence); year-old actors should earn months (~3 months for
a year-old actor); long-lived actors should hit the cap.

The 6-month cap bounds the worst case: even an actor running for many
years cannot bank more than six months of free squat after depletion.

## Consequences

- Positive: long-lived applications survive realistic maintenance lapses
  (months of warning rather than blocks).
- Positive: short-lived spam actors earn negligible grace; the squat
  attack is bounded by `1/4` of the spam actor's purchased life.
- Positive: maximum storage liability per cleared actor is bounded by
  the cap.
- Positive: only one state bit (`frozen`) and two integers
  (`activated_at_block`, `balance`) per actor are needed to implement.
- Negative: the 4× divisor and 6-month cap are calibrated parameters
  rather than first-principles derivations. Adjusting either requires
  governance (supermajority soft fork, same threshold as vbyte
  introduction rate).
- Negative: the "active blocks" counter must reset on every unfreezing,
  which means an attacker cannot bank grace across multiple freeze/
  unfreeze cycles. Implementations must be careful to reset, not
  accumulate.
- Follow-up: Consensus Engineer must add a deterministic-replay test
  vector covering freeze, mid-grace top-up, and grace expiry. VM
  Engineer must implement the frozen-state call rejection. Block
  storage layer (Integrator) must implement the 100-block maturity
  for recycled vbytes.
- Affected artifacts: `design.md` §Resources / Depletion and grace,
  `flamevm/design.md` "Depletion and grace period" subsection,
  `consensus/` test vectors.

## References

- `flamevm/design.md` — Resources / Storage / Depletion and grace period.
- ADR 0004 (per-vbyte sizing; active life is measured in vbyte-paying
  blocks).
- Ethereum state-rent literature (multiple EIP drafts, 2017–2019) —
  examines the squat / grace trade-off; informs the 4× divisor.
