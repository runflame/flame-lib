# ADR 0009 — Gas calibration strategy

- **Status:** proposed
- **Date:** 2026-05-24
- **Proposers:** Architect (prompted by VM Engineer questions G.1–G.4 in
  `flamevm/todo.md`, "Cross-cutting — Gas calibration").
- **Deciders:** Architect
- **Supersedes / related:** ADR 0002 (arena memory cap — pre-existing
  resource discipline that this ADR builds on, not replaces).

## Context

`design.md` §Block resource pools commits Flame to per-block parallel
and serial gas pools `B_par` and `B_ser` (4:1 ratio). The *existence*
of gas is settled. What is not settled is: how do we pick the gas cost
for any given opcode?

Without a calibration methodology:

- Per-opcode gas values are guesses.
- Adversarial inputs to under-priced opcodes are an unbounded DoS
  surface — a single transaction can stall a validator past its block
  time without ever exhausting its gas budget.
- Re-pricing later is a soft fork. The cost of "guess wrong" grows
  monotonically with deployed state.

Constraints the user has already pinned:

- **Tolerance ±30–50% is acceptable** — we want stable *ratios* across
  opcodes, not exact cycle counts. Different nodes will run the same
  gas table at different absolute speeds; we accept that.
- **No multi-client diversity in scope.** Flame has one Rust client.
  Cross-validation comes from running the *same* binary on different
  hardware, not from running different binaries on the same hardware.
- **No parallel intra-tx execution.** Opcode cost is well-defined in
  the sequential sense; Sui-style bucketing-for-parallelism is not
  motivated.

A survey of peer chains (see *References*) surfaced six recurring
patterns. Five are worth stealing:

1. **Deterministic instruction count + wall-clock, take the worse of
   the two** — NEAR's two-metric posture defends against either
   metric's blind spots (icount misses memory stalls; wall-clock
   misses cache-cold tails).
2. **Marginal-difference program construction** — Ethereum's
   gas-cost-estimator and Substrate's frame-benchmarking both fit
   `total_time = base + a·n` over programs with `n` and `n+1` copies
   of the opcode; the slope is the per-opcode cost, freed from
   dispatch overhead.
3. **Worst-case is the default** — Substrate explicitly benches the
   most-expensive path; NEAR uses "byzantine benchmarking" assuming a
   third of validators collude to maximize cost. We adopt the same
   posture.
4. **Generated gas table committed to the repo** — Substrate's
   `WeightInfo` trait pattern. The table is data, regenerable, and
   tracked in git so any change is reviewable.
5. **Containerized estimator** — NEAR's QEMU+TCG runs inside Podman so
   the toolchain is pinned. Without this, numbers drift across
   developer machines for reasons unrelated to the VM.

The sixth pattern — splitting compute and I/O into independent lanes
(Aptos: compute + storage; NEAR: instructions + read-bytes +
write-bytes) — is deferred. Flame's Cell access goes through Utreexo
and an in-memory actor registry; we do not yet know whether I/O is
dominant enough to motivate a separate lane. Default for v0: single
gas lane, with per-byte rates folded into the opcodes whose cost is
I/O-bound (`load`, `save`, `output`, `hash`, `encode`, etc.). If
measurement reveals I/O misprices the table, a follow-up ADR splits
the lane.

## Options considered

### G.1 — Reference machine

1. **Pinned cloud instance (AWS `c7i.large`) + Podman-containerized
   estimator** (chosen).
   - Pros: reproducible by anyone with an AWS account; vendor's CPU
     and kernel are documented; container pins the Rust toolchain,
     libc, and benchmark binaries so numbers are stable across
     developer machines.
   - Cons: AWS dependency; cloud-instance steal time is non-zero
     (mitigated by running 3× and taking median); EBS-backed disks
     introduce noise for I/O-touching benchmarks (mitigated by
     pinning a single instance type with documented gp3 IOPS).
2. **Bare-metal box of fixed spec.**
   - Cons: most durable, but the worst at reproducibility — every
     contributor would need access to the same physical box, or
     calibration becomes "the box one person owns."
3. **Synthetic per-node reference** (each node calibrates locally;
   consensus uses the gas table from the *slowest* recent participant).
   - Cons: introduces a consensus protocol for gas-table convergence.
     Massive scope creep. Defers to a future ADR if multi-client
     diversity ever lands.

### G.2 — Gas unit and anchor rule

1. **1 gas = 1 ns of reference-CPU time. Anchor rule: "1 ms = 1
   Mgas"** (chosen).
   - Pros: opcode dynamic range falls in [~5 gas, ~10 Mgas] —
     cheapest ops are integers above 1, heaviest crypto ops fit in
     u32 with headroom. Block budget at 200 ms reference-CPU is
     200 Mgas, comfortably below `u32::MAX` and trivially below
     `u64::MAX` even with multi-thousand-tx blocks.
   - Cons: re-priced if reference CPU changes generation; ties the
     unit's *meaning* to the reference machine (acceptable — that's
     also what makes calibration possible).
