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
which influence derives. VM flavor `0` identifies Flame but does not define its
issuance.

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

## Governance

Governance is **TBD**. It must enumerate mutable parameters, legal ranges,
proposal and activation delays, electorate, voting threshold, emergency powers,
and rollback policy. It must cover membership, block limits, virtual-byte
issuance, fees, versions, and cryptographic deprecation.

VM bytecode, encoding, hash domains, state transitions, and signature payloads
are consensus surfaces. Upgrades must activate deterministically by height or
epoch; implementations must reject unknown active versions rather than guess.

## Decisions required before implementation

1. Minting and Bitcoin coupling.
2. Validator/minter eligibility and rotation.
3. BFT family, threshold, certificates, and view change.
4. Finality and weak subjectivity.
5. Rewards, penalties, and equivocation handling.
6. Governance electorate, thresholds, delays, and bounds.
7. Network and state-sync trust model.
