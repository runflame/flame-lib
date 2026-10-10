# Actor storage

This document defines the consensus rules for persistent actor storage. VM
opcode encodings and stack behavior are specified in
[flamevm.md](flamevm.md).

The `flamechain` actor registry and the FlameVM storage opcodes implement this
lease model.

Actors lease storage by burning Flame. Storage is not a token: a lease cannot
be transferred, shortened, or refunded. Every lease lasts exactly one storage
year, and its bytes return to the global pool only when the lease expires.

## Units and parameters

All byte quantities exposed to FlameVM are literal bytes. Lease purchases and
the global pool are restricted to whole `STORAGE_UNIT_BYTES` units.

| Parameter | Initial value |
| --- | ---: |
| `SPARKS_PER_FLAME` | `100_000_000` |
| `STORAGE_UNIT_BYTES` | `1_024` |
| `INITIAL_POOL_BYTES` | `128 * 1_024 * 1_024 = 134_217_728` |
| `INITIAL_POOL_UNITS` | `131_072` |
| `LEASE_DURATION_CORE_BLOCKS` | `52_500` |
| `ISSUED_BYTES_PER_CORE_BLOCK` | `8 * 1_024 = 8_192` |
| `ISSUED_UNITS_PER_CORE_BLOCK` | `8` |
| `MIN_LEASE_BYTES` | `1_024` |
| `MIN_REMAINING_POOL_BYTES` | `1_024` |
| `LEASE_RECORD_BYTES` | `16` |
| `INITIAL_PRICE_SPARKS_PER_UNIT` | `10 * SPARKS_PER_FLAME = 1_000_000_000` |

One Flame is divisible into `100_000_000` sparks, analogous to satoshis in
Bitcoin. All Flame token quantities and storage fees are integral sparks.

A core-block height is the protocol's Bitcoin-coupled block counter. Lease
duration and expiry use that counter, never timestamps; every internal
transaction executed in one core block observes the same height.

The fixed price numerator is derived from the initial conditions:

```text
STORAGE_PRICE_PRODUCT
    = INITIAL_POOL_UNITS * INITIAL_PRICE_SPARKS_PER_UNIT
    = 131_072_000_000_000 sparks * units
```

These names identify consensus parameters. Their initial values may be tuned
before launch; changing one on a live network requires an explicit protocol
upgrade. `STORAGE_PRICE_PRODUCT` is derived once from the activated initial
values and does not change as bytes are issued, recycled, or purchased.

The exact global unit price is represented by two integers:

```text
price_dividend = STORAGE_PRICE_PRODUCT
price_divisor  = available_storage_units
unit_price     = price_dividend / price_divisor
```

Only the divisor changes. No rounded or floating-point base price is stored.
Storage units, byte amounts, expiry heights, and the available pool are
consensus `u64` values; every conversion and update is checked.

## Global state and block order

The chain state contains:

- `available_storage_units`, the unleased reserve;
- a derived expiry index containing every outstanding lease; and
- for each actor, an ordered list of `(expiry_height, units)` leases.

Leases with the same actor and expiry height must be coalesced. The expiry index
survives actor destruction because destroying an actor does not refund its
unexpired leases. It is a cache derived from the committed per-actor lease
lists; block validation checks that both representations match before commit.

At the beginning of core block `h`, before executing transactions:

1. Remove leases whose `expiry_height == h` and return their units to the pool.
2. Mark every actor whose expiring lease leaves its capacity below usage for freezing.
   Such actors cannot execute in this block.
3. Add `ISSUED_UNITS_PER_CORE_BLOCK` to the pool.

Storage purchases then execute serially in transaction order and update the
pool immediately. This makes every quote a deterministic function of preceding
block execution.

At the end of the block, marked actors are frozen in lexicographic actor-ID
order. Their resident code/state bodies are removed, but identity, code/state
CellIDs, size metadata, and outstanding leases remain. No system internal
transaction is generated and no token or other linear value is retired.
Unexpired leases recycle only at their original expiration heights.

