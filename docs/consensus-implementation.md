# Consensus Implementation

Note that the numbers in this document are speculative and may (and most likely will) change.

## BFT

BFT consensus produces one block every 5 seconds, or 180 blocks per 15 minutes.

## Core Blocks

This section describes the mechanism for selecting core blocks from Flame blocks. Core blocks are the only Flame blocks
that can be minted.

### Guarantees

This algorithm provides the following guarantees:

* For the same Bitcoin block, at most one finalized corresponding Flame core block can exist.
* Whenever honest minters submit mint proofs for the same Bitcoin block, they reference the same finalized Flame core
  block.
* Bitcoin reorgs do not cause finalized Flame blocks to be reverted or reorganized. Core blocks anchored to orphaned
  Bitcoin blocks remain part of the finalized Flame chain.

### Implementation

During BFT consensus, the leader can mark a produced block as a core block. Whether a block is a core block is
determined by a flag in the block header. The following additional data is included in the header of a core block:

```
btc_anchor_hash: H256
```

where `btc_anchor_hash` is the hash of the latest known Bitcoin block at the time the core block is created.

The flag and `btc_anchor_hash` are part of the block hash.

The following algorithm is triggered when:

1. A node observes a new BTC block (referred to as the `arrived_block` below).
2. A new BFT leader is selected. In this case, `arrived_block` refers to the latest known BTC block.

Core block selection and mint proof submission are performed as follows:

1. Find a finalized core block satisfying the following condition:
    - `core.btc_anchor_hash == arrived_block.parent_hash`
2. If such a core block is found and is not marked as minted, submit a mint proof referencing the selected Flame core
   block to the Bitcoin chain. Mark the selected core block as minted. Continue with step 4.
3. If no such core block is found, check `LATE_MINTING_POLICY`. If the policy allows submitting mint proofs after the
   BTC block has arrived, set up a task to submit the mint proof when a finalized Flame core block with
   `btc_anchor_hash == arrived_block.parent_hash` becomes available.
4. If the node is not the current BFT leader, return. Otherwise, continue.
5. Check whether a finalized core block with `btc_anchor_hash == arrived_block.hash` already exists. If one exists,
   return. Otherwise, continue.
6. If the `notarize` step has not started, use the current Flame block. Otherwise, wait until the current Flame block is
   finalized and continue before the next Flame block is produced.
7. Mark the selected Flame block as a core block:
    - Set `btc_anchor_hash` to `arrived_block.hash`.

During consensus, when a node receives a core block header during the `verify` step:

1. Find the BTC anchor: `anchor = lookup(btc_anchor_hash)`.
2. Check whether `anchor` is a valid Bitcoin block. If not, reject the block.
    - A valid block in this context means a valid Bitcoin block regardless of whether it is part of the current Bitcoin
      chain. If the node cannot find the block in its local BTC node, it must request it from the network.
3. Check whether there is already a finalized core block with `core.btc_anchor_hash == anchor.hash`. If so, reject the
   block.

> **Note:** A Byzantine leader may propose a core block referencing an old Bitcoin block. Such a core block can still
> satisfy the current validation rules, but it will not be mintable because its corresponding Bitcoin minting window has
> already passed. This may affect core-block and minting liveness, although it does not prevent the Flame BFT chain
> itself from progressing.
>
> Preventing this requires validators to enforce a notion of Bitcoin anchor freshness. Doing so safely requires a
> well-defined Bitcoin chain liveness and synchronization model, since validators may temporarily observe different
> Bitcoin tips or forks. This is outside the scope of the current design.

### Coinbase Transactions after Reorgs

Before unlocking coinbase outputs after the maturity period, validators must check whether the corresponding mint proof
still exists in the main Bitcoin chain according to their local node. If the mint proof is no longer present due to a
reorg or for any other reason, the coinbase outputs must be marked as invalid.

The current maturity period is 100 Bitcoin blocks, which protects against Bitcoin reorgs up to 100 blocks deep. Bitcoin
reorgs deeper than 100 blocks are considered impossible and are outside the current safety assumptions.
