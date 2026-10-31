# Flame DEX

A constant-product exchange built from FlameVM's persistent actors, a
command-line client that drives it from a `flamewallet` file, and a web page
that reads it from a node (in the infrastructure repo, beside the devnet
explorer: `dev/playground/flamed/explorer/dex.html`, served at
<https://dev-flamed.safely.app/explorer/dex.html>).

Two kinds of actor, both in [`src/actors.rs`](src/actors.rs):

- A **coin** issues one public flavor, `flavor_from_actor(coin, tag)`, with
  `issuepub`: anyone may mint up to a per-request cap until a fixed supply is
  reached. Its id commits its tag and its issuer's key, so there is one coin
  per tag and issuer: anyone can have a `USD` of their own, and nobody can
  deploy one under someone else's key. Every coin runs the same code.
- A **pool** holds a reserve of FLM and of one coin and trades them at
  `x·y = k`, keeping 0.3% of every input for its liquidity providers. It
  issues its own `LP` flavor for deposits and retires it on withdrawal. Its id
  commits both flavors.

A coin's limits are not part of its name; its issuer signs them. The deploy
message carries the terms, a small program that returns `{max, cap}` and
fails unless the coin's constructor calls it, and the issuer's signature on
it. The constructor runs the terms with `signcall` on a contract under the
issuer's key, which checks the signature, and saves the limits they return.
A deploy under a key the sender cannot sign for bounces. The issuer key is
the wallet's `m/…/2/0`, beside the receiving (0) and change (1) branches and
never handed out as an address: the signature lets anyone run the terms
against a contract under that key, so the key should hold nothing.

Everything an actor holds is a `ClearToken`: an actor runs without a
constraint system and can only price what it can see. So a DEX transaction
opens the wallet's coins (`decrypt` with the note's opening for a
confidential coin, as is for a clear one), splits off what the message
carries, returns the change to the wallet's change address as a clear coin,
and `send`s one Dict to the actor. The actor pays out with `output` to the
recipient point in the message. The node indexes outputs that internal
transactions create, so the wallet sees them like any payment;
`flamewallet balance` lists them too.

## Messages

| key | coin | pool |
|---|---|---|
| 0 | op: 1 mint, 4 renew | op: 1 swap, 2 add, 3 remove, 4 renew |
| 1 | amount; FLM for renew or deploy | token in; FLM for add; LP for remove; FLM for renew or deploy |
| 2 | — | the coin, for add |
| 3 | recipient point | recipient point |
| 4 | — | minimum out, for swap |
| 5 | the terms, for deploy | — |
| 6 | the terms' signature, for deploy | — |

A deploy message has no op: it goes to the actor's `Init` address and runs
the constructor.

A swap that would pay less than its minimum pays the input back instead of
failing. A failed delivery seals the whole message, tokens included, in a
refund contract whose payload is that Dict, and no wallet spends that. Malformed
messages still fail and bounce; that is what happens to a mint over the cap.

The first deposit sets the price and mints one LP unit per spark of FLM;
later deposits mint the smaller of their two proportional shares, and what a
deposit brings beyond that share stays in the pool. `flamedex add` computes
the matching amount from the reserves.

## Storage

