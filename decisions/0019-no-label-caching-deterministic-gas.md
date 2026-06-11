# ADR 0019 — No label caching: gas is a pure function of code and inputs

- **Status:** accepted
- **Date:** 2026-06-11
- **Proposers:** Architect
- **Deciders:** Architect
- **Supersedes / related:** amends ADR 0018 (drops its "per-code
  label-table cache" follow-up, both VM- and node-layer), ADR 0015.

## Context

ADR 0018's in-bytecode dispatch makes a forward `jumpif` scan past
earlier handlers on first jump. A per-tx label-table cache was added so
repeated calls to the same actor skipped the re-scan, with a planned
node-layer cross-tx variant. The cache required invalidation machinery
(`setcode` epoch guard) and — the deciding flaw — made the *work
performed* depend on cache state: the first call to an actor scanned,
later calls didn't.

## Decision

Remove label caching entirely. Every frame collects its labels on the
fly; every call pays the full scan cost every time.

Reasons:
1. **Deterministic resource planning.** Gas charged must be a pure
   function of (code, inputs), never of warm/cold cache state —
   otherwise either gas mispredicts the work done, or gas itself
   becomes cache-dependent and unplannable.
2. **No discount → no point.** If users must be charged the full scan
   price for determinism anyway, a cache that reduces only the *actual*
   work creates a permanent gap between priced and real cost for zero
   user benefit.
3. **Code is short by design.** Actor blobs are small; the scan is
   cheap; the invalidation machinery (epoch guard, harvest/seed, the
   stale-reharvest bug class it already produced) cost more complexity
   than the scan costs time.

## Consequences

- Positive: gas = f(code, inputs) exactly; the `setcode` epoch guard and
  harvest/seed logic are deleted; one less consensus-adjacent state.
- Negative: repeated dispatch into the same actor within a tx re-scans
  its prologue — accepted as immaterial for short code.
- The node-layer cross-tx cache idea is dropped with the same rationale.
- Affected artifacts: `flamevm/src/vm.rs` (cache, epoch, harvest/seed
  removed), spec §gas (determinism sentence).
