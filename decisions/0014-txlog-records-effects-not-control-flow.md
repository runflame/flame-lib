# ADR 0014 — TxLog records effects, not control flow

- **Status:** accepted
- **Date:** 2026-05-26
- **Proposers:** Architect, VM Engineer
- **Deciders:** Architect
- **Supersedes / related:** related to ADR 0013 (predicate-call isolation) and the call-failure-marker contract introduced alongside per-call anchor splitting

## Context

`TxEntry::Call { callee, method, pre_state_root, callee_anchor }` was emitted by every `op_call` to "bind the callee's identity and pre-call state hash into the internal TxID merkle root". That entry was control-flow attribution, not a state effect. The VM is deterministic given (inputs, registry state, block context), so the call sequence is fully derivable from the script + state — the entry added no information needed by a state machine consuming the TxLog.

Meanwhile actor-state mutations from `op_save` were **not** recorded in the TxLog. A consumer of the TxLog could not derive the new actor state without re-running the script.

The right invariant is: **TxLog = the complete set of structural effects a state machine applies**. Control flow is internal to the VM; effects are external. Three categories cleanly emerge:

1. Structural effects → TxLog → TxID. State derivable from the trusted TxLog alone.
2. Constraint system (R1CS / Bulletproofs) → verified at tx end. Not in TxLog.
3. Batch crypto (MSM, multi-sig batch) → verifier-side perf optimization. Not committed.

Once this framing is named, the asymmetry between `op_call` (emitted a control-flow entry) and `op_save` (emitted nothing) is the bug.

## Options considered

1. **Keep `TxEntry::Call`, add `TxEntry::ActorSave`** — both entries coexist.
   - Pros: minimal disruption; existing tests/audit notes that reference Call entries still apply.
   - Cons: TxLog now mixes control flow and effects; the "TxLog = effects" invariant is still violated; every future opcode requires a fresh judgment call about whether it goes in.
2. **Drop `TxEntry::Call`, add `TxEntry::ActorSave`** — clean separation. *(taken)*
   - Pros: TxLog has exactly one role; "verify by construction" is precise (given a trusted TxLog, the state machine alone can mutate the registry); future opcodes get a clear test.
   - Cons: one-time wire-format change (consensus-breaking); audit notes referencing Call entries need refresh.
3. **Carry the full new `ActorState` in `ActorSave`** (instead of just `post_state_root`).
   - Pros: state machine doesn't need to consult the script to learn the new state.
   - Cons: bloats TxLog by O(state size) per save. Deferred: the registry already holds the post-state; the hash is enough for TxID binding, and registry consumers can read the actor.

## Decision

Remove `TxEntry::Call`. Add `TxEntry::ActorSave { actor: ActorID, post_state_root: [u8; 32] }`, emitted by `op_save` after the registry write succeeds. The TxLog enum is otherwise unchanged.

The `(External TxID, Internal TxID)` merkle roots are unchanged in shape — still a root over the ordered TxEntry list — but their byte contents change because the entry set changed. This is a one-time consensus break.

## Consequences

- Positive:
  - TxLog is now exactly the set of effects a state machine replays. No control-flow attribution noise.
  - `op_save` is now visibly observable in the TxLog — auditors can scan for it without inferring it from the absence of an unmatched `load`.
  - Future opcodes get a sharp test: "does this produce a structural effect a state machine must apply? Then it emits a TxEntry. Otherwise it doesn't."
  - Aligns with the call-failure-marker model: control-flow failures (reentrancy, ActorNotFound) leave no log trace, which is what the state machine wants — failures emit no effects to apply.
- Negative:
  - One-time wire change; any external systems pinned to the old TxLog shape need updating.
  - Tests that asserted `TxEntry::Call` shape were repurposed (small mechanical pass).
- Follow-up work required:
  - Possibly add ADR for per-frame batch merging under call failure (related but separable; design.md and spec.md note it inline for now).
  - Open: CS effects under call failure (deferred — see design discussion).
- Affected artifacts: `design.md` §"TxLog records effects, not control flow", `flamevm/spec.md` §call + §save + §send (clarifications), `flamevm/src/tx.rs` (enum), `flamevm/src/vm.rs` (`op_call`, `op_save`), test suite.

## References

- Design discussion 2026-05-26 (this session) — three-category effects framing.
- Related: ADR 0003 (no re-entrancy), ADR 0013 (predicate-call isolation), call-failure-marker contract.
