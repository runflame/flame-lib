# Flame design overview

This document is the canonical, project-wide design reference. It is intentionally
brief: it captures the architectural choices that other roles must respect, points
at the ADRs that justify each choice, and defers detail to the specialized
documents that the relevant engineer owns.

When the design changes, this document and a new ADR change together. Other
documents (the VM spec, consensus crate docs, the threat models) follow.

## Scope

Flame is a UTXO-and-actor blockchain that runs alongside a Bitcoin node. The
network state has two substrates:

- A **Utreexo accumulator** of unspent transaction outputs. Mutated by external
  transactions that consume and produce cells (see ADR 0001).
- An **actor registry** of long-living, multi-user stateful entities. Mutated by
  internal transactions that deliver messages and invoke methods.

A single stack VM — **FlameVM** — verifies both transaction types. External
transactions run in an external context with access to Utreexo and Bulletproofs;
internal transactions run in an internal context with access to actor state. See
`flamevm/design.md` for the type system and operation taxonomy and
`flamevm/spec.md` for the canonical instruction set.

## Architectural commitments

The following commitments are load-bearing across the system. They cannot be
revised without an ADR. Other roles must not work around them locally.

### Linear types and bearer values

- Tokens, cells, objects (constraint-system handles), variables, expressions,
  constraints, Merlin transcripts, and MultiscalarMul are **non-copyable** and
  **non-droppable** (per their type discipline; see `flamevm/design.md`).
- Bearer tokens cannot be made copyable by any opcode, ADR, or implementation
  shortcut. This is non-negotiable.

### No re-entrancy

- An actor cannot be entered via `call` while it already has an unfinished
  invocation on the current internal transaction's call stack. See ADR 0003.
- Intra-method recursion is permitted; the boundary is the actor, not the method.
- Cyclic interaction patterns are expressed via asynchronous `send`.

### One memory cap, one fee, one storage unit

- Transient memory per call is capped at 4× the actor's persistent vbyte size.
  No per-call memory grants. See ADR 0002.
- A single external-transaction fee covers (a) the external script's gas and
  (b) all gas allotments forwarded to message sends. See `flamevm/design.md`
  §Resources / Gas.
- Persistent storage is metered per-vbyte with no quantization. See ADR 0004.

### Wire format: little-endian everywhere

- All multi-byte fixed-width integers in serialized form (wire format,
  on-disk format, hash-transcript inputs, VM data types) are encoded in
  little-endian byte order. This includes `u8`/`u16`/`u32`/`u64`
  width fields, `Int253` magnitudes, and length prefixes / domain tags /
  integer parameters appended to cryptographic transcripts.
- The varint and sub-varint families (`flamevm/src/encoding.rs`) remain
  low-order-byte-first by construction; no exception.
- Cryptographic primitives whose outputs are conventionally rendered
  big-endian (e.g., SHA-256's length field) are consumed as opaque byte
  strings or re-encoded at the boundary; no Flame-level field is BE.
- Every new decoder is accompanied by a decode-then-re-encode-then-
  bit-compare canonicality test. See ADR 0006.

### Cells and actors

- The single-use payload container materialized from an output is a **cell**.
- The long-living stateful entity holding methods is an **actor**.
- These names are load-bearing across all artifacts. See ADR 0001.
- A cell's spend authority is gated by a `Predicate` (Key / Program /
  Tree). The Tree variant uses a Taproot-compressed construction
  `P = X + H(X, M)·B` with per-program blinding leaves, a
  Pedersen-secondary-generator NUMS internal-key default, four
  Flame-specific Merlin transcript domains, and a `2 × programs.len()`
  leaf count. All transcript labels and domain separators are
  consensus-fixed; any rename is a hard fork. See ADR 0008.

### Calls and isolation

- **Execution of a program under a predicate is always isolated via a
  call frame.** This applies to taproot-revealed cell-open scripts
  (`open`), signed cell-bound scripts (`signcall`), and synchronous
  actor-to-actor invocations (`call`). All three opcodes create a new
  `CallFrame` with its own stack, gas budget, transient-memory cap,
  identity, and control-flow scope. Results cross the boundary only via
  the explicit `return k` opcode; failure aborts to the parent frame.
