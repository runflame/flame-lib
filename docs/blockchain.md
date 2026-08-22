# Flame blockchain state machine

## Scope and trust boundary

`flamechain` is the deterministic state machine around FlameVM. Given a prior
state, consensus parameters, an authenticated core-block context, and a
candidate Flame block, it either produces one new state plus complete undo data
or rejects the block without changing state.

It owns cells and their accumulator, actors, storage leases, block resource
accounting, block application, local reorganization, and basic transaction
admission. It does **not** choose a Bitcoin branch, talk to Bitcoin Core, select
minters, run BFT, persist data, or provide networking. Those responsibilities
remain outside the state machine:

| Component | Responsibility |
| --- | --- |
| `flamevm` | Verify external scripts and execute actor messages under an explicit context. |
| `flamechain` | Apply deterministic Flame state transitions and make them reversible. |
| `btc-integration` | Observe and validate Bitcoin data, then supply authenticated core-block context and branch changes. |
| consensus / node | Choose the accepted branch, authenticate context, store blocks and undo, and call detach/attach operations. |

The block header commits an opaque `core_block_hash`, while FlameVM receives the
corresponding height as a plain integer rather than a Bitcoin RPC object. This
split makes the state machine reproducible and easy to test, but creates an
important trust boundary: `flamechain` cannot by itself prove that a supplied
Bitcoin hash or height is genuine or that two supplied hashes are parent and
child. The caller must authenticate that context according to
[consensus.md](consensus.md).

This document is the architectural target, not a claim that every wire format,
commitment, or persistence path is implemented. Open launch requirements are
called out explicitly below.

## Chain state

At a selected tip the persistent state contains:

- tip height and block identifier;
- the Utreexo forest committing to every live cell;
- the actor registry: actor code, state, and leases;
- the available actor-storage pool and a lease-expiry index derived from the
  committed per-actor leases; and
- any consensus resource counters that affect later validation.

Mempool contents, proof caches, Bitcoin RPC state, and transient VM frames are
not consensus state.

All state arithmetic is checked integer arithmetic. A failed check, invalid
proof, resource-limit breach, commitment mismatch, or VM failure required by
the transaction rules rejects the candidate transition. Applying a block is
atomic: no persistent mutation becomes visible until all checks pass.

## Cells and Utreexo

Cells are single-use values. An external transaction proves each input's
membership in the parent accumulator, deletes it once, and inserts its outputs.
The Flame-specific Utreexo implementation represents the accumulator as a
forest of perfect binary Merkle trees:

- `Forest` is the committed compact state;
- `WorkForest` stages insertions and deletions without mutating `Forest`;
- a `Proof` identifies a transient item or supplies a committed Merkle path;
- normalization produces the next canonical `Forest`; and
- `Catchup` updates surviving proofs across that transition.

Committed inputs prove membership in the declared parent root and no cell may
be deleted twice. A `Transient` proof intentionally permits a later transaction
in the same ordered block to spend an output created earlier in that block; the
same rule lets FIFO mempool children depend on admitted parents. The work forest
is normalized once after the ordered batch, and its resulting commitment must
equal the block's committed cell root.

This lets a validating node keep a small accumulator instead of every cell
payload. It does not make cells literally free or solve data availability:
owners must retain payloads and proofs, proofs need updating, blocks still carry
witness data, and archival history remains a separate cost.

## Actors and leased storage

Actors are reusable code-and-state records and therefore remain in the common
validated state. Their capacity, pricing, expiry, destruction, and opcodes are
defined in [storage.md](storage.md). `flamechain` owns this state; FlameVM reaches
it only through the actor-registry interface and an explicit block context.

At the beginning of core block `h`, before user execution, the state machine:

1. expires leases at `h` and returns their units to the pool;
2. marks newly under-capacity actors for destruction and prevents their
   execution in this block; and
3. issues the configured storage units for `h`.

Storage purchases execute serially in transaction order. Each successful
purchase immediately changes the reserve used by the next quote. At block end,
marked actors are destroyed in lexicographic actor-ID order and each destruction
is represented by a derived system internal transaction. Unexpired leases of a
destroyed actor remain locked until their original expiry.

Serial pricing is deterministic and maintains a hard storage bound, but ordering
has economic value: a proposer can place one valid purchase before another.
Burning storage fees prevents the proposer from directly recovering the fee; it
does not remove ordering advantage or general MEV. The specified endpoint quote
also makes split purchases cheaper than a single bulk purchase; this is an
explicit harmonic marginal-price policy, not a sybil-resistant bulk premium.

## Blocks and commitments

