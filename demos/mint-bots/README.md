# Mint bots

Three automated minters for the [mint playground](../mint-playground/README.md), giving it background activity during a demo. They use the playground's public HTTP API, the same one its pages use, so they show up there like any other minter. A control page next to them turns them on and off, changes their settings and shows what they did. Like the playground, they deal only in simulated satoshis. Their FLM rewards are real devnet payouts when the playground pays out on chain.

- **Alice, the clock bot,** fills blocks nobody else mints in, so their reward is not lost. When the next block with no Independent or Service contribution is within its lookahead (30 blocks by default), it submits a minimal Independent contribution: 1 sat burned per block plus the 1,000-sat fee, over 144 blocks. It starts a few blocks early, because the playground counts a delay from its height when it accepts the submission. Delegations alone do not count as minting: they are eligible only while their service burns.
- **Bob, the minimax bot,** is a service with a finite budget, spread over a window of time (a week by default). Its pace allows it an even share of the budget by each moment. Each poll, it looks for the window in which one chunk (an hour's share) buys the most FLM. It weighs 144 to 4,464 blocks, starting up to its lookahead (600 blocks) ahead, given every schedule the playground knows. It spends a chunk when its pace allows it and the best price, in sats per FLM with fees included, is at most its going rate: an average of recent best prices with a one-hour half-life. Once it falls four chunks behind its pace, it takes the best window on offer at any price. Delegations to it count as competition in its price, since they share the same rewards; its commission on them is not counted.
- **Charlie, the commission farmer,** is a service that lives on its delegators. Its offer is a low commission (1% by default). Delegating pays no fees, so at commission *c* it beats minting alone for anyone burning less than 1,000 × (1 − *c*) / *c* sat a block: 99,000 at 1%. A service is a pool, though. Its 1,000-sat fee a block is fixed, and Charlie earns it back only when its commission on all delegations exceeds that fee: delegated burn over 1,000 / *c*, 100,000 sat a block at 1%. No single delegator below the break-even reaches that, so it takes a crowd. Charlie therefore stays **closed**, spending nothing, until enough players (2 by default) other than the bots mint Independent below its break-even. Those are the players who would gain by delegating. Then it **opens**: it keeps a minimal Service contribution running (1 sat burned per block plus the fee) and adds the next one when its service is within its lookahead (60 blocks) of ending. It closes again once they have been gone for a while (3,600 blocks by default). While it is closed, empty blocks are Alice's. Open or closed, Charlie covers every delegation to it in advance, to the delegation's end, a day of blocks at a time, so a delegator's burn is never ineligible while Charlie is enabled. A paused Charlie covers no new delegations. While closed, the playground lists Charlie as an inactive service with a zero estimate, and someone delegating to it with no delay loses the blocks until its next poll, a few seconds. Every opening and closing is journaled with the player count. Commission is retroactive in the playground: changing it reprices every past delegation.

Each bot has an ID (`alice`, `bob`, `charlie`) and a display name, initially Alice, Bob and Charlie, that the control page can change. The roster is `ROSTER` in [`src/config.rs`](src/config.rs). To add a bot, append an entry: its place in the roster is its key index, so entries are never reordered or removed. A bot with a new strategy also needs a `Kind`, its settings, a planning function in [`src/strategy.rs`](src/strategy.rs), its arm in `Runner::step`, and a `strategy-<kind>` template in [`web/index.html`](web/index.html). A journal saved with an older roster keeps the settings that still fit and gives new bots their defaults.

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

The phrase's testnet receiving addresses `m/35263'/1'/0'/0/n` are the bots', in roster order: Alice's is `…/0/0`, Bob's `…/0/1`, Charlie's `…/0/2`. A wallet restored from the phrase (`flamewallet restore` on testnet) owns them and can spend what the playground pays them. `--new-key` prints a fresh phrase and nothing else, for piping into a secret store: `mint-bots --new-key | podman secret create <name> -`.

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
| `samples` | One telemetry row per bot per minute: committed sats, budget and pace (Bob), the playground's lifetime burned/fees/rewards for the bot, its own burn, burn delegated to it and from how many delegators, all budgets delegated to it, and everyone's eligible burn. |
| `settings` | The bots' settings, the ledger count and the last height seen. |

Nothing is trimmed. A week is about 20,000 samples and a few thousand events: a few megabytes on disk. Memory use does not grow with them: the page reads at most 500 samples per chart (thinned evenly) and 100 events per page. Payouts are not recorded; they are on chain.

Like the playground, the service holds an exclusive lock on its journal, so a second copy cannot run against it. Stop it before copying or inspecting the file.

### Ledger resets

When the playground's height goes backwards, its ledger was reset (a new demo). The bots journal a `reset` event, register again, drop any submission still pending for the old ledger, and count a new ledger. Bob's budget applies to each ledger separately, counting what it committed since its window opened.

## API

| Endpoint | |
|---|---|
| `GET /api/status` | Settings by bot ID, each tagged with its `strategy`, and the latest poll: height, ledger count, per-bot identity, live numbers, pace, best window, going rate, service and delegation ends, cheapest rival, pending submission and current failure. |
| `GET /api/samples?bot=<id>&since=<unix ms>` | Up to about 500 samples, thinned evenly. |
| `GET /api/events?before=<id>&filter=<bot id>\|errors` | 100 events, newest first. |
| `PUT /api/config` | The whole settings object, as `/api/status` returns it; validated, journaled with its changes, applied on the next poll. |

The bots poll the playground every 5 seconds: one `GET /api/state`, plus a registration or contribution when they act. Changing Bob's or Charlie's commission reprices every delegation to it, past ones included, as the playground does for anyone.
