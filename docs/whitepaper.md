# Flame: market-driven expansion of Bitcoin


**Abstract.** A purely peer-to-peer version of electronic cash would allow online
payments to be sent directly from one party to another without going through a
financial institution. Bitcoin provides part of the solution as a proven
peer-to-peer store of value, but the main benefits are lost if trusted third parties
are still required to execute financial contracts, fast payments or provide confidentiality to transfers.
We propose a solution based on Bitcoin peer-to-peer network as a consensus layer.
The expanded network continuously burns bitcoins as a means to timestamp blocks of transactions in the expansion network.
The network offers confidential transactions, powerful infrastructure for smart contracts, higher throughput and low latency.
As an incentive, network issues a new electronic currency _flame_.

## Introduction

Existing blockchains trade decentralisation for functionality, or vice versa.
Bitcoin maximises the former, while other popular platforms strive to expand the latter.

Flame is designed to resolve this trade-off. It combines Bitcoin-level decentralisation
and security with the functionality and scalability required by blockchains supporting
use cases such as digital finance. This is achieved through fast, conservative cryptography
for confidential transactions; scalable smart-contract architecture; high throughput
(approximately twenty times Bitcoin’s transaction volume); low latency consensus protocol.

Flame is an extension of Bitcoin and stands on its security model.
Participants irrevocably burn bitcoins to secure the chain, collecting newly issued currency *flames* as an incentive. 
Similarly to the proof-of-work in Bitcoin, the cost of running the consensus is the basis of selecting the best chain. Flame continues Bitcoin’s strict ethos through scaling and enhancing its network without changes to the protocol. Flame does not provide special privileges to miners, investors, or developers.

Flame transactions are verified using the set of network rules called FlameVM: the Flame Virtual Machine.
Each Flame transaction is a string of executable code that is interpreted by FlameVM to determine its validity.
FlameVM supports confidential transactions with zero-knowledge proofs, custom cryptographic protocols, custom covenants and multi-user smart contract applications.


## Proof-of-burn consensus

In an open network proof-of-work is the only known solution to a variation of Byzantine Generals’ Problem: how the network of arbitrary participants agrees on a single history of transactions Proof-of-work consensus solves the problem by assuming the majority of nodes are honest and they channel their resources into a single history, making it impossible for dishonest nodes to outrun them and double-spend the coins.

Flame follows the principle of proof-of-work standing on top of Bitcoin. Burning bitcoins is a “first derivative“ of mining: while *miners* burn electricity to allocate *bitcoins*, Flame *minters* burn bitcoins to allocate *flames* issued at a fixed rate as an incentive for running the network. Just like in proof-of-work, the input resource is committed irrevocably: bitcoins are permanently removed from circulation in a provable and irreversible way via sending to an unspendable address. There is no two-way bridge, and no trusted parties or complicated cross-chain protocols that allow withdrawing bitcoins back.

Each Bitcoin block, Flame minters participate in an open auction: they publicly vote for the agreed-on block of Flame transactions by sending bitcoins to an unspendable address that points to that block. The block with the most bitcoins burnt is considered part of the main chain and all minters split the allocated incentive in proportion to their sacrificed bitcoins.

In order to agree on which block to vote on, minters run the BFT consensus protocol among themselves, as defined by a sliding window of large number of Bitcoin blocks. This pre-consensus allows to confirm transactions in seconds since BFT consensus is run by minters continuously with as little latency as the network allows, to have the most recent “block candidate” for voting on Bitcoin blockchain once a new Bitcoin block gets published. Minters are not obliged to vote for the block defined by the BFT consensus, but since deviating from the majority choice is punished by lost bitcoins, there is an incentive to follow the protocol. As a result, recipients may choose to use the BFT agreement as a basis for fast finalisation of low-value transfers.

Flames are incentives for keeping network secure. Flames are issued at a similar schedule as bitcoins: 50 flames per block, halving every 4 years, with maximum supply below 21 million units. Each flame is divisible by 100 million atomic units called *sparks*. Flames are distributed at an open auction to all participants in proportion to destroyed bitcoins. The total cap of both coins (bitcoins + flames) remains below 21+21=42M units. Permanent destruction of bitcoins and a continuous stream of mining fees from Flame consensus directly and indirectly increase the value of Bitcoin mining rewards and therefore improve the security budget of Bitcoin.

## Network operation

Flame is a peer-to-peer network of nodes that implement the rules of the Flame protocol. 
Nodes may vary in the degree of their participation: some may observe the blocks of transactions in order to validate payments, other participate in the process of minting. For the purposes of this section, we will use term *minters* to describe subset of nodes that burn bitcoins, timestamp transactions and collect *flames* as a reward.

Minters operate on both networks simultaneously — Flame and Bitcoin. The steps to run the network are as follows:

1. New transactions are broadcast to all minters.
2. Minters publish their *stakes* on the Bitcoin network. Stake is a transaction that declares minter’s cryptographic identity and destroys some amount of bitcoins. Each stake has a limited duration of two weeks.
3. At each Bitcoin block at height H-1, the minters compute the membership set based on the currently active stakes to perform the BFT protocol.
4. During the BFT protocol, minters agree on a chain of ordinary Flame blocks, forming a *candidate chain*.
5. When Bitcoin block appears at height H, minters finalize the Flame candidate chain with a *core block* and broadcast Bitcoin transactions where they vote for that core block. Each vote is securely linked to the active stake of each minter.
6. Nodes accept the Flame block only if all transactions in it are valid and not already spent. 
7. If multiple valid *core blocks* are present at the given height, nodes choose the one with the larger weight calculated based on amount of burned coins.
8. The set of minters continuously changes each Bitcoin block. New set of minters express their acceptance of the block by creating the next block in the chain, using the hash of the accepted block as the previous hash.

