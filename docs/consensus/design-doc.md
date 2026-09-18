# Consensus Design

This document contains design docs for the Flame Consensus.

## 1. Overview

Flame Consensus consists of two parts: BFT and Minting. BFT produces blocks, which are then settled in the
Bitcoin via Minting. Minting is a process of voting for Flame candidate blocks in the Bitcoin chain via creating special
Bitcoin transactions by burning Bitcoins. Burned Bitcoins are translated into the Minting Power, which is then used to
determine main chain.

### Consensus Layers

Consensus in Flame consists of two layers: BFT and Minting.

BFT purpose is to allow Minters to agree on created blocks. BFT does not participate in the finality of the network,
and blocks created by the BFT are not final by any means.

Some blocks produced by the BFT are called Core Blocks. Those are blocks that are determined to be minted in the
following Minting process.

Minting is a process of voting for Core Blocks. When Minter mints a block, it first needs to acquire Minting
Power, and then vote for Core Blocks in each Bitcoin block. All transactions performed by the minter occurs in the
Bitcoin chain.

### Consensus Weight

Consensus weight is represented by Minting Power. Minting Power is acquired by burning BTC and is used both as BFT
voting weight and as Minting voting weight.

### Participants

There are two forms of participating in a Consensus:

- A Minter is an entity that owns Minting Power and may vote for Core Blocks through Minting.
- A Validator is a Minter eligible to participate in BFT.

Participation in BFT is not required to produce or mint a valid Flame block.

### Consensus Flow

1. In order to participate in Consensus, Minter creates Acquisition transaction in the Bitcoin Block to acquire
   Minting Power. Minter burns Bitcoins and gaining Minting Power proportional to the burnt coins.
2. BFT produces Core Block and distributed it in the network. Minters vote for the block by sending special transactions
   in the Bitcoin network.
3. After votes are settled in the Bitcoin Chain, main chain is selected. Main chain is a chain with the greatest cumulative
   Core Block weight.

BFT can produce blocks between Bitcoin blocks. However, it is not required, and system can achieve finality without
intermediate non-Core blocks.

## 2. Minting Power

Minting Power represents the consensus weight owned by a minter. It is acquired by burning BTC and is used as voting
power in both BFT and Minting.

Minting Power is temporary. Each acquisition becomes active after a maturity period, remains active for a fixed
duration, and then expires.

### Acquisition Lifecycle

An Acquisition is created at Bitcoin block height `X`.

Each Acquisition has:

- a config-defined maturity period `Z`;
- a duration `D`;
- an amount `A`, equal to the number of satoshis burned.

For a selected Bitcoin block height `Y`, the Acquisition is active if:

```text
X + Z <= Y < X + Z + D
```

Before `X + Z`, the Acquisition is maturing and contributes no Minting Power. At and after `X + Z + D`, the Acquisition
is expired and contributes no Minting Power.

### Base Minting Power

The Minting Power provided by an active Acquisition is equal to the amount of BTC burned.

A minter may have multiple active Acquisitions. Its Minting Power at Bitcoin height `Y` is the sum of all its active
Acquisitions at that height divided by their Duration, rounding down:

```text
MintingPower(minter, Y) = Σ AcquisitionAmount / AcquisitionDuration
```

The total Minting Power of the network at Bitcoin height `Y` is:

```text
TotalMintingPower(Y) =
    Σ MintingPower(minter, Y)
```

### Effective Minting Power

Minting Power defines how much voting weight a minter owns. Effective Minting Power defines how much of that weight is
assigned to a particular Minting vote.

A Core Block specifies a target Bitcoin block height `T`. If a Minting Transaction voting for that Core Block is
included at Bitcoin block height `V`, the vote delay is:

```text
delay = V - T
```

A Minting Transaction cannot vote before its target Bitcoin height, therefore:

```text
delay >= 0
```

The Effective Minting Power of the vote is:

```text
EffectiveMintingPower(minter, delay) =
    if delay > 10 then 0
    else MintingPower(minter, T) / 2^delay
```

Therefore:

```text
delay = 0  → 100%   of Minting Power
delay = 1  →  50%   of Minting Power
delay = 2  →  25%   of Minting Power
delay = 3  →  12.5% of Minting Power
delay > 10 →  0%    of Minting Power
```

