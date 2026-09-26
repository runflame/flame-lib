# flamed

One archival Flame node. It opens a chain, keeps a current Utreexo proof for
every unspent contract, indexes history by contract and by predicate,
archives every block it accepts, and answers wallets over JSON-RPC 2.0.

It is not a peer. It has no network but its RPC port, no reorganization, no
consensus, and no way of learning about a block someone else made. A devnet
build mints on a timer because one node with a timer is the smallest thing
that produces blocks at all; a build without the `devnet` feature mints
nothing and holds no allocation, which makes it an honest node that can open
and serve a chain and can create neither blocks nor funds.

## What the node keeps that the chain forgets

A `Blockchain` keeps roots and an accumulator. Three things a wallet needs
are not in it:

- **A proof.** The accumulator is a set of merkle roots; a membership proof
  is the holder's to keep. Except that a `Catchup` repairs a proof across
  exactly one block, so a proof two blocks old cannot be repaired at all, and
  a wallet that is not following every block cannot maintain one. The node
  does follow every block, so `UtxoSet` holds one live proof per unspent
  contract and refreshes all of them on every connect. That is the Utreexo
  bridge role, and it is the single reason a wallet can go offline.
- **Which transaction did what.** `connect` reports only
  `ExecutionRecord { kind, txid }`. The node re-executes each transaction to
  recover its effect log and archives the log in `TxIndex`.
- **Which predicate locks which contract.** The chain never knew: an id
  commits the predicate, but nothing indexes the reverse. `OutputIndex` keeps
  that map, which is what makes `scan` possible and what Phase 3's wallet
  discovery will be built on.

All three live in memory and are rebuilt by replaying `blocks.bin`. The only
durable state this node has is that file and `genesis.json`.

## The three files

**`chainparams.toml`** is the network. Every node on one network reads
exactly these bytes. It carries the protocol version, optional `[storage]`
and `[limits]` overrides — every field falls back to the upstream default —
and, on a devnet, the `[[genesis]]` allocations. Each allocation names its
holder by exactly one of `address` (a bech32f address, read with the file's
`network` HRP) and `predicate` (a hex point, for a holder with no address to
print).

**`genesis.json`** is derived from it once, by `flamed genesis`, and read on
every start. It records the genesis hash, every chain parameter in full, and
each allocation's index, id, anchor, resolved predicate, quantity **and
bytes**.

The bytes are there because a genesis contract appears in no effect log and
the chain keeps only roots: this file is the only place that contract
exists. And because the genesis hash commits the ids through the accumulator
root and nothing else in the file, `Node::open` decodes every contract,
recomputes its id, and refuses to start on a mismatch — then checks the
recorded predicate, anchor and quantity against those same bytes, and that
the token is native Flame. The id check is what stands between a node and an
unspendable allocation; the field checks are what stand between an operator
and a file that lies about who holds the money; the flavor check is what
tells an operator that a `genesis.json` predates a change of `FLAME_FLAVOR`,
since such a file agrees with itself everywhere else.

Two nodes that agree on `genesis_hash` can still disagree on `limits`: a
block header commits the version, the two roots and the storage pool size,
not the limits. Comparing whole `chainparams.toml` files is what makes a
network agree.

**`flamed.toml`** is one node's local policy: where its data lives, where to
listen, how often to mint, and the fee it will not go below. None of it is
consensus — the block header carries no timestamp, and a fee floor is an
opinion about what is worth relaying.

`blocks.bin` sits beside them in the data directory: one record per block, a
`u64` little-endian length then the block's canonical bytes. A torn trailing
record means the process died mid-append; the node refuses to start and
names the byte offset, and truncating the file there is the recovery.

## The RPC

Seven methods, no prefix, positional parameters. Ids and points are
lowercase hex strings; blobs are standard base64.

| Method | Parameters | Answers |
|---|---|---|
| `tip` | — | the tip hash, its height, the contract root |
| `proof` | `id` | `unspent` with a proof, `spent` with where, or `unknown` |
| `proofs` | `ids` | the same, for up to 1024 ids at once |
| `contract` | `id` | the contract as published, with its height and predicate |
| `submit_tx` | `block_tx` | the txid, once the mempool has taken it |
| `tx_status` | `txid` | `unknown`, `mempool`, or `confirmed` with where |
| `scan` | `predicates`, `since_height` | every contract created under those predicates since that height |

Server-defined error codes, in the JSON-RPC 2.0 server range: `-32001`
`NOT_FOUND`, `-32002` `MEMPOOL_REJECTED`, `-32003` `LIMIT_EXCEEDED`,
`-32004` `INVALID_BYTES`. Anything else is `-32603`, which means the node is
broken rather than the request.

`proofs` answers about the tip it holds at that moment, and a proof survives
exactly one block after the one it was made against. A wallet that fetches
proofs and then takes its time before `submit_tx` will be refused with
`MEMPOOL_REJECTED`; the answer is to fetch, build and submit together, and
to refetch on that code rather than to retry with the same bytes.

The protocol lives in its own crate, `flamed-rpc`, which depends on neither
`flamevm` nor `flamechain` and needs neither: every wire type is a newtype
over bytes with a public field, so a conversion is a one-liner at the call
site. That is what lets an explorer, a monitor or a WASM client speak these
seven methods without the VM behind them. The crate defines them once with
`#[rpc(server, client)]`, so the node's server and every consumer's client
are generated from the same declaration and cannot drift.

## A recorded session