- Gas allotment for a call is chosen by the caller: pass an explicit
  limit, or pass `remaining_gas` to lend the full caller budget.
  Unused gas refunds to the parent on clean return. Memory is allotted
  the same way; both are bounded by the parent's remaining resources.
- The three call-creating opcodes share one mechanism; their
  differences are dispatch (who the callee is) and authentication
  (taproot proof, signature, or actor-id):
  - `open` — taproot-revealed cell script (external + internal).
  - `signcall` — cell-holder-signed script bound to TxID (external + internal).
  - `call` — synchronous method invocation on another actor (internal only).
- Isolation eliminates the confused-deputy class of bugs by
  construction: an unlocked cell script cannot reach the host's
  actor state or impersonate its identity. Authors of methods that
  accept cells from untrusted callers do not need to audit the
  cell's predicate as a global authorization filter — the call frame
  *is* the sandbox.

### Atomic external-transaction effects

- An external transaction either commits all of its effects (input, output,
  send, fee, issuance, retirement, data) or none. Failure produces no state
  change and deducts no fee. This makes external-transaction outcomes
  fully deterministic at signing time, which in turn permits parallel
  validation across the block.

### TxLog records effects, not control flow

- The transaction log is a list of structural **effects** — entries
  that a thin state machine can apply to mutate UTXO and actor state
  without re-running the VM. The canonical categories are: cell
  lifecycle (`Input`, `Output`), supply (`Issue`, `Retire`),
  actor-state (`ActorSave`), fees (`Fee`), messages (`Send`), and
  audit (`Data`, `Header`).
- Control flow — `call`, `open`, `signcall`, `return`, branching —
  produces no txlog entries. The decisions a callee makes are visible
  only through the effects it emits.
- Three categories of work the VM does, listed by where they end up:
  1. **Structural effects** → TxLog → TxID. The VM's job is to
     produce a TxLog the state machine can replay; given a trusted
     TxLog, blockchain state is derivable without the VM.
  2. **Constraint system** (R1CS / Bulletproofs) → verified once at
     tx end; not part of the TxLog.
  3. **Batch crypto** (MSM, multi-signature batching) → pure
     verifier-side optimization; not committed anywhere on chain.
- This separation is load-bearing for every new opcode: ask first
  whether the opcode produces a structural effect (write a TxEntry),
  a CS effect (append to the CS), or merely orchestrates other ops
  (no entry, no CS effect). See `flamevm/spec.md` and the per-opcode
  txlog notes for the canonical mapping.

### Batch rollback under call failure

- The verifier-side batch (Schnorr / MuSig signatures + MSM Sigma-
  protocol assertions) is a single per-tx accumulator owned by the
  delegate. Its RNG is supplied at construction (no
  `thread_rng()` calls inside the VM core). At every call entry
  (`call` / `open` / `signcall`) the VM takes a `BatchCheckpoint::
  snapshot` of the accumulator and stores it on the parent frame.
  On call failure, `fail_current_call` calls
  `BatchCheckpoint::restore` to truncate the accumulator back to
  its pre-call state — any MSM the failed callee appended is
  dropped from the batch, so the caller's proof verifies. On clean
  return the snapshot is discarded. Composition mirrors the
  existing rollback of `TxLog`, `deferred_sigs`, and `total_fee`,
  which use the same snapshot/truncate pattern.

### CS rollback under call failure

