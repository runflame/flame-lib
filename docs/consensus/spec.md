# Flame Consensus: Specification

## 0. About this document

### 0.1. Purpose and status

This document defines the Flame consensus rules: which data from Bitcoin and Flame are taken into account, how Minting
Power, block weight and the canonical chain are computed from them, how rewards are issued, and what consensus requires
from BFT.

Status: draft. All sections are written; open questions are listed in section 13. The previous documents, [design-doc.md](design-doc.md) and
[core-blocks.md](core-blocks.md), stay as they are, and the divergence marks refer to them (see 0.3).

All sections are normative except section 1 (overview), the "Rationale" subsections and section 13. "Rationale"
explains why a rule is the way it is. Section 13 lists what is not decided yet.

### 0.2. Related documents

| Document | Role |
|----------|------|
| [minting-encodings.md](minting-encodings.md) | Normative. Byte encodings of the Burn Output, the Minting Output and the minter authentication prefix. Section 2 refers to it and only clarifies it. |
| [design-doc.md](design-doc.md) | Previous version. Its content is carried over here, clarified, or explicitly marked as dropped. |
| [core-blocks.md](core-blocks.md) | Previous version of the core block rules. Carried over into sections 5 and 10. |
| `design-doc/bft.md` from [PR #61](https://github.com/runflame/flame-lib/pull/61) | BFT details on Simplex/Commonware. This document (section 8) covers only the interface between BFT and Minting. At the time of writing the PR is not merged. |
| [blockchain.md](../blockchain.md), [encoding.md](../encoding.md) | The Flame state machine and encodings, including the Flame block header and `hash(B)`. Section 2 refers to them. |
| [consensus.md](../consensus.md) | Early stub: consensus requirements and a TBD list, no Minting rules. |
| [flame_consensus.qnt](flame_consensus.qnt), [flame_consensus_test.qnt](flame_consensus_test.qnt), [flame_search.qnt](flame_search.qnt) | Quint model of the current implementation. Findings FL-01…FL-12 are mentioned next to the rules they concern. |

### 0.3. Marks in the text

Marks are placed directly in the text. While a question is open, the mark has a footnote link to an item of section 13
of the form `GAP-07`.

| Mark | Meaning |
|------|---------|
| ⚠ **diverges from the previous doc and the code** | The rule differs both from [design-doc.md](design-doc.md) / [core-blocks.md](core-blocks.md) and from the `flame-lib` code. If it differs from only one source, the mark says "diverges from the previous doc" or "diverges from the code". While the question is open, the mark has a footnote to section 13. After a decision the footnote is removed, and what exactly diverges is stated in the text next to the mark. |
| **Not defined** | There is no decision: the sources contradict each other or all are silent. The options are listed only in the section 13 item; the text does not pick any of them. |
| *clarified from the code* | The previous doc is silent or allows several readings; the rule is written the way the code does it. The section 13 item exists to confirm that this is intended. |

A section 13 item contains: where in the text it is referenced; the essence; what the previous doc, the code, the PRs
and the Quint model say; the options; the status.

When a gap is resolved, the decision is written into the text, and the item is removed from section 13 together with
all links to it. Its number is never reused.

### 0.4. Normative keywords

The key words MUST, MUST NOT, SHOULD, SHOULD NOT and MAY, in capitals, are to be interpreted as described in RFC 2119.

Rules about validity and computed values are addressed to every node: two nodes with the same inputs (section 2) MUST
get the same result.

### 0.5. Rule identifiers

Every normative rule starts with an identifier of the form `**MP-3.**`. The identifier does not change in translation
or when the text is edited. A removed rule leaves its number unused.

| Prefix | Section |
|--------|---------|
| `IN` | 2. Inputs |
| `MP` | 3. Burn and Minting Power |
| `EN` | 4. Endorsements |
| `CB` | 5. Core blocks and block validity |
| `FC` | 6. Weight and canonical chain selection |
| `RW` | 7. Emission and rewards |
| `BFT` | 8. BFT: interface |
| `DL` | 9. Delegation |
| `HB` | 10. Honest participant behavior |
| `PROP` | 12. Guarantees and assumptions |

### 0.6. Notation

- All quantities are non-negative integers unless stated otherwise. BTC amounts are in satoshis.
- `⌊a / b⌋` is integer division rounding down. Division without `⌊ ⌋` is not used in formulas, except where it is
  explicitly said that the result is not an integer.
- `log2(x)` is the binary logarithm as a real number.
- `Σ` over an empty set is `0`.
- `a ‖ b` is concatenation of byte strings.

Notation used throughout the document:

| Symbol | Meaning |
|--------|---------|
| `h` | height of a Bitcoin block |
| `h₀` | height of the Bitcoin start block (IN-1); it is also the target of genesis (CB-2) |
| `N` | height of the Bitcoin tip in the node's view |
| `X` | height of the Bitcoin block that includes the Burn transaction |
| `A`, `D` | Burn amount in satoshis and its duration in Bitcoin blocks (MP-1, MP-3) |
| `Z` | maturity: how many blocks after inclusion a Burn becomes active (MP-4) |
| `T` | target Bitcoin height of a core block; it is also the slot number |
| `V` | height of the Bitcoin block that includes the Minting Transaction |
| `d` | endorsement delay, `V − T` (EN-5) |
| `k` | blocks of delay beyond the grace window, `max(0, d − GRACE_DELAY)` (EN-5) |
| `B` | a Flame block |
| `C` | a core block; `C_prev` is the previous core block of its chain (5.2) |
| `c` | core height, the ordinal number of a core block in its chain, 0 for genesis; `c = T − h₀` (CB-4) |
| `M` | a minter (its Minting identity) |
| `hash(B)` | hash of a Flame block |
| `P` | Minting Power of a group of minters or of the whole network; whose exactly is said in place |
| `α` | the adversary's share of all Minting Power |
| `V_ban(M)` | ban height of minter `M` (EN-7) |
| `K` | depth beyond which a Bitcoin reorg is considered impossible (FC-7, section 12) |
| `α_max` | the largest adversary share for which finality still holds, `α_max < 1/2` (FC-7, section 12) |
| `C_T` | the core block of slot `T` produced by BFT (1.8) |
| `E_T` | the deterministic empty core block of slot `T` (1.8, rule 5) |
| `R` | Bitcoin height up to which, inclusive, data for the rewards of a batch is read: `R = T_end + 11 = h₀ + c − 89`, where `c` is the core height of the payout block and `T_end` is the target of the last core block of the batch (RW-7) |

Parameters are written in capitals (`GRACE_DELAY`, `MAX_DELAY`, `MIN_DURATION`, `MAX_DURATION`, `EMPTY_WAIT`) and are
collected in section 11. Local notation is introduced where it is used: `H`, `r`, `T_end` in 1.7; `L`, `T_last`, `ρ`
in 1.8; `S_D`, `Λ` in MP-6; `F` in EN-9; `w`, `w_alt`, `G` in FC-7; `e`, `C_w` in 8.2.

A Flame block has two different heights: its height in the Flame block tree, and its core height (for a core block).
They differ because there are regular blocks between core blocks. Where it matters which one is meant, it is said
explicitly.

### 0.7. Glossary

The "Where" column gives the section with the exact definition.

| Term | Short | Where |
|------|-------|-------|
| Bitcoin view | The canonical Bitcoin chain as seen by a node; input to all consensus computations | 2 |
| Start block | The Bitcoin block (height and hash) that Flame genesis is anchored to; Bitcoin below it is not read | 2, 11 |
| Flame block | A Flame block: a regular block or a core block | 2, 5 |
| Core Block | A Flame block with a core header: core height, target and the `hash` of the previous core block. Only core blocks receive endorsements and issue rewards | 5 |
| Target (`T`) | Target Bitcoin height of a core block: endorsements are valid from it on, and Minting Power is taken at it | 5 |
| Slot | The target height `T` as a place for a core block. Not in the previous doc; introduced to talk about the place of a core block independently of the branch, and about slots without weight | 5, 6 |
| Live slot | A slot whose endorsement can still get non-zero weight: with the Bitcoin tip at height `N`, `T ≥ N − 10` | 1.8, 10 |
| Catch-up slot | A live slot with `T ≤ N`: it has already passed. If BFT did not produce a core block for it, it is closed by the deterministic empty core block `E_T` (1.8) | 1.8, 10 |
| Burn | A Bitcoin transaction output that burns BTC in exchange for temporary Minting Power | 3 |
| Minter, Minting identity | The owner of Burns who endorses core blocks. The identity is the Minter P2WSH | 3 |
| Minting Power (MP) | A minter's weight at a given Bitcoin height, derived from its active Burns | 3 |
| Endorsement | A minter's vote for a core block, expressed as a Bitcoin transaction | 4 |
| Minting Transaction | A Bitcoin transaction with an endorsement: its inputs authenticate minters, its Minting Output points to a core block | 4 |
| Delay | Endorsement delay: `V − T` | 4 |
| Effective Minting Power (EMP) | The contribution of one endorsement: the minter's MP at height `T`, reduced for delay | 4 |
| Double sign | Two records of one minter with the same core height and different hashes; the blocks need not be known | 4 |
| Block Weight, Chain Weight | Weight of a core block from the sum of EMP of its endorsements; weight of a chain is the sum of the weights of its core blocks | 6 |
| Canonical chain | The Flame chain chosen by Chain Weight | 6 |
| Validator | A BFT participant: a Validator Pubkey; its weight is the total Minting Power of all identities with that key | 8 |
| Access Predicate | The Flame predicate given in a Burn: the final recipient of the rewards for that Burn | 3, 7 |
| Flame Predicate | The Flame predicate revealed by a minter when endorsing; rewards pass through it | 4, 7 |
| Validator Pubkey | The minter's Ed25519 key for BFT, revealed together with the Flame Predicate | 4, 8 |
| Delegation | A Burn with someone else's Minting identity and one's own Access Predicate | 9 |
| Distribution epoch | A batch of 100 core blocks whose rewards materialize together | 7 |

## 1. Overview

This section is non-normative: it describes how consensus works as a whole. The rules are in sections 2–10.

### 1.1. Two layers

Flame consensus has two layers.

**BFT** is the coordination layer. Validators agree on a sequence of Flame blocks and on which of them become core
blocks. A BFT decision is not final: a block accepted by BFT can be reverted if Minting picks another chain. BFT MAY
produce any number of regular blocks between Bitcoin blocks, but does not have to: consensus works with core blocks
alone.

**Minting** is the layer that chooses the canonical chain. It happens entirely in Bitcoin and consists of two
operations:

1. **Burn.** A minter burns BTC and, after some time, gets Minting Power for a limited period (section 3).
2. **Endorsement.** A minter votes for a core block with a Bitcoin transaction. The weight of the vote is its Minting
   Power, reduced for delay (section 4).

The canonical Flame chain is the chain with the largest total weight of core blocks, and the weight of a core block is
determined by the endorsements for it. When weights are equal, BFT decides: the new block on top of the heaviest one,
which minters have not had time to vote for yet, is the one BFT finalized (section 6). When a block becomes final is
defined in the same section (FC-7): the weight of a slot freezes 11 Bitcoin blocks after its target, and from then on
everything is computed from Bitcoin data.

### 1.2. Consensus weight

The only source of weight is Minting Power obtained by burning BTC. Both the endorsement weight in Minting and the
voting weight in BFT are derived from it (how exactly is in sections 4 and 8).

### 1.3. Participants

- A **Minter** owns Burns, and therefore Minting Power, and endorses core blocks.
- A **Validator** is a minter that takes part in BFT (section 8).
- A **Delegator** burns BTC in favor of someone else's Minting identity: the weight goes to that minter, the rewards to
  the delegator (section 9).

Taking part in BFT is needed neither to create a valid Flame block nor to endorse it. A block created outside BFT is
processed by Minting like any other (sections 5 and 8).

### 1.4. Inputs and outputs

Consensus is a computation over two inputs (section 2):

- the canonical Bitcoin chain as seen by the node: it contains Burns and endorsements;
- the set of Flame blocks known to the node, with BFT finalization certificates for them.

From them, the minters' Minting Power, the core block weights, the canonical Flame chain and the rewards are computed.
Nodes with the same inputs get the same results.

On a Bitcoin reorg the first input changes: some Burns and endorsements drop out, others appear at other heights.
Flame always follows the canonical Bitcoin chain, for a reorg of any depth. The only exception is a reorg that replaces
the start block: then the network halts (IN-3). The rules, and an analysis of reorgs of depth 1, 3 and 15 blocks, are
in 1.7.

### 1.5. One slot

The usual course of events for one core block. The rules are in sections 4, 5 and 10; the waiting windows and a
comparison with another option are in 1.6. ⚠ **diverges from the previous doc and the code**: in
[core-blocks.md](core-blocks.md) the target was `h + 2`, and in [design-doc.md](design-doc.md) and in the code the delay
penalty starts with the very first block, without a window, both for fork choice weight and for rewards.

1. Bitcoin block `h` arrives. BFT creates core block C with target `T = h + 1`.
2. As soon as C is finalized in BFT, minters send endorsements for C right away. There is no need to wait for the next
   Bitcoin block: an endorsement can no longer get into block `h`, because `h` was found before C was created.
3. An endorsement included in block `T = h + 1` or `T + 1 = h + 2` gets full weight. Block `T + 1` is the grace window
   (`GRACE_DELAY = 1`).
4. After that the weight halves with every block, and an endorsement in block `T + 2 + MAX_DELAY = h + 13` or later is
   invalid.

```text
Bitcoin block that includes   h+1    h+2     h+3   h+4   ...   h+12     h+13 and later
the endorsement (V):          = T    = T+1
share of the minter's         1      1       1/2   1/4   ...   1/1024   0
Minting Power (before rounding down):
```

Minting Power is always taken at height `T` (section 3), whatever the delay. The formula (exact statement in section 4,
parameters in section 11):

```text
d   = V − T
k   = max(0, d − GRACE_DELAY)
EMP = ⌊MintingPower(M, T) / 2^k⌋

GRACE_DELAY = 1, MAX_DELAY = 10
```

Here `d` is the delay. An endorsement is valid only when `0 ≤ d ≤ GRACE_DELAY + MAX_DELAY`; earlier or later it has
no effect. `k` is the number of blocks of delay beyond the grace window.

`MintingPower(M, T)` is an integer (section 3). Rounding down happens once, over the minter's total Minting Power, not
over each of its Burns.

Every core block issues a reward; it is split among the Burns whose endorsements count for this core block, in
proportion to their weight (section 7).

The scheme assumes that BFT manages to finalize C before the next Bitcoin block. If it does not (two Bitcoin blocks in a
row, BFT downtime, a Bitcoin reorg with a longer new branch), BFT catches up on the missed slots, one core block per
slot, and minters endorse those that can still get weight. This is analyzed in 1.8.

### 1.6. Waiting windows and why `T = h + 1`

Notation: `I₁` is the interval between Bitcoin blocks `h` and `h + 1`, `I₂` between `h + 1` and `h + 2`. Both are
random, 10 minutes on average. `β` is the time BFT needs to finalize C after block `h` arrives. `δ` is the time for an
endorsement to get from the minter into the miners' block templates.

Waiting windows within a slot:

- **BFT**: from the arrival of block `h` to the finalization of C. There is no hard deadline, but all time beyond the
  necessary is taken from the minters' window. If C is finalized only after block `h + 2`, endorsements land no earlier
  than `h + 3` and get at most half the weight.
- **Minter**: from the finalization of C to block `h + 2`: an endorsement included in this interval gets full weight.
  The window length is `I₁ + I₂ − β − δ`, almost 20 minutes on average.
- **Weight of C in fork choice**: timely endorsements accumulate by block `h + 2`. Late ones can add weight up to block
  `h + 12` inclusive.

Comparison with the option from [core-blocks.md](core-blocks.md):

| | `T = h + 2` (core-blocks.md) | `T = h + 1` with a grace window (this document) |
|---|---|---|
| When a minter can send an endorsement | Only after block `h + 1` arrives. One sent earlier can land in `h + 1 < T` and is invalid | Right after C is finalized |
| Minter's window for full weight | One interval `I₂`, 10 minutes on average | `I₁ + I₂ − β − δ`, almost 20 minutes on average |
| Probability that the window is shorter than 30 s / 1 min / 2 min¹ | 4.9% / 9.5% / 18% | 0.12% / 0.47% / 1.8% |
| Effect of BFT speed | None while BFT finishes before block `h + 1`; after that it eats the minters' window | Always subtracted from the window, but the window is twice as long |
| Earliest block where C gets weight | `h + 2` | `h + 1` |
| Last block with full weight | `h + 2` | `h + 2` |
| Endorsement weight in block `V ≥ h + 2` | `⌊MintingPower / 2^(V − h − 2)⌋` | the same |
| Last block with non-zero weight | `h + 12` | `h + 12` |
| Height at which Minting Power is taken | `h + 2`: it includes Burns from block `h + 1`, which does not exist yet when C is created | `h + 1`: with maturity ≥ 1, only Burns from blocks up to `h` inclusive. Everything is known when C is created, and it is the same cut as the anchor block of the BFT session |
| Consistency with `bft.md` (PR #61) | No, it has `h + 1` | Yes |

¹ Ignoring `β` and `δ`, assuming Bitcoin blocks form a Poisson process with a mean interval of 10 minutes. With `β` and
`δ` both options get worse. Right after a new block miners often work on an empty template, which also makes both
worse.

**What does not change.** For endorsements included in block `h + 2` and later, the weights are the same in both
options. The decision only adds block `h + 1` as allowed with full weight. So the protection against late
endorsements, for which the delay penalty exists (section 4), does not get weaker.

**Cost.**

- In the code the delay formula changes, both for fork choice weight and for rewards.
- The minter algorithm from [core-blocks.md](core-blocks.md) is rewritten (section 10): an endorsement is sent when the
  core block is finalized, not when the next Bitcoin block arrives.
- If a Bitcoin reorg replaces block `h` itself, an endorsement can land in a new block at height `h < T` and be
  invalid; the minter has to send it again. The `h + 2` option has the same risk with block `h + 1`.

### 1.7. Bitcoin reorgs of depth 1, 3 and 15 blocks

Flame always follows the canonical Bitcoin chain, for a reorg of any depth. The exception is a reorg that replaces the
start block (IN-3). This relies on the rules of sections 2, 6, 7, 8 and 10:

1. **Determinism.** Everything consensus computes is a function of the canonical Bitcoin chain and the set of known
   Flame blocks and BFT certificates. After a reorg a node arrives at what it would have computed from scratch. An
   implementation must survive a reorg of any depth: by rolling back from stored data or by recomputing everything from
   scratch.
2. **Fork choice** takes the whole current Bitcoin chain into account.
3. **Rewards** read Bitcoin only up to height `R`, which follows from the core height of the payout block (RW-7).
   ⚠ **diverges from the code**: in the code rewards are computed from the Bitcoin tip the node sees at the time of
   computation. [design-doc.md](design-doc.md) only says that rewards are delayed so that a Bitcoin reorg does not
   affect them. Why `R` is deep:
   - `R` is the last height at which an endorsement can still change the weight of the core blocks of the batch being
     paid: `R = T_end + GRACE_DELAY + MAX_DELAY = T_end + 11`, where `T_end` is the target of the last core block of the
     batch.
   - By [design-doc.md](design-doc.md), a batch is paid in the core block that comes 100 core blocks after the last block
     of the batch. The target grows by exactly 1 with every core block (CB-4), so the target of the payout block is
     `T_end + 100`. A core block is created once the Bitcoin tip has reached at least the height one below its target,
     that is `T_end + 99`. So `R` lies at least `99 − 11 = 88` blocks below the tip.
   - In the code rewards are computed 50 core blocks after the batch (and paid 50 later). The same count gives
     `49 − 11 = 38` blocks.
   - As long as a reorg is shallower than this depth, `R` is not affected by it, and paid rewards do not change. Over
     time the payout block gets deeper, and the margin only grows. The exact schedule is in section 7.
4. **BFT.** If the anchor block of a session drops out, the session stops, and its certificates stop counting (BFT-9).
   The root of the new session is chosen anew by BFT-5, from the new canonical Flame chain and the new tip `N`. If BFT
   has fallen behind, past slots are closed by empty `E_T`, the root becomes `E_N`, and the anchor is block `N − 1` of
   the new branch (BFT-3). The session immediately produces a core block for the live slot `N + 1`, as with any BFT lag
   (1.8). Core blocks of dead slots (older than `N − 10`) get no weight.
5. **The minter** resends dropped endorsements for core blocks that stayed canonical, if they did not return to the
   chain by themselves, but only for live slots. Dropped endorsements for dead slots it replaces with RBF (1.8).

Notation:

- `H` is the height of the Bitcoin tip before the reorg. A reorg of depth `r` replaces blocks at heights
  `H − r + 1 … H`.
- `N` is the height of the tip of the new branch. The new branch must have more work, so it is usually longer by at
  least one block: `N ≥ H + 1`.
- A block of the old branch is denoted by its height: `H`, `H − 2`. A block of the new branch by its height with a
  star: `H*`, `(H + 1)*`. The star is used above `H` too, even though the old branch never reached those heights.
- Slots `T`, inclusion heights `V` and delays are just numbers. They do not depend on the branch.

Parameters: `GRACE_DELAY = 1`, `MAX_DELAY = 10`, maturity `Z = 1` (section 11).

There are two kinds of reorg, with different consequences:

- **Honest race.** Two miners found blocks almost at the same time. The new branch is built from the same mempool: it
  has almost the same transactions, at the same or neighboring heights.
- **Foreign branch.** The branch was mined separately: by an adversary in secret or, on testnet, as a stream of
  minimum-difficulty blocks. Endorsements and Burns from the dropped blocks are not in it. They return to the mempool
  and land in blocks after the new tip, that is with a larger delay. The adversary could have put its own endorsements
  for its own Flame branch into its branch in advance.

**What does not depend on the depth.**

- Core blocks stay valid: a core block refers to height `T`, not to a Bitcoin block. Only their weight is recomputed.
  BFT certificates of old sessions do not affect block validity (section 8).
- Validator keys revealed only in dropped blocks stop counting until they are revealed again.
- Double sign evidence from dropped blocks disappears unless it made it into the new branch; bans are recomputed.
- If endorsements for some slots lost weight, this happens equally on all Flame branches. The canonical Flame chain
  changes only if some Flame branch has endorsements in the new Bitcoin branch and the canonical one does not. In
  practice this means that the author of the new Bitcoin branch endorsed its own Flame branch.

#### Depth 1

Block `H` drops out, the new branch is `H*` and `(H + 1)*`, `N = H + 1`. Usually this is an honest race.

- **Endorsements.** The dropped block `H` contained endorsements for slot `T = H` (on time) and for slot `T = H − 1`
  (in the grace window). In an honest race block `H*` almost certainly contains the same transactions, and nothing
  changes. If some transaction did not make it into `H*`, it goes into `(H + 1)*` or later:
  - slot `T = H`: landing in `(H + 1)*`, that is `V = T + 1`, still gives full weight thanks to the grace window;
    landing at height `H + 2` gives half;
  - slot `T = H − 1`: the grace window has already been used, now at most half.
- **Burns** from block `H` land in `H*` or `(H + 1)*`; their activity window shifts by at most one block. If a Burn
  shifted, its owner may lack that power in slot `T = H + 1`.
- **BFT.** The session with anchor `H` stops, and its certificates no longer count in FC-5 (BFT-9). The validator
  chooses the root of the new session anew by BFT-5: by weights, certificates and `EMPTY_WAIT`. Usually it is
  `C_{H+1}`, created after block `H`: its certificate was issued by the session with anchor `H − 1`, which did not drop
  out. If there is no certificate for `C_{H+1}` and `EMPTY_WAIT` has passed after block `(H + 1)*`, the root is
  `E_{H+1}`. Both have target `H + 1`, so the anchor of the new session is `H*` (BFT-3). The tip is already `(H + 1)*`,
  and the session immediately proposes the core block of slot `H + 2` (BFT-4). No slot is lost.
- **Rewards, Flame history**: unchanged.
- **The minter** usually does nothing: dropped transactions return to the mempool by themselves.

Result: the reorg is almost invisible. The grace window covers the main case, an endorsement for the last slot that
moved by one block.

#### Depth 3

Blocks `H − 2 … H` drop out, the new branch is `(H − 2)* … (H + 1)*`, `N = H + 1`. Three blocks found in a row in an
honest race is an almost impossible coincidence. So such a reorg is more likely a foreign branch: an attack with a large
share of hashrate, or testnet.

- **Endorsements in an honest race** land at neighboring heights; some slots lose half their weight.
- **Endorsements in a foreign branch** return in block `(H + 2)*` and later. Weight of the slots whose endorsements
  were in the dropped blocks, when they land in `(H + 2)*`:

  | Slot `T` | `H − 3` | `H − 2` | `H − 1` | `H` | `H + 1` |
  |---|---|---|---|---|---|
  | Delay `H + 2 − T` | 5 | 4 | 3 | 2 | 1 |
  | Weight share | 1/16 | 1/8 | 1/4 | 1/2 | 1 |

  Slot `H − 3` loses only the endorsements that landed in the grace window (in the dropped block `H − 2`).
- **If the Bitcoin branch was made by the adversary** and it put its own endorsements into it without delay, in these
  slots it has full weight against reduced honest weight. Let `α` be the adversary's share of the total Minting Power of
  all minters at the slot's height, the honest minters hold the rest, `1 − α`, and all voted on time. After the reorg
  the honest weight of slot `H − 2` is `(1 − α)/8` (delay 4, table above), and the adversary has its full `α`. The
  adversary wins the slot if `α > (1 − α)/8`, that is `9α > 1`, `α > 1/9 ≈ 11%`. Example: out of 100 units of power the
  adversary has 12, the honest minters 88; after the reorg the honest weight in this slot is `88/8 = 11 < 12`. In the
  same way the adversary wins slot `H − 1` (the honest minters keep 1/4) with `α > 1/5 = 20%`, and slot `H` (they keep
  1/2) with `α > 1/3 ≈ 33%`. Winning individual slots does not yet mean winning the chain: that depends on the weight
  formula (section 6). If it does win, BFT and the honest minters switch to its branch by fork choice, and the last
  Flame slots are rewritten. This requires finding 4 Bitcoin blocks in secret faster than the rest of the network,
  which on mainnet is unrealistic without a huge share of hashrate.
- **Burns** from the dropped blocks mature 2–4 blocks later. In the next slots their owners have less power.
- **BFT**: as for depth 1, but anchor blocks `H − 2` and `H − 1` also drop out. Certificates of sessions with an anchor
  in the replaced blocks no longer count in FC-5 (BFT-9); certificates with an anchor below `H − 2` remain. To choose
  the root by BFT-5, the validator also uses certificates it verified before the reorg (BFT-5, item 1). If the last core
  block with weight is still the same `C_w`, they continue the same honest chain: usually the root is the same
  `C_{H+1}`, the anchor is `H*`, the next target is `H + 2`, and there are no holes. If the weights changed, as in the
  attack above, `C_w` and the root are recomputed from the new weights.
- **Rewards**: unchanged; 3 blocks is far less than the depth of `R`.
- **The minter** checks that the dropped endorsements returned to the chain and resends them if needed.

Result: in an honest race, a small loss of weight in a few slots. In an attack, an adversary able to rewrite 3 Bitcoin
blocks can rewrite the last Flame slots with 11% of Minting Power or more.

#### Depth 15

Blocks `H − 14 … H` drop out, the new branch is `(H − 14)* … N*`, `N ≥ H + 1`. This happens only with a foreign branch:
on mainnet an attack, on testnet a stream of minimum-difficulty blocks. On testnet the new branch can also be much
longer than the old one.

- **Endorsements** return in blocks after the new tip. With `N = H + 1` they land in `(H + 2)*` and later:
  - slots `T = H − 14 … H − 10` get a delay of 12 or more, that is **zero**. These are holes: in these slots no Flame
    branch has weight;
  - slots `T = H − 9 … H` get a delay from 11 down to 2, that is from 1/1024 to 1/2 of the weight.

  If `N > H + 1`, even more slots are zeroed.
- **If the new branch was mined without regard to Flame** (a stream of empty blocks on testnet), the holes and losses
  are the same on all Flame branches. The canonical Flame chain does not change; these slots simply have no weight left.
- **If the branch was made by the adversary** with its own endorsements, it has full weight in ~15 slots where the
  honest minters have zero or crumbs. Its Flame branch becomes canonical, BFT and the honest minters switch to it, and
  Flame rolls back about 15 slots, about 2.5 hours. This is a direct consequence of Flame being no safer than Bitcoin
  against Bitcoin miners.
- **New slots.** If `N > H + 1`, slots `H + 2 … N − 1` have already passed and are closed immediately by empty `E_T`,
  and slot `N` after `EMPTY_WAIT` if BFT does not make it (1.8, rule 1). Endorsements for them land in `(N + 1)*` and
  later, with a delay. If the new branch is longer than the old one by more than 11 blocks (`N > H + 12`), slots
  `H + 2 … N − 11` are no longer live, and their `E_T` get no weight. These are holes in the honest chain itself.
- **Burns** mature 15 or more blocks later. During all that time their owners have less Minting Power, and the share of
  the others, the adversary included, is higher.
- **Validators** whose keys were revealed only in the dropped blocks drop out of the validator set until they reveal the
  key again.
- **Rewards**: unchanged; 15 blocks is less than the depth of `R`.
- **The node** must be able to roll back 15 Bitcoin blocks: from stored rollback data or by recomputing from scratch.
- **The minter** should only resend endorsements for live slots (`T ≥ N − 10`), which can still get weight in the new
  branch. Dropped endorsements for dead slots return to the mempool by themselves; it replaces them with RBF by
  endorsements for live slots (1.8).

Result: in the honest case about 15 slots lose weight, about 5 of them completely, but Flame history does not change. In
an attack the adversary rewrites about 15 Flame slots.

#### Conclusions

- As long as a reorg is shallower than the depth of `R` (88 blocks, 38 in the code), it does not touch paid rewards;
  only fork choice changes.
- Loss of weight by itself does not change the Flame branch: it is the same on all branches. The only dangerous case is
  when the author of the Bitcoin reorg endorsed its own Flame branch. Against Bitcoin miners Flame is no safer than
  Bitcoin (section 12).
- A deep reorg creates holes in slots. With the sum of `log2` (FC-2) a hole is very expensive (FL-12), one more reason
  to reconsider the formula. Weakened but non-empty slots also lower the attack threshold; the calculation is in 1.8.

#### Why neither a threshold nor a record in Flame

Two more options were considered, both rejected:

- **A threshold `K_FINAL`:** a node does not apply a reorg deeper than the threshold and halts. But a new node syncing
  from scratch sees only the new Bitcoin branch and keeps working. Old and new nodes diverge, and the network stays
  split until manual intervention.
- **Bitcoin data is recorded in Flame as a fact** (a service record about switching to another Bitcoin branch), and
  Flame does not recompute the past. Then a Burn that dropped out in a reorg and was not included again leaves the
  minter both the power and the rewards, even though its BTC were never actually burned: one could "burn", collect
  rewards and get the BTC back.

Both options trade a rare deep rollback for a worse problem: a network split or free power.

### 1.8. When BFT lags behind Bitcoin

The scheme of 1.5 assumes that BFT finalizes the core block for slot `h + 1` before block `h + 1` arrives. If not, BFT
lags behind Bitcoin. This happens when:

- two Bitcoin blocks arrived almost back to back, faster than BFT finalizes a core block;
- BFT is idle: validators are unavailable or there is no quorum;
- after a Bitcoin reorg the new branch is longer than the old one by several blocks (1.7, depth 15).

The rules are in sections 5, 8 and 10.

Notation: `N` is the height of the Bitcoin tip, `T_last` the target of the last core block of the canonical Flame chain,
`L` how many slots BFT is behind.

#### Which slots are still live

An endorsement sent now lands no earlier than block `N + 1`, and it has non-zero weight only if
`V − T ≤ GRACE_DELAY + MAX_DELAY = 11` (1.5). So a slot is live if

```text
T ≥ N + 1 − GRACE_DELAY − MAX_DELAY = N − 10
```

There are at most 12 live slots: 11 catch-up slots (`N − 10 … N`) and the current one, `N + 1`. Weight of an
endorsement included in block `N + 1`:

| Slot `T` | `N − 10` | `N − 9` | … | `N − 2` | `N − 1` | `N` | `N + 1` |
|---|---|---|---|---|---|---|---|
| Delay `d = N + 1 − T` | 11 | 10 | … | 3 | 2 | 1 | 0 |
| Penalty `k = max(0, d − 1)` | 10 | 9 | … | 2 | 1 | 0 | 0 |
| Weight share | 1/1024 | 1/512 | … | 1/4 | 1/2 | 1 | 1 |

If the endorsement does not make it into `N + 1`, each following Bitcoin block takes away another half of the weight,
and the oldest slot stops being live.

#### Rules

1. **BFT catches up on all slots in order.** The next target of an honest BFT is `T_last + 1`: slots cannot be skipped
   (CB-4). If it is less than `N`, the slots from it to `N − 1` have already passed, and for each of them BFT takes the
   deterministic empty core block `E_T` (rule 5) instead of assembling its own. No BFT rounds are needed for them: this
   block is the same for everyone, and minters endorse it too. Slot `N` is closed by the empty `E_N` after
   `EMPTY_WAIT` from block `N`, if BFT did not manage to finalize its own core block for it (rule 5, BFT-5). BFT creates
   a regular core block with transactions for the live slot `N + 1` once the Bitcoin tip has reached `N`. With a lag of
   `L ≥ 12` slots, `L − 11` of the empty blocks are dead slots that will get no weight.
2. **Empty core blocks are allowed.** The leader does not wait for transactions to create a core block.
3. **The minter endorses all live core blocks** of the honest chain (BFT-5, HB-3), from old to new, with a separate
   transaction for each: a Minting Transaction has exactly one Minting Output
   ([minting-encodings.md](minting-encodings.md)). It waits neither for earlier endorsements to confirm nor for the
   next Bitcoin block. ⚠ **diverges from the code**: in the code the minter, at each new tip `h`, endorses only the core
   block with target `h + 1` and does not endorse catch-up slots at all.
4. **The minter keeps only live endorsements in the mempool**, and replaces dead ones with RBF (below).
5. **If BFT did not produce a core block in time, the minter endorses an empty one.** For slot `T` there is a
   deterministic empty core block `E_T`: its parent is the core block of slot `T − 1` of the honest chain (BFT-5), and
   it has no transactions except the reward payout, which is deterministic too. The header is fully defined by
   CB-3 … CB-5, so all honest participants that chose the same parent get the same block, and their votes do not split.
   The parent is taken from the honest chain, and its weightless tail may differ between participants until the
   endorsements for the slot are in Bitcoin (PROP-8).
   - The minter endorses `E_T` if, `EMPTY_WAIT = 5 s` after it received Bitcoin block `T`, it has no BFT-finalized core
     block `C_T` for this slot. The endorsement lands in `T + 1` and gets full weight.
   - If `C_T` is finalized later and the endorsement for `E_T` is not yet confirmed, the minter replaces it with RBF by
     an endorsement for `C_T`. This is not a double sign: both transactions spend the same input, and only one of them
     gets into Bitcoin.
   - A BFT validator that received block `T` and waited `EMPTY_WAIT` votes for slot `T` only for `E_T`. This way a slow
     BFT converges to the same block as the minters.

   With this rule honest weight does not drop when BFT lags, and the calculation below applies only to minters that do
   not follow it. `EMPTY_WAIT` is an honest behavior parameter; it does not affect block validity (section 11). Why
   5 seconds is explained below, in "How long to wait before an empty block".

**Why a minter benefits from endorsing even almost dead slots.** The reward of a core block is split among the
endorsements for this block in proportion to weight (section 7). The delay penalty reduces a minter's share only if the
other endorsements for the same core block have a smaller delay. If all endorsements for a catch-up slot are equally
late, they share the whole reward. A minter that endorses such a slot alone gets it all, even with weight 1/1024. And
the reward of a core block with `TotalEMP = 0` is burned (RW-5). So endorsing a live slot pays off as long as the share
of the reward exceeds the Bitcoin transaction fee.

#### How long to wait before an empty block

Notation: `I_T` is the interval between Bitcoin blocks `T − 1` and `T`, random, 600 s on average; `β` is the time BFT
needs to finalize `C_T` after block `T − 1`. `β` includes delivery of block `T − 1` to validators (1–3 s), block
assembly by the leader, and 2–3 network delays of Simplex finalization (0.3–1 s), usually 2–5 s in total.

**BFT has `I_T + EMPTY_WAIT`, not 5 seconds.** Minters switch to `E_T` only if block `T` arrived faster than
`β − EMPTY_WAIT`:

| `β` | No wait | With `EMPTY_WAIT = 5 s` |
|---|---|---|
| 5 s, healthy BFT | 0.8% of slots | 0 |
| 10 s | 1.7% | 0.8% |
| 30 s, leader change by timeout | 4.9% | 4.1% |

An empty slot is not a failure: transactions go into the next core block.

**The minter has until block `T + 1`, 10 minutes on average.** The wait only shifts that time. The endorsement is late,
landing in `T + 2` (half weight), if block `T + 1` is found before the transaction gets into the miners' templates;
pools update templates about every 30 s. Without the wait that is about 4.9% of slots, with it about 5.7%. The wait
costs about +0.8% of slots with half weight. Making it longer does not pay: an empty slot costs only transaction delay,
while a late endorsement reduces weight, that is security.

**Vote split.** It is possible only if `C_T` is finalized after the switch, minters replace their endorsements with
RBF, and block `T + 1` is found before the replacements reach the miners' templates. That is about 5% of the already
rare late cases, on the order of 0.04% of slots. Even then part of the honest weight of one slot is lost. Which of the
two branches to continue is decided by the weights: the root is chosen by BFT-5 starting from the last core block of the
canonical chain with weight, and if both `E_T` and `C_T` have weight, FC-4 and FC-5 choose between them.

**Catch-up.** With rule 5 the lag hardly accumulates: every slot BFT did not make it for is closed by `E_T` right away,
and the honest minters endorse it on time. When BFT comes back, it builds on the last `E_T`, already for a live slot.
Catch-up remains after a Bitcoin reorg, when the tip jumped by several blocks at once, and when the minters themselves
were offline. Then all past slots are closed by `E_T` blocks both by BFT and by the minters (rule 1). There are no BFT
rounds, no RBF and no vote split; only the delay penalty remains, because the slots really are in the past.

#### Dead endorsements and RBF

An endorsement in the mempool dies if a Bitcoin block came out, the endorsement did not make it into it, and its slot
stopped being live. If it gets into the chain later, it gives neither weight nor reward, but the minter pays the fee.
This also happens after a Bitcoin reorg: dropped endorsements for old slots return to the mempool by themselves (1.7).

The minter replaces a dead endorsement with RBF (replace-by-fee). The new transaction spends the same input, but its
Minting Output points to a live core block the minter has not endorsed yet, usually the core block of the slot that
just opened. The fee then goes to a useful endorsement instead of a dead one.

- **Cancelling without a replacement does not pay.** By the mempool replacement rules of Bitcoin nodes (BIP125), the
  new transaction pays at least the total fee of all transactions it replaces, plus its own size. A "return the BTC to
  myself" transaction costs more than letting the dead endorsement get into a block. It pays off only when the
  replacement is a needed endorsement too. So the minter replaces a dead endorsement as soon as there is a live core
  block it has not endorsed yet. If the dead one gets into a block first, its fee is lost.
- **The replacement also raises the fee.** An endorsement usually misses a block because of a low fee or because the
  block was found right after it was sent. The replacement pays more and so is more likely to get into the next block.
- **Separate UTXOs.** The authenticating input of an endorsement spends an output to the Minter P2WSH
  ([minting-encodings.md](minting-encodings.md)). If endorsements are chained (each spends the change of the previous
  one), replacing one evicts all the following ones from the mempool, and by the same rules one has to pay for the whole
  chain. So the minter needs a separate UTXO on the Minter P2WSH for each live slot, up to 12 of them, and must spend
  them independently of each other.

#### What a BFT lag costs

Catch-up endorsements of honest minters get a penalty. Meanwhile, while BFT lags, the adversary can build its own Flame
branch outside BFT and endorse it on time: blocks created outside BFT are processed by Minting like any others (1.3). So
a BFT lag lowers the share of Minting Power the adversary needs for its branch to become heavier than the honest one.

Calculation with the sum of `log2`: the weight of a core block is `log2 Σ EMP`, the weight of a chain is the sum of the
weights of its core blocks (FC-2, FC-3). Conditions:

- BFT is behind by `L ≤ 11` slots: the last core block is for slot `N − L`, the Bitcoin tip is `N`. There are `L + 1`
  slots in total: catch-up slots `N − L + 1 … N` and the current `N + 1`.
- Honest minters with total Minting Power `P` endorse all `L + 1` core blocks, and all endorsements land in block
  `N + 1`. Slot `N − j` gets penalty `k = j` (table above), the current slot `0`. The total penalty is
  `Σk = 0 + 1 + … + (L − 1) = L(L − 1)/2`.
- The adversary with Minting Power `ρP` (`ρ` is the ratio of its power to the honest power) builds its own Flame branch
  from the first slot of the lag: its own core blocks for the same `L + 1` slots, each endorsed on time, without
  penalty.
- Rounding down and changes of power between slots are ignored.

```text
weight of the honest branch on these slots:   (L + 1)·log2(P) − L(L − 1)/2
weight of the adversary's branch:             (L + 1)·(log2(P) + log2(ρ))

the adversary is heavier if:                  log2(ρ) > −L(L − 1) / (2(L + 1))
its share of all Minting Power:               α = ρ / (1 + ρ)
```

For comparison, the last column gives the same calculation with a linear sum of EMP instead of `log2` (the chain weight
is the sum of EMP over slots). Then the honest weight is `P·(1 + 1/2 + … + 1/2^(L−1) + 1) = P·(3 − 2^(1−L))`, and the
adversary is heavier if `ρ > (3 − 2^(1−L)) / (L + 1)`. The choice of weight formula is GAP-14.

| `L` | BFT lag, on average | Threshold `log2(ρ)` | Adversary share, `log2` | Adversary share, linear sum |
|---|---|---|---|---|
| 0–1 | none or one fast block | 0 | > 50% | > 50% |
| 2 | 20 min | −0.33 | > 44% | > 45% |
| 3 | 30 min | −0.75 | > 37% | > 41% |
| 4 | 40 min | −1.2 | > 30% | > 36% |
| 6 | 1 h | −2.14 | > 18% | > 30% |
| 8 | 1 h 20 min | −3.11 | > 10% | > 25% |
| 11 | 1 h 50 min | −4.58 | > 4% | > 20% |

With `L ≥ 12` the honest minters catch up only on the 12 live slots, and the remaining `L − 11` are holes where the
adversary has full weight. With `log2` the threshold then also depends on `P` itself. With `P = 2^40` the adversary needs
0.6% at `L = 12`, 0.1% at `L = 13` and less than 0.01% at `L = 15`. With a linear sum the honest weight is at most `3P`,
and the adversary needs a share of `3/(L + 4)`: 19% at `L = 12`, 16% at `L = 15`, 11% at `L = 24`.

The adversary's branch is heavier only temporarily: in the following slots the honest minters endorse on time again and
win back `−log2(ρ)` per slot. But that is enough to rewrite the last Flame slots. The adversary does not need to stop
BFT itself: it is enough to notice the lag and start its own branch.

#### Summary

- A BFT lag of one Bitcoin block costs nothing: the grace window covers it completely.
- Holes in the honest chain appear only if more than 11 slots passed before the honest minters learned about them: after
  a Bitcoin reorg in which the new branch is longer than the old one by more than 11 blocks, or after downtime of the
  minters themselves of more than ~2 hours. BFT downtime by itself creates no holes (rule 5).
- Weakened slots would lower the attack threshold: with the sum of `log2`, an hour of BFT lag would lower it from 50%
  to 18% of Minting Power. Rule 5 removes this: honest minters endorse empty core blocks on time, their weight does not
  drop, and the threshold stays at 50% for any BFT lag. The penalty remains only for slots that passed before the
  minters learned about them: after a Bitcoin reorg or their own downtime. The same empty block mode also serves for a
  deliberate network halt (12.6, PROP-16).

## 2. Inputs

Consensus is computed over two inputs (1.4): the Bitcoin view and the set of Flame blocks known to the node together
with BFT certificates. This section defines what each of them contains and how Burn Outputs and Minting Transactions
are recognized in Bitcoin transactions. What they mean is defined in sections 3 and 4.

### 2.1. Bitcoin view

**IN-1.** A node's Bitcoin view is the chain of Bitcoin blocks from the start block to the tip that the node considers
canonical: the chain, valid under Bitcoin rules, with the most accumulated work. The start block anchors Flame genesis
to Bitcoin: Burns and endorsements in blocks below it are not taken into account. The Bitcoin network and the start
block (its height and hash) are deployment parameters (section 11), the same for all nodes of a network; the consensus
rules do not depend on them.

**IN-2.** A Bitcoin block enters the view as soon as it becomes part of the canonical chain. Consensus requires no
additional confirmations: the depth of a block only determines whether it can still drop out in a reorg (1.7).
Mempool transactions are not part of the view.

**IN-3.** On a Bitcoin reorg the view changes as a whole: the blocks of the dropped branch and everything extracted from
them stop counting, and the blocks of the new branch start counting. The reorg depth is not limited (1.7), with one
exception: if a reorg replaces the start block, the node MUST halt. It does not apply this reorg or anything after it,
and produces no new consensus results. The network continues only after a manual restart with new deployment
parameters.

**IN-4.** From every block of the view, consensus takes:

- the height and hash of the block;
- all Burn Outputs (2.3);
- all Minting Transactions (2.4), together with the outputs their inputs spend (prevouts): minter authentication is
  checked against them.

Everything else (timestamps, difficulty, other transactions) does not affect consensus.

**IN-5.** Transactions in the blocks of the view are valid under Bitcoin rules: input scripts are executed and
signatures are checked. Flame does not re-check them and relies on this for minter authentication (IN-14).

### 2.2. Flame blocks

**IN-6.** The second input is the set of Flame blocks known to the node: received from BFT, from the network, or created
outside BFT (1.3). Together with the blocks it includes the BFT finalization certificates known to the node. A
certificate does not affect block validity (CB-8), only the choice of the tip among equal weights (FC-5). It is
verified against the validator set of the session, which is derived from the Bitcoin view, so the check is
deterministic; how exactly, and what happens if the session's anchor block dropped out in a reorg, is in section 8. A
node does not have to know all existing blocks and certificates. Nodes with different sets may choose different
canonical chains; the same result is required only for the same inputs (0.4).

**IN-7.** Consensus uses the following fields of the Flame block header:

| Field | Type | Meaning |
|-------|------|---------|
| `height` | u64 | Height of the block in the Flame tree. 0 for genesis |
| `parent` | 32 bytes | `hash` of the parent block. Zeros for genesis |
| `core_block` | absent, or a pair `(height: u32, target_btc_height: u32)` | The core header. If present, the block is a core block with core height `c = core_block.height` and target `T = core_block.target_btc_height` |
| `core_block_hash` | absent, or 32 bytes | Present exactly when `core_block` is present. Equals the `hash` of the previous core block of the chain, that is the nearest core block among the ancestors. Genesis has no previous core block, and the field is all zeros |

The other fields (`version`, `witness_root`, `effects_root`, `state`) are defined by the Flame state machine
([blockchain.md](../blockchain.md)); Minting consensus does not read them. `hash(B)` is the CellID of the header
([encoding.md](../encoding.md#blocks-and-utreexo)); this is what the Minting Output points to (2.4). `core_block` and
`core_block_hash` are part of the header and therefore of `hash(B)` ([core-blocks.md](core-blocks.md)): an endorsement
of `hash(B)` also fixes the core height and the target of the block. The header layout in encoding.md is outdated: it
has no core header.

A block whose `core_block_hash` is absent while `core_block` is present, is present without it, or does not point to the
previous core block, is invalid (section 5). The check depends only on the Flame chain, not on the Bitcoin view, so core
blocks stay valid across Bitcoin reorgs (1.7). ⚠ **diverges from the code**: in the code every block has this field, it
is not checked, and the `flamed` node puts a placeholder there and takes the block time from it. It also diverges from
[blockchain.md](../blockchain.md), where the field is described as the identity of a Bitcoin block that the calling code
authenticates.

**IN-8.** The validity rules for Flame blocks and core blocks, genesis included, are in section 5.

### 2.3. Burn Output

**IN-9.** A Bitcoin transaction output is a Burn Output if its amount is greater than zero and its script exactly
matches one of the two forms of [minting-encodings.md](minting-encodings.md) (section "Burn Output"):

```text
OP_RETURN OP_PUSHBYTES_69 "FLMS" 0x01 <Minter P2WSH: 32> <Access Predicate: 32>
OP_RETURN OP_PUSHBYTES_71 "FLMS" 0x01 <Minter P2WSH: 32> <Access Predicate: 32> <Duration: u16 LE>
```

The Access Predicate MUST be the canonical compressed encoding of a Ristretto point. Otherwise the output is not a Burn
Output. Byte-level details are only in minting-encodings.md; in case of a discrepancy, it prevails.

**IN-10.** Each Burn Output is processed separately and is identified by the pair `(txid, vout)`. A transaction may
contain any number of them. The transaction's inputs are not checked, and among the other outputs only a Minting Output
matters (IN-16). Which Burn a Burn Output creates, whether its duration is allowed, and what Minting Power it gives is in
section 3.

### 2.4. Minting Transaction

**IN-11.** A Minting Output is an output whose script exactly matches

```text
OP_RETURN OP_PUSHBYTES_41 "FLMB" 0x01 <Flame Height: u32 LE> <Flame Hash: 32>
```

([minting-encodings.md](minting-encodings.md), section "Minting Output"). The output amount can be anything and does not
affect weight. The "Eligible Minting Output" of minting-encodings.md is an output recognized by this rule, and only such
an output. Whether the referenced block exists is not checked at this step: the record is needed even without the
block, double sign is counted from it (EN-6). An output with "FLMB" but a different version or extra bytes is not a
Minting Output and does not interfere with others.

**IN-12.** The Flame Height is the core height `c` of the endorsed core block, not its height in the Flame tree. The
Flame Hash is `hash(C)`. The pair `(c, hash(C))` points to a core block. From `c` alone, without the block, its target is
known: `T = h₀ + c` (CB-4).

**IN-13.** A transaction is a Minting Transaction if it has exactly one Minting Output. A transaction without a Minting
Output, or with two or more, gives no endorsements.

**IN-14.** An input of a Minting Transaction authenticates a minter if all of the following hold:

1. The input spends a native P2WSH output (script `OP_0 <32 bytes>`).
2. The last element of its witness, the witness script, starts with the minter authentication prefix
   ([minting-encodings.md](minting-encodings.md), section "Minter Authentication Prefix"):

   ```text
   OP_PUSHBYTES_68 "FLMV" <Flame Predicate: 32> <Validator Pubkey: 32> OP_DROP
   ```
3. The Flame Predicate is a canonical compressed Ristretto point, and the Validator Pubkey is a valid Ed25519 key.

That the SHA-256 of the witness script matches the program of the P2WSH output is checked by Bitcoin itself (IN-5).
This hash is the Minter P2WSH, that is the minter's identity `M`. The Flame Predicate and the Validator Pubkey are taken
from the prefix. Other inputs have no effect: they can be used, for example, to pay the fee. P2WSH nested in P2SH and
Taproot do not authenticate a minter.

**IN-15.** From a Minting Transaction in block `V`, one record is extracted per authenticating input:
`(M, Flame Predicate, Validator Pubkey, c, hash(C), V, txid)`. What to do with several inputs of one minter, with
several minters in one transaction, and when a record gives a valid endorsement is in section 4.

**IN-16.** A transaction that has both a Burn Output (IN-9) and at least one Minting Output (IN-11) creates neither Burns
nor records: a transaction can have only one role. ⚠ **diverges from the code**: the code extracts both Burn Outputs and
endorsements from such a transaction.

### 2.5. Rationale

**Why no confirmations (IN-2).** [design-doc.md](design-doc.md) only says that the canonical chain is selected "after
endorsements are settled in the Bitcoin Chain", without specifying a depth. A confirmation threshold is not needed:
Flame follows the canonical Bitcoin chain for a reorg of any depth (1.7), and a threshold would only delay weight. And
if a node does not apply reorgs deeper than a threshold, nodes diverge just as with the rejected `K_FINAL` (1.7). The
code has no confirmations either: a block counts as soon as the indexer sees it in the canonical chain.

**Why recognition depends only on the transaction (IN-9 … IN-16).** Burn Outputs and Minting Transactions are
recognized from the Bitcoin transaction itself and its prevouts, without regard to known Flame blocks or other Burns.
So all nodes with the same Bitcoin view extract the same records, even if they know different Flame blocks. Strict
byte forms rule out different encodings of the same data: each record has a single representation.

**Why a Burn and an endorsement are never in one transaction (IN-16).** This way each transaction has one role, and
recognition is simpler. There is no benefit in combining them other than saving on fees: a Burn gives no power in the
block it is included in anyway (MP-5). Also, there are no transactions with two `OP_RETURN` outputs, which Bitcoin Core
before version 30 does not relay.

**Why a reorg of the start block halts the network (IN-3).** Such a reorg rewrites the whole Bitcoin history Flame relies
on, on mainnet months and years of blocks. This is far beyond any assumptions (section 12), and there is nothing to
recover from automatically. The halt is not to be confused with the rejected `K_FINAL` threshold (1.7): there, a new
node syncing from scratch saw only the new branch and kept working, and the network split. Here a new node also checks
the start block hash and also halts, so all nodes behave the same way.

## 3. Burn and Minting Power

This section defines which Burns are created from Burn Outputs (2.3), when a Burn is active, and how much Minting Power
it gives. How Minting Power turns into endorsement weight is in section 4; how it is used in BFT is in section 8; a Burn
in favor of someone else's Minting identity is in section 9.

### 3.1. Burn

**MP-1.** Every Burn Output (IN-9) with an allowed duration (MP-3) in a transaction without a Minting Output (IN-16)
creates exactly one Burn. A Burn is identified the same way as its output: by the pair `(txid, vout)`. A Burn has:

| Symbol | Meaning |
|--------|---------|
| `A` | the output amount in satoshis, `A > 0` (IN-9) |
| `D` | duration in Bitcoin blocks (MP-3) |
| `X` | height of the block of the Bitcoin view that includes the transaction |
| `M` | the Minter P2WSH from the Burn Output: the minter the Burn gives Minting Power to |
| Access Predicate | from the Burn Output: the final recipient of the rewards for this Burn (section 7) |

`X` is a height in the current Bitcoin view. After a reorg the transaction may end up at another height or drop out
entirely (IN-3), and then the Burn has a different `X` or does not exist.

**MP-2.** A Burn requires nothing from the transaction's inputs. A Burn with Minter P2WSH `M` gives power to minter `M`,
whoever created it; delegation is built on this (section 9). The BTC in a Burn Output are spent forever: an `OP_RETURN`
output cannot be spent.

**MP-3.** The duration `D` equals the Duration field if it is present in the Burn Output, otherwise `D = MIN_DURATION`.
A Burn Output with `D < MIN_DURATION` or `D > MAX_DURATION` creates no Burn. `MIN_DURATION = 144` blocks (about a day),
`MAX_DURATION = 300` blocks (about two days; the value is preliminary, section 11). ⚠ **diverges from the previous doc
and the code** (FL-06): the rationale in [design-doc.md](design-doc.md) relies on a minimum of 2048 blocks; in the code
the minimum is 1, there is no upper bound other than the field limit (65535), and without Duration a separate value of
100 is used.

### 3.2. Lifecycle

**MP-4.** A Burn is active at Bitcoin height `Y` if

```text
X + Z ≤ Y < X + Z + D
```

where `Z` is the maturity, a protocol parameter (section 11). Before height `X + Z` the Burn is maturing; from
`X + Z + D` on it has expired; in both cases it gives no power. A Burn is active for exactly `D` blocks.

**MP-5.** `Z ≥ 1`.

### 3.3. Minting Power

**MP-6.** The Minting Power of minter `M` at height `Y`:

```text
MintingPower(M, Y) = ⌊ Σ A_b / D_b ⌋      over all Burns b with Minter P2WSH M active at height Y
```

The sum of the fractions `A_b / D_b` is taken exactly, in rational numbers; only the total is rounded down. If there
are no active Burns, `MintingPower(M, Y) = 0`. ⚠ **diverges from the code**: in the code `⌊A_b / D_b⌋` is computed for
each Burn separately and the results are added up. [design-doc.md](design-doc.md) §2 contradicts itself: it says both
that an active Burn gives power equal to the burned amount and that power is `Σ A / D`. The formula applies here.

The exact result can be obtained like this: group the Burns by duration, `S_D = Σ A_b` over the Burns with `D_b = D`;
then

```text
MintingPower(M, Y) = Σ ⌊S_D / D⌋ + ⌊ Σ (S_D mod D) / D ⌋      over all distinct D
```

In the second term each fraction is less than 1, and only this term needs rational arithmetic. There are at most 157
distinct values of `D` (MP-3), so all fractions can be brought to the common denominator `Λ = LCM(144, …, 300)`, a
432-bit number. Then the second term equals `⌊ Σ (S_D mod D)·(Λ / D) / Λ ⌋`, and the numerator is less than `157·Λ`:
512-bit integers suffice.

**MP-7.** The unit of Minting Power is satoshis per block. A Burn with `A < D` gives less than one unit on its own, but
its fraction counts in the sum: 1000 Burns of 100 satoshis each with `D = 200` give `⌊1000·100/200⌋ = 500`, not 0. The
Minting Power of all minters together is at most the number of satoshis that can exist at all, `21·10^14 < 2^51`, so
there is no u64 overflow.

**MP-8.** Total Minting Power of the network at height `Y`:

```text
TotalMintingPower(Y) = Σ MintingPower(M, Y)      over all minters M
```

It is needed for finality (FC-7). BFT computes its own total, `BftTotal`, without banned identities (BFT-7).

**MP-9.** The weight of an endorsement of a core block with target `T` is computed from `MintingPower(M, T)`, at
whatever height `V ≥ T` the endorsement gets into Bitcoin (1.5, section 4).

**MP-10.** Apart from MP-4, only a ban for double sign can take a minter's power away. According to
[design-doc.md](design-doc.md) §4, after a double sign "all Burns of such Minter became unactive". From which height the
ban applies, whether it is counted by `T` or by `V`, and what happens to the minter's endorsements is defined in
section 4; what happens to delegators, in section 9.

### 3.4. Minting identity

**MP-11.** A minter's Minting identity is its Minter P2WSH, that is the SHA-256 of the witness script (IN-14). All Burns
with the same Minter P2WSH give power to one minter. The Flame Predicate and the Validator Pubkey are part of the witness
script, so the Minter P2WSH determines them uniquely. Changing either of them, or the authorization script, gives a
different identity, and the earlier Burns do not move to it. A Burn itself reveals neither the Flame Predicate nor the
Validator Pubkey: the node learns them from the minter's endorsements (IN-14). How Minting Power maps to validator weight
in BFT when one Validator Pubkey appears in several identities is in section 8.

### 3.5. Rationale

**Why Burn.** Consensus weight is backed by burned BTC, so it has an external economic cost.

**Why Burn and endorsement are separate operations.** A minter has to commit power in advance and hold it for the whole
duration of the Burn. If power could be bought and spent at once, a short fork would need just over 50% of the power of
a single slot. As it is, power cannot be bought for less than `MIN_DURATION = 144` blocks, even if the adversary needs
it for only a few slots. Power `P` costs at least `144·P` satoshis. Honest minters with total power `P_h` burn, in a
steady state, `P_h` satoshis per block. So to outweigh them in even one slot, the adversary burns more than they burn in
a day.

**Why power is divided by `D`.** The same amount over a longer period gives proportionally less power per block, and
power times duration equals the burned amount for any `D`. So the choice of duration does not affect a minter's total
weight, and a longer Burn does not reduce security.

**Why the duration has an upper bound.** Security does not need an upper bound (see above); it simplifies computation:
with `MAX_DURATION = 300` there are at most 157 distinct `D`, and the exact sum in MP-6 is computed in 512-bit integers.
Without a bound there could be up to 65 thousand distinct `D`, and the common denominator would grow to about 94
thousand bits. The cost of the bound is that a minter renews its Burns at least once every 300 blocks, about two days.

**Why `Z ≥ 1`.** With `T = h + 1` (1.5), at height `T` only Burns from blocks up to `h` inclusive are active. So the
power at `T` is already known when the core block is created, and it is the same cut as the anchor block of the BFT
session (1.6). With `Z = 0` a Burn and an endorsement using it could land in the same block: power would appear at the
moment of voting.

**Why power is taken at height `T`.** A late endorsement cannot use Burns made after `T`. Otherwise the adversary could
wait for the outcome of a slot, buy more power and re-vote after the fact.

## 4. Endorsements

This section defines which records from Minting Transactions (IN-15) become endorsements, how much each endorsement
weighs, and how a minter is punished for double sign. How endorsement weights add up to the weight of a core block and
a chain is in section 6; how rewards are split by them is in section 7.

Notation: `C` is a core block with core height `c` and target `T`; `V` is the height of the Bitcoin block that includes
the Minting Transaction.

### 4.1. Endorsement

**EN-1.** A record `(M, Flame Predicate, Validator Pubkey, c, hash(C), V, txid)` (IN-15) points to core block `C` if the
Flame blocks known to the node (IN-6) include a valid (section 5) core block with hash `hash(C)` and core height `c`. If
there is no such block, the record affects neither weight nor rewards; it takes part in double sign even without the
block (EN-6). When the block becomes known, the record counts with the same height `V`, as if the block had been known
from the start: the result depends on the inputs, not on the order in which they arrived (0.4). Two consequences
follow:

- a node keeps records for unknown blocks indefinitely: the block may appear at any moment. An implementation SHOULD
  store them compactly on disk, indexed by `(c, hash(C))`, so it can find them immediately when the block appears.
  ⚠ **diverges from the code**: in the code such records are kept in memory. Why without expiry: 4.4;
- a core block that was hidden and revealed late gets weight by the original `V` of its endorsements. Why this is
  acceptable: 4.4.

**EN-2.** A record of minter `M` pointing to core block `C` with target `T` and included in block `V` is a valid
endorsement if

1. `V ≥ T`;
2. `V ≤ T + GRACE_DELAY + MAX_DELAY`, that is `V ≤ T + 11`;
3. `MintingPower(M, T) > 0` (MP-6).

Nothing else is checked; in particular, it does not matter whether `C` is in the canonical chain. A record that fails
condition 1 or 2 never becomes valid. Its `T = h₀ + c` is known from the record itself (CB-4), so a node can drop it
right away, without waiting for the block. ⚠ **diverges from the previous doc and the code**: there an endorsement with
any delay `d ≥ 0` is valid, and with `d > 10` it weighs 0 and still takes part in double sign; besides, instead of
condition 3 the code checks that `M` has at least one Burn active at height `T`, while by MP-6 the power of such Burns
can round down to zero. Why a late endorsement is invalid: 4.4.

**EN-3.** Records of different minters from one transaction are independent endorsements of the same core block, and
each minter contributes its own weight. This way minters can batch endorsements into one transaction.

**EN-4.** If minter `M` has several valid endorsements of the same core block, from one transaction (several
authenticating inputs) or from different ones, one counts: the one with the smallest `V`. The others have no effect.
[design-doc.md](design-doc.md) only speaks of several inputs in one transaction; repeats from different transactions
are resolved the same way.

### 4.2. Endorsement weight

**EN-5.** The delay of an endorsement is `d = V − T`; for a valid endorsement `0 ≤ d ≤ GRACE_DELAY + MAX_DELAY` (EN-2).
Its Effective Minting Power (EMP):

```text
k   = max(0, d − GRACE_DELAY)
EMP = ⌊MintingPower(M, T) / 2^k⌋
```

Here `k` is the number of blocks of delay beyond the grace window. `GRACE_DELAY = 1`, `MAX_DELAY = 10` (section 11).
Minting Power is taken at height `T` whatever the delay (MP-9). ⚠ **diverges from the previous doc and the code**: in
[design-doc.md](design-doc.md) and in the code there is no grace window: `EMP = ⌊MintingPower(M, T) / 2^d⌋`, zero for
`d > 10`. Why the window is needed: 1.6.

**EMP = 0.** This happens only if `MintingPower(M, T) < 2^k`. Such an endorsement is valid and takes part in double sign
(4.3), but gives neither weight (section 6) nor rewards (section 7).

### 4.3. Double sign and ban

**EN-6.** A record of minter `M` (IN-15) with core height `c` is admissible if it satisfies the conditions of EN-2 for
`T = h₀ + c`: `T ≤ V ≤ T + GRACE_DELAY + MAX_DELAY` and `MintingPower(M, T) > 0`. The block itself need not be known for
this: the target follows from `c` (CB-4). A double sign of minter `M` is two of its admissible records with the same
core height `c` and different block hashes. It does not matter whether these blocks are known to the node, whether they
are valid, or whether they exist at all. ⚠ **diverges from the previous doc and the code**: in
[design-doc.md](design-doc.md) double sign means endorsements of "two different blocks", and the code looks for it only
among endorsements of known core blocks. Why by records: 4.4.

The core height uniquely determines the slot: `c = T − h₀` in any chain (CB-4), so a double sign is two signatures of a
minter for different blocks of the same slot. Consequence for an honest minter (FL-02): if fork choice switched to
another branch, the minter does not endorse that branch's core blocks for slots it already endorsed on the old branch.
There are at most 12 such live slots. The minter must remember its endorsements across restarts as well (section 10).

**EN-7.** The ban height `V_ban(M)` is the smallest height `V` such that the admissible records of `M` (EN-6) included in
blocks not higher than `V` contain a double sign. If there is no such height, `M` is not banned.

**EN-8.** If `M` is banned, the following do not count:

1. all its endorsements included at heights `V ≥ V_ban(M)`, for any core blocks;
2. all its endorsements with those core heights at which there is a double sign already among the records included not
   higher than `V_ban(M)`, even if they were included before `V_ban(M)`.

Its other endorsements included before `V_ban(M)` count as usual. The ban is permanent and applies to the identity `M`,
that is to all its Burns, including those created later. How the ban affects BFT weight, delegators, and rewards for
endorsements before `V_ban(M)` is in sections 7–9.

⚠ **diverges from the code** (author's decision in PR #49, FL-03): in the code the ban is a global flag of the minter.
It is set as soon as the node detects a double sign. Endorsements the node processes after that are rejected even if
they were included earlier, and endorsements processed in the same batch as the double sign are all discarded. The
outcome depends on the order in which the node learned about blocks. [design-doc.md](design-doc.md) only says that after
a double sign "all Burns of such Minter became unactive", without saying from which height.

**EN-9.** Endorsements and their weights are a function of the Bitcoin view and the set of known Flame blocks, while
double sign and `V_ban` are a function of the Bitcoin view alone (EN-6). When the inputs change, everything is
recomputed by the same rules. Together with `V_ban(M)`, by EN-8, the set of counted endorsements of `M` changes, and with
it the weights of core blocks (section 6).

`V_ban(M)` is always the inclusion height of one of the records of `M`: the one at which some core height first got a
second hash. It depends only on records at heights not above it and on the Burns that determine their admissibility.
Such a Burn is active at a height `T ≤ V`, and therefore included below `T` (MP-4, MP-5).

**A Bitcoin reorg** that replaces blocks starting from height `F` (in the notation of 1.7, `F = H − r + 1`):

| Before the reorg | After the reorg |
|---|---|
| `V_ban(M) < F` | Does not change: everything it depends on lies below `F` |
| `V_ban(M) ≥ F` | Recomputed from the new branch and stays `≥ F`. In an honest race the record at which the double sign happened usually returns in the new branch, and `V_ban(M)` becomes its new height, which may be lower or higher than before. If it does not return and `M` has no other double sign in the new branch, the ban is lifted |
| No ban | No ban, or a ban appears with `V_ban(M) ≥ F`: the new branch has a double sign that the old one did not |

A ban cannot appear below `F`: the double sign would have been there before the reorg too.

**A newly known Flame block** does not affect bans: a double sign is visible from the records even before the block is
revealed (EN-6). The block only adds endorsements for itself (EN-1).

⚠ **diverges from the code**: rolling back a Bitcoin block does not return the node to its previous state if the block
contained an endorsement of an already banned minter (FL-11).

### 4.4. Rationale

**Why a delay penalty.** According to [design-doc.md](design-doc.md): so that an attempt to fork the chain after the
canonical one has already been chosen is less effective. A late endorsement for a competing branch weighs less than a
timely one for the canonical branch. The grace window and its cost: 1.6.

**Why the earliest of repeats counts.** A minter may send an endorsement again, for example after a Bitcoin reorg (1.7).
A repeat must not change the weight in either direction. The earliest endorsement has the smallest delay, and extra
copies do not make it worse.

**Why an endorsement after `T + 11` is invalid rather than just weighing 0.** Otherwise a vote could be withdrawn after
the fact. A minter voted for `A` on time, and a year later publishes block `B` with the same core height and endorses
it. The new endorsement has zero weight, but it is valid, so there is a double sign, and by EN-8 the old vote for `A` is
removed too. If the minter's Burns have already expired, the ban costs it nothing, and it could erase its votes for
hundreds of past slots with one Bitcoin block. Past weights, the canonical chain and rewards would change at any depth.
With condition 2 of EN-2, a conflicting record is admissible only up to `T + 11` (EN-6), and after that the votes for
slot `T` do not change (FC-7).

**Why double sign is banned.** An endorsement costs the minter only a Bitcoin fee; no power is spent. Without
punishment a minter would benefit from endorsing all competing branches: its weight would go to whichever wins, and the
choice between branches would stop costing anything.

**Why double sign is counted by records rather than by blocks.** If it were looked for among endorsements of known
blocks, the ban would depend on which blocks the node knows. A minter could sign a public block and a hidden block of
the same slot, and reveal the hidden one a year later. Then `V_ban` would move into the past, and with it the weights of
old slots and already paid rewards would change (RW-8). By records, the ban is a function of the Bitcoin view alone and
is final by `T + 11`. This poses no threat to an honest minter: it never signs two hashes with the same core height
(section 10). And a dishonest one cannot hide a double sign: both records are in Bitcoin.

**Why the ban applies from `V_ban` rather than from the moment of detection.** The moment of detection differs between
nodes: it depends on when the node learned about both blocks. If the ban counted from it, nodes with the same inputs
would get different weights (FL-03), which violates 0.4. `V_ban` is a Bitcoin height; it is the same for all nodes with
the same view.

**Why both endorsements at the double-signed core height are removed.** Neither of the two is better than the other. If
the earlier one were kept, then with two endorsements in the same Bitcoin block the choice would be arbitrary.

**Why endorsements before the ban remain.** They were made when the minter had not yet broken the rules. Removing them
would mean changing the weights of long-past slots after the fact, and with them possibly the canonical chain and
rewards.

**Why records for unknown blocks are kept without expiry (FL-07).** Anyone can create such a record, without a Burn,
and the node does not know whether the block will ever appear. But a record is expensive for the attacker and cheap for
the node:

- one record is one authenticating input. Together with the P2WSH output this input spends, that is at least ≈ 102 vB
  of fee (input ≈ 59 vB, output 43 vB);
- a record takes 100–180 bytes in compact form, about as much as the attack data in the Bitcoin block itself, which any
  non-pruned Bitcoin node stores anyway;
- 1 GB of records costs the attacker ≈ 6.8 BTC at 1 sat/vB (≈ 0.68 BTC at 0.1 sat/vB), while storing 1 GB on disk
  costs on the order of $0.1 per month. Such records give neither weight nor rewards;
- the growth rate is limited by Bitcoin itself: a block fits at most ≈ 9,800 records, about 1.5 MB, and for that the
  attacker has to buy the whole block.

Restricting this to "only minters with Minting Power" does not help: for `MintingPower > 0` one Burn of 144 satoshis per
identity is enough, after which the identity can create any number of records. An expiry is unnecessary and harmful. If
records older than the expiry were dropped and the block were revealed later, old nodes would not count the weight of
these records, while a new node syncing from scratch would, and nodes would diverge in weights. Only records within the
delay window need to be kept: a record outside the window is invalid right away (EN-2). This confirms the author's
position in PR #49 ("the attack is too expensive").

**Why a core block revealed late gets full weight (FL-08).** A hidden branch becomes canonical only if it outweighs the
public one over the whole stretch from the divergence point to the tip, while honest minters endorse the public one all
that time. The attacker cannot endorse both branches: they have the same core heights, that is a double sign, and it is
visible right away from the records even if the branch is hidden (EN-6). So the attacker needs more weight than the
honest minters, summed over all slots of the stretch, that is a majority of Minting Power for as long as the branch is
hidden. It burns more than all honest minters over that period and gets no rewards. With such a majority one can fork
openly too; hiding only adds surprise. And the branch is not fully hidden: its endorsements are in Bitcoin, and any node
sees the records for unknown blocks and the Minting Power of their minters. This is exactly a private-chain attack with
a majority, as in Bitcoin. This confirms the author's position in PR #49 ("an acceptable risk").

## 5. Core blocks and block validity

This section defines which Flame blocks are valid and which of them are core blocks. Validity is needed by section 4 (an
endorsement counts only for a valid core block) and by section 6 (the canonical chain is chosen among valid blocks). How
core blocks are created is in sections 8 and 10.

### 5.1. Flame block validity

**CB-1.** A Flame block `B` is valid if all of the following hold:

1. `B` is genesis (CB-2), or its parent is known to the node and valid, and `B.height = parent.height + 1`;
2. `B` passes the checks of the Flame state machine ([blockchain.md](../blockchain.md)): version, limits, transactions
   and the state transition from the parent;
3. if `B` has a core header, CB-3 … CB-5 hold; if not, it has no `core_block_hash` either (IN-7).

Descendants of an invalid block are invalid. A block whose parent is unknown to the node has no effect until the parent
appears, just like a record for an unknown block (EN-1).

**CB-2.** Genesis is the only block with `height = 0` and a zero `parent`. It is defined by the deployment parameters
(section 11), is the same for all nodes of a network, and is valid by definition. Genesis is a core block with core
height `c = 0`, target `T = h₀`, where `h₀` is the height of the start block (IN-1), and a zero `core_block_hash`. There
are no valid endorsements for it: everyone's Minting Power at height `h₀` is zero, because Burns below the start block
are not taken into account (IN-1, EN-2). So genesis gets neither weight nor rewards. ⚠ **diverges from the code**: in
the code genesis has no core header.

### 5.2. Core header

Notation: `C` is a core block, `C_prev` the previous core block of its chain, that is the nearest core block among the
ancestors of `C` (IN-7). Every core block except genesis has a `C_prev`: genesis is an ancestor of every block.

**CB-3.** Core height: `c(C) = c(C_prev) + 1`. Genesis has `c = 0`, so the first core block after it has `c = 1`; the
first reward epoch starts with it (section 7). ⚠ **diverges from the code**: the core height is not checked when a block
is accepted.

**CB-4.** Target: `T(C) = T(C_prev) + 1`. Genesis has `T = h₀`, so the first core block after it has `T = h₀ + 1`.
Slots cannot be skipped: dead slots (1.8) are closed by empty core blocks. Together with CB-3 this gives `c = T − h₀`
for every core block of every chain, genesis included. ⚠ **diverges from the previous doc and the code**: in
[core-blocks.md](core-blocks.md) the target is compared with the largest target among all core blocks known to the
node, not with the previous one in the chain, and the result depends on which blocks the node knows; the code does not
check the target at all. The rule is stricter than the author's decision in PR #49 (FL-01), where the target grows by at
least 1.

**CB-5.** `core_block_hash(C) = hash(C_prev)`; zeros for genesis (IN-7).

**CB-6.** There are no other conditions on the core header. In particular, validity does not compare `T` with the
Bitcoin view. A core block with `T` above the Bitcoin tip is valid, but endorsements for it become valid only from
height `T` on (EN-2). A core block for a dead slot is valid too but gets no weight (EN-5); a chain cannot skip such a
slot (CB-4). Creating the core block for slot `T` when the Bitcoin tip has reached `T − 1` is a rule of honest BFT (1.8,
sections 8 and 10), not a validity condition.

### 5.3. Validity does not depend on BFT, nor on Bitcoin except for reward payouts

**CB-7.** The validity of a block, except for the reward payout, depends only on the block itself and its ancestors. It
does not depend on the Bitcoin view, so a Bitcoin reorg does not make such a block invalid or vice versa (1.7). The
payout in a payout block is checked separately, against the Bitcoin view up to height `R` (RW-7). If after a Bitcoin
reorg the payout is different under the new view, the payout block is invalid.

While the tip of the Bitcoin view is below `R`, the payout cannot be checked. Such a block is not considered invalid; it
is not yet checked: it and its descendants do not take part in tip selection (FC-4), like a block with an unknown parent
(CB-1), and start taking part when the view reaches `R`. This happens only with a block from the future (CB-6): an honest
leader creates a payout block with the tip at `R + 88` or higher (RW-7). There are no valid endorsements for such a
block at that point either: they are possible only from `V ≥ T = R + 89` (EN-2).

**CB-8.** Validity does not depend on BFT. A block without a BFT certificate and a block created outside BFT are valid
under the same conditions (1.3). A BFT certificate does not make valid a block that fails CB-1: consensus ignores such a
block, whatever BFT decided. A certificate affects only the choice of the tip among equal weights (FC-5).
⚠ **diverges from `bft.md` (PR #61)**: there a node checks a core block against its own Bitcoin view and rejects it if it
has not yet received block `T − 1`, and a finalization certificate can override this check. Here this check is a
condition under which an honest validator votes for the block (section 8), and it does not affect validity. So the open
question of `bft.md` about a certificate for a block that failed the local check concerns BFT only.

**CB-9.** A regular block (without a core header) is valid by CB-1 but receives no endorsements: a record for it does
not find a core block (EN-1). Regular blocks between two core blocks enter the canonical chain together with the core
block they are ancestors of ([design-doc.md](design-doc.md) §3). How regular blocks after the last core block enter it
is in section 6.

### 5.4. Rationale

**Why only core blocks are endorsed.** According to [design-doc.md](design-doc.md) §3: if any block could be endorsed,
weight would fragment across neighboring blocks of the same chain, for example 1 unit on one and 1,000,000 on another.
It would be unclear which blocks are confirmed and where the tip is. A core block is an explicit anchoring point: regular
blocks between two core blocks enter the canonical chain together with the core block (CB-9), and weight is
concentrated on one block per slot (CB-4).

**Why validity does not depend on the Bitcoin view.** Otherwise nodes with different Bitcoin views would disagree on
which blocks are valid, and after a Bitcoin reorg valid blocks would become invalid together with all their
descendants. A core block refers to height `T`, not to a Bitcoin block, so there is nothing to check against Bitcoin
(1.7).

**Why the target grows by exactly 1.** There are two reasons.

*One core block per slot (FL-01).* If one chain could have several core blocks with the same target, the same Minting
Power would count in the chain as many times as there are core blocks in the slot. Example from the test
`fl01CoreSpamTest`: the honest minters have power 7, the adversary 4, and the chain weight is the sum of `⌊log2⌋` over
its core blocks (section 6).

```text
honest chain:       … → C(T)                     weight ⌊log2 7⌋ = 2
adversary's chain:  … → A1(T) → A2(T) → A3(T)    weight 3·⌊log2 4⌋ = 6
```

The adversary endorses all three of its core blocks: they have different core heights, so there is no double sign. With
4/11 of the power it outweighs 7/11. A linear sum does not help: `3·4 = 12 > 7`.

*The core height uniquely determines the slot.* Without skips, `c = T − h₀` in any chain. Then double sign by core
height (EN-6) is the same as double sign by slot, and an honest minter loses at most 12 live slots after switching
branches (FL-02). If slots could be skipped, the same core height would fall on different slots in different branches,
and after switching branches a minter would miss an unbounded number of slots. Reward epochs are also counted by core
height (section 7): without skips, 100 core blocks are always 100 slots.

The cost is that slots for which BFT did not produce a core block have to be closed by empty `E_T` (1.8). BFT downtime by
itself does not lead to core blocks with `TotalEMP = 0`: the honest minters endorse `E_T` on time. `TotalEMP = 0` happens
only if there are no endorsements for the slot or none made it into Bitcoin within the allowed window, for example after
a deep Bitcoin reorg or downtime of the minters themselves. A validity rule cannot allow skipping only dead slots:
deadness depends on the Bitcoin tip, and validity does not depend on the Bitcoin view (CB-7).

Comparing with the previous core block of the chain, rather than with all known ones as in core-blocks.md, makes the
check a function of the chain itself (0.4).

**Why the core height grows by exactly 1.** The core height is the key of double sign (EN-6). If two core blocks of one
chain had the same core height, an honest minter endorsing this chain would get banned. Reward epochs are also counted
by core height (section 7), and gaps in the numbering would shift them.

**Why the target has no upper bound.** A bound by the Bitcoin tip would make validity depend on the Bitcoin view. A block
with a target in the future gets nothing until Bitcoin reaches that target.

## 6. Weight and canonical chain selection

This section defines the weight of a core block and of a chain, and which chain is canonical. Endorsement weights are in
section 4, block validity in section 5.

### 6.1. Core block weight

**FC-1.** The total EMP of core block `C` is the sum of the EMP (EN-5) of all endorsements for `C` that count: valid
(EN-2), without repeats (EN-4), and not removed by a ban (EN-8). If there are none, it is 0.

```text
TotalEMP(C) = Σ EMP(e)      over the counted endorsements e for C
```

**FC-2.** Core block weight:

```text
BlockWeight(C) = log2(TotalEMP(C)),  if TotalEMP(C) ≥ 1
BlockWeight(C) = 0,                  if TotalEMP(C) = 0
```

`log2` is taken exactly, without rounding down (author's decision in PR #49, FL-05). ⚠ **diverges from the previous doc
and the code**: in [design-doc.md](design-doc.md) §5 and in the code `BlockWeight = ⌊log2 TotalEMP⌋`. How to compute and
compare the exact value, and whether to replace `log2` with a linear sum (FL-12, 1.8), is **Not
defined**<sup>[GAP-14](#gap-14)</sup>. A regular block has no weight.

### 6.2. Chain weight

**FC-3.** The weight of the chain ending at block `B`:

```text
ChainWeight(B) = Σ BlockWeight(C)      over core blocks C among B and its ancestors
```

Regular blocks and genesis add no weight. So a core block and all its regular descendants have the same chain weight,
as do a fresh core block without endorsements and its ancestors back to the previous core block.

### 6.3. Canonical chain

**FC-4.** The tip of the canonical chain is the valid (CB-1) block known to the node with the largest `ChainWeight`. All
such blocks are candidates: core blocks and regular ones, created by BFT and outside it (CB-8), except payout blocks
that cannot be checked yet and their descendants (CB-7). The canonical chain is the path from genesis to the tip.

**FC-5.** If several blocks have the largest `ChainWeight`, the tip among them is chosen as follows:

1. blocks with a BFT finalization certificate (IN-6); among several such, the one with the greatest Flame height;
2. if there are no such blocks, or several remain after step 1, the block with the smallest hash. Hashes are compared as
   32-byte arrays, lexicographically: byte by byte, starting from the first.

Ties happen all the time: a core block and its regular descendants, a fresh core block and its ancestors weigh the same
(FC-3). Step 1 makes the last block finalized by BFT the tip. ⚠ **diverges from the previous doc and the code**: in
[design-doc.md](design-doc.md) §5 and in the code a tie is decided only by hash, and all blocks with the largest weight
are candidates.

**FC-6.** The canonical chain is a function of the inputs (0.4): the Bitcoin view, from which endorsements are taken,
and the set of known Flame blocks and certificates. It changes with any change of the inputs and at any depth: a new
endorsement, block or certificate, or a Bitcoin reorg (1.7). If the tip moved to another branch, the blocks of the old
branch after the common ancestor are rolled back and the blocks of the new one are applied ([design-doc.md](design-doc.md)
§5).

**FC-7.** Finality is defined at two levels and is computed only from the Bitcoin view and known blocks. It does not
affect fork choice: it is an estimate for those who rely on a block (an exchange, a bridge, a wallet), not a chain
selection rule. Unlike the rejected `K_FINAL` (1.7), the node rejects nothing and does not halt. Notation:

- `N` is the height of the Bitcoin tip;
- `K` is the depth beyond which a Bitcoin reorg is considered impossible;
- `α_max < 1/2` is the largest adversary share of Minting Power.

`K` and `α_max` are chosen by whoever relies on finality; the defaults and the meaning of the assumptions are in
section 12.

1. **Final slot.** Slot `T` is final if `N ≥ T + GRACE_DELAY + MAX_DELAY + K = T + 11 + K`. There are no new valid
   endorsements for slot `T` after `T + 11` (EN-2), and by assumption the Bitcoin blocks with the existing ones will not
   drop out. So the set of records for slot `T` in Bitcoin is final, and it shows everything that can still change in
   the slot's weights: which core blocks, hidden ones included, can get weight. The bans that remove votes for slot `T`
   are final by then too: both records of a double sign are admissible only in the window of their slot (EN-6).
2. **Final block.** Block `B` of the canonical chain with slot `T_B` is final (for a regular block, `T_B` is the slot of
   the nearest core block among its descendants in the canonical chain; while there is none, a regular block is not
   final) if there is a final slot `T* ≥ T_B` such that for every branch diverging from the canonical one no later than
   `B`:

   ```text
   Σ (w(T) − w_alt(T))   over slots T from the divergence point to T*   >   G
   ```

   - `w(T)` is the weight of the canonical core block of slot `T` (FC-2), with bans already accounted for (EN-8);
   - `w_alt(T)` is the largest weight a core block of another branch can get on slot `T` from records in Bitcoin,
     including records for unknown blocks: their EMP is computed without the block, from `T = h₀ + c` (CB-4). The
     maximum is taken over all core blocks of slot `T` except the canonical one and known descendants of `B`,
     regardless of whether they are compatible across slots. This makes `w_alt` an upper bound for any branch diverging
     no later than `B`: descendants of `B` cannot displace `B`, and a record for an unknown block has unknown ancestors,
     so it always counts;
   - `G` is how much the alternative can gain on the non-final slots `T* + 1 … N + 1`, assuming the canonical chain has
     no weight there and the alternative has the weight of power `α_max · TotalMintingPower(T)` on each slot (weight
     formula: FC-2).

   After `N + 1`, by the assumption of section 12, the adversary has at most `α_max < 1/2` of the power, and honest
   minters' endorsements get into Bitcoin on time. Then with every new slot the alternative only falls further behind.

Why this is so, and what it means in blocks: 6.4.

### 6.4. Rationale

**Why weight is computed exactly (FL-05).** With `⌊log2⌋`, different powers weigh the same: an honest core block with
power 7 and an adversary core block with power 4 in the same slot both have weight `⌊log2⌋ = 2`. Then the hash decides,
and the adversary can grind it. This way 4/11 of the power beats 7/11 (test `fl05TieGrindTest`).

**Why `log2`.** [design-doc.md](design-doc.md) gives no reasons. The sum of `log2` over slots is `log2` of the product of
`TotalEMP`, that is chains are compared by the geometric mean of power per slot. The drawbacks of this formula are
computed in 1.8: it lowers the attack threshold during a BFT lag more than a linear sum, and it makes holes very
expensive (FL-12). The choice is GAP-14.

**Why BFT decides among equal weights.** BFT exists so that a majority of validators chooses the new block on top of the
heaviest one. Until minters have voted for this block, its weight equals its parent's, and BFT chooses it. Endorsements
exist so that minters can disagree with BFT: the branch with more weight wins, whether it has a certificate or not. The
hash decides only when both the weight and the BFT choice are equal. If the hash alone decided ties, the adversary could,
for free and outside BFT, create a descendant of the heavy block, grind it a hash smaller than that of the fresh BFT
block, and knock that block out of the canonical chain.

**Why all known blocks are candidates.** BFT coordinates but is not part of safety (1.1, CB-8). If only a block with a
BFT certificate could be the tip, a failure or capture of BFT would stop chain selection. A certificate only breaks
ties.

**Why finality is computed from Bitcoin (FC-7).** Four properties make it objective:

- *Slot weight freezes.* An endorsement for slot `T` is valid only up to `T + 11` (EN-2). After that no core block of
  slot `T` gains weight, however much Minting Power one has.
- *Power is known in advance.* Weight is computed from `MintingPower(M, T)`, and it includes only Burns below `T` (MP-4,
  MP-5). So the weight ceiling of any block of slot `T`, `TotalMintingPower(T)`, is known already at height `T − 1`. Power
  for a past slot cannot be bought.
- *Hidden blocks are visible.* By CB-4, `T = h₀ + c`, and a record contains `c`, `M` and `V`. So the EMP of a record for
  an unknown block is computed without the block itself, and the weight of any hidden branch on final slots can be read
  from Bitcoin.
- *Chains are compared slot by slot.* Every branch has exactly one core block per slot (CB-4), so
  `ChainWeight(A) − ChainWeight(B)` is the sum of per-slot weight differences after the divergence point.

A vote cannot be withdrawn after the fact either: both records of a double sign are admissible only in the window of
their slot (EN-6), so a ban that removes a vote for slot `T` is visible in Bitcoin already by `T + 11` and is accounted
for in `w`.

**What it costs to overtake a final block.** Final slots cannot be changed for any money, short of a Bitcoin reorg
deeper than `K`. The canonical chain can be overtaken only on non-final and future slots. The adversary's net gain per
slot is `log2(α / (1 − α))` with the sum of `log2`, or `(2α − 1) · P` with a linear sum, where `P` is
`TotalMintingPower`. As long as `α < 1/2`, it is negative: the adversary only falls further behind with every slot. A
minority gains temporarily only when honest votes are delayed: a BFT lag, censorship or a Bitcoin reorg (1.7, 1.8). This
is what the margin `G` covers. To win back the lead, the adversary needs a majority of power for many slots in a row,
and the power has to be burned in advance and for at least `MIN_DURATION = 144` blocks (3.5): more than the honest
minters burn over the same time, plus lost rewards.

**How long to wait.** Let all honest minters vote on time, `K = 6`, `α_max = 1/3`, `P = 2^40`, and let the alternative
diverge from the canonical chain at slot `T_B`. We need `n` final slots `T_B … T_B + n − 1`, and the last of them is
final at `N = T_B + n − 1 + 11 + K`. There are `12 + K = 18` non-final slots, and on each of them `G` gives the
alternative the weight of power `P/3`.

- *No competing records.* On a final slot `w_alt = 0`, and the lead is the whole honest weight, the weight of power
  `2P/3`. With a linear sum, `n · 2P/3 > 18 · P/3`, that is `n ≥ 10`, and the block is final at `N = T_B + 26`, after
  about 4 h 20 min. With the sum of `log2` the lead per slot is `log2(2P/3) ≈ 39.4`, and `G = 18 · log2(P/3) ≈ 691.5`. We
  need `n ≥ 18`, that is `N = T_B + 34`, about 5 h 40 min. Almost as many final slots are needed as there are non-final
  ones: with `log2`, the weight of power `P/3` is almost equal to the weight of `2P/3`.
- *The adversary keeps endorsing the alternative* with all its power `P/3`. Then on a final slot `w_alt` is the weight of
  power `P/3`. With a linear sum the lead per slot is `P/3`, we need `n ≥ 19`, and the block is final at
  `N = T_B + 35`, after about 6 hours. With the sum of `log2` the lead per slot is `log2 2 = 1`, we need `n ≥ 692`,
  `N = T_B + 708`, almost 5 days.

This is another argument for GAP-14.

## 7. Emission and rewards

This section defines how much FLM each core block issues, how the reward is split among Burns, and when it gets into
the Flame state. Rewards for delegated Burns are in section 9.

### 7.1. Emission

**RW-1.** New FLM is issued only by core blocks; regular blocks issue nothing. The reward of a core block with core
height `c`, in sparks (1 FLM = 10⁸ sparks):

```text
Reward(c) = ⌊50 · 10⁸ / 2^⌊c / 210,000⌋⌋
```

The reward halves every 210,000 core blocks, counting genesis (`c = 0`, CB-2). From `c = 33 · 210,000` on it is 0.
[design-doc.md](design-doc.md) §8 says that the reward becomes zero after 64 halvings; in sparks it is already zero after
the 33rd, so it is essentially the same thing.

The sum of `Reward(c)` over all `c ≥ 0` is 20,999,999.9769 FLM, as in Bitcoin. But genesis gets no reward: there are no
endorsements for it (CB-2), and its 50 FLM are burned (RW-5). So at most 20,999,949.9769 FLM is issued in total.

### 7.2. Distribution of a core block reward

**RW-2.** The reward of core block `C` is split among the counted endorsements for it (FC-1) in proportion to their EMP:

```text
reward(e) = Reward(c) · EMP(e) / TotalEMP(C)
```

`TotalEMP(C)` is the sum of EMP of only those endorsements that count for this core block `C` (FC-1). The power of
minters who did not vote, or voted for another block of the same slot, is not in the divisor. So if `TotalEMP(C) > 0`,
the reward of `C` is distributed in full, and if `TotalEMP(C) = 0`, it is burned (RW-5). This can happen with valid
endorsements too: each of them may have `EMP = 0` (EN-5). ⚠ **diverges from the code**: in the code the share is
computed in `f64` from the sum of `A / D / 2^d` over the minter's Burns, that is without the grace window (1.5) and
without the rounding of MP-6 and EN-5. A minter that did not endorse `C` does not take part in the split, even if it had
power.

**RW-3.** Within an endorsement `e` of minter `M`, the reward is split among the Burns of `M` active at height `T`, in
proportion to `A_b / D_b`:

```text
reward(e, b) = reward(e) · (A_b / D_b) / Σ (A_b' / D_b')      over Burns b' of minter M active at T
```

The recipient of the reward for Burn `b` is the pair `(Flame Predicate of minter M, Access Predicate of Burn b)`: the
amount goes to the Flame Predicate, and the Access Predicate is passed to it as an argument ([design-doc.md](design-doc.md)
§4, §8). What the Flame Predicate does with the reward (a commission, forwarding to the Access Predicate) is not checked
by consensus.

**RW-4.** The shares of RW-2 and RW-3 are computed exactly, in rational numbers. Rewards for all Burns with the same
pair `(Flame Predicate, Access Predicate)` are summed over the whole batch of 100 core blocks (RW-6), and the sum is
rounded down to sparks once. The rounding remainder is burned, like the reward of a core block with `TotalEMP = 0`
(RW-5). ⚠ **diverges from the code** (FL-04): in the code shares are computed in `f64`, and the reward of a pair is
rounded down in each core block separately.

**RW-5.** If `TotalEMP(C) = 0`, the reward of `C` is burned: nobody gets it, and it is not carried over anywhere. This
happens if there are no endorsements for the slot or none made it into Bitcoin within the allowed window (EN-2): the
minters are offline, miners censor endorsements, or a deep Bitcoin reorg pushed them out of the window (1.7). BFT
downtime by itself burns no rewards: slots are closed by `E_T`, and honest minters endorse them on time (1.8, rule 5).
So emission falls short of the RW-1 maximum by the rewards of such slots: a day without a single timely endorsement is
144 burned rewards. The code does the same. [design-doc.md](design-doc.md) §8 considered this case a deadlock; there is
no deadlock, because by CB-4 the chain moves on.

### 7.3. Distribution epochs and payout

**RW-6.** Rewards are computed for each core block separately, and get into the Flame state in batches of 100 core
blocks with a delay of one batch. A core block with `c % 100 = 0` and `c ≥ 200` is a payout block: it pays the rewards of
batch `c − 199 … c − 100` of its chain, that is of its ancestors. The first payout is in core block 200, for core blocks
1…100. Core blocks of a losing branch are never paid: their payout block does not get into the canonical chain, and the
endorsements for them get no rewards. ⚠ **diverges from the code**: the code computes the rewards of a batch already in
core block `c − 50`, from the Bitcoin tip the node sees at that moment, and defers them until core block `c`.

**RW-7.** The rewards of a batch are computed from the Bitcoin view up to height
`R = T_end + GRACE_DELAY + MAX_DELAY = T_end + 11`, where `T_end = h₀ + c − 100` is the target of the last core block of
the batch. All valid endorsements for the core blocks of the batch are included not higher than `R` (EN-2), so Bitcoin
blocks above `R` do not affect the batch's rewards. The payout block has target `h₀ + c = R + 89` and is created once
the Bitcoin tip has reached at least `R + 88` (1.7).

The payout block fixes `R` by its height, `R = h₀ + c − 89`; the hash of block `R` is not needed in it. The payout is a
function of the ancestors of the payout block and of the Bitcoin view up to height `R` (EN-2, RW-8), and a node checks it
by recomputing from its own view. After a Bitcoin reorg the payout is recomputed from the new view. If it changed, the
payout block is invalid (CB-7), and Flame is rewritten from it, as with any reorg (1.7). For this the reorg must replace
blocks not higher than `R`, that is be deeper than 88 blocks; with `K < 88` (FC-7) this does not happen. How the payout
is written into the Flame state is **Not defined**<sup>[GAP-18](#gap-18)</sup>.

**RW-8.** Endorsements removed by a ban (EN-8) get no rewards. All bans that remove endorsements for the core blocks of
a batch are visible in Bitcoin not higher than `R`: the records of a double sign are admissible only in the window of
their slot (EN-6), and the last slot of the batch is `T_end`. So later events do not change the payout.

**RW-9.** Fees for transactions in Flame blocks (BFT fees) and their recipients are **Not
defined**<sup>[GAP-20](#gap-20)</sup>.

### 7.4. Rationale

**Why emission is tied to core blocks.** According to [design-doc.md](design-doc.md) §8: BFT can produce any number of
regular blocks, while the number of core blocks is tied to the number of Bitcoin blocks. With CB-4 this holds exactly:
there is exactly one core block per Bitcoin block, and core height `c` sits at Bitcoin height `h₀ + c`. So a Flame halving
(every 210,000 core blocks) happens exactly every 210,000 Bitcoin blocks, about once every four years. The interval is
the same as for Bitcoin's own halving, but it is counted from the start block `h₀`, so Flame and Bitcoin halvings do not
coincide in time. If slots could be skipped, there would be fewer core blocks than Bitcoin blocks, and Flame halvings
would lag behind.

**Why the reward is split by EMP.** It is the same quantity as in fork choice ([design-doc.md](design-doc.md) §8): a late
endorsement has less effect on chain selection and gets less reward.

**Why the divisor is `TotalEMP(C)` rather than all Minting Power.** The reward is distributed in full, and a minter
benefits from endorsing even an almost dead slot (1.8). The flip side: a minter's share grows if other endorsements for
the same block did not get into Bitcoin on time. But for that, miners have to skip others' transactions in several
blocks in a row, losing their fees, for a slightly larger share of one core block's reward. Such censorship does not pay
for miners.

**Why the reward of a dead slot is burned.** There is nowhere to carry it without creating a new incentive. If it went to
the next core block with weight, a minter would benefit from others' endorsements not getting into Bitcoin. If it were
split within the batch, the reward for the slot would go to those who did not vote for that slot.

**Why rewards are computed exactly and rounded once per batch.** Nodes must get the same amounts, while `f64` and
intermediate rounding make the result depend on the order of computation and lose more. With one rounding per pair per
batch, less than 1 spark per pair is lost per 100 core blocks. The remainder is burned for the same reason as the reward
of a dead slot: redistributing it would create an incentive that should not exist.

**Why the payout is delayed and done in batches.** According to [design-doc.md](design-doc.md) §8, the delay protects
against Bitcoin reorgs, and batches reduce the number of payouts. `R` is the last height at which an endorsement for the
batch can still appear (EN-2). After `R` the data for the batch's rewards is complete, and the margin of 88 blocks below
the tip protects the payout from Bitcoin reorgs (1.7).

## 8. BFT: interface

This section defines what Minting consensus requires from BFT and what it gets from it: what a BFT session consists of,
how the validator set is computed from the Bitcoin view, when an honest validator votes for a core block, and when a
finalization certificate counts in tip selection (FC-5). How BFT itself works (Simplex on Commonware, signatures, leader
election, weighted quorum) is described in `design-doc/bft.md` from
[PR #61](https://github.com/runflame/flame-lib/pull/61), below simply bft.md.

Notation: `N` is the height of the Bitcoin tip in the view of the node or validator; `e` is the epoch of a session
(BFT-3); `E_T` is the deterministic empty core block of slot `T` (1.8, rule 5).

### 8.1. Role of BFT

**BFT-1.** Consensus requires three properties from BFT ([design-doc.md](design-doc.md) §6):

1. BFT produces a sequence of Flame blocks and finalization certificates for them;
2. validator votes are weighted by Minting Power (BFT-7);
3. BFT can be started from any given core block: every session starts from a root (BFT-3), and the root is chosen by
   Minting weights (BFT-5).

**BFT-2.** BFT is not part of safety. BFT blocks are valid under the same conditions as blocks created outside BFT
(CB-8). The canonical chain is chosen by weight (FC-4), and a certificate only breaks ties (FC-5). If minters chose
another branch, BFT moves to it (BFT-5). A BFT failure delays transactions but not slots: every slot for which BFT did
not produce a core block is closed by `E_T` (BFT-10).

### 8.2. Sessions

**BFT-3.** A session is a BFT instance with one validator set. It is defined by three things:

- the root `root`, the core block it builds from (BFT-5);
- the epoch `e = T(root) − 1`;
- the anchor, the Bitcoin block at height `e` in the node's view. The validator set is computed from the view up to and
  including the anchor (BFT-6, BFT-7). For the session with genesis as root, the anchor is the parent of the start
  block (BFT-8).

A session extends the chain from `root`: first regular blocks, then one core block of the next slot, `e + 2`.

```text
root (core, slot e + 1) → B1 → B2 → … → Bk → C (core, slot e + 2)
                          regular blocks       the session's core block
```

The session's core block is either `C`, assembled by the leader on top of the last finalized regular block, or the empty
`E_{e+2}`. Both have core height and target one greater than `root`, and `core_block_hash = hash(root)`
(CB-3 … CB-5). The parent of `E_{e+2}` is `root` itself (1.8, rule 5), so blocks `B1 … Bk` are not in its chain, and
their transactions return to the mempool. The finalized core block becomes the root of the next session. The rest of the
session descriptor (protocol version, network, validator set hash, signing namespace) is in bft.md.

In the normal course (1.5) the epoch equals the height of the Bitcoin tip while the session produces regular blocks, and
the core block the session produces after block `h = e + 1` arrives gets target `h + 1 = e + 2`. This is the bft.md rule
"target = epoch + 2". ⚠ **diverges from bft.md (PR #61)**: there the epoch is the height of the Bitcoin tip at the time
the session is created. The difference shows only during catch-up (1.8): with tip `N` the root is `E_N`, the epoch is
`N − 1`, and the session immediately produces a core block for the live slot `N + 1`. By bft.md the epoch would be `N`,
and target `N + 2` would not match the root `E_N` (CB-4).

**BFT-4.** An honest validator with Bitcoin tip `N`, in a session with epoch `e`:

1. signs regular blocks of the session only while `N = e`;
2. votes for the core block of slot `T = e + 2` only when `N ≥ e + 1`, that is when it has itself received Bitcoin
   block `T − 1`;
3. once slot `e + 2` has passed (BFT-5, item 2), votes for it only for `E_{e+2}`;
4. takes part only in the session whose root it chose itself by BFT-5, with an anchor from its own Bitcoin view.

Item 2 replaces the check of a core block against the Bitcoin view, which in bft.md is part of validity: by CB-8 it is a
voting condition. Item 3 is rule 5 of 1.8: this way a slow BFT converges to the same block as the minters.

**BFT-5.** An honest validator chooses the session root as follows. Let `C_w` be the last core block of the canonical
chain (FC-4) with `BlockWeight > 0` (FC-2), or genesis if there is none. The slots after `C_w` are filled in order,
`T = T(C_w) + 1, T(C_w) + 2, …`, and for slot `T` the validator takes:

1. the core block of slot `T` continuing the chain chosen so far, if the validator has a finalization certificate for
   it: one that counts by BFT-9, or one the validator verified by BFT-9 earlier, while the session's anchor was still in
   its Bitcoin view. If there are several such blocks, the one with the smallest hash (FC-5, step 2);
2. otherwise `E_T`, if the slot has passed: the validator has Bitcoin block `T + 1`, or `EMPTY_WAIT` has passed since it
   received block `T` (1.8, rules 1 and 5);
3. otherwise slot `T` is still open, and the choice stops there.

The root is the last chosen core block. If the certificate for the core block of slot `T` arrived after the validator
chose `E_T`, the choice is recomputed by item 1, just as a minter replaces its endorsement with RBF (1.8). After a
Bitcoin reorg, `C_w` and `N` change and the root is chosen anew (1.7, item 4), but certificates already verified stay
in force (item 1). Minters endorse core blocks of the same chain and consider a core block finalized in the same way
(section 10).

A certificate verified before a reorg counts only here, not in FC-5. Choosing the root is honest validator behavior,
not a consensus computation: it does not have to agree across all nodes with the same inputs (0.4).

### 8.3. Validator set

**BFT-6.** The Validator Pubkey of identity `M` is known at height `e` if a Bitcoin block not above `e` contains at
least one record of `M` (IN-15): whether it is a valid endorsement and whether its block is known does not matter. The
key is part of the witness script, so an identity has exactly one (MP-11). A key revealed only in blocks dropped by a
reorg stops being known (1.7). bft.md requires the same: to become a validator, one has to endorse at least once, and
the key must be revealed by the anchor.

**BFT-7.** The validator set of a session with epoch `e`. A validator is a Validator Pubkey `v`, not an identity: its
weight is the total power of all identities with this key. ⚠ **diverges from the previous doc**: in
[design-doc.md](design-doc.md) §6 a validator is a minter, that is an identity, and its weight is the Minting Power of
that identity. bft.md also counts weight by key. Power is taken at height `e + 1 = T(root)`, excluding identities banned
not above `e` (EN-7):

```text
BftWeight(v, e) = Σ MintingPower(M, e + 1)    over identities M with Validator Pubkey v known at height e (BFT-6),
                                              not banned or with V_ban(M) > e
BftTotal(e)     = Σ MintingPower(M, e + 1)    over all identities M not banned or with V_ban(M) > e
```

A ban removes only the banned identity. Other identities with the same key stay in the weight of `v`.

Validator `v` is in the validator set if `BftWeight(v, e) > 0` and

```text
1024 · BftWeight(v, e) ≥ BftTotal(e)
```

So there are at most 1024 validators. The vote of `v` weighs `BftWeight(v, e)`; a quorum is more than 2/3 of the total
weight of the validator set (BFT-9). Delegated Burns count in the `MintingPower` of an identity like any others
(section 9).

BFT-6 and BFT-7 are a function of the Bitcoin view up to and including the anchor: with `Z ≥ 1`, the Burns active at
`e + 1` are included not above `e` (MP-4, MP-5), and `V_ban(M) ≤ e` is visible from records not above `e` (EN-9). So
nodes with the same anchor have the same validator set.

**BFT-8.** If the validator set is empty, there are no certificates (BFT-9), and every slot is closed by `E_T` (BFT-5,
item 2). This is how the network starts. For the session with genesis as root, the epoch `h₀ − 1` lies below the start
block, and the view has no Burns or keys yet (IN-1). The first slots are closed by empty blocks; minters endorse them
(1.8, rule 5) and thereby reveal their keys (BFT-6). BFT starts working from the first session whose validator set is
non-empty. In bft.md the empty validator set case is left as a TODO.

### 8.4. Finalization certificate

**BFT-9.** A finalization certificate for block `B` from a session with root `root` and anchor `a` counts (IN-6, FC-5)
if, in the node's view, all of the following hold:

1. `a` is the block of the view at height `e = T(root) − 1`;
2. `root` is the nearest core block among the ancestors of `B`, that is `B` is a regular block of this session or its
   core block of slot `e + 2`;
3. the certificate is signed by distinct validators of this session's validator set (BFT-7) with a total weight of more
   than 2/3 of the total weight of the whole validator set. The signature format and the session namespace are in
   bft.md.

If the anchor dropped out in a Bitcoin reorg, the certificate does not count; if the anchor returned to the view, it
counts again (IN-3). With an empty validator set condition 3 cannot hold: the total weight is 0. A certificate does not
affect the validity of `B` (CB-8).

### 8.5. Failures and fees

**BFT-10.** A session does not finalize a core block if more than a third of its weight is unavailable, behaves
dishonestly, or works in another session because of a different anchor (bft.md, section Discussion). Then the slot is
closed by `E_T` (BFT-5, item 2), and the next session starts with a new validator set. Epoch changes do not depend on a
quorum. So the open problem of bft.md "One Third of the Quorum Can Block an Epoch Transition" is resolved by the very
solution proposed there: a deterministic empty block. More than a third of the BFT weight can delay transactions for as
long as it holds a third of the weight in every session, but it cannot stop slots or change weights (BFT-2). This is a
liveness assumption (PROP-4, PROP-13).

**BFT-11.** There is no separate reward for taking part in BFT: a validator gets rewards only as a minter (section 7).
Fees for transactions in Flame blocks are **Not defined**<sup>[GAP-20](#gap-20)</sup>.

### 8.6. Rationale

**Why the epoch is counted from the root (BFT-3).** The target grows by exactly 1 (CB-4), so the slot of the session's
core block is set by the root, not by the Bitcoin tip. In the normal course this is the same as in bft.md. During
catch-up, epoch `N − 1` allows a core block with transactions to be produced right away for the live slot `N + 1`
(1.8, rule 1). The validator set is then one block older than the tip, just as in the normal course when the session
produces a core block after block `e + 1` arrives.

**Why power is taken at `e + 1` (BFT-7).** It is the same power with which minters endorse the session's root (MP-9):
whoever has more weight in Minting at the root's slot has more weight in BFT too. It can already be computed from the
view up to the anchor, because `Z ≥ 1` (3.5). Power at height `e` would lag one block behind: it would miss the Burns
that matured by `e + 1` and include those that expired at `e + 1`. [design-doc.md](design-doc.md) speaks of "current
Minting Power", bft.md of power by the anchor; neither names a height.

**Why the key must be revealed by the anchor (BFT-6).** The validator set must be a function of the view up to the
anchor; otherwise nodes that see blocks above the anchor differently would disagree on the session's membership.
Someone else's identity cannot be taken over: the Minter P2WSH is the SHA-256 of the script containing the key.

**Why a validator is a key and a ban is per identity (BFT-7).** A BFT signature is made with a key, and session
participants are keys (bft.md), so identities with the same key vote with one signature. One operator may have several
of them: a new commission means a new Flame Predicate, that is a new identity (DL-5), while delegators' Burns stay on the
old one for up to 300 more blocks (DL-3); the same with different commissions for different delegators and with a change
of the authorization script. With weight by key, the operator stays one validator with the same weight all that time.
If each identity had to pass the 1/1024 threshold separately, an operator whose power is split during a commission change
could drop out of BFT.

A ban is for a double sign in Minting, and it is proven by spending an output of a specific identity (IN-14, EN-6). The
BFT key plays no part in it and says nothing about the owner: anyone can put someone else's Validator Pubkey into their
own witness script. If a ban removed the whole key, the adversary would put someone else's key into its own identity,
double sign, and knock the key's owner out of BFT. Putting someone else's key into one's own identity only gives the
key's owner a vote in BFT; the adversary keeps the Minting rewards (DL-5), and there are no rewards for BFT (BFT-11).
Stalling BFT with power that does not sign is possible with one's own key too, so this gives no new attack.

**Why banned identities are not in `BftTotal(e)`.** According to [design-doc.md](design-doc.md) §4, after a double sign
the minter's Burns are inactive, and a banned identity gives weight neither in Minting (EN-8) nor in BFT. If its power
stayed in `BftTotal(e)`, the 1/1024 threshold would grow with power that votes for nothing, and honest validators would
drop out of the validator set.

**Why the 1/1024 threshold.** It limits the validator set to 1024 participants ([design-doc.md](design-doc.md) §6): a
quorum is assembled from validator signatures, and their number has to be bounded. A minter below the threshold endorses
as usual and only loses participation in BFT, which is not rewarded separately (BFT-11). Small holders can pool power
with one validator through delegation (section 9).

**Why the root is the last core block with weight, followed by certificates and empty blocks (BFT-5).** Where core
blocks already have weight, minters have chosen the branch, and BFT follows it (BFT-2), even if it was created outside
BFT. Where there is no weight yet, the tip by FC-5 is decided by a certificate, and without one by the hash. The
adversary could create a core block with a ground hash outside BFT and lead the root of the next session onto its own
branch. So beyond the last block with weight, only what BFT itself finalized, or `E_T`, which is the same for everyone,
can serve as the root. A certificate is good even if the validator has not yet received block `T − 1`: the validity of
a core block does not depend on the Bitcoin view (CB-7). So the bft.md question "Finalization Certificates Override
Local Validation" does not arise for Flame.

An already verified certificate stays in force for choosing the root even if the anchor of its session dropped out in a
Bitcoin reorg (BFT-5, item 1). Otherwise a reorg of depth 2 would break the normal course. Core block `C_{h+1}` was
finalized by the session with anchor `h − 1`, and minters are already endorsing it, but their endorsements are not yet
in Bitcoin. The reorg replaces block `h − 1`, the certificate stops counting by BFT-9, and `C_{h+1}` has no weight yet.
Then validators would move to `E_{h+1}` after `EMPTY_WAIT`, while minters stayed on `C_{h+1}`, and the votes for the
slot would split between two branches.

**Why a certificate is bound to the anchor and the slot (BFT-9).** The validator set of a session is derived from the
view up to the anchor (BFT-7). If the anchor dropped out, the node cannot check whether the signers had a quorum: under
the new view they have different weights. Conditions 1 and 2 bind the certificate to a slot: a session with epoch `e`
signs only blocks of slots `e + 1` and `e + 2`. Otherwise a validator set from the past, in which the adversary had more
than 2/3 of the weight, could sign new blocks and win ties.

## 9. Delegation

This section defines how a participant gives its Minting Power to someone else's Minting identity and gets rewards for
it. There is no separate mechanism for this: delegation is an ordinary Burn (section 3), and everything below follows
from the rules of sections 3, 4, 7 and 8.

Notation: `M` is the Minting identity being delegated to; the delegator is whoever burns BTC in its favor.

### 9.1. Delegated Burn

**DL-1.** Delegation is a Burn with the Minter P2WSH of someone else's identity `M` and the delegator's Access Predicate
(MP-1, MP-2). The protocol does not distinguish delegated Burns from `M`'s own Burns. An "own" Burn is a Burn whose
Access Predicate belongs to the owner of `M`, and the protocol does not know that. So all rules for the Burns of `M`
apply to delegated Burns in the same way.

**DL-2.** The power of a delegated Burn is part of `MintingPower(M, Y)` (MP-6). Only `M` endorses with it: an
endorsement is authenticated by spending an output to the Minter P2WSH `M` (IN-14). The delegator signs nothing and
sends no Minting Transactions. The delay penalty (EN-5) applies to all of `M`'s power equally. Delegated power is part
of the BFT weight of the validator, the Validator Pubkey of `M`, and counts toward its 1/1024 threshold (BFT-7).

**DL-3.** A delegation lasts while the Burn is active: `D` blocks after maturing (MP-4). It cannot be revoked or moved
to another identity ([design-doc.md](design-doc.md) §7). The delegator chooses only the duration of the Burn and whether
to make the next one.

### 9.2. Rewards

**DL-4.** The reward for a delegated Burn `b` is computed the same way as for an own one (RW-3, RW-4): it is the share
of the reward of `M`'s endorsement proportional to `A_b / D_b`, and the recipient is the pair
`(Flame Predicate of M, Access Predicate of b)`. If `M` did not endorse a core block, or its endorsement does not count,
the delegator gets nothing for that core block.

**DL-5.** The commission of `M` is set by its Flame Predicate: the reward goes to it, and the Access Predicate is passed
to it as an argument. Usually the Flame Predicate keeps a commission and forwards the rest to the Access Predicate
([design-doc.md](design-doc.md) §7), but consensus does not check this (RW-3). How exactly the Access Predicate is
passed to the Flame Predicate depends on how the payout is written into the Flame state, which is **Not
defined**<sup>[GAP-18](#gap-18)</sup>. The Flame Predicate is part of `M`'s witness script, so it is the same for all
delegators of `M` and does not change while their Burns live: a different Flame Predicate is a different identity
(MP-11).

### 9.3. Ban

**DL-6.** A ban of `M` (EN-7, EN-8) applies to all Burns of `M`, delegated ones and those created after the ban
included. Endorsements of `M` from height `V_ban(M)` on do not count, so for the rest of their Burns' lifetime the
delegators get no rewards, and the power of `M` drops out of BFT (BFT-7). Rewards for endorsements of `M` included
before `V_ban(M)` are received by the delegators as usual, except at the double-signed core heights (EN-8, item 2;
RW-8). A Burn in favor of an already banned identity is created (MP-1) but yields nothing. A delegator loses at most the
remainder of its Burn, that is at most `MAX_DURATION = 300` blocks. ⚠ **diverges from the code**: in the code the ban is
a global flag of the minter without a height (EN-8), and if the rewards of a batch are computed after the node detected
the double sign, the delegators also lose the rewards for endorsements before it.

### 9.4. Rationale

**Why delegation is an ordinary Burn.** No separate transaction type, no separate state and no consensus-level
commission are needed: each `M` sets its own commission with its Flame Predicate ([design-doc.md](design-doc.md) §7).

**Why a ban affects delegators.** The protocol does not distinguish delegated Burns from own ones (DL-1). If a ban spared
Burns with someone else's Access Predicate, the owner of `M` would burn under another Access Predicate and double sign
without losses. So a delegator shares the risk of whoever it delegates to. The risk is bounded: a Burn lives no longer
than `MAX_DURATION`, and short Burns reduce it further.

**Why delegation cannot be revoked.** Revocation would need a separate transaction type, and the power of `M` would
change in the middle of a Burn's term. With `D ≤ 300` a delegator can move to another `M` in about two days anyway: it is
enough not to renew the Burn.

**How a delegator checks `M`.** The Minter P2WSH is the SHA-256 of the witness script, so from the script that the owner
of `M` publishes, or that is visible in its past endorsements, the delegator checks both the Flame Predicate, that is the
commission policy, and the Validator Pubkey. Whether `M` is banned is visible from Bitcoin (EN-7).

`M` must not be chosen by Validator Pubkey. Anyone can create their own identity `M'` with the key of a well-known
validator and their own Flame Predicate. The BFT weight of `M'` goes to the key's owner (BFT-7), and from the outside
`M'` looks like an identity of that validator. But the rewards for Burns in favor of `M'` pass through the adversary's
Flame Predicate (DL-5), and it can keep them. So a delegator chooses a Minter P2WSH and checks its Flame Predicate.
Explorers and wallets had better show weight and commission per identity, not per key.

## 10. Honest participant behavior

This section describes how an honest BFT leader and an honest minter behave. These are not validity conditions and not
part of the consensus computation: a block or an endorsement made against these rules is processed like any other
(CB-8, EN-2). But the guarantees of section 12 rely on them, and a minter that breaks them risks a ban (EN-7) and loses
weight and rewards. The rules for an honest validator are in 8.2 (BFT-4, BFT-5).

Notation: `N` is the height of the Bitcoin tip in the participant's view; `M` is a minter. The *honest chain* is the
chain chosen by BFT-5: the canonical chain (FC-4) up to the last core block with weight, `C_w`, and after it, slot by
slot, a core block with a certificate or `E_T`.

### 10.1. BFT leader

**HB-1.** The leader of a session with epoch `e`, once it has itself received Bitcoin block `e + 1`, proposes the core
block of slot `e + 2`: the next block of the session on top of its last finalized block, with a core header by
CB-3 … CB-5. The block MAY be empty (1.8, rule 2). If a core block has already been finalized in the session, the
leader does not propose a second one (BFT-3). If slot `e + 2` has passed (BFT-5, item 2), the leader proposes
`E_{e+2}`. ⚠ **diverges from the previous doc**: in [core-blocks.md](core-blocks.md) the leader sets the target two
above the block that arrived (1.5). There is no leader code: in demo-consensus core blocks and their targets are set by
hand.

**HB-2.** Into a payout block (RW-6) the leader writes the payout of the batch by RW-2 … RW-8. In what form is **Not
defined**<sup>[GAP-18](#gap-18)</sup>.

### 10.2. Minter: what to endorse and when

**HB-3.** An honest minter `M` MUST endorse the core block of slot `T` of the honest chain if

1. the slot is live: `T ≥ N − 10` (1.8);
2. `MintingPower(M, T) > 0`, otherwise the record is invalid (EN-2);
3. the minter has not yet signed another hash for this core height, or can replace it by HB-5, item 3.

The minter chooses the chain the same way as a validator (BFT-5). In particular, it takes into account a certificate it
verified by BFT-9 before a Bitcoin reorg that threw out the anchor. So minters and validators stay on the same branch.

**HB-4.** The minter sends an endorsement as soon as the core block of the slot is chosen by HB-3:

- a core block with a certificate: as soon as it receives the certificate (1.5, step 2);
- `E_T`: once the slot has passed, `EMPTY_WAIT` after block `T`, or right away if block `T + 1` already exists (1.8,
  rule 5).

It waits neither for the next Bitcoin block nor for earlier endorsements to confirm. It endorses live slots from old to
new, one transaction per slot (1.8, rule 3). ⚠ **diverges from the previous doc and the code**: in
[core-blocks.md](core-blocks.md) (steps 1–3) and in the code the minter acts once per new Bitcoin block `h`: it endorses
the core block with target `h + 1` if one already exists. In the code this is the first block with that target known to
the node, whether it is on the canonical branch or not. The code does not endorse catch-up slots, and does not endorse a
core block that appeared later at all (FL-09). In core-blocks.md endorsing such a block is allowed by the optional
`LATE_MINTING_POLICY`.

### 10.3. Minter: double sign

**HB-5.** A minter MUST NOT sign two records with the same core height and different hashes (EN-6). For this:

1. It keeps a journal `c → hash(C)` and writes the endorsement into it, on persistent storage, before sending the
   transaction. The journal survives restarts.
2. If the journal already has a different hash for `c`, the minter does not endorse this core block, except for a
   replacement by item 3. This happens after a branch switch: endorsements for the core blocks of the new branch at the
   same slots are lost, but there are at most 12 such live slots (EN-6).
3. The hash for `c` can be changed only by a transaction that spends the same input of the minter as all its earlier
   transactions for `c`. At most one such transaction gets into any Bitcoin branch, so there is no double sign. This is
   how a minter replaces an endorsement for `E_T` with an endorsement for `C_T` (1.8, rule 5). The same
   `(c, hash(C))` can be resent with any input: that is a repeat, not a double sign (EN-4).
4. The journal entry for slot `T` MAY be deleted once `N ≥ T + 11 + K` (FC-7). A new record for this slot would land
   in Bitcoin later than `T + 11` and would be inadmissible (EN-6).

⚠ **diverges from the code**: in the code the journal is kept in memory and lost on restart (FL-02), and there is no way
to change the hash for `c`.

### 10.4. Minter: transactions

**HB-6.** For each live slot the minter SHOULD keep a separate UTXO on the Minter P2WSH: endorsements for different
slots should not spend each other's outputs, change included (1.8). The fee MAY be paid with other inputs (IN-14), but
they should not chain endorsements together either.

**HB-7.** If sending an endorsement failed, the minter retries as long as the slot is live. A dead endorsement in the
mempool it replaces with RBF by an endorsement for a live core block it has not endorsed yet (1.8). After a Bitcoin
reorg it resends dropped endorsements for live slots, if they did not return to the chain by themselves (1.7, item 5).

⚠ **diverges from the code** (FL-10): in the code an endorsement is funded only from confirmed outputs of the Minter
P2WSH, and the change goes back there. So while the previous endorsement is not in a block, there is nothing to fund the
next one with. Meanwhile the core height is already reserved in the journal and is never sent again. The code has no RBF
replacement.

### 10.5. Rationale

**Why the minter endorses on the certificate, not on the Bitcoin block (FL-09).** With `T = h + 1`, the core block of
slot `h + 1` is created only after block `h` arrives (1.5). A minter that looks once at each new Bitcoin block almost
never finds it and misses the slot. In PR #49 the author called endorsing only an already known core block the simplest
strategy, since a late endorsement gets less anyway. But under the scheme of 1.5 such an endorsement is not late, it is
the normal one: it lands in the grace window. A slot missed by the honest minters lowers the attack threshold (1.8).

**Why the journal is kept on disk (FL-02).** The model found this scenario. A minter endorses the core block of a slot
and restarts. Fork choice switches to another branch, and the minter endorses that branch's core block of the same slot.
That is a double sign and a permanent ban (EN-8), which also hits the delegators (DL-6).

**Why the hash can be changed only through a common input (HB-5, item 3).** It is the only way to change one's mind
without risking a ban: Bitcoin itself guarantees that at most one of the transactions with a common input gets into a
branch. It is needed for switching from `E_T` to `C_T` when the certificate arrived after `EMPTY_WAIT` (1.8, rule 5).

**Why the journal can be pruned (HB-5, item 4).** An entry is needed only while a new record for the same slot could
still be admissible (EN-6). After `T + 11` any new record is inadmissible. The `K` margin covers a Bitcoin reorg after
which the new record would end up at a height not above `T + 11`.

**Why separate UTXOs (HB-6, FL-10).** If endorsements are funded only from confirmed outputs, at most one endorsement of
the minter gets into a Bitcoin block at a time, and it takes 12 blocks to catch up on 12 live slots. Spending unconfirmed
change, as in the FL-10 fix in the model, is possible, but then endorsements are chained, and replacing one with RBF
evicts all the following ones from the mempool (1.8).

## 11. Parameters

This section collects all numeric parameters in one place, with where they come from. The values themselves are set in
the rules referenced in the "Where" column. Parameters are grouped by who sets them and what happens if participants
disagree on them.

### 11.1. Deployment parameters

Set when the network is launched, and the same for all its nodes. Nodes with different values are different networks.
The consensus rules do not depend on the values (IN-1).

| Parameter | Meaning | Where |
|-----------|---------|-------|
| Bitcoin network | mainnet, testnet, signet or regtest | IN-1 |
| Start block | height `h₀` and hash of the Bitcoin block genesis is anchored to | IN-1, IN-3 |
| Genesis | the Flame block with `height = 0`: core height 0, target `h₀`, initial Flame state | CB-2, [blockchain.md](../blockchain.md) |
| `network_id` | network identifier in the BFT session descriptor | bft.md |

### 11.2. Protocol parameters

The same in all networks, and part of the computation of weights, rewards and the validator set. Nodes with different
values choose different canonical chains, so changing any of them is a hard fork.

| Parameter | Value | Where | [design-doc.md](design-doc.md) and code |
|-----------|-------|-------|-----------------------------------------|
| `MIN_DURATION` | 144 blocks | MP-3 | design-doc: the rationale relies on 2048; code: 1 |
| `MAX_DURATION` | 300 blocks, preliminary value | MP-3, MP-6 | in neither; the code has only the field limit, 65535 |
| Default Duration | `MIN_DURATION` | MP-3 | code: 100 |
| `Z`, maturity | 1 block | MP-4, MP-5 | design-doc: "config-defined", no value; code: 1 |
| `GRACE_DELAY` | 1 block | EN-5 | no grace window in either |
| `MAX_DELAY` | 10 blocks | EN-2, EN-5 | the same |
| Initial reward | 50 FLM = 5·10⁹ sparks | RW-1 | the same |
| Halving interval | 210,000 core blocks | RW-1 | the same |
| Reward batch | 100 core blocks | RW-6 | the same |
| Payout delay | one batch: the payout block `c` pays for `c − 199 … c − 100` | RW-6 | design-doc: the same; the code computes rewards at `c − 50` |
| Validator set threshold | `1/1024` of `BftTotal(e)` | BFT-7 | design-doc: the same; the code has no BFT |
| BFT quorum | more than `2/3` of the total weight of the validator set | BFT-9 | bft.md: the same |

Encoding constants (the `FLMS`, `FLMB`, `FLMV` prefixes and versions) are defined in
[minting-encodings.md](minting-encodings.md), Flame state machine parameters (block limits, storage fees) in
[blockchain.md](../blockchain.md).

### 11.3. Honest behavior parameters

They do not affect validity or the consensus computation. A participant with a different value breaks nothing; it only
does worse itself.

| Parameter | Value | Where |
|-----------|-------|-------|
| `EMPTY_WAIT` | 5 s | 1.8 (rule 5), BFT-4, BFT-5, HB-4 |

### 11.4. Finality parameters

Chosen by whoever relies on a block being final: an exchange, a bridge, a wallet. They do not affect fork choice
(FC-7). The defaults are in section 12.

| Parameter | Meaning | Where |
|-----------|---------|-------|
| `K` | depth beyond which a Bitcoin reorg is considered impossible; the minter also prunes its journal by it | FC-7, HB-5 |
| `α_max` | the largest adversary share of Minting Power, `α_max < 1/2` | FC-7 |

### 11.5. Derived values

| Value | Equals | From |
|-------|--------|------|
| Last height of a valid endorsement for slot `T` | `T + GRACE_DELAY + MAX_DELAY = T + 11` | EN-2 |
| Live slots at once | `GRACE_DELAY + MAX_DELAY + 1 = 12` | 1.8 |
| Smallest weight share of a valid endorsement | `1 / 2^MAX_DELAY = 1/1024` | EN-5 |
| Distinct Burn durations | `MAX_DURATION − MIN_DURATION + 1 = 157` | MP-6 |
| Common denominator `Λ = LCM(144, …, 300)` | 432 bits; the exact sum of MP-6 fits in 512 bits | MP-6 |
| Halvings until the reward is zero | 33 | RW-1 |
| Maximum emission | 20,999,949.9769 FLM: the sum of `Reward(c)` without the 50 FLM of genesis | RW-1 |
| First payout | in core block 200, for core blocks 1 … 100 | RW-6 |
| Height `R` for a batch | `T_end + 11 = h₀ + c − 89` | RW-7 |
| Margin of `R` below the Bitcoin tip at payout | at least 88 blocks | 1.7, RW-7 |
| Largest validator set | 1024 validators | BFT-7 |

### 11.6. Rationale

The values are justified next to the rules: Burn durations in 3.5, the grace window and `MAX_DELAY` in 1.6 and 4.4,
`EMPTY_WAIT` in 1.8, the validator set threshold in 8.6, batches and the payout delay in 7.4.

**Why `Z = 1`.** It is the smallest value MP-5 allows. A new Burn works from the very next block, including when the
honest minters need to respond to an attack quickly. A larger `Z` would protect the power of the current slot from
Bitcoin reorgs shallower than `Z`. But after a reorg of any depth everything is recomputed deterministically anyway
(1.7), so a larger `Z` would only mean fewer recomputations at the price of slower new power.

## 12. Guarantees and assumptions

This section collects the assumptions under which the rules work and the properties that follow from the rules.
Properties are consequences, not new rules: each one says what it rests on. [design-doc.md](design-doc.md) §9 "Risks"
is empty; the risks are collected here, in 12.5.

### 12.1. Assumptions

The properties of 12.2 and 12.3 hold as long as the assumptions below hold. What happens when one of them is violated is
in 12.5 and 12.6.

**PROP-1. Bitcoin.**

1. The canonical Bitcoin chain of all honest nodes eventually agrees, and there are no reorgs deeper than `K`. A reorg
   that replaces the start block never happens (IN-3).
2. Miners do not censor Minting Transactions: a transaction with a market fee gets into one of the next blocks.
   Systematic censorship does not pay for miners (7.4), but against a majority of hashrate Flame is no better protected
   than Bitcoin itself (1.7).

**PROP-2. Minting Power.** The adversary has less than half of all Minting Power in every slot: `α < 1/2`. Whoever
relies on finality assumes `α ≤ α_max` (FC-7). Power can be bought only by a Burn for at least `MIN_DURATION` blocks
(MP-3), so even a short-lived majority costs more than the honest minters burn in a day (3.5).

**PROP-3. Network.** Honest participants receive Bitcoin blocks, Flame blocks and BFT certificates within seconds. A
block that honest minters vote for eventually becomes known to every honest node: without the block a record gives no
weight (EN-1). An endorsement reaches the miners' templates in about 30 s (1.8).

**PROP-4. BFT.** More than 2/3 of the validator set weight of every session belongs to honest validators that are
online. This is needed only for transaction liveness (PROP-13); BFT does not affect safety (BFT-2).

**PROP-5. Cryptography.** Security of SHA-256 (Minter P2WSH, Flame block hashes), Ed25519 (BFT signatures), and the
Bitcoin signatures with which a minter spends the output to its Minter P2WSH.

### 12.2. Safety

**PROP-6. Determinism.** Nodes with the same inputs get the same Minting Power, bans, weights, canonical chain and
rewards (0.4). The result does not depend on the order in which the node learned about blocks and records (EN-1, EN-9),
and after a Bitcoin reorg of any depth it equals the result of computing from scratch (1.7). This rests on the ban being
counted from a Bitcoin height rather than from the moment of detection (EN-7), on double sign being counted by records
(EN-6), and on weights and rewards being computed exactly, without `f64` (FC-2, MP-6, RW-4).

**PROP-7. One core block per slot.** In every valid chain the target of core blocks grows by exactly 1, and
`c = T − h₀` (CB-3, CB-4). So the same power counts in a chain at most once per slot.

**PROP-8. An honest core block is not displaced.** Let `α < 1/2`, and in every slot after core block `C` let all honest
minters endorse the same honest chain (HB-3), with their endorsements getting into Bitcoin without penalty. Then any
branch diverging from the honest chain no later than `C` is lighter than it. In every slot the adversary's branch has
at most `αP` of EMP, the honest one at least `(1 − α)P`, and the slot weight is strictly increasing in EMP. So in every
slot the honest weight is strictly greater, and there is no tie for a ground hash to decide. With the sum of `log2`
the weight is strictly increasing only from `TotalEMP ≥ 2` on (FC-2), so there `(1 − α)P ≥ 2` is also needed (GAP-14).

That all honest minters endorse one chain is an assumption, not a consequence of the rules. The rules only give that a
divergence is temporary. Honest minters can diverge in the tail after `C_w`, where certificates and `EMPTY_WAIT` decide
(BFT-5): some received the certificate for `C_T` before `EMPTY_WAIT`, some after. Usually an RBF replacement brings them
to one block (1.8, rule 5). If it did not make it in time, then once the endorsements of the slot are in Bitcoin and the
certificates have reached everyone (PROP-1, PROP-3), the honest minters again have one `C_w` and one root. How often
this happens depends on the network and on the time between Bitcoin blocks, not on the rules; the estimate in 1.8
("Vote split") is an illustration, not a guarantee.

**PROP-9. An honest minter is not banned.** A minter following HB-5 does not double sign whatever the network, BFT and
the adversary do. A double sign consists only of the minter's own records (EN-6), and nothing can be recorded in its
name without its spending key (IN-14, PROP-5). The price of the rule is that after a branch switch the minter misses up
to 12 slots (EN-6).

**PROP-10. The past cannot be rewritten after the fact.** The weight of slot `T` freezes at height `T + 11`: there are
no new valid endorsements for it (EN-2), and the bans that remove votes for the slot are already visible by then (EN-6,
EN-8). So a block that is final by FC-7 can be rewritten only by a Bitcoin reorg deeper than `K` or by a coordinated
majority, and even then only "forward" (PROP-15).

**PROP-11. Rewards.**

1. The payout of a batch is a function of the ancestors of the payout block and of the Bitcoin view up to height `R`.
   A Bitcoin reorg shallower than 88 blocks does not change it (RW-7).
2. Every core block of a chain is paid exactly once, by the payout block of its own chain. Core blocks of losing
   branches are not paid (RW-6).
3. At most 20,999,949.9769 FLM is issued in total. The rewards of core blocks with `TotalEMP = 0` and rounding
   remainders are burned (RW-1, RW-4, RW-5).

### 12.3. Liveness

**PROP-12. Slots go on without BFT.** As long as the honest minters are online, every slot has a core block with honest
EMP: `C_T` from BFT or `E_T` (1.8, rule 5; BFT-5). A failure or capture of BFT does not stop the chain. The attack
threshold stays at 50% of Minting Power (1.8, "Summary") if all of the following hold:

1. honest endorsements get into Bitcoin on time (PROP-1, PROP-3);
2. the honest minters endorse one chain, that is they chose the same parent for `E_T` (PROP-8);
3. with the sum of `log2`, the honest EMP of the slot is at least 2 (FC-2, GAP-14).

**PROP-13. Transactions.** Under PROP-4, transactions get into BFT blocks. If more than a third of the BFT weight is
unavailable or dishonest, slots are closed by empty `E_T`, and transactions wait until the honest side regains more than
2/3 of the weight (BFT-10). Safety does not suffer.

**PROP-14. An honest minter does not miss live slots.** A minter following HB-3 … HB-7 endorses every live slot of the
honest chain, except those where it already signed another hash (EN-6). Sending an endorsement does not wait for earlier
ones to confirm (HB-6).

### 12.4. Finality: defaults

`K` and `α_max` are chosen by whoever relies on a block (FC-7, 11.4). The defaults are `K = 6`, as is customary for
Bitcoin, and `α_max = 1/3`, as in the BFT assumption of a third of dishonest weight. With them, a block without
competing records is final 26 Bitcoin blocks after its slot with a linear sum of weights, and 34 with the sum of
`log2`. If the adversary keeps endorsing an alternative, it takes 35 and 708 blocks respectively (6.4).

For large amounts `K` and `α_max` can be taken larger. A block with slot `T_B` is final at `N = T_B + n − 1 + 11 + K`,
where `n` is the number of final slots. With a linear sum and no competing records,
`n > (12 + K) · α_max / (1 − α_max)` is needed, and if the adversary endorses an alternative,
`n > (12 + K) · α_max / (1 − 2α_max)`. As `α_max` approaches 1/2, the wait grows without bound.

### 12.5. Attacks and their cost

| Attack | What the adversary needs, cost | Where |
|--------|--------------------------------|-------|
| A short fork by a minority | Impossible while `α < 1/2` and the honest minters endorse on time | PROP-8 |
| A temporary majority for a few slots | A Burn for at least 144 blocks: more than the honest minters burn in a day | 3.5 |
| Several core blocks in one slot (FL-01) | Invalid | CB-4 |
| Grinding a hash among equal weights (FL-05) | Does not help: weight is exact, a BFT certificate breaks ties | FC-2, FC-5 |
| Its own branch outside BFT while BFT lags | Does not help: the honest minters endorse `E_T` on time, the threshold stays at 50% | 1.8 |
| Splitting honest votes between `E_T` and `C_T` | A potential scenario, not verified: an adversary leader times the certificate to `EMPTY_WAIT`, and a lost RBF race is also needed. Having received the certificate, the honest minters converge already in the next slot; part of the weight of slot `T` is lost. There is no working trace yet in which this displaces a final block | 1.8, PROP-8 |
| A hidden branch revealed late (FL-08) | A majority of power for as long as the branch is hidden; its records are visible in Bitcoin | 4.4 |
| Junk records for unknown blocks (FL-07) | ≈ 6.8 BTC per 1 GB of records at 1 sat/vB; storage ≈ $0.1 per month | 4.4 |
| Withdrawing a vote after the fact | Impossible: a record after `T + 11` is invalid | EN-2, 4.4 |
| Getting an honest minter banned | Needs its spending key | PROP-9 |
| Someone else's Validator Pubkey in one's own identity | Only gives the key's owner a vote in BFT; a delegator who chooses by key can be fooled | 8.6, 9.4 |
| Stopping transactions through BFT | More than a third of the BFT weight; slots and weights are not affected. The honest side responds by burning more | BFT-10, PROP-13 |
| Miners censoring endorsements | Miners lose fees. Late honest endorsements lower the attack threshold just like a BFT lag without rule 5 | 7.4, 1.8 |
| A Bitcoin reorg deeper than `K` | Breaks FC-7 finality. Reward payouts change only with a reorg deeper than 88 blocks | 1.7, RW-7 |
| Holes in slots with the sum of `log2` (FL-12) | A slot without honest weight outweighs many slots with an honest lead | **Not defined**<sup>[GAP-14](#gap-14)</sup> |

### 12.6. What a coordinated majority can do

Collected here are consequences of the rules, not a separate mechanism: what minters with a majority of Minting Power
can do if they coordinate, for example after an exchange key is stolen. Notation: `A` is the canonical branch, `B` the
branch the majority chooses, `d` how many slots passed from the divergence point to the moment the majority switched to
`B`.

**PROP-15. Coordinated reorg.** A majority can rewrite the canonical chain from any block, but only "forward". Slots
whose window has closed are frozen (EN-2): neither `A` nor `B` gets any more votes for them. So `B` catches up with `A`
only on future slots, where the majority endorses `B` instead of `A`. The old votes for `A` and the new ones for `B` go
to different core heights, so there is no double sign. If a minority with share `β` keeps endorsing `A`, then with the
same network power `P` and full participation, `B` overtakes `A` after

```text
with log2:          d · log2(P) / log2((1 − β) / β)      slots; with β = 0, d slots
with a linear sum:  d / (1 − 2β)                         slots
```

Example: the theft was noticed after 6 hours (`d = 36`), the thief has 10% of the power, `P = 2^40`. With a linear sum
the reorg takes about 45 slots (7.5 hours), with `log2` about 454 slots (3 days). This is an argument for GAP-14.

- *Cost.* The slots of `B` from the divergence point to the switch are dead, and their rewards are burned (RW-5): the
  rewards of `d` slots. Transactions of `A` after the divergence point return to the mempool, as with any reorg.
- *Departure from the rules.* By BFT-5 and HB-3, BFT builds on the honest chain and minters endorse it; up to the last
  core block with weight it coincides with the canonical chain. Until `B` becomes canonical, the majority has to build
  and endorse `B` against these rules, that is run different software for a while.
- *Finality.* FC-7 assumes that the adversary has less than half of the power. Here the majority itself rewrites, so
  blocks of `A` that are final by FC-7 are rewritten too. This is the same as 51% in Bitcoin.
- In practice only a shallow reorg makes sense. For a deep one a hard fork is simpler.

**PROP-16. Halting the network.** Stopping BFT is not enough: blocks outside BFT are valid (CB-8), and if the honest
minters stop endorsing, the branch of any minority becomes canonical, because the others have no weight. The network can
be halted only by the empty block mode (1.8, rule 5): the majority keeps endorsing on time, but only empty core blocks.
A branch with transactions weighs less, and transactions stand still until the majority returns to normal operation.

**PROP-17. What a majority cannot do.** Revert a single transaction without rewriting the whole branch after it. Change
the state, for example return stolen funds. Vote for a slot whose window has closed. All of this requires a hard fork,
that is a change of rules outside the protocol.

## 13. Gaps and open questions

Items are added together with the sections that reference them. Numbers follow the order of first appearance in the
text.

---

<a id="gap-14"></a>**GAP-14. Core block weight formula and `log2` precision (FL-05, FL-12)**

- **Where:** 6.1 (FC-2), 6.4; calculations in 1.7 and 1.8; 12.2 (PROP-8), 12.3 (PROP-12), 12.5, 12.6.
- **Essence:** three questions.
  1. The author decided to compute `log2` exactly, without `⌊ ⌋` (FL-05). It is not defined how: the exact `log2` of an
     integer is irrational, and nodes must get the same result.
  2. Whether to keep `log2` at all. The sum of `log2` makes a hole very expensive: in a slot where the honest minters
     have no weight, the adversary gets `log2(αP)`, and one hole outweighs many slots of honest lead (FL-12). During a
     BFT lag the attack threshold with `log2` is lower than with a linear sum (table in 1.8). By CB-4 the honest chain
     does not skip slots, but dead slots with zero weight remain. If an adversary with `α_max = 1/3` keeps endorsing an
     alternative, finality with `log2` comes after 708 blocks, almost 5 days, and with a linear sum after 35 (6.4).
  3. Any weight formula must give `w(0) = 0` and be strictly increasing in `TotalEMP`. Otherwise less power can tie with
     more, and a ground hash decides the tie. The sum of `log2` violates this: `log2(1) = 0 = w(0)`, so a block with one
     unit of EMP weighs as much as a block without endorsements. This is why PROP-8 and PROP-12 require, with `log2`, an
     honest EMP of at least 2 in every slot.
- **Previous doc:** [design-doc.md](design-doc.md) §5: `BlockWeight = ⌊log2 TotalEMP⌋`, without rationale.
- **Code:** `⌊log2⌋` via `checked_ilog2().unwrap_or(0)`
  (`flame-minting/src/consensus/storage/weighted_header_storage.rs:50-57`,
  `flame-minting/src/consensus/engine/weight/weighter.rs:177-179`).
- **PR #49:** the author proposes weights in decimals, without floats (FL-05).
- **Model:** FL-05 (test `fl05TieGrindTest`), FL-12 (test `fl12SkippedSlotTest`). With the FL-05 fix the model compares
  chains exactly, via the product of powers (`exactWeight`).
- **Options:**
  1. `log2` in decimals with fixed precision. The number of digits, the rounding, and a `log2` algorithm that is the same
     in all implementations have to be defined. If weights are equal after rounding, the hash decides again, though
     less often.
  2. Exact comparison without rounding. Comparing sums of `log2` is the same as comparing the products of
     `max(TotalEMP, 1)` over the core blocks after the common ancestor of the two tips; this is what the model does.
     These are integers of about 51 bits per core block of the stretch: a divergence of 1000 slots means 51 thousand
     bits.
  3. A linear sum: `ChainWeight = Σ TotalEMP`. Integers without rounding, a hole costs the adversary at most `αP`, the
     threshold during a BFT lag is higher (1.8), the requirement of item 3 holds.
  4. `log2(1 + TotalEMP)`: `w(0) = 0`, the weight is strictly increasing, the requirement of item 3 holds. The other
     arguments against `log2` (item 2) apply to it too; its economics need a separate analysis.
- **Status:** author's decision: `log2` without rounding; the method of computation and the choice between `log2` and a
  linear sum: no decision.

<a id="gap-18"></a>**GAP-18. How the payout is written into the Flame state**

- **Where:** 7.3 (RW-7); 9.2 (DL-5); 10.1 (HB-2).
- **Essence:** it is not defined in what form the payout gets into the Flame state: explicit transactions or outputs in
  the payout block, which a node checks against its computation, or an implicit credit applied when the state machine
  applies the block.
- **Previous doc:** [design-doc.md](design-doc.md) §8: rewards are "distributed once every 100 Core Blocks", and the
  Rationale says it "reduces the amount of coinbase transactions": so the payout is apparently coinbase transactions,
  but their format is not described. In [consensus.md](../consensus.md) FLM issuance is TBD.
- **Code:** the rewards of a batch are computed in core block `c − 50` from the current Bitcoin tip and stored in the
  rewards storage under height `c` (`flame-minting/src/rewards/engine/rewards_engine.rs:30-40`). They are not written
  into the Flame state: nobody reads the deferred rewards.
- **Options:**
  1. Coinbase transactions in the payout block, as design-doc.md implies: one per predicate pair. A node checks them
     against its computation by RW-2 … RW-8.
  2. A credit by the state machine when it applies the payout block, next to the storage issuance
     ([blockchain.md](../blockchain.md), step 2 of block application). Nothing extra is in the block, and the result is
     part of the state commitment.
- **Status:** no decision; the format needs to be agreed with [blockchain.md](../blockchain.md).

<a id="gap-20"></a>**GAP-20. Fees in Flame blocks (BFT fees)**

- **Where:** 7.3 (RW-9), 8.5 (BFT-11).
- **Essence:** it is not defined who gets the fees for transactions in Flame blocks, and when.
- **Previous doc:** [design-doc.md](design-doc.md) §8: "BFT Fees: TBD"; Open Questions: "Should BFT fees be distributed
  between only Validators or Minters as well?". [blockchain.md](../blockchain.md): storage fees are burned so that the
  block author cannot get them back.
- **Code:** there is no fee distribution.
- **Options:**
  1. To BFT validators: the block author, or everyone who signed the certificate.
  2. To minters, together with the core block reward, by EMP.
  3. Burn them, like storage fees.
- **Status:** no decision.
