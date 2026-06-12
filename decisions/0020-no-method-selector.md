# ADR 0020 — No VM-level method selector: calls deliver N args as-is

- **Status:** accepted
- **Date:** 2026-06-13
- **Proposers:** Architect
- **Deciders:** Architect
- **Supersedes / related:** completes ADR 0018 (actor code/state split).

## Context

ADR 0018 moved method dispatch out of the VM and into contract code,
yet the VM still privileged one `Int253` as "the selector": a dedicated
operand on `send`/`call`, a field in `Message` (hashed into SendID), a
slot in `CallKind`, and a `method` opcode to read it. Mechanism and
policy at once — the last vestige of the deleted VM-level ABI. A
contract with a single action has no use for a method name at all.

## Decision

Remove the selector from the VM. `send` and `call` deliver their `N`
payload values as-is:

```
send:  args… k refund gas bytes addr → ø
call:  args… k gas bytes addr → results… k' {1|0}
```

Deleted: `Message.method` (+ its slot in the wire encoding / SendID),
the `method` operands, `CallKind::{InternalRoot,ActorCall}.method`, the
`method` opcode (byte `0xe3` freed), and the `RECV_METHOD` constant.

**Convention (non-normative):** a multi-action contract reads its
selector from the **top-of-stack argument** — the dispatch prologue is
`push sel; eq; jumpif …` directly on it. Selectors may be any
comparable value, including `String` names. Single-action contracts
consume args directly with zero dispatch overhead.

## Consequences

- Positive: one less operand, field, frame slot, and opcode; typed
  (string) selectors free; an empty send (`k = 0`) is a pure vbyte
  transfer with no reserved-method residue; bounce returns the complete
  call (selector included, since it is payload).
- Negative: wire/SendID change (pre-launch); tooling cannot assume a
  standardized selector slot — the convention lives in the authoring
  layer (method-list builder API).
- Affected artifacts: `flamevm/src/{send,vm,ops,program,actor,tx}.rs`,
  spec §send/§call/§Actors (the §method section is deleted), golden
  vectors regenerated.