- The Bulletproofs R1CS constraint system is also rolled back via
  the same snapshot-on-entry, restore-on-failure pattern, using
  `bulletproofs::r1cs::CheckpointableConstraintSystem::checkpoint`
  and `rollback`. At every call entry the VM captures a `Checkpoint`
  (a fixed-size record: `n_constraints`, `n_multipliers`,
  `n_committed`, `n_deferred`, `pending_multiplier` flag, plus a
  cloned Merlin transcript — ~208 bytes) and stores it on the parent
  frame. On call failure, `fail_current_call` calls `rollback` which
  truncates the witness vectors (`a_L`, `a_R`, `a_O`, `v`,
  `v_blinding`) and the constraint vectors back to the recorded
  lengths in place, zeroizes the dropped witness slots, restores the
  pending-multiplier flag, and replaces the transcript with the
  saved clone. On clean return the snapshot is dropped (the child's
  CS contributions stay). Both Prover and Verifier walk the same
  script and therefore checkpoint/rollback at the same sites — their
  CS state and transcript stay in lockstep across the failure
  boundary. With CS rollback, an unsatisfiable constraint introduced
  by a failed `open` / `call` / `signcall` is dropped from the proof,
  so the caller's R1CS verifies as if the call had never happened.

### Internal-transaction grace and freeze

- When an actor's vbyte balance reaches zero it freezes (calls rejected, state
  preserved). The grace window is `min(active_blocks/4, blocks_per_6_months)`.
  A top-up during grace unfreezes and resets the active-blocks counter.
  Elapse without top-up clears the actor; vbytes return to the pool after a
  100-block maturity. See ADR 0005.

### TxID binding

- Every transaction's identifier is a merkle root over its ordered effects
  list. Signatures and ZK proofs bind to TxID. No effect can be added,
  removed, or altered without invalidating the binding. Anchors for message
  sends derive from TxID; anchor uniqueness is the replay defense for
  internal transactions.

### Concurrency

- External transactions are verified in parallel (each binds only to its own
  set of consumed UTXOs and to the immutable confirmed state).
- Internal transactions are verified serially within a block (they share the
  actor registry). The serial order is fixed by the order of their
  originating external transactions.

### Block resource pools

- Per-block parallel gas pool `B_par` and per-block serial gas pool `B_ser`,
  initially with ratio `B_par : B_ser = 4 : 1`. Sub-sends emitted by internal
  transactions consume their originator's already-allotted budget — they do
  not add new charges to `B_ser`. Both caps adjustable by supermajority soft
  fork. See `flamevm/design.md` §Resources / Gas limits.
- Per-block vbyte introduction rate 5000, adjustable up to 2× per cycle by
  supermajority. Recycled vbytes rejoin the pool after 100 blocks.

### Bitcoin coupling

- The node runs as a dual-node alongside a Bitcoin Core companion. Bitcoin
  reorgs deeper than the maturity window are out of scope for this design
  and must be surfaced by Integrator as an operational failure mode.

### Chain-state introspection (forward-looking)

Not yet implemented. The VM will eventually expose a small read-only
window onto the surrounding chain state — both Flame's own block context
and the coupled Bitcoin chain — so scripts can express height-, hash-,
burn-, or weight-conditioned logic. A 100-block maturity guard caps how
fresh the visible state can be, so Bitcoin reorgs shallower than that
window never cause Flame execution to fork.

Tentative opcode shortlist (subject to ADR):

| Opcode | Push | Sketch |
|---|---|---|
| `height` | `n` | Current Flame block height. |
| `blockhash` | `h₃₂` | Block hash at a queried height (with maturity guard). |
| `blockburn` | `n` | Bitcoin sats burned at a queried height. |
| `blockweight` | `n` | Bitcoin block weight at a queried height. |
| `blockrate` | `n` | Smoothed difficulty / hashrate proxy. |
| `chainstate` | `Dict` | Aggregate chain stats (composition TBD). |

Each height-parameterized opcode hard-fails `BlockHeightImmature` when
the queried height is within the last 100 blocks. Wiring the read path
needs a `BlockContext` plumbed from the consensus crate into the VM; the
exact set, encoding, and gas charge are open questions deferred until
the consensus / dual-node surface stabilises.

## Cross-role artifact map