Each constructor buys a 2 KiB lease with the Flame it is sent and keeps the
rest as the actor's storage fund (state key 9). A lease lasts 52,500 core
blocks, about three days at the devnet's five-second blocks. After that the
actor freezes. `renew` (op 4) buys another 2 KiB lease from the fund, after
adding any Flame the message carries. Anyone can send it, so whoever cares
about a pool can keep it alive. A unit costs `K / R_after`
([storage.md](../../docs/storage.md#pricing)): about 10 FLM per KiB on a fresh
chain, less as the pool of units grows.

## The client

```sh
cargo build                 # its own lockfile; CARGO_TARGET_DIR=../mint-playground/target reuses that build
flamedex --wallet w.json deploy-coin EMBER                     # max 21,000,000, at most 10,000 per mint
flamedex --wallet w.json deploy-coin ASH --max 1000 --cap 500
flamedex --wallet w.json issuer                                # the key both are deployed under
flamedex --wallet w.json deploy-pool EMBER@me
flamedex --wallet w.json mint EMBER@me 10000
flamedex --wallet w.json mint ASH@<issuer> 100                 # anyone's coin, by its issuer
flamedex --wallet w.json mint EMBER 100                        # a listed coin, by its tag
flamedex --wallet w.json add EMBER 100 --amount 5000           # the first deposit sets the price
flamedex --wallet w.json add EMBER 10                          # later ones follow it
flamedex --wallet w.json swap EMBER flm 1.5 --slippage 1
flamedex --wallet w.json swap EMBER coin 100 --min-out 1.9
flamedex --wallet w.json remove EMBER 5000000000
flamedex --wallet w.json renew EMBER --pool --fund 10
flamedex --node URL status EMBER ASH@<issuer>
flamedex --wallet w.json describe EMBER@me                     # its coin-list entry
flamedex --wallet w.json balance EMBER
```

There is no registry. A coin's actor id is the hash of its constructor,
which is built from its tag and its issuer's key and nothing else; its
flavor is `flavor_from_actor(id, tag)`. A pool's constructor holds only FLM
and the coin's flavor. So a COIN argument is enough to compute every id and
flavor: `TAG@ISSUER` with the issuer's key in hex, `TAG@me` for this
wallet's own, a `TAG` from the coin list, or a coin actor's id in hex. An id
is accepted only if the constructor rebuilt from the tag and issuer in its
state hashes to that id; anything else is refused as not a flamedex coin.
Before sending, every command checks that its actor exists, and `deploy-*`
checks that it does not: a message to the wrong side of that bounces, and a
bounce seals its tokens in a contract no wallet spends. A coin deploy's
bounce is sealed under `m/…/2/1`, which no wallet scans: the message
carries a program, and `flamewallet` fails to read an address holding one.

### The coin list

Tags are not unique, so a bare `TAG` names a coin only through
[`coins.json`](coins.json), a list changed by pull request, as token lists
are on other chains. Each entry is a coin's symbol, issuer, display name and
description, and the ids and flavors those derive:

```json
{
  "symbol": "EMBER",
  "name": "Ember",
  "description": "The demo coin.",
  "issuer": "<the issuer key, hex>",
  "actor": "<the coin's actor id>",
  "flavor": "<its flavor>",
  "pool": "<the FLM/EMBER pool's actor id>",
  "lp_flavor": "<the pool's LP flavor>"
}
```

To list a coin, deploy it, run `flamedex describe TAG@me`, give the entry a
name and a description, and add it to `coins.json` in a pull request.
`cargo test` checks that every entry derives from its symbol and issuer and
that no symbol is listed twice; the [`Flame DEX`](../../.github/workflows/dex.yml)
workflow runs it on every pull request that touches the list. Reviewers
decide the rest: whether this coin deserves its symbol.

The client is built with the list and checks it again at start; `--list`
(`FLAMEDEX_LIST`) names another file. The web page reads the list from
GitHub's `main` branch and checks each coin's state against its entry.

`--wallet` (`FLAMEWALLET`) is a `flamewallet` file; the client reads its
secret, network and node, and watches its receiving addresses and its change
address `1/0`. `--node` (`FLAMEWALLET_NODE`) overrides the node; `status` and
`describe` need only a node. FLM amounts are in flames with up to eight
decimals; coin and LP amounts in whole units. Each command waits for its
block unless `--no-wait`. `balance` names FLM and the flavors of the coins
it is given, with their LP; other flavors print as hex.

Every DEX transaction spends at least one coin, because a `send` needs the
anchor an input seeds. A mint or a renewal with nothing to pay spends a Flame
coin and returns it whole. A coin created by a pending transaction has no
proof yet, so commands from one wallet go one block apart.

## Test

```sh
demos/dex/e2e.sh
```

It starts a throwaway devnet node and runs the whole DEX on it. The genesis
wallet deploys two coins and a pool, lists the coins in a list of the
test's own, mints, and deposits; deploying a coin again is refused. A fresh
wallet deploys an EMBER of its own, fails on the signature check to deploy a
coin under the first wallet's key, mints and swaps both ways. A swap that misses its minimum is refunded, a mint
over the cap bounces, and liquidity is added at the pool's price and taken
out again. Anyone renews a lease. A coin named by its actor id is checked against its
state, and a pool's id is refused as a coin. It checks balances on the way. `KEEP=1`
and `PORT` (default 18548) work as in [`e2e/devnet.sh`](../../e2e/devnet.sh).

## FlameVM notes

Three things the actor code depends on that are easy to miss:

- Cleartext `eq` leaves both operands on the stack
  ([flamevm.md](../../docs/flamevm.md#eq)); `eqd` in `actors.rs` drops them.
- Code installed with `setcode` must be the program's Cell
  (`ScriptBuilder::to_cell`), not its bytecode: literals such as a coin's tag
  are child Cells, and bytecode alone carries only a source marker for them.
- `save` keeps the Cell bodies a frame constructs, not the ones it was
  handed ([storage.md](../../docs/storage.md#content-availability-and-recovery)).
  A literal saved into state as is leaves only its id there, so the coin
  constructor copies its tag and issuer into Cells it builds (`fresh`).
