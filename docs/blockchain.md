# Flame blockchain state machine

## Scope

This document defines the state surrounding FlameVM: cells, actors, blocks,
resource limits, and transaction admission. VM execution is specified in
[flamevm.md](flamevm.md); agreement on blocks belongs in
[consensus.md](consensus.md).

Only FlameVM is currently implemented. Exact block encoding, persistent
accumulator, mempool rules, and consensus integration are **TBD**.

## State

At height `h`, chain state consists of the previous block identifier and height;
a Utreexo-style commitment to unspent cells; the actor registry; the virtual-byte
pool and immature recycled bytes; token-supply commitments; and consensus state.

Cells are single-use. Spending verifies membership against pre-block state,
removes the cell, and inserts emitted cells. Actors persist and are updated by
ordered internal transactions. Actor records contain code, state, virtual-byte
balance, lifecycle counters, and freeze status.

## Utreexo

The retained design is a forest of perfect binary Merkle trees whose ordered
roots and leaf count commit to the cell set. A proof identifies its accumulator
generation and leaf position and supplies sibling hashes to an applicable root.

The state machine must verify and delete consumed leaves, append created leaves,
normalize the forest, and publish catch-up data sufficient to update proofs
between generations. The old ZkVM algorithm is research input only: Flame hash
domains, proof encoding, normalization cadence, and catch-up retention require
new test vectors before becoming normative.

## Blocks and ordering

A block contains a header, ordered external transactions, and internal
transactions caused by their messages. The header must commit to protocol
version, height, parent, time, transaction root, resulting cell and actor roots,
resource usage, and a consensus certificate. Exact encoding is **TBD**.

External transactions may be verified in parallel against immutable pre-block
state. Conflicting cell spends invalidate the block. Effects are applied in block
order. Messages are delivered in originating-transaction and emission order;
internal execution, including nested actor calls, is serial. Actor rent,
freezing, expiry, and byte-pool maturation run after transaction effects.

## Applying a block

1. Validate the header and consensus certificate.
2. Canonically decode transactions and enforce static size limits.
3. Verify external FlameVM executions, proofs, and signatures.
4. Reject missing or duplicate inputs and apply external transaction logs.
5. Deliver messages deterministically and atomically apply each successful
   internal transaction log.
6. Apply actor rent, freeze/expiry, and matured-byte recycling.
7. Recompute committed roots and resource totals and compare them to the header.

Any mismatch invalidates the whole block.

## Limits

Consensus must independently bound encoded block and transaction bytes, parallel
external gas, serial internal gas, newly introduced virtual bytes, proof and MSM
work, and any actor/message counts not already bounded by those resources.

The current design discusses separate parallel and serial gas pools at an initial
`4:1` ratio and `5000` new virtual bytes per block. These values are provisional.
Recycled bytes mature after 100 blocks.

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
