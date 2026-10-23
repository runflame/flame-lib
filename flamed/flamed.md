# flamed

One archival Flame node. It opens a chain, keeps a current Utreexo proof for
every unspent contract, indexes history by contract and by predicate,
archives every block it accepts, and answers wallets over JSON-RPC 2.0.

It is not a peer. It has no network but its RPC port, no reorganization, no
consensus, and no way of learning about a block someone else made. A devnet
build mints on a timer because one node with a timer is the smallest thing
that produces blocks at all, and issues flames for the burns its core
indexer reports ([Minting](#minting)); a build without the `devnet` feature
mints nothing and holds no allocation, which makes it an honest node that
can open and serve a chain and can create neither blocks nor funds.

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
  discovery will be built on. It also keeps the note that follows each
  output, the `Data` entry right after it in its log, byte for byte and
  never parsed ([Confidential payments](../docs/payments.md#the-node)).

All three live in memory and are rebuilt by replaying `blocks.bin`. The only
durable state this node has is that file and `genesis.json`. Block times are
in the block headers; the mock core indexer keeps its own chain in its own
directory ([Times](#times)).

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
listen, how often to mint an iblock (`block_interval_secs`, 5 by default) and
a mock core block (`core_block_interval_secs`, 15 by default), the fee it
will not go below, where the mock core indexer keeps its table
(`indexer_dir`, `<data_dir>/indexer` by default), and optionally a mint
playground to follow as the core chain instead (`playground_url`). None of it is consensus —
the header's time is only required not to go backwards, and a fee floor is
an opinion about what is worth relaying.

`blocks.bin` sits beside them in the data directory: one record per block, a
`u64` little-endian length then the block's canonical bytes. A torn trailing
record means the process died mid-append; the node refuses to start and
names the byte offset, and truncating the file there is the recovery.
SIGINT and SIGTERM do not cause one: the node stops minting at the next
tick boundary, finishes a block already under way, answers the RPC calls in
flight, and exits once nothing holds the node. A second signal skips the
wait for RPC clients but still waits for the archive: it takes the node
lock, which no append is ever outside of, and exits holding it. Only
SIGKILL can tear a record.

## The RPC

Methods have no prefix and use positional parameters. Ids and points are
lowercase hex strings; blobs are standard base64.

| Method | Parameters | Answers |
|---|---|---|
| `tip` | — | the tip hash, its height, the contract root |
| `proof` | `id` | `unspent` with a proof, `spent` with where, or `unknown` |
| `proofs` | `ids` | the same, for up to 1024 ids at once |
| `contract` | `id` | the contract as published, with its height and predicate |
| `query_actor` | `id`, `argument`, `gas_limit`, `expected_block?` | read-only result `{block, height, value, gas_used}` at the current tip |
| `submit_tx` | `block_tx` | the txid, once the mempool has taken it |
| `tx_status` | `txid` | `unknown`, `mempool`, or `confirmed` with where |
| `scan` | `predicates`, `since_height` | every contract created under those predicates since that height, each with its note |
| `unspent` | `address`, `with_pending?` | every unspent contract locked to a receiving or tracking address, with its note, block time and a proof at the tip; with `with_pending`, mempool spends are removed and pending outputs are added with `received_at` instead of a height, time or proof |
| `history` | `addresses`, `cursor?`, `limit`, `with_pending?` | the transactions that created or spent contracts of up to 256 addresses, a page at a time from either end; see [History](#history) |

Server-defined error codes, in the JSON-RPC 2.0 server range: `-32001`
`NOT_FOUND`, `-32002` `MEMPOOL_REJECTED`, `-32003` `LIMIT_EXCEEDED`,
`-32004` `INVALID_BYTES`, `-32005` `STALE_BLOCK`, `-32006` `QUERY_FAILED`,
`-32012` `INVALID_ADDRESS`. `-32007` through `-32011` are skipped because
jsonrpsee uses them for its own errors, such as an oversized request.
Anything else is `-32603`, which means the node is
broken rather than the request.

`query_actor` invokes the actor with one data argument, no authenticated
caller, and `readwrite = false`. Its argument and result are base64
`CellEnvelope`s encoding one VM `Value`: Scalar, Point, Cell, or a portable
Dict containing only these values recursively. Empty and nested Dicts are
accepted; token and runtime values are rejected at any depth. Envelopes
are limited to 64 KiB, typed Dict decoding is bounded, and execution gas must
be 1..=1,000,000. The result's
`gas_used` includes VM work but excludes bounded envelope decoding. No fee
is paid and no effects persist, including on failure. Ordinary writes enter
through messages in transactions submitted with `submit_tx`.

An optional `expected_block` pins the query to that current tip; it returns
`STALE_BLOCK` if the tip has changed. It does not query historical state.
For a multi-hop lookup, reuse the first result's `block` and restart the
lookup if it becomes stale. A block hash labels the answer; it is not a
proof that a remote node executed the getter correctly.

`proofs` answers about the tip it holds at that moment, and a proof survives
exactly one block after the one it was made against. A wallet that fetches
proofs and then takes its time before `submit_tx` will be refused with
`MEMPOOL_REJECTED`; the answer is to fetch, build and submit together, and
to refetch on that code rather than to retry with the same bytes.

The protocol lives in its own crate, `flamed-rpc`, which depends on neither
`flamevm` nor `flamechain` and needs neither: every wire type is a newtype
over bytes with a public field, so a conversion is a one-liner at the call
site. That is what lets an explorer, a monitor or a WASM client speak these
methods without the VM behind them. The crate defines them once with
`#[rpc(server, client)]`, so the node's server and every consumer's client
are generated from the same declaration and cannot drift.

## History

`history` is the log of a wallet: every transaction that created a contract
for one of its addresses or spent one, with those contracts' bytes and notes.
The node cannot see amounts; the wallet opens the notes. A spent contract
comes with the note it was created with, so a wallet restored from its seed
can price a spend before it has paged back to the receipt. An entry never
changes once confirmed: it says what happened, and `unspent` says what is
true now. A transaction that touched several of the addresses, like a
payment that sends change to another address, is one entry. The genesis
allocation is an entry at height 0 with an all-zero txid and no fee.

A wallet holds two cursors around what it has read, and the range only
grows:

| Call | Reads | Returns |
|---|---|---|
| no `cursor` | the newest entries, newest first | `older`, and `newer` at the tip |
| `older` cursor | the entries before it, newest first | the next `older`, null at the beginning |
| `newer` cursor | the entries after it, oldest first | the next `newer`, and `has_newer` when the page was cut short |

`with_pending` adds the mempool's transactions for these addresses to a first
or newer page, as `pending`: a fresh snapshot each time, never part of the
paging, capped at 100 (`pending_truncated`). A pending transaction that
confirms arrives once, through the newer cursor.

A cursor is a position, never an offset: `older:12:3:<block>:<addresses>` is
the 4th execution of block 12. The chain only grows, so the entries on either
side of a position are the same however many blocks come after. The cursor
also names the block at that height and fingerprints the addresses: a cursor
on a block the chain no longer has is `STALE_BLOCK`, and the wallet starts
again without one; a cursor asked with other addresses, or one this node did
not write, is invalid params. The format is for reading in a debugger and is
promised to no one: pass a cursor back as it came, and expect it to change
with any release.

Each address's history is a list in chain order, kept in RAM and rebuilt
by replay like the rest of the index. A page is a binary search into each
list and a merge, so its cost is the page and the number of addresses, not
the length of the history.

## Times

Flame blocks are iblocks: they come faster than core (Bitcoin) blocks, and
consecutive ones often anchor to the same `core_block_hash`. So a block's
time cannot be its core block's; each header carries its own `time`, the
minter's clock when it built the block, which consensus only requires not to
go backwards. A transaction has three times, each vouched for by someone
different.

| Field | Where | Who vouches for it |
|---|---|---|
| `time` | `BlockSummary` (and `header.time`), and `ScanEntry` and `UnspentEntry` for the creating block | the minter, in the header |
| `block_time` | `TransactionResult` | the minter, through the block |
| `locktime` | `TransactionSummary`, `PendingTransaction` | the sender: signed, never checked |
| `received_at` | `PendingTransaction`, `TransactionResult`, and `UnspentEntry` while pending | this node's clock |

All are Unix seconds. `time` and `block_time` are absent for genesis.
`received_at` is absent on a confirmed transaction this node never held
pending, and after a restart, because the mempool is not durable and neither
are its times.

**The core indexer** is the `CoreIndexer` trait in `src/indexer.rs`: `tip`
names the core block the next header commits, and `blocks` lists the burns
of each core block in a range. The minter asks both once per iblock; replay
never does, because everything a block needs is in it.

`MockIndexer` has no Bitcoin behind it and no burns on it.
Its chain grows only when `mine` is called: a devnet build runs a second
timer for that, every `core_block_interval_secs`, independent of the minter,
so about three 5 s iblocks share each 15 s core block. Core block `h` is
`core_block_hash(h)` with this machine's clock as its time; each is appended
to `<indexer_dir>/core-times.txt` (`<hex hash> <unix seconds>` per line, from
height 1) and synced before `mine` returns. A fresh chain mines its first
block on the minter's first ask. A torn last line is dropped; any other bad
line, or a hash that is not its height's, stops the node from starting. If
the table is lost, the archive still replays; the core chain starts over.

`PlaygroundIndexer` follows a [mint playground](../demos/mint-playground)
instead, when `flamed.toml` sets `playground_url`: the playground's blocks
are the core blocks, `core_block_hash(h)` again, and its eligible burns are
theirs. No mock core chain is mined then.

A connector to a real Bitcoin node replaces either through
`Node::open_with`, and the node stays the same.

## Minting

Flames enter the chain one way besides the devnet genesis: a block that
moves its core anchor from height `p` to `h` carries a **mint proof**, the
burns of core blocks `p + 1` to `h`, and issues each core block's
allotment to them. A block that stays on its parent's core block proves
nothing and issues nothing. That is the anchor rule `flamechain` enforces:
no block reaches a new core block without proving it.

- **Allotment.** 50 FLM per core block, halved every 210,000 core blocks,
  as Bitcoin's subsidy is: 21 million in all.
- **Split.** By burned sats: each share floored, then the leftover sparks one
  each to the largest remainders, the earlier burn first on a tie. A core
  block nobody burned in issues nothing, and the allotment is lost.
- **Outputs.** One cleartext Flame output per reward predicate, summed over
  every core block the proof covers, in a single `mint` execution that runs
  before the block's transactions. It appears in `history`, `scan` and
  `unspent` like any other transaction, with `execution.kind` `mint`.
- **Catching up.** One block proves at most `max_core_blocks` core blocks
  (1,024) and `max_burns` burns (100,000), whole core blocks only; a node
  far behind its core chain closes the gap a block at a time.

A burn names a predicate and an amount, exactly what a Bitcoin mint proof
output in `btc-integration` carries as `flame_reward_address` and its burned
value. The chain checks the proof's shape and pays it; that the burns are
really on the core chain is the node's to check against its indexer, as
the core block hash is. A devnet node builds the proof from its own
indexer, so there is nothing to check yet.

**`locktime`** is documented as Bitcoin's: below `500_000_000` a height,
otherwise Unix seconds before which the transaction is invalid, and `0`
for no lock: include as soon as possible. Nothing enforces it yet, and the
node shows it as signed. A wallet that wants a signing time on record can
set it to the current Unix time, which is already reached and so does not
delay the transaction; it is the sender's claim, covered by the TxID.

## A recorded session

```console
$ cargo run -p flamed --features devnet -- genesis \
      --chainparams flamed/configs/chainparams.toml --out ./genesis.json
genesis a7291837c9a24b3932c1259994e59b7208bc9a50d1bbfd5acb8de43fe3a9424a with 1 allocation(s) -> ./genesis.json

$ cargo run -p flamed --features devnet -- run --config flamed/configs/flamed.toml
flamed: height 0 with 1 unspent contract(s), 0 block(s) archived
flamed: serving JSON-RPC on http://127.0.0.1:8545
flamed: mining a mock core block every 15s
flamed: minting every 5s
core block 1 447bf649df413cac10b1ed24934496eebbbf623c911f968a60c0e81b1fba5ea0
block 1 f183312ff09e2d10e728137707ca27781c22ebe15444d701a1b1db48c22b962c core=1 burns=0 txs=0 dropped=0
block 2 7f462631eb9a96ead2ee6bf24a7e2c64f3a9b800fbeb006cf2d70f61b430f2e0 core=1 burns=0 txs=0 dropped=0
block 3 452eed43c1b1aee04fde318be5b17046d24176efb194e4f58068357fca9a2a4a core=1 burns=0 txs=0 dropped=0
core block 2 425bf3a649d848b33b0e05123253bdea186f701b927f240de4e728130b0c2576
block 4 2b530890399d1c74b1b09e5856c2aa53091155a31ae52ebf66c53badae13dd20 core=2 burns=0 txs=0 dropped=0
```

Block hashes differ from run to run, because each header carries the time
it was minted. Running `genesis` twice on the same network definition gives identical
bytes, which is the property that lets two operators check they are on the
same network by comparing files.

```console
$ curl -s -X POST -H 'content-type: application/json' \
    --data '{"jsonrpc":"2.0","id":1,"method":"tip","params":[]}' \
    http://127.0.0.1:8545
{"jsonrpc":"2.0","id":1,"result":{"hash":"9eaac245…","height":0,"contract_root":"8669b06a…"}}

$ curl -s -X POST -H 'content-type: application/json' \
    --data '{"jsonrpc":"2.0","id":2,"method":"proof","params":["e870be9e…"]}' \
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
`flamepayments`, submitted to the node, and confirmed.

## The image

`flamed/Dockerfile` builds one devnet node as an image, and CI
(`.github/workflows/flamed-image.yml`) pushes it to GHCR as
`ghcr.io/runflame/flamed:<full commit hash>` from every push to `main` that
touches flamed or a crate it is built from, moving `:main` along. A pull
request into `main` runs the same build and smoke test and pushes nothing.
Nothing deploys the image; a host is deployed by naming a commit, never
`:main`. The tag and the `org.opencontainers.image.revision` label are the
same full hash.

| Path | What |
|---|---|
| `/etc/flamed/flamed.toml` | [`configs/flamed.image.toml`](configs/flamed.image.toml): RPC on `0.0.0.0:8545`, data in `/var/lib/flamed` |
| `/etc/flamed/chainparams.toml` | `configs/chainparams.toml` with the genesis holder replaced |
| `/etc/flamed/genesis.json` | derived from it at build time |
| `/var/lib/flamed` | the volume: `blocks.bin`, nothing else |

The genesis holder is the first receiving address of a BIP-39 mnemonic with
no passphrase, the address a wallet restored from that phrase shows first.
The build reads the phrase from the build secret `genesis_mnemonic`, turns
it into that address with `flamed address`, and keeps nothing else of it: no
layer of either stage holds the phrase. CI passes the org secret
`FLAMED_GENESIS_MNEMONIC` on `main` and fails without it; a dry run uses
BIP-39's public all-zero phrase (`abandon` ×11, `about`) and checks the
address it gets.

The same mnemonic and the same `chainparams.toml` give the same genesis, so
consecutive images are normally one chain. A run summary names the genesis
hash; an image whose hash differs from the running one is a new chain, and
it will not start on the old `blocks.bin`.

Stopping the container is a kill: `flamed` installs no signal handler, and
as PID 1 it is not killed by SIGTERM or SIGINT, so a stop ends in SIGKILL.
A kill mid-append leaves a torn record, the node refuses to start naming
the byte offset, and truncating `blocks.bin` there is the recovery. The
RPC has no authentication and no rate limit: publish it on loopback and put
a proxy in front.

## The devnet feature

Everything that exists only because minting and consensus are not designed
yet is behind `devnet`: the genesis allocations and the interval minter.
Without it, `flamed` has no `genesis` or `address` subcommand, contains no `Minter`, and
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
- **Block times are only as good as the indexer.** The mock's times are
  this machine's clock and can be lost with its directory. Nothing about
  them is consensus until a real indexer authenticates the core block.
- **The mempool does not survive a restart**, and neither do the indexes —
  they are rebuilt by replaying every block from genesis. Replay decodes the
  whole archive into memory before connecting any of it, so the node is
  slowest and hungriest to start exactly when it has been running longest.
  Fine for a devnet; not a strategy for a long chain.
