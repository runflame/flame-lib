# Flame DEX

A constant-product exchange built from FlameVM's persistent actors, a
command-line client that drives it from a `flamewallet` file, and a web page
that reads it from a node (in the infrastructure repo, beside the devnet
explorer: `dev/playground/flamed/explorer/dex.html`, served at
<https://dev-flamed.safely.app/explorer/dex.html>).

Two kinds of actor, both in [`src/actors.rs`](src/actors.rs):

- A **coin** issues one public flavor, `flavor_from_actor(coin, tag)`, with
  `issuepub`: anyone may mint up to a per-request cap until a fixed supply is
  reached. Its id commits its tag and limits, so two deployments of the same
  coin are the same actor.
- A **pool** holds a reserve of FLM and of one coin and trades them at
  `x·y = k`, keeping 0.3% of every input for its liquidity providers. It
  issues its own `LP` flavor for deposits and retires it on withdrawal. Its id
  commits both flavors.

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
| 1 | amount | token in; FLM for add; LP for remove; FLM for renew |
| 2 | — | the coin, for add |
| 3 | recipient point | recipient point |
| 4 | — | minimum out, for swap |

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
flamedex --wallet w.json --registry dex.json deploy-coin EMBER --name Ember --max 21000000 --cap 10000
flamedex --wallet w.json --registry dex.json deploy-pool EMBER
flamedex --wallet w.json --registry dex.json mint EMBER 10000
flamedex --wallet w.json --registry dex.json add EMBER 100 --amount 5000   # the first deposit sets the price
flamedex --wallet w.json --registry dex.json add EMBER 10                  # later ones follow it
flamedex --wallet w.json --registry dex.json swap EMBER flm 1.5 --slippage 1
flamedex --wallet w.json --registry dex.json swap EMBER coin 100 --min-out 1.9
flamedex --wallet w.json --registry dex.json remove EMBER 5000000000
flamedex --wallet w.json --registry dex.json renew pool:EMBER --fund 10
flamedex --registry dex.json --node URL status
flamedex --wallet w.json --registry dex.json balance
```

`--wallet` (`FLAMEWALLET`) is a `flamewallet` file; the client reads its
secret, network and node, and watches its receiving addresses and its change
address `1/0`. `--node` (`FLAMEWALLET_NODE`) overrides the node. The registry
(`FLAMEDEX_REGISTRY`, default `dex.json`) is where `deploy-*` record what
they deployed, and what every other command and the web page look actors up
in. FLM amounts are in flames with up to eight decimals; coin and LP amounts
in whole units. Each command waits for its block unless `--no-wait`.

Every DEX transaction spends at least one coin, because a `send` needs the
anchor an input seeds. A mint or a renewal with nothing to pay spends a Flame
coin and returns it whole. A coin created by a pending transaction has no
proof yet, so commands from one wallet go one block apart.

## Test

```sh
demos/dex/e2e.sh
```

It starts a throwaway devnet node and runs the whole DEX on it. The genesis
wallet deploys two coins and a pool, mints, and deposits. A fresh wallet
mints and swaps both ways. A swap that misses its minimum is refunded, a mint
over the cap bounces, and liquidity is added at the pool's price and taken
out again. Anyone renews a lease. It checks balances on the way. `KEEP=1`
and `PORT` (default 18548) work as in [`e2e/devnet.sh`](../../e2e/devnet.sh).

## FlameVM notes

Two things the actor code depends on that are easy to miss:

- Cleartext `eq` leaves both operands on the stack
  ([flamevm.md](../../docs/flamevm.md#eq)); `eqd` in `actors.rs` drops them.
- Code installed with `setcode` must be the program's Cell
  (`ScriptBuilder::to_cell`), not its bytecode: literals such as a coin's tag
  are child Cells, and bytecode alone carries only a source marker for them.
