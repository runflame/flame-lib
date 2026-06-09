# ADR 0018 — Actor ABI: separate code from state; in-bytecode dispatch

- **Status:** accepted
- **Date:** 2026-06-08
- **Proposers:** Architect, VM Engineer
- **Deciders:** Architect
- **Supersedes / related:** 0015 (streaming label control flow), 0017 (re-entrancy lock)

## Context

The actor model baked an ABI into the VM/registry: an actor's state was a
`Dict` with a mandated shape `{0x00 → public, 0x01 → private}`, methods
were stored as bytecode `String`s in slots of the public sub-dict, and
dispatch was `resolve_method(state, method) = state[0x00][method]` — a
native, registry-side selector→slot lookup. Two impositions: the VM
dictated **method layout** (selectors route into the dict) and **state
shape** ({public, private}).

The streaming-label control flow (ADR 0015) makes an in-bytecode method
table cheap: a dispatch prologue reads the `method` opcode and `jumpif`s to
the right handler label. That lets us push the ABI out of the VM entirely.

## Decision

An actor is **`(code, state)`**:

- **`code`** — a single bytecode blob, set at deploy and replaced by the
  new **`setcode`** opcode. Dispatch is method-agnostic: `load_code(actor)`
  returns the blob; the blob's prologue dispatches on the `method` opcode.
  No per-method registry slots.
- **`state`** — **any portable `Value`** (not just a `Dict`, and no
  mandated shape). `op_load` / `op_save` move it; the author structures it
  however they like. The `state == None` checkout remains the re-entrancy
  lock (ADR 0017); `code` is read-only-shared during a call.

Deployment keeps **ID-commits-to-code**: `ActorID::Constructor(bytes)` with
`to_hash() = H(bytes)` already binds the id to its constructor blob.

`setcode` is a recorded effect: **`TxEntry::SetCode { actor, code }`**,
symmetric with `ActorSave`. Its merkle leaf commits to
`(actor.to_hash(), code_root(&code))`. An actor's full commitment is thus
state-root (via `ActorSave`) plus code-root (via `SetCode`).

**Upgrade is author policy over a VM mechanism.** The VM provides
`setcode`; who may call it is gated in the actor's own code. Crucially,
**actors authenticate by caller identity** (`callerid`), not signatures —
internal context has no constraint system or signature batch
(`InternalDelegate` panics on both), so a typical upgrade gate is
`require(callerid() == GOV)`, with signature checks living at the external
cell/predicate boundary.

## Options considered

1. **Methods-in-dict (status quo).** Native O(1) dispatch, but bakes ABI +
   state shape into the VM/registry; `setcode` would mean rewriting a
   method map.
2. **Single code blob + in-bytecode dispatch (chosen).** Registry stores
   `(code, state)`; the blob dispatches. Simplest registry, trivial
   `setcode`, author-defined ABI *and* state.
   - Cost: a streaming verifier forward-scans to the target handler label
     (O(code-before-it) per call). Mitigated by caching the code's
     label→position table (immutable between `setcode`s) — a `Vec<usize>`,
     not a `Vec<Instruction>`. Deferred; small contracts don't need it.

## Consequences

- **Positive:** VM imposes neither method layout nor state shape;
  `setcode` is one opcode; `load_code` is method-agnostic; state is any
  `Value`; aligns with the label/jump control flow and the streaming
  `Code::Bytes` execution path.
- **Negative:** in-bytecode dispatch costs a forward-scan on the streaming
  verifier until the label-table cache lands. Existing `save`-rejects-
  non-Dict behavior is gone (`save` now accepts any portable value).
- **Follow-up:** per-code label-table cache; precise gas-on-scan accounting
  (ADR 0015); the high-level method-list API and C-style contract language
  are front-ends that lower to this dispatch.
- **Affected artifacts:** `flamevm/src/{actor,vm,ops,program,tx}.rs`,
  `flamevm/spec.md` §Storage/Actors. Registry trait change is contained to
  flamevm (`MemRegistry` + test `StubRegistry`); no consensus-crate impl
  exists yet — flag to the Consensus Engineer for the persistent registry.

## References

- ADR 0015 (label control flow) — enables the in-bytecode dispatch table.
- ADR 0017 (re-entrancy lock) — `state == None` checkout, unchanged.
- Design discussion (this session): EVM selector dispatcher, CosmWasm
  `ExecuteMsg` routing, Solana `process_instruction` — mechanism-not-policy
  ABI precedents; actors-authenticate-by-identity correction.
