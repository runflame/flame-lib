# Mint playground

Multi-player minting simulator: a Rust web service, SQLite ledger, and a small HTML/JavaScript UI. Satoshis and rewards are simulated. It records rewards by Flame receiving address; **it does not create real UTXOs or accept Bitcoin payments**.

## Run

From this directory:

```sh
./run.sh
```

Open [the dashboard](http://127.0.0.1:3000/) or [minting page](http://127.0.0.1:3000/mint). The script builds the existing `flamewallet-wasm` browser wallet and starts the service. It needs the repository's Rust toolchain, the `wasm32-unknown-unknown` target, and `wasm-bindgen-cli` matching the repository's `Cargo.lock`. If the CLI is missing, the script prints an installation command. Browser wallet generation needs a secure context: localhost works; a remote deployment needs HTTPS.

The default database is `data/mint.sqlite3` in this directory. To choose a persistent file and a different listening address:

```sh
MINT_LISTEN=127.0.0.1:3001 ./run.sh /absolute/path/mint.sqlite3
```

Once browser-wallet assets have been built, `cargo run --locked` starts just the service. A binary deployed away from this source tree finds them through `MINT_WALLET_DIR`, the directory holding `flamewallet_wasm.js` and `flamewallet_wasm_bg.wasm`; the [container image](Dockerfile) sets it, along with `MINT_LISTEN`. This is a standalone crate with its own lockfile; it does not change the protocol workspace's dependencies.

```sh
cargo test --locked
node --test web/test.mjs
```

### Reset the database

Stop the service, then run:

```sh
./reset.sh
# Or choose another existing database:
./reset.sh /absolute/path/mint.sqlite3
```

The script creates a uniquely named, self-contained SQLite backup beside the database. It prints the backup's absolute path to stdout and flushes it **before** reinitializing and clearing the ledger. Progress/errors go to stderr. The next run starts at block zero with no users or contributions; browser wallets are unchanged. The service is not started automatically.

A running database owner, missing/invalid database, failed backup or failed stdout write stops the reset before data is cleared. Existing backups are never overwritten. Keep the printed backup if you want to restore the previous ledger.

## How it works

User chooses:

- budget: amount of satoshis to spend total,
- duration: how many blocks to spend it over,
- delay: how many blocks to wait before starting,
- mode: Independent, Service or Delegating.

Each contribution starts at `committed_height + 1 + delay_blocks`. Delay defaults to zero (the next block). Its slider covers 0 to 4,464 blocks; the duration slider covers 144 to 4,464 blocks, both at single-block resolution. Typed values may exceed these slider ranges. Waiting blocks incur no burn, fee, or reward.

| Mode | Budget and fees | Rewards |
|---|---|---|
| Independent | Reserves 1,000 sat per active block for fees; burns the remainder. | Keeps its reward allocation. |
| Service | Same fees and burn schedule as Independent; activates the user's service for its duration. The identity's commission defaults to 2% if none was saved. | Keeps its own allocation and earns commissions from eligible delegations. |
| Delegating | Burns the entire budget; pays no mining fees. | Pays its chosen service's current commission and keeps the rest. |

Burns use an integer quotient per block, with one extra sat in each of the first remainder blocks. Thus each complete budget is spent exactly, without fractional sats. Fees remain per active Independent/Service contribution, including overlapping contributions belonging to the same user; delegates do not add fees.

Service offers belong to the user, not one contribution, and remain available after that user's Service contributions expire. A delegate may choose any schedule; it need not fit a service contribution's window. The provider is **active only while it has at least one active Service-mode contribution**. An Independent contribution owned by the same user does not activate the service. While the provider is inactive, its delegates still burn their scheduled coins, but receive no reward, pay no commission and generate no mining fee. Those burned coins are lost, never refunded or rewarded retroactively. A later Service contribution can reactivate the provider for the remaining delegation window.

Commission is a mutable property of the provider's identity, never a contribution-specific quote. Each block fixes it, though: a delegated accrual records the provider's rate in force when its block is committed, and that per-block [delegation contract](delegation.md#per-block-delegation-contract) is what a Flame node mints. Saving a new rate applies it **from the next block on** to every delegator of that provider, including running delegations; committed blocks, balances and issuance never change. Saving is explicit and separate from contribution submission: a new contribution or stale retry cannot restore an old rate.

The budget slider runs linearly from 100,000 to 100,000,000 sat, in steps of 100,000. The adjacent text fields accept larger budgets, durations and delays independently of the slider positions; typed budgets retain single-satoshi precision. Dragging a slider replaces only that field's typed value. Currency/time switches and refreshes preserve the underlying sats/blocks. Inputs are bounded by JavaScript's exact integer ceiling, `9,007,199,254,740,991`; the exclusive end block must also fit. Independent/Service submissions require `budget >= duration × 1,001`; Delegating requires at least one burned sat per block without the fee reserve. Invalid combinations are explained and cannot be submitted. Sliders retain their original limits.

Blocks advance once per second while the service is running. Restarting resumes at the last committed height; downtime does not produce blocks or retroactive rewards. A block with eligible burned coins distributes exactly 50 FLM in proportion to those burns, excluding fees. Independent/Service burns are eligible; delegated burns are eligible only when their provider is active. Inactive delegations remain part of total spending, but not the reward denominator. If no eligible burn exists, that block's 50 FLM are permanently lost: never accumulated, carried forward, or awarded to later minters. Scheduled contributions that have not started and expired contributions do not count as active. One FLM is 100,000,000 sparks. Allocation uses integer arithmetic: floor each exact share, then assign leftover sparks to largest fractional remainders, breaking ties by contribution ID. Commission then splits each eligible delegate's allocation between its owner and provider without creating or losing sparks. Charts and estimates use actual known contribution and service-activity windows, including future schedules; unknown future submissions are not predicted.

The blocks / days switch changes display only and is remembered independently of currency. Days use 144 blocks/day: block 1 begins January 11, 2027, and block 145 begins January 12. Calendar labels use UTC (`Jan 11`, `Mar 13`); durations and delays are shown in days while controls, requests, and storage retain integer blocks. This is an accelerated simulation calendar, not wall-clock scheduling.

Both pages have − / + chart zoom controls, initially showing ±500 blocks around now. `−` doubles the range on each side; `+` halves it, rounding to whole blocks, down to a minimum of ±10. There is no preset maximum range beyond JavaScript's exact-integer ceiling. Wide views render schedule-change boundaries rather than allocating a sample for every block, retaining short contributions and fee spikes. Zoom is remembered independently of time and currency display; the old fixed-step preference is superseded so the new default takes effect. Zoom changes only the chart, not contribution settings, estimates or the latest-200 list limit.


The existing Flame wallet runs in the browser via WebAssembly and uses FlameKD to generate a mnemonic and testnet receiving address (`t1…i1…`; see [address formatting](../../docs/address.md)). These are remembered in browser local storage, so subsequent contributions share the same reward destination. **The mnemonic, seed and private keys never go to the service or database.** Request parsing rejects extra fields, including mnemonic/private-key fields. Only the address and contribution parameters are submitted. The server validates the address with FlameKD and stores its canonical lowercase encoding.

Local storage is only suitable for this demo. Never reuse this mnemonic for real funds; clearing browser data loses it, and scripts running on the same origin can read it. The server cannot recover it. Address identity is public attribution, not authentication: this simulation does not prove address ownership, debit a real balance, or prevent someone submitting simulated contributions to another address. In particular, anyone knowing a public address can change that identity's service commission for later blocks through the unauthenticated profile API. Keep the default loopback binding unless intentionally sharing the demo. A real paid deployment needs authentication for identity/rate changes, payment verification and abuse controls.

## Data model

One dedicated thread owns the SQLite connection. HTTP requests and block ticks go through its bounded command queue; it never shares a connection behind a mutex or blocks an async worker with SQL. SQLite foreign keys, strict integer columns and checked arithmetic protect accounting. Block processing is one transaction: block record, contribution accruals and accumulated address rewards either all commit or all roll back. A tick error stops the service rather than silently skipping accounting.

The connection holds an exclusive file lock: only one service may own a database, preventing accidental double-speed minting from two processes. External SQLite readers are also blocked while it runs; use the HTTP API, or stop the service before inspecting/copying the database. Excess HTTP requests receive a retryable busy response; the internal clock waits for queue capacity instead of shutting down under load.

| Table | Stored data | Meaning |
|---|---|---|
| `accounts` | `id` (integer primary key), unique `address`, optional `name`, optional `service_commission_bps`, `reward_sparks` | One user per canonical receiving address, with a display name, persistent service terms and materialized lifetime rewards. No private key or mnemonic. |
| `contributions` | `id`, unique `request_id`, `user_id` (foreign key to `accounts.id`), `budget_sats`, `duration`, `delay_blocks`, `start_block`, `mode`, optional `service_user_id` | Immutable submission, burn schedule and selected provider. A delegate references a provider identity, not a service contribution or stored rate. Several contributions and modes may belong to one user. |
| `blocks` | `height` (primary key), `burned_sats`, `fees_sats`, `reward_sparks` | Committed simulator clock and per-block totals, including empty blocks. |
| `accruals` | `(height, contribution_id)` (primary key), `burned_sats`, `fees_sats`, `reward_sparks`, `commission_sparks` | Per-block spending and immutable gross reward allocation, with a derived commission recomputed when the provider's rate changes. The owner's net is gross minus commission. Inactive delegates have burn accruals with zero reward and commission. |

Relationships: an account has many contributions; a contribution and a block each have many accruals. Earned amounts and fees on the contribution table are derived from accruals, not maintained as another mutable copy. `accounts.service_commission_bps` is the only canonical rate (100 basis points = 1%). Each delegated accrual stores `commission_bps`, the provider's rate when its block was committed, and its commission `floor(gross_reward_sparks × commission_bps / 10,000)`, with the remainder retained by the owner. The account total is deliberately materialized: own net accruals plus incoming commissions. A rate change touches neither accruals nor balances; gross allocations and the total number of sparks never change. All money is stored as SQLite signed 64-bit integers, never floats; proportional multiplication uses 128-bit intermediates. Cumulative monetary values in JSON are decimal strings to avoid JavaScript's integer precision limit.

User IDs start at 1 and increment when a new canonical address registers its minter profile or makes its first accepted contribution. Opening the Mint page registers/loads the browser wallet's profile, allowing its default name `Minter N` to appear before it contributes. Repeat profile requests, submissions, retries and uppercase spellings reuse the same user ID. IDs persist across service restarts; they are distinct from contribution IDs. Custom names are trimmed, limited to 1–64 Unicode characters without control characters, and stored on the account; otherwise the name is derived from its stable ID. Names are cosmetic, need not be unique, and do not authorize access or change reward ownership. Like the rest of this unauthenticated demo, profile updates do not prove address ownership.

The snapshot includes owner identity, receiving address, mode, provider identity and its current commission for every contribution, plus account-level minter summaries and service offers. Response rates are derived from the identity, not stored independently on each contribution. Summaries use all history, not just the latest 200 contribution rows. A user's latest contribution determines its summary mode/color; this is presentation, not an account-wide restriction on mixed modes. Existing integer-user-ID databases are migrated without resetting the ledger. Legacy Independent schedules, IDs, gross allocations and retries remain intact. Previous per-contribution commission snapshots are replaced by the provider's current identity rate, repricing old delegated net balances consistently, and that rate becomes the recorded rate of every accrual committed before per-block rates (schema version 4). Old schemas without delays get zero delays. Historical durations below the new minimum remain valid records; the older address-keyed schema is not supported.

The submission ID is generated in the browser. Repeating an ID with identical canonical address, budget, duration, delay, mode and provider returns the original contribution and start block without creating another one. Reusing it with different parameters is rejected. Omitted delay means zero and omitted mode means Independent, including old saved requests. Matching retries of previously accepted legacy durations remain valid. Legacy commission quotes do not fix a rate or change the identity; pending browser requests discard them and follow the provider's current rate. Because submissions and block ticks share one queue, the delay is applied relative to the committed height observed by the first accepted submission, not by its retries.

A pending public request is saved locally before submission, keeping its ID and settings across network errors and page reloads. Retrying is safe. Explicitly discarding a pending request does not cancel an already-accepted contribution; the UI warns before doing so.

### HTTP API and future wallet integration

| Endpoint | Purpose |
|---|---|
| `GET /api/state` | Dashboard snapshot: height, server-computed lifetime totals/count, `minters` summaries, persistent `services` offers, latest 200 detailed contributions by descending ID (no age filter), and lightweight `chart_contributions` schedules for all contributions. |
| `GET /api/state?full_history=true` | Mint-page snapshot with complete contribution history, preserving personal lifetime totals. |
| `POST /api/minter` | JSON `{address, name?, service_commission_bps?}`; registers/loads the canonical profile or independently saves supplied name/rate fields. A rate change applies from the next block on. Omitted fields remain unchanged. |
| `POST /api/contributions` | JSON `{address, budget_sats, duration, delay_blocks, request_id, mode?, service_user_id?}`. Mode is `independent` (default), `service`, or `delegating`; Delegating requires `service_user_id`. A new Service contribution initializes an absent identity rate to 200 basis points but never overwrites a saved rate. Returns `{id, user_id, start_block}`. |
| `GET /api/rewards` | Address-sorted accumulated rewards: `[{"address":"t1…i1…","reward_sparks":"5000000000"}]`. |
| `GET /api/core/tip` | The last committed block as a Flame node's core chain sees it: `{height, time}`, `height` 0 before the first block. `time` is the simulated calendar's: block 1 at 2027-01-11T00:00:00Z plus 600 s per block. |
| `GET /api/core/blocks?from=A&to=B` | Committed blocks `A..=B` (at most 1,024, `A >= 1`), each `{height, time, burns: [{address, sats}]}` with the burns eligible for that block's reward in contribution-ID order: Independent and Service burns, and delegated burns only while the provider's Service is active. A delegated burn adds `service: {address, commission_bps}`, its provider and the rate recorded for that block. Fees and inactive delegations are excluded. |

Service offers contain `{user_id, name, commission_bps, active}`. Contribution responses expose the owner's net `reward_sparks`, outgoing `commission_sparks` and identity-derived rate, alongside owner/provider names and IDs; accrual storage retains gross rewards. Minter summaries contain lifetime own burn/fees, total account rewards including incoming commissions and contribution count; their displayed mode follows the latest contribution, while service percentages come from current identity settings.

### Connecting a Flame node

A devnet `flamed` can follow the playground as its core chain: set `playground_url = "http://127.0.0.1:3000"` in its `flamed.toml`. Each Flame block that moves to a later playground block carries those blocks' eligible burns as its mint proof and issues their 50 FLM allotments as real, spendable outputs locked to the minters' receiving addresses, using the same proportional split and tie-break as this ledger. See [flamed's Minting section](../../flamed/flamed.md#minting). Differences from this ledger:

- **Delegation.** The same: each delegated burn carries its provider and that block's rate, and the chain pays the provider `floor(share × rate / 10,000)` and the delegator the rest. Because the ledger never reprices a committed block, the chain and the ledger agree on every split.
- **Halving.** The chain halves the allotment every 210,000 core blocks, which is 58 hours at one block per second; this ledger keeps paying 50 FLM.
- **Reset.** Resetting the playground under a running node puts the core chain behind the Flame anchor, and minting stops with an error. Reset both together.
- **Network.** The node reads the playground's testnet addresses with the network in its `genesis.json`, which must be testnet.

The reward export represents **currently attributed lifetime rewards**, not a spendable balance or an instruction to pay them again. A commission change affects only later blocks, so attributed rewards only grow. This service creates no Flame UTXOs itself; a node following `/api/core/blocks` does. It uses stored receiving addresses directly and requires no private keys.

The database retains all contributions and per-block accruals; the dashboard list limit does not delete or archive records. The server selects the latest 200 before aggregating their accrued amounts. Separate chart schedules retain mode, provider and commission alongside IDs, budgets and block windows, so older service activity and delegates still affect charts and forecasts. The mint page retains full history for personal totals and estimates. Pagination, archival, authentication, real Bitcoin/Flame connectivity, cancellation and withdrawals are intentionally absent. No external assets, analytics or third-party scripts are loaded by the UI; both pages make a public price-data request for the BTC/USD ticker.

The sat / BTC / USD switch changes display only and remembers the selection in this browser. FLM counts remain whole numbers; sat amounts and rates have no decimals, BTC has exactly eight, and USD has two (three in the top total, up to six for $/FLM rates). Top counters use short currency-free labels (`Burned`, `Earned`, and `Current rate` or `Effective rate`) above values with units: `$X.XXX`, `X btc`, or `X sat`; `X FLM`; and `X $/FLM`, `X BTC/FLM`, or `X sat/FLM`, using non-breaking spaces between numbers and units. Decimal fractions are truncated for display only. Calculations, stored satoshis/sparks, submissions, and API values retain their precision. Effective rates use the untruncated reward amount, not the displayed whole-FLM value.

Both pages always show the BTC/USD price in a reserved single-line ticker. Coinbase's public [BTC/USD spot endpoint](https://docs.cdp.coinbase.com/coinbase-app/track-apis/prices) is requested without credentials on page load and refreshed once a minute, regardless of selected currency. The last valid quote stays visible during refresh; failed or invalid quotes clear it and leave USD values unavailable. Switching to USD uses the preloaded quote without a new request. The current reference quote revalues all amounts, including historical totals; these are not historical USD costs or execution prices.

## User UI

URL: `/mint`

Dashboard and Mint links are first in the top-left navigation on both pages. Only Mint shows a name field and Save button in the top bar. Save (or Enter) persists the display name to the database; refreshing or polling does not overwrite an unsaved edit. Name-service failures can be retried without disabling contributions or changing the browser wallet.

The top counters show only this user's accrued totals: own sats burned, mining fees paid, FLM earned (net rewards plus incoming commissions), and effective rate `own sats burned / FLM earned`, excluding fees. They use server-computed lifetime minter summaries, not the chart window or visible rows. Scheduled future spending and the unsubmitted draft are excluded. Effective rate is `—` until rewards are earned; all numeric values are `—` while the wallet is unavailable, with their labels and units retained.

User sees a chart of the burn rate in satoshis per block, with the equivalent mint rate (sat/FLM) on the right-hand axis.

- Chart is centered around the current block, initially ±500 blocks, with its range controlled by zoom and labels shown as block numbers or calendar dates.
- Other users' Independent/Service contributions are grey; delegations use blue. Everyone's known schedules are drawn exactly, including future starts and expirations; an inactive delegation still appears because it still burns coins.
- The user's own Independent/Service contributions use the warm user-ID palette. Own delegations and delegations linked to this user's service use the blue palette; linked contributions represent their owner's burn, not extra spending by the provider.
- The draft is a dashed line in the corresponding mode color and changes with the controls. Before a user ID is known, its color is neutral grey.

Controls:

- Independent / Service / Delegating radio buttons, selected per contribution. Service shows the identity's commission field (0–100%, initially 2%) with a separate Save button. Saving changes every delegator's attribution from the next block on; typing or submitting a contribution does not save a rate. Delegating selects a named service and sees its current commission and activity state. Inactive services remain selectable with the lost-burn risk described above.
- slider "budget": 100,000 to 100,000,000 satoshis (linear scale, in steps of 100,000).
- slider "duration": 144 to 4,464 blocks (1 to 31 days), in steps of one block.
- slider "delay": 0 to 4,464 blocks (0 to 31 days), in steps of one block; default zero.
- Adjacent text fields accept larger values than the sliders, in the selected currency/time units.
- display: projected total amount of flames to be earned over the selected delayed window, total amount of satoshis spent on fees, burned sats, and resulting average mint rate (burned sats/FLM), excluding fees.
- button "submit".

For a draft starting at `H + 1 + delay_blocks`, each eligible block `b` estimates `50 FLM × draft_burn(b) / total_eligible_burn(b)`. A Delegating draft deducts its provider's current saved commission and earns zero while that service is inactive. Self-delegation keeps the gross share because the commission returns to the same account. A Service draft's own activity can enable existing linked delegates, increasing the eligible denominator, but its estimate includes only its own share, not speculative commission income. All known overlapping contributions and service windows follow their actual schedules throughout the duration, even outside the chart window. The estimator sums intervals between burn-rate and service-activity changes instead of iterating over every block, so very long durations do not stall the page. Later submissions or identity rate changes can change the result; pending requests do not freeze a commission. Dates beyond the calendar's representable range display a block number instead.

The contributions table shows own contributions and incoming delegations linked to this user's service:

- Identity: `User name #N`, then total budget / duration; a separate Minter column names the service for delegated contributions and the owner otherwise.
- Status: `Scheduled at …`, `Finished on …`, or `Active X%`; progress measures the proportion of the burn budget consumed, not elapsed wall time. Dates or block numbers follow the display switch.
- Own rows show own burned sats, net FLM earned, mining fees and burned-sats/FLM rate, excluding fees.
- Linked delegation rows show only the provider's commission as earned FLM, with zero provider burn and fees. This prevents counting delegates' spending or retained rewards as the service's own.

The total line matches the user's lifetime summary: own burn, net rewards plus commissions, fees and effective rate. The dashboard uses the same burned-coins-only rate definition.

The reward address and recovery controls appear below the contributions table. Both pages end with a row of small squares showing the participant palette in its assigned order.

Blocks are issued every 1 second. Only blocks with eligible burns distribute 50 FLM; rewards from blocks with no eligible burns are permanently lost.

Use the top-left Dashboard link to return to `/`.

## Dashboard

URL: `/`

Similar to per-user UI, but visible to everyone. The chart uses the selected symmetric zoom around the current block, including everyone's known future commitments and older schedules. Contribution summaries show their budgets, durations, and schedules, including future starts. The server returns at most the latest 200 detailed contributions by ID, newest first, regardless of block age. Older contributions remain in chart schedules and lifetime totals, but not in the table. Calendar mode uses the same selected block window.

The top counters and table footer use server-computed lifetime totals, including contributions omitted from the response. The footer's contribution count is also all-time, not just the number of visible rows.

The dashboard chart, "Burn rate and mining fees", shows historical and scheduled burned sats above zero and mining-fee income downward: 1,000 sat per active Independent/Service contribution per block, including overlapping contributions to the same address. Delegations incur zero fees even when their service is inactive. Fees use a grey diagonal fill. Both areas share one scale in sat/block on the left; the right-hand scale is the same values divided by 50 FLM (sat/FLM), including negative ticks below zero. The downward direction separates fees from burning, not a loss of income. Both charts mark the current block and distinguish their future portion; `/mint` uses the same axis units without the fee area. Plotted burn includes inactive delegations; it is spending, not a guarantee of reward eligibility.

The dashboard always colors contributions by user and mode, with no color toggle. Independent/Service use the existing twelve warm colors: six originals plus rounded RGB midpoints, ordered for adjacent perceptual contrast. Delegating uses the twelve blue shades specified in [delegation.md](delegation.md). Both fixed palettes are shared by all clients and indexed by `user_id % palette.length`; colors repeat without randomization or browser-local assignment. Participant colors are opaque; mining fees remain grey and hatched. The palette footer displays the assigned colors.

The dashboard groups contributions into user/mode color bands, keeping the newest contributions above older ones. The contribution table remains newest-first and names both the owner and the executing minter. Delegator reward amounts are net of commission.

A sortable minter table summarizes every contributing user by name, own total burn, total rewards (including incoming commission), mining fees and effective rate. The current identity-level Service commission appears beside the name, not as an extra column. Numeric sorts use raw accounting values rather than formatted currency strings. A user's latest contribution selects its displayed mode and warm/blue color; existing contributions retain their own modes and provider links, never individual rates.

On very top show four totals/rates:

- total satoshis burned from the beginning,
- total mining fees paid from the beginning,
- and total flames distributed,
- current mint rate: this block's total burned sats divided by 50 FLM, excluding fees; `—` when no eligible burn exists and no reward is issued. This includes lost spending from inactive delegations whenever other eligible minters do issue the reward. It is not the lifetime average.

Use the top-left Mint link to open `/mint`.
