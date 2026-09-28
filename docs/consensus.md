# Flame consensus

## Status

This is the canonical location for minting, BFT, and governance. None is
implemented in the retained repository, and the legacy tree contained no settled
protocol. Requirements are recorded here; unresolved choices remain **TBD**.

## Required properties

Consensus must provide one deterministic block order, explicit Byzantine safety
and liveness assumptions, authenticated domain-separated proposals and votes,
finality and reorganization rules, deterministic membership, replay protection,
bounded state-machine resources, and deterministic protocol upgrades.

## Minting

Flame is intended to run alongside Bitcoin and may use Bitcoin burn, work, or
chain data in issuance or minter selection. The mechanism is **TBD**. It must
specify proposer eligibility and selection; Flame issuance schedule and rounding;
any authorizing Bitcoin proof; maturity and Bitcoin-reorg handling; rewards,
fees, penalties, and equivocation consequences; and the scarce resource from
which influence derives. VM flavor `1` (`FLAME_FLAVOR`) identifies Flame but
does not define its issuance.

## BFT

No BFT family has been selected. Before implementation, choose one protocol and
define its validator set and Byzantine threshold, proposal and vote messages,
round/view changes, quorum certificates, locking rules, leader selection,
genesis and membership changes, synchronization, finality, weak subjectivity,
and partition recovery. A generic HotStuff/Tendermint hybrid is not a design.

Until these choices and conformance vectors exist, no block can claim BFT
finality.

## Ordering

Consensus orders external transactions. The state machine derives internal
message order from external order and ordered send effects. Proposers may select
and order valid transactions within block limits. Fair ordering, encrypted
mempools, and MEV controls are not specified.

Minter transaction selection should account for the lower parallelism of actor
execution without changing FlameVM gas or block validity. Let
`direct_send_gas` be the sum of Message gas grants in an external transaction's
TxLog. Because those grants are already included once in `gas_used`, the local
selection score uses:

```text
effective_gas = gas_used + (internal_gas_weight - 1) * direct_send_gas
feerate       = total_fee / effective_gas
```

The default `internal_gas_weight` is 4, approximating the wall-clock difference
between parallel external verification and dependency-heavy internal actor
execution on a typical four-core node. Only direct sends are counted: nested
sends spend the already-reserved grant and must not be counted twice. The weight
is minter policy, not a validity rule, so it can be recalibrated without a
protocol fork. The current FIFO mempool remains unchanged until block selection
is dependency-aware; globally sorting FIFO entries could place a transient child
before its parent.

## Governance

Flame follows the Bitcoin style of minimal governance. The protocol offers slow super-majority agreement mechanism for
adjusting several hard limits and signal soft-fork expansion of the VM to allow expansion.

* Gas and size limits per block.
* Actor storage allocation rate.

## Decisions required before implementation

1. Minting and Bitcoin coupling.
2. Validator/minter eligibility and rotation.
3. BFT family, threshold, certificates, and view change.
4. Finality and weak subjectivity.
5. Rewards, penalties, and equivocation handling.
6. Governance electorate, thresholds, delays, and bounds.
7. Network and state-sync trust model.