A block has a header and an ordered body of external transactions. Internal
transactions are execution records derived by the state machine; they are not
independently submitted or selected by a proposer.

The in-memory header commits, with domain-separated hashes, to:

- protocol/network version, parent, and height;
- the authenticated core-block context;
- the exact ordered external-transaction witness;
- the resulting cell-accumulator commitment;
- the resulting actor/storage-state commitment; and
- consensus resource totals needed to validate the transition.

The exact wire encoding of the header and external transaction is **TBD**. The
current hash preimages and actor-state commitment must be frozen with additional
conformance vectors before consensus launch. In particular, committing only to
effect logs is insufficient when signatures or proofs bind to an external
transaction witness.

The first implementation may use an in-memory `BlockBody` and locally verified
effects while canonical `ExternalTx` transport is being finalized. Such values
are an internal API, not a consensus wire block: effects received across a trust
boundary must never be accepted instead of re-executing the transaction, and an
exact witness hash is required before launch.

## Staged block application

Block application follows one shared validation path for normal extension and
reorganization attachment:

1. Check parent, height, version, context relation, static limits, and canonical
   encodings that are already defined.
2. Stage lease expiry, recycling, actor marking, and issuance.
3. Verify external transactions against the declared parent cell state. Reject
   missing or duplicate inputs and accumulate their resource use.
4. Apply external effects in block order and enqueue their sends in effect
   order.
5. Execute the message queue serially. A synchronous `call` remains inside its
   current internal transaction; `send` effects append new messages to the
   queue in emission order. Successful effects update the staged state. A failed
   delivery rolls its actor changes back and deterministically creates one
   refund cell containing the original portable payload.
6. Derive storage-purchase records and other internal records from execution;
   never trust proposer-supplied versions.
7. Derive the ordered actor-destruction records and apply them.
8. Normalize Utreexo, recompute all state commitments and resource totals, and
   compare them with the header.
9. Commit the staged state and retain complete undo data.

The message discipline is FIFO: external transactions seed the queue in block
and send-effect order, and sends made by an internal transaction append in that
transaction's effect order. This is simple and deterministic, but it makes a
long send chain serial and lets early transactions influence all later actor and
storage outcomes. Gas, message-count, call-depth, and block limits must bound
that work.

No duplicate-message table is part of consensus. Every accepted `Send` gets its
anchor by splitting the VM's current ratchet: the left child enters the Message
and the right child continues execution. Ratchet roots come from a spent input
CellID or the unique anchor of the delivering Message, and synchronous calls
split disjoint child and caller-continuation subtrees. The queue is derived once
from each executed `Send` effect and fully drained before commit. A
reorganization may execute a message again only after rolling back its former
delivery or refund.

Delivery and recovery share the block's outer atomic checkpoint. Actor changes
from a failed delivery are rolled back before the original payload is sealed in
one refund Cell under `refund_predicate`. If constructing or applying that Cell
fails, the whole candidate is rejected, including the Send that originated the
message; a failed candidate never commits consumption without recovery.

## Derived internal transactions

An internal transaction is a receipt of deterministic execution, not another
consensus input. It starts from a `Send`, records its `Receive`, runs the target
actor and synchronous calls, and records the resulting ordered effects. A
lease-expiry destruction is a system internal transaction with
`ActorDestroy(actor)` instead of `Receive`; it recursively records token
retirements and binds the expiry height. A failed delivery records `Receive` and
exactly one refund `Output`; the Cell uses the left split of the message anchor,
the same unique slot the delivery's first successful output would have used.
Its execution-record kind distinguishes failure from success.

Re-derivation prevents a proposer from forging actor changes and avoids a second
admission path. Its cost is that every validator must repeat serial actor
execution. Internal receipts may be stored or committed for audit and light
clients, but they cannot replace re-execution unless a future proof system is
specified.

## Consensus limits

Consensus parameters independently bound work that is not already bounded by a
smaller enclosing value, including:

- encoded block and external-transaction bytes;
- external and internal gas;
- cell inputs, outputs, and proof work;
- messages, actor executions, and call depth;
- cryptographic multiplication/MSM work; and
- issued and purchased storage.

Execution RAM has no independent storage-derived allowance. FlameVM charges
logical byte/item allocation work against external or internal gas, so these
gas ceilings also bound hostile active-memory growth. Persistent actor capacity
continues to bound stored state only.

Message gas is not created by the delivery loop. Every `send` permanently
debits its grant from the sending frame before the effect is committed. An
internal transaction and all of its descendant sends therefore partition the
grant originating in an external transaction. The independent block-wide
internal-gas and message-count limits remain conservative admission bounds for
the serial delivery work.

