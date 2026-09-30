# Core Blocks Selection Algorithm

During BFT consensus, block can be marked as a Core block. Whether a block is a Core block is determined by a flag in
the block header. The following additional data is included in the header of a core block:

```text
btc_height: u32
```

where `btc_height` is the target Bitcoin block height for the core block. It is set to the height of the latest known
Bitcoin block plus two.

The flag and `btc_height` are part of the block hash.

Core block selection and mint proof submission are performed as follows:

1. Find a finalized core block satisfying the following condition:
    - `core.btc_height == arrived_block.height + 1`
2. If such a core block is found and is not marked as minted, submit a mint proof referencing the selected Flame core
   block to the Bitcoin chain. Mark the selected core block as submitted. Continue with step 4.
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

1. Check that `core.btc_height` is strictly higher than the `btc_height` of the Core block with the highest known
   `btc_height`. If it is not strictly higher, reject the block.