2. **1 gas = 1 ps (NEAR convention, `1 ms = 1 Tgas`).**
   - Cons: 1000× wider range than we need; tiny ops cost thousands of
     gas, large ops billions. Numbers become harder to read in spec
     and in fee estimators with no benefit at our granularity.
3. **1 gas = 100 ns** (round-number alternative).
   - Cons: cheapest ops (`dup`, `pop`) collapse to 1 gas, making them
     indistinguishable. Loses signal in the cheap range where typical
     scripts spend most of their gas.

### G.3 — Lanes (compute vs I/O)

1. **Single lane (compute-gas), with per-byte rates baked into
   I/O-touching opcodes** (chosen for v0).
   - Pros: matches Ethereum and Substrate; one block budget; one fee
     dimension for the wallet UX; one number per opcode in the spec.
     Per-byte rates inside `load`/`save`/`hash`/`encode` cover I/O
     amplification.
   - Cons: bundles two physically-distinct costs under one number;
     pricing an I/O-bound opcode requires the calibration harness to
     simulate cold-cache reads honestly.
2. **Two lanes (compute + bytes-touched).**
   - Cons: doubles protocol surface (two block budgets, two fee
     dimensions). Useful if measurement later reveals I/O dominates;
     a future ADR can split the lane without invalidating the
     calibration framework introduced here.
3. **Three lanes (compute + read-bytes + write-bytes), NEAR pattern.**
   - Cons: as above, plus a write-vs-read distinction that depends on
     Cell-store implementation choices that are not yet settled.

### G.4 — Block compute budget

1. **200 ms of reference-CPU per block, derived: `budget_gas =
   200_000_000` from the G.2 anchor rule** (chosen).
   - Pros: leaves the rest of the block interval (whatever it is —
     not yet pinned at the design.md level) for gossip, signature
     aggregation, Utreexo updates, persistence, and slow-node margin.
     A budget derived from an anchor rule + a fraction is one fewer
     magic number than a budget pinned in isolation.
   - Cons: ties budget to a fraction of "block interval" before block
     interval is itself pinned. Acceptable — when block interval
     lands in another ADR, this number recomputes mechanically.
2. **Fixed Mgas number unrelated to block interval.**
   - Cons: re-justifying it every time block interval changes.
3. **Budget per second of wall-clock (e.g., `1 Mgas/ms` × wall-clock
   ms used in this slot).** Closer to "real" cost but creates a
   non-deterministic scheduler-style budget.
   - Cons: introduces wall-clock to consensus. Not acceptable.

### Calibration methodology (cross-cutting)

1. **Criterion-rs (wall-clock) + iai-callgrind (instruction count) +
   take the worse of the two; marginal-difference program
   construction; per-release recalibration + per-PR iai regression
   gate** (chosen).
   - Pros: criterion gives a real-time number (captures cache misses
     and memory stalls); iai-callgrind gives a deterministic number
     (catches regressions without flaky CI). The "take worse" rule is
     defensive — neither metric can hide cost from the other. The
     marginal-difference loop construction eliminates dispatch and
     setup overhead from the per-opcode number.
   - Cons: two tooling dependencies; iai-callgrind requires Valgrind
     installed in CI; criterion's statistical machinery is harder to
     gate deterministically (we use it for absolute numbers, not for
     PR gating).
2. **Single metric (wall-clock only).** Simpler; flakier CI; no
   defense against regressions where wall-clock noise hides a real
   slowdown.
3. **Single metric (iai-callgrind only).** Deterministic but blind to
   memory-bound opcodes — exactly the DoS surface we are trying to
   bound.

## Decision

The following choices are pinned. Each is reversible only via another
ADR.

1. **Reference machine.** AWS `c7i.large` Linux instance, pinned
   kernel + governor=performance + ASLR disabled for benches, running
   the calibration estimator inside a Podman container that pins the
   Rust toolchain, libc, criterion, iai-callgrind, and Valgrind
   versions. Container image tag is committed to the repo.

2. **Gas unit.** `1 gas = 1 ns` of reference-CPU execution time.

3. **Anchor rule.** `1 ms reference-CPU = 1 Mgas`. This is the rule
   the calibration harness enforces: every opcode's published gas
   value, when summed over a synthetic worst-case block, must keep
   total wall-clock under the budget at this rate.

4. **Block compute budget.** `B_par + B_ser = 200_000_000` gas
   (`200 ms` reference-CPU per block). The existing 4:1 split between
   `B_par` and `B_ser` from `design.md` §Block resource pools is
   preserved.