A later transaction can recover missing bodies from its committed execution
Cell index. This is not redeployment: the existing actor identity and linear ownership
remain intact. The current implementation retains registry metadata and both
code/state roots, not literally one 32-byte record. Explicit self-destruction
after dismantling checked-out state remains a separate operation.

## Actor usage and capacity

Actor usage is measured deterministically as:

```text
usage = sum(canonical Cell record bytes in the resident code/state graph)
      + LEASE_RECORD_BYTES * number_of_lease_records
```

Shared resident bodies are counted once within one actor's code/state graph.
Pruned descendants occupy only reference hashes in their resident parents;
their absent bodies are not charged. A fully frozen actor with no remaining
leases has zero charged usage, although its registry metadata remains.

Each coalesced lease record retains the policy charge for two canonical `u64`
fields: expiry height and storage units. Registry/header and lease-Trie wrapping
are not additionally charged; bounding the permanent metadata of frozen actors
remains a node/protocol design concern rather than a claim that it is free.

An actor's capacity at core-block height `h` is:

```text
capacity(h) = STORAGE_UNIT_BYTES
            * sum(lease.units where lease.expiry_height > h)
```

Expiration is exclusive: bytes from a lease expiring at height `E` are not
available during block `E`.

An existing actor must satisfy `usage <= capacity(current_height)` whenever its
state is committed with `save`. A newly constructed actor may execute
provisionally so it can purchase its first lease, but its creating transaction
commits only if the same invariant holds at the end. Failure rolls back the
actor, purchases, burns, and all other transaction effects.

Execution memory does not count toward persistent usage, and storage capacity
does not grant execution RAM. FlameVM charges variable-size allocation work to
the active frame's gas budget. A provisional constructor therefore needs gas,
not a storage-derived bootstrap allowance, to execute `addstorage`. The
transaction-end `usage <= capacity` check remains unchanged. Fetching a missing
body into the current execution is not a storage purchase or persistent restore.

### Content, availability, and recovery

Actor layouts and their ordered lease Tries are specified in
[encoding.md](encoding.md). `code_root` and `state_root` are ordinary CellIDs
for native executable code and one encoded Value. The actor registry commits
both its actor-layout root and the exact resident graph's `snapshot CellID`.

The VM resolver is scoped to the current actor's resident code/state body set
and the initiating external transaction's frozen execution Cell index. Actor-layout
and lease-index metadata belong to the registry commitment and archival export,
not this implicit execution source. Instruction-boundary scope refreshes share
the immutable body set rather than copying or rebuilding it. Loaded bodies do not
silently become rent-bearing. `save` and `setcode` retain newly constructed
reachable bodies and previously owned reachable bodies; they do not union the
external witness bag into actor storage. Removed branches cease to contribute
to stored availability or rent. Partial Dicts retain their authenticated
portability and linearity metadata when their descendants are absent.

Storage reads decode canonical Cells even when an in-memory Value cache exists.
This prevents earlier executions' private witnesses or loaded Dict branches
from changing behavior after a restart. Each logical root/continuation access
still goes through the metered resolver. Failure and re-entrancy checks preserve
checkout ownership: successful `load` checks out the state once, `save` requires
that checkout, and enclosing call/transaction rollback restores it.

`Blockchain::actor_storage` returns a read-only `StoredActor` snapshot containing
the layout root and exact resident Cell index for archival and witness construction.
Expiry freezing is implemented; a dedicated VM operation for arbitrary partial
pruning or persistent restoration is not yet provided.

## Pricing

Let:

- `R` be `available_storage_units` immediately before an operation;
- `q` be the requested byte amount;
- `Q = q / STORAGE_UNIT_BYTES` be the requested number of units; and
- `K` be `STORAGE_PRICE_PRODUCT`.

The current marginal price of one unit is the exact rational `K / R`. A quote
uses the reserve after the requested purchase:

```text
R_after    = R - Q
fee_sparks = ceil(Q * K / R_after)
```

Equivalently, the quoted unit price is `K / R_after`, multiplied by `Q` and
rounded up once. Implementations must use checked wide intermediates and the
identity `ceil(a / b) = a / b + (a % b != 0)`; floating-point arithmetic is
forbidden.

