# flamewallet-cli

`flamewallet`, a development wallet for a `flamed` node: something to play
with, to drive end-to-end tests with, and to script against. The library
behind it (`flamewallet_cli`) is the wallet; `main.rs` only parses
arguments and prints, so a terminal or graphical wallet can take the
library and leave the binary.

It signs through `flamewallet-ffi`, the facade Safely and the browser
wallet run, so a send from here goes through the same code as a send from a
phone.

**The wallet file holds the secret in the clear.** It is created readable
by its owner only, and that is all. Do not put real funds behind it.

## State

The wallet keeps one file (`./wallet.json`, or `--wallet` / `FLAMEWALLET`):
the secret, the network, the node URL, the receiving counter and any other
paths handed out. Nothing else is cached, and nothing needs to be: the node
indexes outputs by predicate, so a balance is one `unspent` call per
watched address and a history page is one `history` call, however long the
chain.

Watched addresses are every receiving address issued (index 0 always), one
change address, `m/…/1/0`, that a send returns change to by default, and
every path `address --path` has handed out.

## Paths

A path is `branch/index` below the account, `m/35263'/network'/0'/…`:
branch 0 is receiving, 1 change, and any other branch is the user's own.
`address --path 2/0` prints the address at `2/0` and watches it from then
on. `send --from PATH` spends only coins at that path and returns all
change there; `issue --from PATH` mints with that path as the issuer.
Other wallets watch only branches 0 and 1 and will not see coins elsewhere.

## Commands

```console
$ flamewallet new                      # fresh mnemonic, prints it and the first address
$ flamewallet restore < phrase.txt     # from a mnemonic on stdin
$ printf 'a1%.0s' $(seq 64) | flamewallet restore --seed   # the devnet's genesis holder
$ flamewallet address [--new | --path 2/0]
$ flamewallet balance
$ flamewallet coins
$ flamewallet send ADDRESS 12.5 [--from PATH] [--memo TEXT] [--flavor HEX] [--clear] [--fee SPARKS] [--wait]
$ flamewallet issue 1000 --tag soot [--from PATH] [--to ADDRESS] [--memo TEXT] [--fee SPARKS] [--wait]
$ flamewallet flavor soot [--from PATH]   # the flavor that issue mints
$ flamewallet history [--limit N] [--older CURSOR]
$ flamewallet status TXID
$ flamewallet wait TXID
$ flamewallet tip
```

`--json` on any command prints one JSON value instead of text. Quantities
in JSON are exact decimal strings in base units (sparks for Flame), as the
node writes them; flavors are 64 hex digits.

`--node URL` (or `FLAMEWALLET_NODE`) names the node. `new` and `restore`
store it in the file; every other command uses it instead of the stored
one.

`history` names the other side of each transaction by tracking address:
`from` whoever paid the wallet, `to` whomever it paid. `to` is left empty
when someone else's coins were spent too, since what a payer creates beside
the wallet's coins is the payer's change.

## Flavors

Every amount is per flavor. Flame (`FLM`) is written in flames with up to
eight decimals; any other flavor in whole units, since nothing records how
many decimals it has. `balance`, `coins` and `history` list every flavor
the wallet holds, Flame first.

`send --flavor HEX` pays another flavor. The fee is always Flame, so such
a send spends coins of both flavors and returns change in each.

`send --clear` pays a cleartext token: its amount, flavor and recipient are
public on chain, it has no note, and the memo follows it in the clear. The
change stays confidential, and nothing about the coins spent is revealed.
The recipient's `coins` lists it as `clear`.

## Issuing

`issue AMOUNT --tag TAG --from PATH` mints a new confidential flavor and
prints it. The flavor is a hash of the tag and the key of the coin spent
to mint it, so the path is the **issuer**: every `issue --tag soot --from
2/0` from one wallet is one fungible flavor, while `soot` from another path,
or another wallet, is another. `--from` defaults to `0/0`. `flavor TAG
--from PATH` prints the flavor without minting. Nothing on chain records
the tag, so other wallets see only the flavor's hex.

An issuance spends the largest confirmed Flame coin at the issuer's path,
pays the fee from it and returns the rest to the same address, so the
next issuance has a coin once this one is mined. The natural setup is a
minting address of its own: `address --path 2/0`, a payment of some Flame
there, then any number of `issue --from 2/0`, and `send --from 2/0` to pay
the tokens out with the change kept at `2/0`. A plain `send` takes coins
from anywhere, a minting address included, and sends change to `1/0`.

The minted coin goes to the issuer's address, or to `--to`. The coin is
spent with `signcall` rather than `signtx`, because only a contract's own
frame can issue. A `signcall` signature covers its program and not the
transaction, so the signed program fixes everything itself: it checks it
runs on that one coin, mints the amount, pays the fee and creates every
output. A copy of a published issuance can therefore publish only that
same issuance, once. `flamepayments::build_issue` has the details.

Tokens cannot be burned yet.

## Sending

A send reads the coins with the mempool applied, picks the largest coins of
each flavor it needs until they cover the amount (and the fee, in Flame),
proves, signs and submits. Coins created by a pending transaction, change
included, have no membership proof until they are mined, so they show as
pending and cannot be spent until the next block: wait with `--wait`
before sending again from the same coins.

If a block lands between reading the coins and submitting, the proofs are
stale and the node refuses the transaction; the send then rebuilds once
from fresh coins.

The default fee is 1000 sparks and the default gas limit 1 000 000, what
the node's tests and Safely use.

## Devnet tests

```sh
e2e/devnet-smoke.sh
```

Starts a throwaway node with one-second blocks, restores the genesis
wallet, pays a fresh wallet with a memo, pays part of it back, and checks
every balance. `KEEP=1` keeps the scratch directory; `PORT` changes the
RPC port (default 18545).

```sh
e2e/devnet-tokens.sh
```

The same throwaway node, with tokens: the genesis wallet funds minting
addresses `2/0` and `2/1`, mints `soot` and `ash` from them, pays them to
a fresh wallet with memos, takes some back, refuses an overspend, and
keeps minting soot of the same flavor from `2/0` with no top-up, sends
included. The fresh wallet mints a `soot` of its own, a different flavor.
Checks every balance on the way. `KEEP=1` and `PORT` (default
18546) as above.

```sh
e2e/devnet-counterparty.sh
```

The same throwaway node, checking that `history` names the other side:
the genesis wallet pays a fresh wallet, which pays part of it back, and
each side's newest item, pending and then confirmed, is `from` or `to`
the other's tracking address. `KEEP=1` and `PORT` (default 18547) as
above.
