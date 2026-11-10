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
- **Minter wallets:** the watch-only wallet calls a minter makes (`createwallet`, `importdescriptors` with a `raw()`
  descriptor, `listunspent`, `fundrawtransaction` with preselected inputs), so `BitcoinConnection::endorsements` works
  unchanged with `network: overlay`. Signing stays with the minter. A wallet never runs dry: whenever it lists its
  coins and a watched script has fewer than 20 confirmed unspent ones, the overlay mints made-up 0.001 BTC coins,
  counted as confirmed since the start block and logged like everything else. Fees are 1 sat/vB, and change under
  330 sats goes to the fee.
- **Client lag:** the RPC username names the client (the password is not checked). A client can be given a lag, and
  then sees every tip change only once it is that old: tip, heights, confirmations, `waitfornewblock`, and blocks it
  has not seen yet are not found. Through a reorg it keeps the old branch until the reorg is old enough. Contents are
  the same for everyone; transactions are never delayed. A lag is seconds (`30`) or a range (`"10..120"`), where each
  tip change gets its own delay picked from the client name and the block hash. Changes are seen in order, so a long
  delay holds back the next block too. At most 24 hours.
- **Not served:** wallets with private keys, other descriptors, `getblock` verbosity 1 and 2.

Lags come from `--lag-config lags.json`, a JSON object by username:

```json
{ "node2": 30, "node3": "10..120" }
```

and can be changed while running, until the next restart: `setclientlag node2 300`, or `setclientlag node2 null` for
none. Clients without a lag, and requests without credentials, see the live chain. Blocks served before the overlay
started (or restarted) are history every client sees at once.

`getoverlayinfo` returns the start height, live tip and queue length, and the caller's name, lag and tip;
`btc-integration` uses it to refuse the overlay on a node configured for `mainnet`.

Tests run against an in-memory header chain. The Blockbook client has two live tests:
`BLOCKBOOK_URL=http://127.0.0.1:8080/indexer/mainnet cargo test -p btc-overlay -- --ignored`.
