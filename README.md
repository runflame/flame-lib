# Flame: decentralized electronic cash with confidentiality and smart contracts

Flame is an electronic cash network based on Bitcoin. Flame introduces decentralized proof-of-burn consensus with a unified architecture that combines confidentiality and powerful programming environment. The network continuously burns bitcoins as a means to timestamp blocks of transactions. As an incentive, network issues new units of flames.

**Confidential:** Flame transactions use Bulletproofs, a compact zero-knowledge proof system, to keep amounts and asset types confidential by default. Custom conditions can be verified without revealing the underlying data, protecting privacy within smart contracts. The transaction graph remains public.

**Programmable:** Flame enables confidential smart contracts and decentralized apps. FlameVM combines Bitcoin-style contracts with persistent, message-driven actors. It provides first-class tokens, rich data types, and built-in cryptographic tools for custom zero-knowledge proofs.

**Decentralized:** Flame consensus runs on top of Bitcoin via a Proof-of-Burn protocol derived from Proof-of-Work. Minters continuously burn bitcoins to endorse Flame blocks and receive a proportional share of newly issued *flames*. Flames are issued on a fixed schedule, with a hard cap of 21 million coins. There is no preallocated fund or built-in governance structure; anyone can validate the network or participate in minting under the same rules.

**Future of Bitcoin:** Flame creates an additional incentive for securing Bitcoin. Burning bitcoins to mint flames reduces Bitcoin’s available supply, while minting transactions create demand for Bitcoin block space and pay fees to miners. With sustained demand, greater scarcity and additional fee revenue can help support Bitcoin’s long-term security budget. In turn, stronger Bitcoin security provides a firmer foundation for Flame consensus. With widespread adoption, bitcoins serve as a store of value, while flames act as a powerful decentralized currency for everyday use.

### Status

The project is a work-in-progress, many parts are changing and breaking all the time.

### Documentation

- [FlameVM Specification](docs/flamevm.md)
- [Key Derivation and Bech32f](flamekd/flamekd.md)
- [Payment Keys and Transfers](flamepayments/flamepayments.md)
- [Confidential Payments](docs/payments.md)
- [The Node and its JSON-RPC](flamed/flamed.md)
- [Benchmarks](docs/benchmarks.md)
- [Actor Storage Specification](docs/storage.md)
- [Blockchain State Machine](docs/blockchain.md)
- [Consensus](docs/consensus.md)