A request is unavailable when:

- its byte amount is less than `MIN_LEASE_BYTES`;
- its byte amount is not a multiple of `STORAGE_UNIT_BYTES`;
- `R_after` would be below `MIN_REMAINING_POOL_BYTES / STORAGE_UNIT_BYTES`;
- the byte amount, pool update, or expiry height does not fit its consensus
  `u64` representation;
- an intermediate calculation overflows; or
- the resulting positive fee cannot be represented as a `Scalar` with a
  non-negative centered `ClearToken` quantity.

The final unit is therefore never purchasable: its quoted price is
mathematically unbounded.

### Default quote vectors

These exact values pin rounding and the reserve boundary for the initial
parameters:

| Request from the initial pool | Reserve after | Fee (sparks) | Result |
| ---: | ---: | ---: | --- |
| `1_024` bytes at height `10` | `131_071` units | `1_000_007_630` | lease expires at height `52_510` |
| `134_216_704` bytes | `1` unit | `17_179_738_112_000_000_000` | available |
| `134_217_728` bytes | `0` units | — | unavailable |

Storage-purchase effects use the TxEntry Cell encoding in
[encoding.md](encoding.md): the expected actor identity, byte amount, expiry,
and fixed-width Scalar fee are committed by the transaction-log Trie. Actor
registry roots likewise use Cell Tries, including lease-only tombstones.
Tests cover exact price vectors, content/availability commitments, canonical
public storage reads, expiry freezing, and checkpoint/reorganization recovery.

This endpoint-price formula is deliberately **not path independent**. Splitting
one large request into many minimum-size purchases pays the successive marginal
prices and is cheaper than pricing the whole batch at its final, highest price.
That is equivalent to adopting a discrete harmonic marginal-price curve, not a
sybil-resistant bulk premium. If bulk and split purchases must cost exactly the
same, this formula must be replaced before activation by a state-potential
delta; per-actor or per-transaction purchase limits do not solve the sybil case.

## Purchasing and quoting

Only an actor may purchase storage. It may fund itself from Flame in its state,
its call arguments, or a delivered message. There is no transferable virtual-
byte asset and callers do not allocate storage directly to callees.

For a valid byte request `q`, [`quotestorage`](flamevm.md#quotestorage) returns
the positive integral `fee_sparks` without changing state.
[`addstorage`](flamevm.md#addstorage) recomputes the same quote, immediately
deducts the units from the pool, and adds a lease expiring at:

```text
current_height + LEASE_DURATION_CORE_BLOCKS
```

It emits a storage-purchase effect and returns
`ClearToken(-fee_sparks, FLAME_FLAVOR)`. The transaction must balance that debt
with actual Flame. A successful storage-purchase effect burns the balanced
amount irrevocably; minters do not collect it. `retire` rejects clear tokens
whose quantities have a negative centered interpretation, so the debt cannot
be erased instead of balanced.

`quotestorage` is advisory, not a reservation. Any intervening successful
purchase changes the reserve and therefore the result of a later `addstorage`.
Both operations have the same request, reserve, arithmetic, and representation
checks. Insufficient Flame is not an opcode failure: an unbalanced transaction
fails at finalization and rolls the purchase back.

## Introspection

Actors can inspect their lease schedule without receiving the underlying list:

```text
height             -> h
usage              -> bytes
h capacity         -> bytes
q quotestorage     -> { fee_sparks 1 | 0 }
q addstorage       -> { flame_debt 1 | 0 }
```

`height` returns the current core-block height during an internal transaction
and zero throughout an external transaction. `usage` returns the current
actor's charged usage. `capacity(h)` returns its capacity at current or future
height `h`. Scalar residues outside the `u64` height range hard-fail `InvalidBitrange`, and a
height below the current block hard-fails `StorageHeightInPast`. `usage` and
`capacity` require actor context and a registry.

When state is checked out by `load`, `usage` continues to describe the checked-
out committed state until `save` supplies its replacement. `save` measures the
replacement before committing it and fails if it exceeds current capacity.
