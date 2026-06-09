# ADR 0013 — Predicate-bound execution is always isolated via a call frame

- **Status:** proposed
- **Date:** 2026-05-25
- **Proposers:** Architect (prompted by VM Engineer cell-open trust-model review)
- **Deciders:** Architect
- **Supersedes / related:** ADR 0008 (Taproot construction); related to ADR 0003 (re-entrancy)

## Context

`spec.md` originally specified `open` and `signrun` as **Run-level**:
the unlocked / signed script runs inside the caller's existing
`CallFrame`, sharing its stack, gas, memory cap, identity, and
control-flow scope. zkvm's `call` opcode is shaped the same way.

This is safe in an external transaction (the tx author chose the
cell and the predicate). It is **not** safe in an internal
transaction. When an actor's method opens a cell whose predicate
was supplied by an untrusted caller, the cell's script runs *as
the actor* — with `op_load`/`op_save`/`op_call`/`op_send`
authority. A malicious cell predicate inserted into a method
argument is a confused-deputy attack on the host actor.

`spec.md` flagged this in a discussion note: "Future review:
tighter sandboxing for cell-opens in internal context may be
desirable." The cost of every actor method auditing the
predicate-as-authorization-filter is high; the structural fix is
to isolate.

The fix can apply only inside the internal context, or
everywhere. The latter is simpler (one rule, no
context-conditional behavior), composes cleanly with the
existing `op_call` machinery (already isolated), and matches the
mental model script authors get from EVM-style platforms.

## Options considered

1. **Simple run everywhere (status quo / zkvm parity)** —
   `open` and `signrun` stay Run-level.
   - Pros: matches zkvm; cheap; free composition (payload pours
     onto caller's stack).
   - Cons: confused-deputy in actor context; gas/memory leak;
     `op_anchor`/`op_actorid` semantically muddy; spec carries
     "tighter sandboxing" footnote indefinitely.

2. **Isolated call everywhere (this ADR)** — `open`, `signcall`
   (renamed from `signrun`), and `call` all create a new
   `CallFrame`. Caller specifies gas + bytes allotment; results
   return via `return k`.
   - Pros: confused-deputy eliminated by construction; gas/mem
     bounded by caller; identity/anchor semantics clean; one
     mechanism for all three call-creating opcodes; mirrors EVM
     mental model; existing `op_call` machinery reused.
   - Cons: +~100 LOC in vm.rs; existing scripts using cell-open
     need explicit `gas`/`bytes` operands and `return k`;
     diverges from zkvm pattern (one opcode shape, not three).

3. **Context-conditional** — `op_open` from `ExternalRoot` →
   Run-level; from internal → isolated.
   - Cons: same opcode behaves differently based on caller
     context, which is harder to reason about than uniform
     isolation. Rejected.

4. **Two opcodes** — `open` (Run-level) plus
   `open_sandboxed` (isolated).
   - Cons: doubles the opcode surface; authors must remember
     which is safe; uniform isolation is simpler.

## Decision

**Execution of a program under a predicate is always isolated via
a call frame.** This applies to:

- `open` — taproot-revealed cell script (external + internal).
- `signcall` — cell-holder-signed script bound to TxID
  (external + internal); renamed from `signrun`.
- `call` — synchronous actor-to-actor invocation (internal only).

All three share the existing `CallFrame` machinery:

- New frame with its own stack, gas budget, memory cap, identity,
  and control-flow scope.
- Caller specifies `gas` and `bytes` allotments (Int253 operands).
  Passing `remaining_gas` lends the full caller budget; explicit
  smaller values cap the callee.
- Cell-open frames (`CallKind::CellOpen { anchor, predicate }`)
  have **no actor identity** — `op_load`/`op_save`/`op_call`/
  `op_send` all error from inside.
- Cell-open frames inherit the host's external/internal context
  for CS access: under `ExternalRoot` the cell script may use
  Bulletproofs opcodes; under `InternalRoot`/`ActorCall` it
  cannot (existing rule preserved).
- Results return via explicit `return k`; refunds unused gas to
  the parent.

## Consequences

**Positive:**
- Confused-deputy class of bugs is structurally impossible for
  actor methods that open cells. The spec footnote about
  "tighter sandboxing" is resolved.
- One mental model for the three call-creating opcodes. Author
  documentation simplifies.
- `CallKind::CellOpen` finally pulls its weight (previously
  vestigial).

**Negative:**
- Existing scripts using `open`/`signrun` need migration: push
  `gas` and `bytes` operands; add `return k` to opened scripts.
- Existing `test_cells.rs` and `test_authorization.rs` tests
  need rewrites to match the new operand shape.
- Diverges from zkvm's flat-VM model: zkvm has no `CallFrame`
  at all because it has no actors. flamevm needs frames anyway
  (for actor calls), so the cost of adding them to cell-open is
  marginal.

**Follow-up work required:**
- Phase 40 implementation per `flamevm/plan.md`:
  - Rename `signrun` → `signcall` everywhere; transcript label
    `flamevm.signrun` → `flamevm.signcall` (consensus-fixed).
  - Reshape `op_open` and `op_signcall` to create `CellOpen`
    frames; consume `gas` + `bytes` operands; return results.
  - `CallKind::CellOpen` accessor semantics: `actor()` /
    `method()` / `caller()` all return `None`.
  - Block `op_load` / `op_save` / `op_call` / `op_send` from
    inside `CellOpen` frames.
- Test rewrite for cell-open / signcall sites (~12 new tests
  + retargeting existing ones).

**Affected artifacts:**
- `design.md` §Calls and isolation.
- `flamevm/spec.md` rows `0x93 open`, `0x95 call`, `0x99 signcall`.
- `flamevm/src/vm.rs` (op_open, op_signcall, dispatch).
- `flamevm/src/ops.rs` (Instruction::Signcall rename).
- `flamevm/src/tests/test_cells.rs`, `test_authorization.rs`
  (test rewrites).

## References

- spec.md "Cell-open trust model" footnote (now superseded).
- zkvm's `call` opcode (the rejected zkvm-parity alternative).
- Conversation log: cell-open isolation comparison (simple run
  vs isolated call) and the user's decision to adopt uniform
  isolation.
