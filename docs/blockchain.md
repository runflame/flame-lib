# Flame blockchain state machine

## Scope

This document defines the state surrounding FlameVM: cells, actors, blocks,
resource limits, and transaction admission. VM execution is specified in
[flamevm.md](flamevm.md), actor storage in [storage.md](storage.md), and
agreement on blocks in [consensus.md](consensus.md).

Only FlameVM is currently implemented. Exact block encoding, persistent
accumulator, mempool rules, and consensus integration are **TBD**.

## State

At height `h` within a selected main chain, _chain state_ consists of:

* current height;
* the previous block identifier;
* a Utreexo-style commitment to unspent cells;
* the actor registry;
* the available storage pool;
* index of active storage leases;

Cells are single-use. Spending verifies membership against pre-block state,
removes the cell, and inserts emitted cells. Cells are spent by _external transactions_.

Actors are multi-use records containing code and arbitrary data.
Actors code is executed in response to messages from external transactions,
actors can make calls to each other. The updates to actors’ state are recorded
as _internal transactions_.

## Utreexo

The set of all cells is stored in a data structure "Utreexo", a forest
of perfect binary Merkle trees. Each input to an external transaction carries a proof
of membership in Utreexo. This way, each user carries the cost of storage by themselves,
without requiring other nodes store that data. Utreexo storage is unbounded and zero-cost.

## Actor registry

Actor registry is a list of actors with their code, data and leased storage allocations.
Actors persist in active memory and process messages from arbitrary users, so their storage 
is bounded. Each actor pays for leased storage with flames.
The network adjusts the storage prices automatically: price rises and drops in response to changes
to available storage. 

To prevent long-range attacks by minters, storage fees are burned and extra storage is unlocked
on every block, slowly growing the total amout of available space.

Application developers are free to design how the storage is renewed, extended and organized.
For massive multi-user applications, individual per-user data can sometimes be offloaded to the cells,
leaving the actor storage only for data that needs to be accessed by all users.

## Blocks and ordering

A block contains a header, ordered external and internal transactions.
The header commits to protocol version, height, parent, time, transaction root, resulting chain state.

External transactions may be verified in parallel against pre-block state.
Conflicting cell spends are prohibited. Effects are applied in block
order. Messages are delivered in originating-transaction and emission order;
internal execution, including nested actor calls and storage purchases, is
serial. Lease expiry, actor destruction, storage recycling, and storage issuance run before
transaction execution.

## Applying a block

1. Validate the header and consensus certificate.
2. Expire leases, recycle their bytes, destroy under-capacity actors,
   and issue the block's new storage bytes.
3. Canonically decode transactions and enforce static size limits.
4. Verify external FlameVM executions, proofs, and signatures.
5. Reject missing or duplicate inputs and apply external transaction logs.
6. Deliver messages deterministically and atomically apply each successful
   internal transaction log, including immediate storage-pool updates.
7. Recompute committed roots and resource totals and compare them to the header.

Any mismatch invalidates the whole block.

## Limits

Consensus independently bound encoded block and transaction bytes, parallel
external gas, serial internal gas, issued and purchased storage, proof and MSM
work, and any actor/message counts not already bounded by those resources.

The current design discusses separate parallel and serial gas pools at an
initial `4:1` ratio. Storage begins with a 128 MiB reserve and issues 8 KiB per
core block; the complete parameters are in [storage.md](storage.md).

## Mempool policy

Mempool policy is local, not consensus. A minimal policy checks canonical
decoding, current cell proofs, VM proofs/signatures, a local fee floor, conflicts,
and next-block resource limits. Replacement, package relay, proof refresh after
accumulator normalization, eviction, and anti-spam limits are **TBD**. A block may
remain valid even if local policy would not have admitted one of its transactions.

## Reorganizations

Nodes must retain prior state or reversible deltas to detach non-final blocks and
apply an alternative branch. Accumulator catch-up data and actor rollback must
cover the permitted reorganization window, which depends on consensus and Bitcoin
coupling and is **TBD**.
