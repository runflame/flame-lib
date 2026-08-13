# Flame — the future of Bitcoin

Flame is a blockchain network that turns Bitcoin into digital cash: confidential, programmable, and truly decentralized.

**Confidential:** Flame transactions use Bulletproofs, a lightweight zero-knowledge proof system, to provide confidentiality for balances, amounts, and other data. Custom conditions can be expressed over encrypted data, protecting privacy even within arbitrary smart contracts.

**Programmable:** Flame enables confidential smart contracts and decentralized apps. FlameVM provides built-in support for first-class issued tokens, powerful data types, and high-level cryptographic utilities for custom zero-knowledge proofs.

**Decentralized:** Flame consensus runs on top of Bitcoin via a "Proof-of-Burn" protocol derived from Proof-of-Work, in which destroyed bitcoins are used to protect the integrity of the network. The network issues a new coin, *flames*, on a schedule similar to Bitcoin’s, with a hard cap of 21 million coins. There is no preallocated fund or built-in governance structure; every node participates on equal footing from day one.

**Future of Bitcoin:** Flame creates an additional incentive for running Bitcoin. Converting bitcoins into flames increases the value of the diminishing supply of mined coins, creating a long-term solution for Bitcoin’s security budget. In turn, the improved security of Proof-of-Work provides a stronger basis for Flame consensus itself. With widespread adoption, bitcoins become a more secure store of value, while flames act as a powerful decentralized currency for everyday use.

### Documentation

- [FlameVM Specification](docs/flamevm.md)
- [Actor Storage Specification](docs/storage.md)
- [Blockchain State Machine](docs/blockchain.md)
- [Consensus](docs/consensus.md)
- [Implementation Plan](docs/plan.md)