Storage parameters are listed in [storage.md](storage.md). Every active parameter
set must be selected by a committed protocol version; node-local configuration
must not silently alter consensus validity. Separate limits are easier to audit
and prevent one cheap resource from exhausting another, at the cost of more
parameters and possible under-utilization between pools.

## Basic bounded mempool

The mempool is local policy, never part of chain state or block validity. A
minimal implementation:

- admits only transactions that pass canonical decoding and the reusable
  transaction checks against the current tip;
- rejects an input conflict already represented by an admitted transaction;
- enforces explicit local count/byte/work bounds;
- enforces a configurable minimum fee and otherwise preserves FIFO order, so a
  transient child is never sorted before its parent; and
- after every tip change, updates Utreexo proofs with `Catchup` where possible,
  then replays or drops transactions that are no longer valid.

No replacement-by-fee, package scoring, persistent mempool, peer reputation, or
fair ordering is implied. A block remains valid even when local policy would
have rejected one of its transactions. Hard bounds prevent unbounded local
memory, but first-admitted FIFO is vulnerable to churn and low-value occupation;
peer-level rate limits belong in networking.

## Bitcoin-like reorganization

Fork choice is deliberately external. The caller supplies an exact sequence of
tip blocks to detach and candidate blocks to attach. `flamechain` must:

1. verify that every requested detach matches the current tip;
2. apply stored undo records in reverse order;
3. validate and stage each attachment through the ordinary block path; and
4. expose the new state only if the whole requested reorganization succeeds.

Undo is *complete* when it can restore the byte-for-byte prior persistent state
without re-running VM code or querying Bitcoin. It therefore covers the tip,
Utreexo forest, actor changes and destruction, storage purchases, issuance,
lease expiry/recycling, expiry indexes, and all other committed counters.
Mempool changes are not undone; the pool is revalidated against the resulting
tip.

Normal detachment stores compact first-write actor/storage undo plus the prior
small Utreexo forest and header. A multi-block reorganization additionally
takes one full in-memory snapshot before detaching, so a failed replacement can
restore the original branch atomically. That rare-path clone is easy to audit
but costs time and memory proportional to live actor state; retained forward
blocks or a persistent transactional store should replace it if profiling shows
the cost matters. A node cannot detach deeper than its retained undo window and
must obtain an older trusted snapshot/state sync instead.

## Decision ledger

| Decision | Benefit | Cost / risk |
| --- | --- | --- |
| Keep Bitcoin tracking outside `flamechain`. | Pure, repeatable state transitions and no RPC dependency in consensus code. | The caller must authenticate and commit context; a forged height corrupts lease timing. |
| Derive internal transactions. | One admission path; actor effects cannot be forged. | Validators repeat serial work; receipts alone are not proofs. |
| Use Flame Utreexo for cells. | Small common live-cell state and user-carried proofs. | Proof transport/catchup and owner availability become operational requirements. |
| Keep actors in bounded leased storage. | Prices scarce common state and caps node burden. | Lease metadata, ordered pricing, expiry cliffs, and irreversible destruction complicate applications. |
| Use endpoint reserve pricing. | One checked rational formula; reserve pressure is immediate. | Purchase splitting follows a much cheaper harmonic path; ordering creates MEV. |
| Stage a whole block atomically. | Invalid tails cannot leave partial state. | Requires transient working state; naive cloning may be expensive. |
| Retain complete undo. | Simple, exact detach and safe failed-branch handling. | Disk/memory grows with state size and reorg window. |
| Keep mempool policy local and bounded. | Limits RAM/CPU DoS and avoids making relay policy consensus. | Nodes may hold different candidates; basic eviction is gameable. |
| Start with in-memory block bodies. | Allows the state model to settle before freezing transport. | Not interoperable or launch-safe until canonical witness encoding and hashes exist. |
| Destroy actors exactly at expiry. | Capacity semantics are simple and deterministic. | Coordinated expiries can concentrate state traversal and retirement work into one block. |

## Launch-critical TBDs

Before this state machine can define production consensus, the project must
freeze and test:

1. canonical block, external-transaction, proof, receipt, and undo encodings;
2. every domain-separated hash preimage, especially the external witness root;
3. the actor/storage-state commitment and state-sync verification rules;
4. authenticated core-block context and its behavior across Bitcoin reorgs;
5. exact resource limits and protocol-version activation;
6. the failure-receipt/error taxonomy, concentrated-expiry work bounds, and all
   queue edge cases; and
7. persistence/crash recovery and the retained undo/state-sync policy.