Voting power is calculated at the target Bitcoin height. A delayed vote uses the Minting Power the minter had at that
target height `T`, regardless of the minter's Minting Power when the vote is later included.

### Rationale

Minting Power is backed by burned BTC so that acquiring consensus weight has an external economic cost.

Delayed votes lose Effective Minting Power so that attempts to fork chain after canonical chain is selected were
less effective.

## 3. Core Blocks

Core Blocks are Flame blocks that may receive Minting votes and therefore participate in Minting.

BFT may produce any number of regular Flame blocks between Bitcoin blocks. Only Core Blocks are visible to the Minting
mechanism; regular Flame blocks are finalized indirectly as ancestors of an anchored Core Block.

### Core Block

A Core Block is a valid Flame block explicitly marked as a Core Block by the protocol.

Each Core Block contains a target Bitcoin block height `T`. It determines the earliest Bitcoin block in which the Core
Block may receive a valid Minting vote and the Bitcoin height at which Minting Power for that vote is evaluated.

For a Core Block with a target Bitcoin height `T`, a Minting Transaction voting for that Core Block may only be included
at the height `T` or higher. A vote included before `T` is not a valid vote for that Core Block. If the vote is included
after `T`, the delayed-vote penalty defined in the Minting Power section is applied.

### Regular Flame Blocks

Regular Flame blocks and Core Blocks belong to the same Flame block tree and follow the same general block validity
rules.

A Core Block may have regular Flame blocks as ancestors:

```text
Core A -> Regular -> Regular -> Core B
```

A Minting vote for `Core B` votes for `Core B` as an anchoring point. If the chain containing `Core B` becomes
canonical, all valid Flame blocks on the path from its previously selected ancestor to `Core B` become part of the
canonical chain as well.

### Rationale

Core Blocks provide explicit anchoring points for Minting votes.

If minters were allowed to vote for any Flame block, voting power could become fragmented across many nearby blocks. For
example, one block could receive 1 unit of Minting Power while another block on the same chain receives 1 000 000.

This would make the meaning of anchoring much less clear: it would be difficult to determine which blocks should be
considered confirmed and where the tip of the chain is.

Core Blocks reduce this ambiguity by restricting Minting votes to explicitly selected anchoring points. All intermediate
Flame blocks are selected together with the Core Block that contains them as ancestors, while consensus weight is
concentrated on a smaller set of well-defined checkpoints.

## 4. Minting

Minting is the mechanism through which Minters cast authenticated votes for Core Blocks using Bitcoin
transactions.

Minting consists of two operations:

1. **Acquisition** - burning BTC to acquire Minting Power.
2. **Minting** - using that Minting Power to vote for a Core Block.

Votes observed in the Bitcoin chain are the authoritative input for Flame chain selection. The rules for selecting the
canonical chain from those votes are defined separately in the Chain Selection section.

Bitcoin script encodings used by Acquisition and Minting transactions are defined in
[Minting Bitcoin Encodings](minting-encodings.md).

### Acquisition

An Acquisition represents a temporary amount of Minting Power created by burning BTC.

An Acquisition is created by a valid output of a Bitcoin transaction. Each valid output creates an independent
Acquisition.

An Acquisition is uniquely identified by:

```text
(txid, output_index)
```

