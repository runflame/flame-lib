# Consensus Implementation

Note that the numbers in this document are speculative and may (and most likely will) change.

## BFT

BFT consensus produces one block every 5 seconds, or 180 blocks per 15 minutes.

## Core Blocks

This section describes the mechanism for selecting core blocks from Flame blocks. Core blocks are the only Flame blocks
that can be minted.

### Guarantees

This algorithm provides the following guarantees:

- For the same Bitcoin block height, at most one finalized corresponding Flame core block can exist.
- Whenever honest minters submit mint proofs for the same Bitcoin block height, they reference the same finalized Flame
  core block.
- Bitcoin reorgs do not cause finalized Flame blocks to be reverted or reorganized. Core blocks created for Bitcoin
  heights affected by a reorg remain part of the finalized Flame chain.

### Implementation

During BFT consensus, the leader can mark a produced block as a core block. Whether a block is a core block is
determined by a flag in the block header. The following additional data is included in the header of a core block:

```text
btc_height: u32
```

where `btc_height` is the target Bitcoin block height for the core block. It is set to the height of the latest known
Bitcoin block plus two.

The flag and `btc_height` are part of the block hash.

The following algorithm is triggered when:

1. A node observes a new BTC block (referred to as the `arrived_block` below).
2. A new BFT leader is selected. In this case, `arrived_block` refers to the latest known BTC block.

Core block selection and mint proof submission are performed as follows:

1. Find a finalized core block satisfying the following condition:
    - `core.btc_height == arrived_block.height + 1`
2. If such a core block is found and is not marked as minted, submit a mint proof referencing the selected Flame core
   block to the Bitcoin chain. Mark the selected core block as minted. Continue with step 4.
3. If no such core block is found, check `LATE_MINTING_POLICY`. If the policy allows submitting mint proofs after the
   BTC block has arrived, set up a task to submit the mint proof when a finalized Flame core block with
   `core.btc_height == arrived_block.height + 1` becomes available.
4. If the node is not the current BFT leader, return. Otherwise, continue.
5. Check whether a finalized core block with `btc_height == arrived_block.height + 2` already exists. If one exists,
   return. Otherwise, continue.
6. If the `notarize` step has not started, use the current Flame block. Otherwise, wait until the current Flame block is
   finalized and continue before the next Flame block is produced.
7. Mark the selected Flame block as a core block:
    - Set `btc_height` to `arrived_block.height + 2`.

During consensus, when a node receives a core block header during the `verify` step:

1. Check that `core.btc_height` is strictly higher than the `btc_height` of the cre block with the highest known
   `btc_height`. If it is not strictly higher, reject the block.