5. **Lanes.** Single gas lane. Per-byte rates for I/O-touching
   opcodes are folded into the opcode's `slope` coefficient (see (7)).
   A future ADR may split into compute + bytes lanes if measurement
   warrants.

6. **Tolerance.** Published gas values carry an implicit ±50%
   acceptance band. The calibration harness reports any opcode whose
   cross-machine ratio drift exceeds 2.0 between the reference and a
   secondary machine; such opcodes are re-priced to the worst of the
   two.

7. **Per-opcode pricing model.** Each opcode's gas is
   `gas = base + Σ slope_i · n_i`, where `n_i` are input dimensions
   that linearly drive cost (string length in bytes, dict size,
   scalar count for MSM, etc.). Non-linear opcodes are forbidden:
   any opcode whose measured cost is super-linear in any input
   dimension must be redesigned, split, or priced at its worst-case
   upper bound (matching Substrate's "no non-linear extrinsics"
   posture).

8. **Calibration tools.**
   - **`criterion-rs`** for wall-clock measurement; produces the
     absolute per-opcode number that lands in the gas table.
   - **`iai-callgrind`** for instruction-count measurement; produces
     the deterministic regression gate that runs in CI on every PR.
   - **`perf-event`** (Linux) as an optional cross-check on the
     reference machine for cycles and cache-miss counts during audit
     review.

9. **Program construction.** For each opcode `O` with arity
   `n → m`, the calibration harness builds programs of the form
   `setup`-`{O, drop_m}·k`-`teardown`. Per-opcode cost is the slope of
   `wall_clock(k)` regressed against `k`, over `k ∈ {100, 1000, 10_000,
   100_000}` chosen to keep total runtime ≥ 10 ms (criterion's noise
   floor). Each opcode has at least two fixtures: a **typical** input
   (median-sized) and an **adversarial** input (maximum size or
   maximum cache-miss pattern). The published gas value is the
   maximum of typical and adversarial slopes.

10. **Two-metric rule.** The published gas value is the maximum of
    `gas_from_criterion` and `gas_from_iai`, with each metric
    independently converted to gas via the G.3 anchor rule (criterion
    via ns directly; iai via an instruction-to-ns conversion
    constant calibrated once on the reference machine and pinned in
    the container).

11. **Cross-machine validation.** Each release runs the same
    calibration suite on at least one secondary machine (Apple
    Silicon or AMD Zen — selected to maximize μarch difference from
    the reference). The ratio `gas_secondary[op] / gas_reference[op]`
    is computed per opcode. Any ratio drift > 2.0 flags the opcode
    for worst-case re-pricing in a follow-up commit before the
    release ships.

12. **Gas table.** Lives at `flamevm/gas_table.toml`. Generated by
    `scripts/recalibrate.sh`. Committed to the repo and consumed by
    the VM at startup. Format:

    ```toml
    [opcodes.add_int253]
    base = 12
    # no slope — fixed cost

    [opcodes.dict_get]
    base = 24
    slope_dict_size = 3      # gas per dict entry

    [opcodes.hash_sha3]
    base = 80
    slope_input_bytes = 6    # gas per input byte
    ```

13. **Cadence.**
    - **Per PR (CI):** iai-callgrind regression gate. Fail the build
      if any opcode's instruction count moves by more than 30% from
      the last committed gas table.
    - **Per release:** full criterion recalibration on the reference
      machine; secondary-machine cross-validation; gas table
      regenerated; release notes include the diff. Any per-opcode
      change > 50% requires an inline note in release commentary.
    - **Per Rust toolchain or VM-architecture bump:** treat as a
      release (codegen changes invalidate prior numbers).

14. **VM modifications required.** The calibration harness needs:
    - A `pub(crate)` `Vm::execute_n(opcode, n_iterations)` entry
      point that bypasses tx-level setup.
    - Pre-built fixture builders for canonical inputs of each opcode
      (median-sized Int253, dict of N entries, max-length string,
      etc.).
    - A `#[cfg(feature = "bench-no-metering")]` flag that compiles
      out gas accounting itself (otherwise we measure the meter, not
      the opcode).
    - A `Vm::reset_for_bench()` that rewinds stack and transcripts
      without re-paying construction cost.

## Consequences

- **Positive: methodology pinned, gas table reproducible.** Any
  contributor with AWS access and the container tag can reproduce the
  reference numbers bit-for-bit (modulo cloud-instance steal time,
  which the take-median rule absorbs).
- **Positive: DoS surface bounded by the worst-case fixture rule.**
  Every opcode in the gas table has a published worst-case input on
  which it was measured; adversarial scripts cannot exceed that
  per-opcode cost without exceeding the published gas charge.
- **Positive: CI catches per-PR regressions deterministically.**
  iai-callgrind has zero noise — a 30% instruction-count jump is a
  real signal, not a flake.