```console
$ cargo run -p flamed --features devnet -- genesis \
      --chainparams flamed/configs/chainparams.toml --out ./genesis.json
genesis d5363a727a98d2125cd171727b3d2de4a0830a38b26e1887906f4e704067b270 with 1 allocation(s) -> ./genesis.json

$ cargo run -p flamed --features devnet -- run --config flamed/configs/flamed.toml
flamed: height 0 with 1 unspent contract(s), 0 block(s) archived
flamed: serving JSON-RPC on http://127.0.0.1:8545
flamed: minting every 15s
block 1 5b817e3798c44a477267332eec46cb633a1ed654e00506be639feb574408deb5 txs=0 dropped=0
```

Running `genesis` twice on the same network definition gives identical
bytes, which is the property that lets two operators check they are on the
same network by comparing files.

```console
$ curl -s -X POST -H 'content-type: application/json' \
    --data '{"jsonrpc":"2.0","id":1,"method":"tip","params":[]}' \
    http://127.0.0.1:8545
{"jsonrpc":"2.0","id":1,"result":{"hash":"d5363a72…","height":0,"contract_root":"93bc2680…"}}

$ curl -s -X POST -H 'content-type: application/json' \
    --data '{"jsonrpc":"2.0","id":2,"method":"proof","params":["30414078…"]}' \
    http://127.0.0.1:8545
{"jsonrpc":"2.0","id":2,"result":{"status":"unspent","proof":"AQAAAAAAAAAAAAAAAA=="}}
```

That proof is thirteen bytes because a one-leaf accumulator has nothing to
prove against: a committed proof is a position and a list of neighbors, and
this leaf has no neighbors yet.

```console

$ curl -s -X POST -H 'content-type: application/json' \
    --data '{"jsonrpc":"2.0","id":3,"method":"submit_tx","params":["AAEC"]}' \
    http://127.0.0.1:8545
{"jsonrpc":"2.0","id":3,"error":{"code":-32004,"message":"the submitted bytes do not decode as a transaction under this chain's limits: insufficient payload bytes"}}
```

Submitting a real transaction takes a wallet, and `src/tests/node.rs` is
that recorded session: two accounts from fixed seeds, a payment, a sweep, a
restart, and a spend by the recipient — every transaction built by
`flamewallet`, submitted to the node, and confirmed.

## The devnet feature

Everything that exists only because minting and consensus are not designed
yet is behind `devnet`: the genesis allocations and the interval minter.
Without it, `flamed` has no `genesis` subcommand, contains no `Minter`, and
refuses a `genesis.json` that has any allocation, naming the feature in the
refusal.

In the library those items are gated `#[cfg(any(test, feature = "devnet"))]`
rather than on the feature alone, so the crate's own tests compile and run
them under a plain `cargo test` — which is what CI runs. The binary is the
exception and gates on the feature alone: `cargo test` builds the binary's
test harness with `cfg(test)` on but links the library built without it, so
an `any(test, ..)` arm there would name items that library does not have.

## What this node does not do well

Stated plainly, because each of these is a real limit and not a detail:

- **Memory grows with the chain.** `Blockchain` keeps one `BlockUndo` per
  connected block — a header, a cloned `Forest`, and an actor undo record —
  and releases them only on a disconnect or a reorganization, neither of
  which a single node with no peers ever performs. At a 15-second interval
  that is roughly 5,800 records a day. A long-lived `flamed run` is bounded
  by memory, not by disk.
- **`scan` is bounded in questions, not in work.** At most 1024 predicates
  per call, but each one walks every contract ever created under it with the
  node lock held, and `since_height` filters records it has already walked
  rather than skipping them. An index keyed by `(predicate, height)` is the
  fix, and Phase 5 is where it belongs.
- **`since_height` filters on the creation height alone.** A contract
  created before that height and spent after it is not reported, so a wallet
  resuming from its last sync point learns about new contracts but not about
  the spending of old ones. Until that changes, a resuming wallet has to ask
  `proofs` about the contracts it already knows; `scan` alone is not enough
  to notice a spend.
- **A transaction whose parent confirms in the same block is dropped.** When
  a block cannot hold every candidate, the leftovers are re-admitted while
  the tip is still unchanged, which is what keeps them valid against the
  state they were built for — but a child spending a contract that very
  block creates is refused there, and its sender has to resubmit. Deferring
  it instead would mean admitting against a tip it was not built for.
- **Contracts created by anything other than an external transaction are
  invisible to the indexes, and that is reachable today.** `connect` reports
  only a kind and a txid per execution, so the node re-derives effects by
  re-running the external transactions; internal logs are produced inside
  the chain and never handed out. This is not only an actor problem. A
  `send` in an external transaction finds no actor to receive it, bounces,
  and the bounce mints a refund contract under the sender's refund
  predicate. That contract is real and in the accumulator, and this node can
  serve neither a proof nor a record for it — the money exists and is
  unspendable through this node. The node prints a line naming the count
  whenever a block contains such an execution, so the gap is visible rather
  than silent. What it holds is never *wrong*: an internal log may not carry
  an `Input`, so nothing the node believes unspent can have been spent
  behind its back. Closing the gap needs an upstream change — `connect`
  returning the logs it already has.
- **The mempool does not survive a restart**, and neither do the indexes —
  they are rebuilt by replaying every block from genesis. Replay decodes the
  whole archive into memory before connecting any of it, so the node is
  slowest and hungriest to start exactly when it has been running longest.
  Fine for a devnet; not a strategy for a long chain.
