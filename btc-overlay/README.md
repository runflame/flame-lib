# btc-overlay

A fake Bitcoin Core for Flame nodes: it follows real mainnet headers from Blockbook and serves blocks that contain only
the transactions sent to it. One overlay is shared by all nodes; each points its Bitcoin RPC URL at it with
`network: overlay`, so every node reads the same blocks.

```sh
cargo run -p btc-overlay -- --blockbook http://127.0.0.1:8080/indexer/mainnet --log overlay.log
```

- **Blocks:** real headers (hash, height, previous hash, time); contents are a stand-in coinbase plus the transactions
  sent with `sendrawtransaction` since the previous new block. The merkle root does not match, and nothing checks it.
- **Fixed per hash:** a block's contents are written to the log the first time it is connected and served unchanged
  from then on, to every client, across restarts and reorgs.
- **Reorgs:** when mainnet reorgs, the orphaned blocks' transactions return to the queue and go into the next new block;
  the orphaned blocks still answer with their old contents. A reorg that replaces the start block stops the overlay.
- **Submitting:** `sendrawtransaction` checks what Core would: a burn above `maxburnamount` (-25), a transaction already
  in the chain (-27), an input spent in the chain (-25). A queued transaction is replaced by one spending the same input.
- **Prevouts:** the output an input spends is known if the overlay served the transaction that created it. Otherwise it
  is made up: a P2WSH of the witness script for a P2WSH spend (enough for the endorsement check), else an empty
  script. Made-up values are zero.
- **History:** on a fresh log the overlay starts 20 blocks below the tip (`--start-height` to choose); the indexer
  reads up to 10 blocks back from the tip.
- **Not served:** wallet calls, `getblock` verbosity 1 and 2. Credentials are accepted and not checked.

`getoverlayinfo` returns the start height, tip and queue length; `btc-integration` uses it to refuse the overlay on a
node configured for `mainnet`.

Tests run against an in-memory header chain. The Blockbook client has two live tests:
`BLOCKBOOK_URL=http://127.0.0.1:8080/indexer/mainnet cargo test -p btc-overlay -- --ignored`.