Its Minting Power and lifecycle are calculated according to the rules defined in [Minting Power](#2-minting-power).

#### Acquisition Transaction

An Acquisition Transaction:

- may contain any number of inputs;
- may contain any number of outputs;
- may contain one or more valid Acquisition Outputs.

Inputs in Acquisition transaction are not validated.

Each Acquisition Output is processed independently. An output that does not satisfy the Acquisition Output rules does
not create an Acquisition.

The BTC amount assigned to an Acquisition Output is permanently unspendable. An Acquisition commits to the Minter's
Minting identity, the final reward beneficiary, the public key used for BFT participation, and optionally an Acquisition
duration.

An Acquisition Output is valid only if:

- its BTC amount is non-zero;
- all required fields are present;
- its duration satisfies the protocol configuration.

The exact Minting Power produced by the Acquisition is defined in [Minting Power](#2-minting-power).

### Minting

Minting is the process of casting a vote for a Core Block through a Bitcoin transaction.

A Minting Transaction contains:

- one or more inputs that authenticate participating Minters;
- exactly one eligible Minting Output identifying the Core Block being voted for.

Multiple Minters may be included in the same transaction, allowing their votes for the same Core Block to be batched.

#### Minter Authentication

A Bitcoin input authenticates a Minter if it spends a P2WSH output whose witness script hash matches the
Minting identity specified by that Minter's Acquisition. The witness script reveals a Flame predicate used for reward
distribution. Its complete script hash must match the Minting identity committed in the Acquisition.

The Flame Predicate is the predicate to which rewards associated with the Minter are issued.

When rewards are distributed, the Access Predicate specified by the Acquisition is passed to the Flame Predicate as
an argument. The `FlamePredicate` defines how the reward is handled and may, for example:

- forward the entire reward to the Access Predicate;
- retain a commission and forward the remainder to the Access Predicate;
- implement any other custom reward distribution logic.

The consensus protocol does not require a specific commission or reward distribution policy inside the `FlamePredicate`.

#### Minting Output

A Minting Output identifies the Core Block being voted for by its Flame height and block hash. Its Bitcoin output value
does not affect vote validity or voting weight and may be zero or non-zero.

A Minting Transaction must contain exactly one eligible Minting Output. If no eligible Minting Output exists, the
transaction does not produce any Minting votes. If more than one eligible Minting Output exists, the transaction does
not produce any Minting votes.

#### Vote Validity

For a Minter `M`, Core Block `C`, and Bitcoin block height `V`, a Minting vote is valid if:

- the transaction contains exactly one eligible Minting Output referencing `C`;
- `C` is a valid Core Block;
- `V` is equal to or greater than the target Bitcoin height `T` specified by `C`;
- at least one transaction input successfully authenticates `M`;
- `M` has non-zero Minting Power at `T`.

Multiple valid inputs authenticating the same Minter within a single transaction still produce only one vote from
that Minter.

The voting weight assigned to the vote is defined in [Minting Power](#2-minting-power).

Multiple distinct Minters authenticated by the same transaction each contribute their own Effective Minting Power to the
referenced Core Block.

#### Double Votes

If after new Bitcoin block arrival it is detected that one minter voted for two different blocks at the same height,
all Acquisitions of such Minter became unactive.

### Rationale

Acquisition and Minting are separate operations because this requires Minters to commit their voting power in advance
and reduces the risk of short-term fork attacks.

Consider a system where Minting Power can be acquired and used immediately, without any time commitment. An attacker
attempting a short-term fork would only need to acquire more than 50% of the Minting Power required for a single block.
In Flame, an Acquisition commits Minting Power for at least the next 2048 active Bitcoin blocks. Therefore, an attacker
attempting the same fork must acquire and maintain more than 50% of the total Minting Power over that period,
significantly increasing the cost of the attack.

Acquisition duration may be extended beyond the minimum because a longer commitment does not reduce protocol security
and may be useful for Minters that intend to participate for longer periods without creating additional Acquisitions.

## 5. Chain Selection

Minting may produce votes for multiple competing Core Blocks. Chain Selection determines which valid Flame
chain is considered canonical based on the cumulative weight of its Core Blocks.

### Core Block Weight

The weight of a Core Block is the base-2 logarithm of the sum of Effective Minting Power of all valid votes for that
block, rounded down. If the sum is zero, the block weight is zero:

```text
TotalEffectiveMintingPower(B) = Σ EffectiveMintingPower(v)

BlockWeight(B) =
    if TotalEffectiveMintingPower(B) == 0 then 0
    else floor(log2(TotalEffectiveMintingPower(B)))
```

where `v` is a valid Minting vote for Core Block `B`.

### Chain Weight

The weight of a Flame chain is the cumulative weight of all Core Blocks contained in that chain:

```text
ChainWeight = Σ BlockWeight
```

### Canonical Chain

The canonical Flame chain is the valid chain with the greatest Chain Weight according to the rules above.

Chain Selection is reevaluated whenever new information affecting chain weight becomes available, including:

- new Minting votes included in Bitcoin;
- arriving of delayed Minting votes;
- Bitcoin reorganization.

A delayed vote may change the relative weight of an older branch and cause it to become canonical.

Only known and valid Flame blocks participate in Chain Selection. A Minting vote referencing a block that is not yet
available cannot affect Chain Selection until the referenced block is available.

### Reorganizations

If a competing valid chain acquires greater cumulative weight than the currently canonical chain, Flame reorganizes to
the heavier chain.

During a reorganization, blocks after the common ancestor on the previous canonical chain are reverted, and blocks from
the selected chain are applied.

Specific rules of reorganization are not covered in this document.

### Bitcoin Reorganizations

Chain weight is derived only from Minting votes contained in the current canonical Bitcoin chain.

If Bitcoin reorganizes, Minting votes contained in removed Bitcoin blocks stop contributing to Flame chain weight. Votes
contained in newly added Bitcoin blocks are processed normally.

Flame Chain Selection is then reevaluated using the resulting set of valid Minting votes.

### Ties

In an unlikely case when two or more chains have equal Chain Weight, tied blocks are sorted by their block hash
converted to 256-bit number, and block with the lowest value is selected as canonical chain.

## 6. BFT

BFT is the coordination layer of Flame Consensus. Its primary purpose is to allow Validators to agree on a sequence of
Core Blocks that Minters are expected to vote for through Minting.

BFT does not provide authoritative finality. A block accepted by BFT may later be reverted if Minting selects
a competing Flame chain with greater cumulative weight.

This document do not describe specific implementation of BFT; instead, it describes properties required to be used
in this consensus protocol.

### Required Properties

A BFT protocol used by Flame must satisfy the following properties:

- it must produce an ordered sequence of valid Flame blocks;
- its voting system must support weighted Validators;
- it must be possible to restart the chain from a selected Core Block.

### Validators

A Validator is a Minter that is eligible to participate in BFT.

Validator eligibility is determined from Minting Power. A Minter whose Minting Power is below the protocol-defined
minimum validator threshold may continue participating in Minting, but does not participate in BFT.

A Minter is eligible to become a Validator when its Minting Power is greater than or equal to `1/1024` of the total
network Minting Power at the evaluation point:

```text
1024 * MintingPower(minter) >= TotalMintingPower
```

This threshold limits the active BFT validator set to at most `1024` Validators.

### BFT Voting Power

A Validator's BFT voting weight is equal to its current Minting Power. New Acquisitions and their expirations may update
the set of the validators.

### Blocks Produced Outside BFT

Participation in BFT is not required for producing a valid Flame block.

A Minter may construct a valid Flame block without receiving BFT agreement and may attempt to anchor a Core Block
belonging to that chain through Bitcoin Minting. Such a block is processed as usual Core block.

### BFT Failures

BFT Failures may harm Flame liveness, but not the finality in form of a Minting. Minters who are alive may
still vote on the blocks using Minting.

## 7. Delegation

Delegation allows one participant to provide Minting Power to a Validator without operating the Validator or
participating in Minting directly.

Delegation separates:

- consensus authority, represented by the Validator's Minting identity;
- reward ownership, represented by the delegator's Access Predicate.

### Delegated Acquisition

A delegation is created through a regular Acquisition.

To delegate Minting Power to a Validator, the delegator creates an Acquisition associated with the Validator's Minting
identity and BFT public key while retaining the delegator's own Access Predicate as the reward beneficiary.

The BTC burned in the Acquisition belongs to the Acquisition itself and produces Minting Power according to the normal
Minting Power rules. The resulting Minting Power is assigned to the Validator for consensus purposes, while rewards
produced by that Acquisition remain associated with the delegator's Access Predicate. A Validator may therefore have
multiple Acquisitions with the same Validator identity but different Access Predicates:

```text
Acquisition A:
    Validator = V
    Access Predicate = Alice

Acquisition B:
    Validator = V
    Access Predicate = Bob

Acquisition C:
    Validator = V
    Access Predicate = Carol
```

### Minting

The Validator performs Minting on behalf of all Acquisitions assigned to it. The delegator does not need to sign or
submit Minting Transactions. Delayed-vote penalties are applied normally to delegated Minting Power.

### BFT Participation

Delegated Minting Power also contributes to the Validator's BFT voting power, and may make Minter Minting Power high
enough to be eligible to become a Validator.

### Reward Distribution

Rewards are accounted for separately for each Acquisition.

When a Validator receives rewards for Minting Power originating from an Acquisition, the Acquisition's Access Predicate
is passed to the Flame Predicate revealed by the Validator during Minting.

The Flame Predicate determines how the reward is distributed. It is expected that the commission will be sent to the
Validator, and the rest to the Access Predicate, but the protocol neither forces nor verifies specific programs
contained in the Flame Predicate.

For example, a Validator may use a predicate that retains a fixed commission and forwards the remainder to the Access
Predicate:

```text
Reward
  |
  v
FlamePredicate
  |
  +-- Validator commission
  |
  +-- Remaining reward --> Access Predicate
```

Two delegators using the same Validator may have different Access Predicates, but the same Validator `FlamePredicate` is
applied to rewards generated by both.

### Delegation Lifetime

Delegation exists for the lifetime of the Acquisition. Once the Acquisition expires, its Minting Power stops
contributing to the Validator's Minting Power. Delegation cannot be reassigned to another Validator.

### Rationale

Delegation allows Validators to define their own delegation fee policy without requiring a consensus-level commission
mechanism.

## 8. Emission and Reward Distribution

Flame uses a finite block-based emission schedule. New FLM is created as a reward for Core Blocks and distributed
between Acquisitions that contributed Minting Power to those blocks.

Only Core Blocks produce new FLM. Regular Flame blocks do not produce emission.

### Emission

The initial reward is `50 FLM` per Core Block. The block reward is halved every `210,000` Core Blocks. After `64`
halvings, the block reward becomes zero and no additional FLM is issued.

### Reward Eligibility

The reward of a Core Block is distributed only between Minting Power that produced a valid Minting vote for that block.
If Minter has Minting Power at the Core block, but does not produce vote, it does not contribute to the Total Effective
Minting Power, therefore does not participate in the rewards distribution.

The reward for the vote is calculated as:

```text
reward = BlockReward * EffectiveMintingPower / TotalEffectiveMintingPower
```

### Reward Destination

Each Acquisition specifies an Access Predicate, while the Validator reveals a Flame Predicate when performing
Minting.

For each rewarded Acquisition, the protocol creates a reward associated with:

```text
(Reward Amount, Access Predicate)
```

and sends it to the `FlamePredicate` used by the Minter's valid Minting vote.

### Distribution Epochs

Rewards are calculated for every Core Block, but are distributed once every `100` Core Blocks with a delay of one full
distribution epoch.

When Core Block `H` is produced and:

```text
H % 100 == 0
```

the protocol distributes rewards for the inclusive Core Block range:

```text
[H - 199, H - 100]
```

Therefore:

```text
Core Block 200 -> distributes rewards for blocks   1 .. 100
Core Block 300 -> distributes rewards for blocks 101 .. 200
Core Block 400 -> distributes rewards for blocks 201 .. 300
```

Reward eligibility and reward shares are still calculated independently for every Core Block. The distribution epoch
only determines when the already calculated rewards are materialized.

The first reward distribution therefore occurs at Core Block `200`, covering Core Blocks `1` through `100`.

### BFT Fees

TBD

### Rationale

Emission is attached to Core Blocks rather than regular Flame blocks because BFT may produce arbitrary number of blocks,
while amount of Core blocks are strictly attached to the amount of Bitcoin blocks.

Delayed votes receive reduced rewards because their contribution to Chain Selection is also reduced. Using the same
Effective Minting Power for both fork choice and rewards aligns the economic incentive with the consensus value of the
vote.

Rewards are delayed by the at least `100` blocks to reduce chances of Bitcoin reorganizations.

Batching reward materialization across `100` Core Blocks reduces the amount of coinbase transactions without changing
the reward calculation for individual blocks.

### Open Questions

Q: How to handle Acquisitions with same Minting identity but different Validator public key? In
current version, Acquisition needs to specify both the FlamePredicate and ValidatorKey. This system has an issue:
delegator may specify correct Minter P2WSH but wrong ValidatorKey.

Q: What is protocol behaviour if a Core Block has zero Effective Minting Power? It may seem that this is unlikely case,
but Bitcoin miners can censor all Flame votes for 10+ blocks, in which case no votes for the Block will ever be
received. Considering that the Core Block is already created but cannot be minted, it is effectively deadlock in the
current protocol.

Q: Should BFT fees be distributed between only Validators or Minters as well?

## 9. Risks
