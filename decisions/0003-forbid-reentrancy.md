# ADR 0003 — Forbid actor re-entrancy

- **Status:** accepted
- **Date:** 2026-05-22
- **Proposers:** Architect (promoted from existing design discussion)
- **Deciders:** Architect
- **Supersedes / related:** —

## Context

Re-entrancy is the single most reliable source of value-loss bugs in smart-
contract systems. The DAO incident (Ethereum, 2016) and the Curve "read-only
re-entrancy" class of bugs (multiple incidents, 2022–2023) cost billions of
dollars cumulatively. In every case, a state-mutating function on one actor
invoked another actor, which called back into the original actor before the
first call's invariants were re-established.

EVM's approach is to leave re-entrancy as a permitted control-flow pattern and
require every author to write defensive guards (`ReentrancyGuard`, the
check-effects-interactions pattern). This approach has failed empirically.
Auditors find new re-entrancy bugs in deployed code year after year, including
in code from teams that knew about the pattern.

Flame's actor model gives us an alternative: enforce no re-entrancy structurally
at the VM, and provide an explicit asynchronous primitive (`send`) for cyclic
interaction patterns. This is the same insight Erlang/OTP and Akka rely on at
the application layer — actors do not call each other synchronously into a
loop; they exchange messages.

The pure no-call-stack-cycles rule is too aggressive. Within a single method
on a single actor, recursion is structurally safe (no other actor's invariants
are crossed), and forbidding it would needlessly complicate algorithms that
naturally express themselves recursively (tree walks, divide-and-conquer over
dicts). The boundary that matters is the actor, not the method.

## Options considered

1. **EVM model — re-entrancy allowed; authors guard.**
   - Pros: full flexibility; precedent.
   - Cons: empirically unsafe; the bug class refuses to die; auditors
     are the load-bearing defense.
2. **Forbid all recursion, including intra-method.**
   - Pros: maximum simplicity for the validator.
   - Cons: rules out natural recursive algorithms; pushes authors to
     stack-explicit transforms with no safety benefit (intra-method
     recursion does not cross any actor's invariants).
3. **Forbid re-entry to an actor already on the call stack** (chosen).
   - Pros: structurally eliminates the bug class with a single check
     (is the target actor's ID present on the current call stack?).
     Intra-method recursion remains free; cross-actor mutual recursion
     is re-expressible via async `send`.
   - Cons: some patterns (synchronous callbacks, mutually-recursive
     cross-actor logic) must be rewritten as async sends or as
     explicit argument-passing. This is a feature, not a bug.
4. **Forbid `call` entirely; use only `send`.**
   - Pros: no call stack at all; no re-entrancy by construction.
   - Cons: synchronous return values become impossible; every cross-
     actor query becomes a multi-block round trip; oracle-style
     read-only queries become impractical.

## Decision

An actor cannot be entered via `call` while it already has an unfinished
invocation on the current internal transaction's call stack. The check is
"is the target actor's ID present on the stack?" — a single O(depth) lookup.
A re-entry attempt fails the call deterministically; with default propagation
semantics this aborts the enclosing internal transaction.

Recursion within a single method is unrestricted. The boundary is the actor,
not the method. Cross-actor cyclic patterns must be expressed via asynchronous
`send`, which schedules a separate internal transaction in a later block-
internal step and therefore has no shared call stack with the originating
invocation.

Async `send` between A and B then B and A is not re-entrancy in the technical
sense: each send produces a distinct internal transaction with its own VM,
its own anchor, and its own ordering position. From A's perspective, the
return is observable only when A is next dispatched, by which time any A-side
invariants the original method established are committed to actor state.

## Consequences

- Positive: removes the bug class structurally. The DAO and Curve incidents
  are not expressible in Flame.
- Positive: mid-call invariants ("the state I just wrote with `save` is
  consistent") cannot be observed mid-flight by a re-entering caller, because
  there is no re-entering caller.
- Positive: the validator implementation is one check on the call stack at
  every `call` dispatch — cheap, deterministic, easy to audit.
- Negative: synchronous mutual recursion across actors is not expressible.
  Authors must use async `send` and the explicit anchor / refund model.
- Negative: oracle-pattern callbacks (A calls B, B wants to call back into
  A for a quick lookup) must be re-architected as either (a) A passing
  the needed data into B as arguments, or (b) B emitting an async send
  back to A.
- Follow-up: VM Engineer must implement the call-stack lookup in the
  execution loop and provide an adversarial test for both direct (A → A)
  and indirect (A → B → A) cycles. VM Auditor's `threats/vm.md` §1.1–1.3
  track this.
- Affected artifacts: `design.md` §Concurrency, `flamevm/design.md`
  "Re-entrancy" subsection, `flamevm/spec.md` (`call` semantics),
  `flamevm/CLAUDE.md` Guardrails.

## References

- DAO incident, June 2016 (Ethereum).
- Curve read-only re-entrancy postmortems, 2022–2023.
- `flamevm/design.md` — "Re-entrancy" subsection under "Send vs call".
- `threats/vm.md` §1 — Re-entrancy category.
- Hewitt actor model — semantics that make this rule natural rather than
  restrictive.
