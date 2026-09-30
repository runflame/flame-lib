# Benchmarks

Flame's benchmarks live in the `flamebench` crate. Each report measures one
part of the system, on one machine, and keeps its numbers in a results file
next to it, so its charts and tables can be redrawn without re-running
anything.

| Report | What it measures | Status |
|---|---|---|
| 1. [What a Flame transaction costs](benchmarks/transactions.md) | Verifying and sending one confidential payment in FlameVM and the wallet library, with its size and gas, on one CPU core. | Published |
| 2. The node | How `flamed` behaves under load: building blocks, keeping proofs current, restarting, serving wallets, and verifying on several cores. | Planned |
