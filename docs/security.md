# Security findings

This document retains findings relevant to current FlameVM and the proposed
chain. Historical reports remain in Git. Statuses were checked against current
code and tests during consolidation.

## Open VM findings

### Expensive work is underpriced — Medium

Gas is currently flat per fetched instruction. Point decompression, hashing,
proof work, signature batching, and MSM growth/finalization are not priced in
proportion to validator cost. Benchmark an opcode schedule and charge or cap MSM
terms before production use.

### Allocation accounting needs a complete audit — Medium

String growth is metered, but every path growing dictionaries, stacks, decoded
payloads, proof state, constraints, or verification batches needs an explicit
bound. Current memory and decoder tests cover important paths, not the entire
allocation surface.

### Actor stale snapshots remain expressible — Medium

Checkout-as-lock prevents re-entrant state observation. It cannot stop an actor
from holding loaded application data across a call and later saving assumptions
invalidated by the interaction. Actor code must follow checks-effects-interactions.

### Chain formats need independent vectors — Medium

The VM has golden transaction-log and hash tests. Block, state-root, accumulator,
proof, and certificate formats still need hard-coded cross-implementation vectors.

### Extension policy is undefined — Low

Unknown tags and opcodes are rejected. Chain versioning must define how future
extensions activate without divergent parsing or meaning.

## Mitigated VM findings

- Actor state checkout blocks write and read-only re-entrancy during updates.
- `MAX_CALL_DEPTH = 64` bounds actor and predicate frames.
- Transient string growth is metered with monotonic high-water accounting.
- Value nesting is capped at 64 and oversized containers are rejected.
- Integer and dictionary encodings are canonical; reserved and unknown tags fail.
- Full-width sub-varint addition is checked for overflow.
- Retained Flame integer encodings are little-endian.
- Linear values cannot be copied or silently dropped on type errors.
- Failed calls roll back logs, registry changes, constraints, signatures, fees,
  and MSM batch entries to checkpoints.
- `send` and `call` accept only nonnegative clear byte tokens with flavor `1`.

## Open chain and consensus findings

Because those systems are not implemented, the following remain open:
equivocation, long-range attacks, proposer withholding and censorship, MEV,
bribery, vote stalling, partitions, eclipse and Sybil attacks, Bitcoin reorgs,
key rotation, resource-market manipulation, governance capture, unsafe upgrades,
and proof-update denial of service. They require renewed analysis after the
choices in [consensus.md](consensus.md) and [blockchain.md](blockchain.md) are made.

## Production gates

1. Resolve all consensus, block-format, and accumulator `TBD`s.
2. Publish independent encoding and state-transition vectors.
3. Enforce benchmarked worst-case VM and block resource limits.
4. Fuzz all network decoders and state transitions under bounded resources.
5. Commission fresh cryptographic, VM, chain, and consensus audits.
