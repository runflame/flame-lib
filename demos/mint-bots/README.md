# Mint bots

Two automated minters for the [mint playground](../mint-playground/README.md), giving it background activity during a demo. They use the playground's public HTTP API, the same one its pages use, so they show up there like any other minter. A control page next to them turns them on and off, changes their settings and shows what they did. Like the playground, they deal only in simulated satoshis. Their FLM rewards are real devnet payouts when the playground pays out on chain.

- **Clock bot** fills blocks nobody else mints in, so their reward is not lost. When the next block with no Independent or Service contribution is within its lookahead (30 blocks by default), it submits a minimal Independent contribution: 1 sat burned per block plus the 1,000-sat fee, over 144 blocks. It starts a few blocks early, because the playground counts a delay from its height when it accepts the submission. Delegations alone do not count as minting: they are eligible only while their service burns.
- **Minimax bot** is a service with a finite budget, spread over a window of time (a week by default). Its pace allows it an even share of the budget by each moment. Each poll, it looks for the window in which one chunk (an hour's share) buys the most FLM. It weighs 144 to 4,464 blocks, starting up to its lookahead (600 blocks) ahead, given every schedule the playground knows. It spends a chunk when its pace allows it and the best price, in sats per FLM with fees included, is at most its going rate: an average of recent best prices with a one-hour half-life. Once it falls four chunks behind its pace, it takes the best window on offer at any price. Delegations to it count as competition in its price, since they share the same rewards; its commission on them is not counted.

## Run

Against a playground on `127.0.0.1:3000`:

```sh
mkdir -p data && cargo run --locked -- --new-key > data/phrase   # once; a fresh 24-word phrase
MINT_BOTS_MNEMONIC_FILE=data/phrase cargo run --locked
```

Then open [the control page](http://127.0.0.1:3100/). Both bots start paused. The default journal is `data/bots.sqlite3` in this directory; pass another path as the only argument.

| Variable | Default | |
|---|---|---|
| `MINT_BOTS_MNEMONIC_FILE` | required | A file holding the bots' BIP-39 phrase, such as a podman secret. |
| `MINT_BOTS_PLAYGROUND` | `http://127.0.0.1:3000` | The playground, plain HTTP: it is reached on loopback. |
| `MINT_BOTS_LISTEN` | `127.0.0.1:3100` | The control page and its API. |

The phrase's first two testnet receiving addresses, `m/35263'/1'/0'/0/0` and `…/0/1`, are the clock bot's and the minimax bot's. A wallet restored from the phrase (`flamewallet restore` on testnet) owns them and can spend what the playground pays them. `--new-key` prints a fresh phrase and nothing else, for piping into a secret store: `mint-bots --new-key | podman secret create <name> -`.

```sh
cargo test --locked
```

## Access

The control page has **no authentication of its own**. Anyone who reaches it can start, stop and reconfigure the bots. Bind it to loopback (the default) and publish it only behind an access gate, such as a Cloudflare Access application. Writes are JSON-only, with no CORS, and refuse cross-origin requests, so another site cannot use a signed-in browser to change settings.

## Journal

The journal is a SQLite file that records what the playground's ledger does not:

| Table | Contents |
|---|---|
| `events` | Every start and stop, settings change (with old and new values), registration, contribution (with the bot's reasons: price, going rate, expected FLM, pace), refusal, ledger reset, error and recovery. A run of identical failures is one `error` event when it starts and one `recovered` event, with its count, when it ends. |
| `submissions` | Each contribution as the bot meant it, written **before** it is sent. After a crash or outage the same request ID is retried, and the playground answers with the original contribution if it already took it, so a contribution is never made twice or lost from the budget. |
| `samples` | One telemetry row per bot per minute: committed sats, budget and pace, the playground's lifetime burned/fees/rewards for the bot, its own burn, burn delegated to it and from how many delegators, all budgets delegated to it, and everyone's eligible burn. |
| `settings` | The bots' settings, the ledger count and the last height seen. |

Nothing is trimmed. A week is about 20,000 samples and a few thousand events: a few megabytes on disk. Memory use does not grow with them: the page reads at most 500 samples per chart (thinned evenly) and 100 events per page. Payouts are not recorded; they are on chain.

Like the playground, the service holds an exclusive lock on its journal, so a second copy cannot run against it. Stop it before copying or inspecting the file.

### Ledger resets

When the playground's height goes backwards, its ledger was reset (a new demo). The bots journal a `reset` event, register again, drop any submission still pending for the old ledger, and count a new ledger. The minimax bot's budget applies to each ledger separately, counting what it committed since its window opened.

## API

| Endpoint | |
|---|---|
| `GET /api/status` | Settings and the latest poll: height, ledger count, per-bot identity, live numbers, pace, best window, going rate, pending submission and current failure. |
| `GET /api/samples?bot=clock\|minimax&since=<unix ms>` | Up to about 500 samples, thinned evenly. |
| `GET /api/events?before=<id>&filter=clock\|minimax\|errors` | 100 events, newest first. |
| `PUT /api/config` | The whole settings object, as `/api/status` returns it; validated, journaled with its changes, applied on the next poll. |

The bots poll the playground every 5 seconds: one `GET /api/state`, plus a registration or contribution when they act. Changing the minimax bot's commission reprices every delegation to it, past ones included, as the playground does for anyone.