- **Positive: cross-machine ratio drift is measured, not assumed.**
  The release-time secondary-machine run is the empirical check on
  the "ratios are portable" assumption. We do not have to *believe*
  it; we have data each release.
- **Positive: single lane keeps wallet UX simple.** One number per
  transaction, one block budget. Defers protocol-surface growth until
  measurement justifies it.
- **Negative: AWS dependency.** If `c7i.large` is deprecated, the
  reference must migrate via a follow-up ADR that re-anchors the gas
  table on the successor instance type. Expected ~3-yearly cadence
  given AWS EC2 history.
- **Negative: recalibration burden.** Every Rust toolchain bump is a
  full release-equivalent recalibration. Acceptable — toolchain bumps
  are already big events for a Rust-only codebase.
- **Negative: published gas table is conservative.** The take-worse-
  of-two-metrics + worst-case-fixture posture systematically prices
  above true average cost. Users overpay on typical inputs; the
  alternative (under-pricing the adversarial case) is a hard fork to
  fix. Conservative is correct here.
- **Negative: single-lane v0 may misprice I/O-dominant opcodes.** If
  Cell `load` turns out to cost much more in bytes-touched than in
  ns, a single-lane charge either over-prices small loads or
  under-prices large ones. The follow-up ADR for lane splitting is
  pre-scoped: it requires measurement data from this framework, so
  the framework lands first.
- **Follow-up work required:**
  - VM Engineer: implement the four VM modifications in (14) under a
    `flamevm/benches/` skeleton with `add_int253` as the first
    end-to-end measured opcode.
  - VM Engineer: write `scripts/recalibrate.sh` that runs the bench
    suite and regenerates `gas_table.toml`.
  - VM Engineer: extend `spec.md` so each opcode row carries a
    placeholder `gas = base + Σ slope_i·n_i` formula (numeric values
    land via recalibration, not by hand).
  - Integrator: provision the AWS `c7i.large` reference instance and
    publish the Podman image tag.
  - VM Auditor: write per-opcode worst-case fixtures alongside the
    typical fixtures, especially for opcodes touching MSM, hashing,
    Bulletproofs, Cell load.
  - Architect (this ADR's author): file a follow-up ADR placeholder
    `**[[adr-future-lane-split]]**` that activates once measurement
    data exists.
- **Affected artifacts:**
  - `design.md` §Block resource pools — add a sentence pointing at
    this ADR for the per-opcode pricing methodology.
  - `flamevm/spec.md` — every opcode row gains a `gas` field; values
    initially TBD, populated by recalibration.
  - `flamevm/design.md` §Resources / Gas limits — add the anchor rule
    and the single-lane decision.
  - `flamevm/gas_table.toml` — new file, generated artifact.
  - `flamevm/benches/calibration/` — new directory.
  - `flamevm/Cargo.toml` — dev-dependencies on `criterion` and
    `iai-callgrind`.
  - `scripts/recalibrate.sh` — new file.
  - CI config — add iai regression gate; add per-release criterion
    job.
  - `flamevm/todo.md` G.1–G.4 — once this ADR is accepted, the
    architect responses point here.

## References

- `flamevm/todo.md` §"Cross-cutting — Gas calibration" — the four
  open questions G.1–G.4 this ADR resolves.
- `design.md` §Block resource pools — the existing `B_par`/`B_ser`
  framework this ADR slots underneath.
- ADR 0002 — Arena memory cap. Pre-existing resource discipline; this
  ADR prices opcode time without touching memory accounting.
- NEAR Runtime Parameter Estimator
  ([docs](https://near.github.io/nearcore/architecture/gas/estimator.html))
  — source of the two-metric rule, byzantine-benchmarking posture,
  and containerized estimator pattern.
- Ethereum EIP-7904 + Gas Cost Estimator Stage IV
  ([report](https://github.com/imapp-pl/gas-cost-estimator/blob/master/docs/report_stage_ii.md))
  — source of the marginal-difference program construction and the
  NNLS regression model for extracting per-opcode cost.
- Substrate `frame-benchmarking`
  ([README](https://github.com/paritytech/substrate/blob/master/frame/benchmarking/README.md))
  — source of the `WeightInfo`-trait pattern (auto-generated gas
  table committed to repo) and the "worst case is the default"
  posture.
- Aptos gas schedule
  ([blog](https://medium.com/aptoslabs/the-making-of-the-aptos-gas-schedule-508d5686a350))
  — source of the on-chain-governance update model and the
  storage-vs-compute lane separation that this ADR defers.
- Sui gas model
  ([blog](https://blog.sui.io/computation-costs-gas-fee-model/)) —
  explicitly *rejected* design (bucketing for parallelism), retained
  as a reminder of when fine-grained per-opcode pricing is the
  *wrong* answer.