| Concern                                  | Owner            | Canonical document          |
|------------------------------------------|------------------|-----------------------------|
| Design overview (this document)          | Architect        | `design.md`                 |
| ADRs                                     | Architect        | `decisions/NNNN-*.md`       |
| VM type system, operation taxonomy       | VM Engineer      | `flamevm/design.md`         |
| VM instruction set, encoding             | VM Engineer      | `flamevm/spec.md`           |
| VM threat model                          | VM Auditor       | `threats/vm.md`             |
| VM audit reports                         | VM Auditor       | `audits/vm/YYYY-MM-DD-*.md` |
| Consensus state machine                  | Consensus Eng.   | `consensus/` (crate docs)   |
| Consensus threat model                   | Consensus Aud.   | `threats/consensus.md`      |
| Consensus audit reports                  | Consensus Aud.   | `audits/consensus/`         |
| Node binary, dual-node ops, runbooks     | Integrator       | `node/`, `integration/`     |
| User-facing surfaces, terminology in UI  | UI Designer      | `ui/`                       |

The VM Engineer's `flamevm/design.md` is the deepest currently-written
reference for VM semantics; this document is the higher-level view that the
non-VM roles should anchor on. Where the two say different things, file a
feedback note rather than guessing.

## Open structural questions

These items are tracked by the auditors and engineers but remain unspecified
at the architectural level. Each will be promoted to an ADR when answered.

- **BFT family.** Which family of BFT consensus do we implement (HotStuff-like,
  Tendermint-like, Narwhal-style mempool with separate consensus)? Tracked
  in `threats/consensus.md` "Open structural questions".
- **Stake / slashing model.** Or non-staking, fee-based minter selection?
  Tracked in `threats/consensus.md`.
- **Finality gadget.** Probabilistic vs absolute finality; weak-subjectivity
  policy; long-range defense. Tracked in `threats/consensus.md`.
- **Validator-set rotation cadence.** Tracked in `threats/consensus.md`.
- **Extension tag (255) forward-compat policy.** Reject vs reserve for
  soft-fork. Tracked in `threats/vm.md` §3.6.
- **Soft 8× internal-gas multiplier.** A validator-side fee-market convention
  has been discussed but not adopted. Tracked in both threat models
  (`threats/consensus.md` §10.2, `threats/vm.md` §10.3).
- **Frontend framework.** UI Designer's bootstrap requires an ADR locking
  in the framework choice. Tracked in `ui/CLAUDE.md`.

When any of the above is resolved, a new ADR replaces the bullet here with
a cross-reference.

## Process

- Engineers raise design questions via `questions/YYYY-MM-DD-<role>-to-architect-<topic>.md`.
  Architect answers in-place and updates this document or files an ADR.
- Auditors file findings via dated audit reports plus per-finding feedback
  notes. Every high/critical finding is acknowledged within the cycle —
  either by accepting (this document or an ADR changes), mitigating
  (recorded here with a pointer to the mitigation), or accepting the risk
  (an ADR with rationale).
- Cross-role design conflicts are adjudicated via ADR.

## ADR index

- ADR 0001 — Rename Object → Cell and Contract → Actor.
- ADR 0002 — Transient memory cap = 4× persistent vbytes.
- ADR 0003 — Forbid actor re-entrancy.
- ADR 0004 — Arbitrary per-vbyte actor sizing (no power-of-two arenas).
- ADR 0005 — Grace period = active_blocks / 4, capped at six months.
- ADR 0006 — Little-endian everywhere (wire, on-disk, transcript, VM).
- ADR 0007 — `readbits` / `readint` / `writeint` opcode set.
- ADR 0008 — Taproot-compressed predicate construction (NUMS default,
  blinding leaves, transcript domains, tree shape, tweak hash).
- ADR 0009 — Gas calibration strategy.
- ADR 0013 — Predicate-bound execution is always isolated via a call
  frame (`open` / `signcall` / `call` share one mechanism); `signrun`
  renamed to `signcall`. *(Pending backfill; numbers 0010–0012 reserved
  for the actor-build backfill — see `flamevm/plan.md` Phase 38.)*
- ADR 0014 — TxLog records effects, not control flow. Drop `TxEntry::Call`,
  add `TxEntry::ActorSave`. Three-category framing (structural / CS /
  batch crypto).
