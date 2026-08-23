# Flame implementation plan

This list is ordered by dependency and risk. Resolve conservation and consensus
semantics before optimizing or extending the system. A task is complete only
when the specification, implementation, and focused regression tests agree.

1. [x] Make synchronous call failure preserve ownership.

   - [x] Define one rule for values moved into `call`, `open`, and `signcall`:
     a failed operation must restore each entry-owned value to the caller, or
     propagate failure to a transaction boundary that restores it. A successful
     enclosing transaction must never silently discard a `Token`, nonzero
     `ClearToken`, `WideToken`, `Cell`, or token-bearing `Dict`.
   - [x] Fix pre-entry `call` failure. Target lookup, call-depth checks, and
     child-activation failure now return the original arguments followed by
     `k 0`.
   - [x] Fix entered-call failure. [`VM::fail_current_call`](../flamevm/src/vm.rs)
     returns the entry escrow after rollback: actor arguments, or the original
     locked Cell plus explicit `open`/`signcall` arguments. Cell payload remains
     sealed inside the Cell and is never returned separately. The failure count
     is the explicit argument count `k`; it excludes the contextual Cell.
   - [x] Apply the same rule to runtime errors, out-of-gas,
     `StackNotClean`, `BadReturnArity`, and explicit verification failure.
   - [x] Put every rollback checkpoint before its boundary side effects. In
     particular, a failed `signcall` must not retain the signature recorded
     before its snapshot, and storage-pool and lease changes must roll back with
     their transaction.
   - [x] Cover pre-entry rejection, entered failure, dirty EOF, bad return
     arity, nested rollback, and Token restoration. Downward call arguments are
     portable-only; non-portable liabilities may return upward but cannot be
     delegated to another callee.
   - [x] Cover `Token`, positive `ClearToken`, nested token-bearing `Dict`, and
     contextual `Cell` restitution across entered failure, out-of-gas, dirty
     EOF, bad return arity, nested unwind, `open`, and `signcall`. Retain
     explicit upward returns for negative `ClearToken` and `WideToken`, prove a
     successful call discards its hidden escrow copy, and verify storage pool,
     lease, expiry-index, capacity, and actor-root conservation across nested
     commit followed by outer rollback.

2. [x] Make asynchronous message failure conserve payload assets.

   - [x] Implement the documented bounce: on failed delivery, seal the original
     payload into exactly one cell under `refund_predicate`.
   - [x] Ensure a failed constructor delivery rolls back the provisional actor,
     storage purchases, burns, message consumption, and bounce creation in one
     atomic boundary.
   - [x] Cover missing, pending-destruction, and checked-out actors; malformed
     or failing code; dirty return stacks; anchor-ratcheted message uniqueness;
     and failure while producing the bounce.
   - [x] Prove with tests that a message carrying each portable bearer type can
     be delivered or recovered, but never lost or recovered twice.

3. [x] Close internal signature authorization gaps.

   - [x] Make `signtx` external-only, as internal transactions have no TxID-bound
     signature finalization.
   - [x] Verify `signcall`'s explicit signature
     during internal execution before entering the signed script.
   - [x] Reject arbitrary 64-byte signatures in internal execution and test both
     invalid and valid signatures through the public transaction path.
   - [x] Ensure a failed external `signcall` removes its deferred signature together with
     the failed frame.

4. [x] Enforce portability at domain transitions using sticky Dict metadata.

   - [x] Keep generic value encoding representation-only. A negative
     `ClearToken`, and a Dict containing one, can be encoded and decoded for
     diagnostics; the codec does not decide whether that value may enter a
     persistent or asynchronous domain.
   - [x] Cache a sticky `portable` flag in each Dict, initialized to `true` and
     cleared on successful insertion of a non-portable value.
   - [x] Reconstruct the flag from decoded members in both Dict wire forms
     without an additional recursive portability scan. Historical taint is not
     serialized: debug round-trips preserve the represented members, not a
     sticky flag left by a member that was later removed.
   - [x] Make Cell payloads immutable and reject non-portable values in the
     public constructor using the O(1) Dict flag.
   - [x] Route `Cell::decode` through the same Cell-domain admission rule. A
     top-level payload scan is sufficient because each nested Dict lookup is
     O(1); this is a Cell check, not an encoding check.
   - [x] Make Message payloads immutable and admit them only through a checked
     constructor.
   - [x] Apply the O(1) Dict check at synchronous actor calls and at VM and
     registry actor-state save/deploy boundaries.
   - [x] Test Message/send, call, Cell decode, and actor-state admission with
     nested non-portable Dicts.
   - [x] Restrict public `Token` construction so every `Token` is portable by
     construction. Portability must not depend on whether a commitment happens
     to carry a prover-only opening.
   - [x] Audit public construction and decoding paths. Dict entries and
     Cell/Message payloads are private; Cell/Message constructors and Cell
     decoding admit payloads; raw Token construction is crate-private and its
     public cleartext constructor enforces the quantity range. ClearToken,
     Address, and TxLog remain policy-neutral in-memory carriers; the receiving
     Cell, Message, call, or actor-state boundary performs admission.

5. [x] Reconcile canonical wire and opcode contracts.

   - [x] Specify the implemented contiguous sub-varint U64 base
     `4_295_033_088` and pin every sub-varint and container-prefix boundary.
   - [x] Specify and test `decrypt` stack order `T f f' q q'`.
   - [x] Defer invalid `decrypt` openings to final batch verification externally,
     check them immediately internally, and test nested-call argument restitution.
   - [x] Correct the `call`/`open`/`signcall` diagrams to include success
     `results... k 1`, clean fall-through `0 1`, and failure restitution
     `entry-values... n 0`.
   - [x] Replace the global claim that soft failure preserves every operand with
     exact per-opcode stack shapes. Copyable count/key operands consumed by
     `readstr`, `getopt`, and related optional opcodes are not restored.
   - [x] Align documented error variants for bit-count bounds with the actual
     `IndexOutOfRange`/`BitCountOutOfRange` behavior.
   - [x] Add cross-implementation vectors for all width boundaries, failure
     shapes, and supported value tags.

6. [x] Make memory and gas accounting non-bypassable.

   - [x] Remove the separate memory limit, `memlimit` opcode, root limit,
     call operands, storage-derived multiplier, and block/mempool memory fields.
   - [x] Charge `writebits`, `writeint`, Dict growth, stack growth, decoded
     payloads, constraint state, deferred signatures, and MSM/batch terms.
   - [x] Audit hostile variable-size paths and charge logical bytes/items before
     allocation; fixed-size pushes remain bounded by the instruction charge and
     nested depth remains hard-capped.
   - [x] Specify that EOF consumes gas: an `N`-instruction clean program needs
     at least `N + 1` gas.
   - [x] Keep an immutable frame-creation gas cap for `gaslimit`; refunds must not
     make the reported cap grow.
   - [x] Debit the full gas attached to `send` from the active frame. The grant
     is not refunded, and descendant messages must divide an existing budget.
   - [x] Benchmark hashing, point decompression, signatures, proof operations,
     and MSM growth/finalization. Prepay the measured hash, signature, R1CS, and
     MSM work at the scheduling opcode; the measured curves are safely bounded
     by linear prices, so no arbitrary crypto-size cap is needed.

7. [x] Settle actor and call-context semantics.

   - [x] Keep `open` and `signcall` as isolated `CellOpen` predicate contexts.
     They inherit external/CS availability, but never inherit an actor identity
     or actor authority; actor-state, storage, code, issuance, and `selfid`
     operations remain unavailable.
   - [x] Store only the direct invoking actor's canonical 32-byte id in
     `CellOpen` as `caller_id: Option<[u8; 32]>`, not a cloned `ActorID`.
     `callerid` reports that id, or zero when the direct parent has no actor
     context. A nested `CellOpen` therefore sees zero rather than transitively
     inheriting an earlier actor's identity.
   - [x] Restrict synchronous `call` to frames with a current actor identity.
     Reject it from `ExternalRoot` and `CellOpen` before consuming operands, and
     remove the fabricated zero-actor caller fallback.
   - [x] Keep asynchronous `send` available from `CellOpen`, but always record
     `Message.caller = None`: read-only `callerid` attribution does not delegate
     the invoking actor's authority. Specify zero/`None` as "no authenticated
     actor principal", not necessarily "originated in an external tx".
   - [x] Align the instruction context table and handlers: `selfid` remains
     actor-context-only, while `callerid` is also available in `CellOpen` with
     the direct-caller semantics above.
   - [x] Remove the claim that `send` immediately fails for a checked-out actor.
   - [x] Restrict `open` and `signcall` arguments to portable values, matching
     actor `call` and asynchronous `send`. Keep `return` unrestricted so
     negative ClearTokens, WideTokens, and other VM-local values can travel
     upward for resolution by the caller.
   - [x] `issuepub` is valid in both `InternalRoot` and `ActorCall`.
   - [x] Remove the documented implicit default gas grant: `call`, `open`, and
     `signcall` always consume an explicit gas operand.
   - [x] Keep deploy-on-first-delivery bytecode-reachable: `send` accepts an
     exact encoded `ActorID::Constructor` and preserves its code in the Message.
   - [x] Build one context matrix test covering ExternalRoot, InternalRoot,
     ActorCall, and external/internal CellOpen for every restricted opcode.

8. [x] Finish actor-storage lease coverage.

   - [x] Add remaining golden vectors for quote rounding, pool boundaries, lease
     coalescing, expiry, destruction, wire effects, and nested rollback.

9. [ ] Reconcile value capabilities and public data models.

   - [ ] Decide and document droppability for MSM, Merlin, Variables,
     Expressions, Constraints, zero-quantity `ClearToken`, and non-empty Dicts;
     align `drop` and its tests.
   - [ ] Decide whether Dict keys are signed or nonnegative. Code and tests
     currently support negative keys.
   - [ ] Remove unsupported `WideToken`, `Object`, and `Merlin` entries from the
     “encodable types” table, or implement their encodings. Use `Cell`, not the
     removed `Object` name.
   - [ ] Clarify that MSM point decompression happens during `verify` even though
     batch acceptance is deferred.
   - [ ] Remove the obsolete `method` field from exported
     `Address::MessageTarget`, or restore a method field consistently across
     Message, `send`, and the specification.

10. [ ] Make the effect model and chain formats complete.

    - [ ] Add `SetCode` to every supposedly exhaustive effect list.
    - [ ] Pin TxLog, block, state-root, accumulator, proof, and certificate
      formats with hard-coded vectors generated independently of the production
      encoders.
    - [ ] Define canonical decode and validation responsibility for formats that
      FlameVM intentionally exposes as encode-only.
    - [ ] Test that replaying the committed effect log produces exactly the same
      state transition as execution.

11. [ ] Decide how actor authors handle stale loaded snapshots.

    - [ ] Either prevent holding loaded state across an external interaction, or
      keep it legal and state checks-effects-interactions as an explicit author
      obligation with a safe example.
    - [ ] Add a regression example showing that checkout blocks observation but
      does not make a stale application-level decision safe.

12. [ ] Enforce complete transaction and block resource limits.

    - [ ] Resolve and implement the documented block gas, script-size,
      multiplication, gas-credit, and added-storage limits; current `Limits`
      covers only per-execution gas and memory.
    - [ ] Apply the same limits during mempool admission, block construction, and
      block verification.
    - [ ] Test aggregate limits across many individually valid transactions.

13. [ ] Define extension and version activation rules.

    - [ ] Specify how new opcodes, value tags, transaction versions, and chain
      formats activate without old and new nodes assigning different meaning to
      the same bytes.
    - [ ] Test pre-activation rejection, activation-boundary behavior, and
      post-activation replay.

14. [ ] Resolve blockchain and consensus design gaps.

    - [ ] Resolve every consensus, block-format, storage, accumulator, and
      mempool `TBD` in [consensus.md](consensus.md) and
      [blockchain.md](blockchain.md).
    - [ ] Rebuild the threat model around the selected design: equivocation,
      long-range attacks, proposer withholding and censorship, MEV, bribery,
      vote stalling, partitions, eclipse and Sybil attacks, Bitcoin reorgs, key
      rotation, resource-market manipulation, governance capture, unsafe
      upgrades, and proof-update denial of service.

15. [ ] Validate the finished design adversarially.

    - [ ] Fuzz every network decoder and state transition with explicit memory,
      recursion, and work bounds.
    - [ ] Run differential tests against an independent implementation using the
      vectors from tasks 5 and 10.
    - [ ] Commission fresh cryptographic, VM, chain, and consensus audits only
      after the preceding semantics and formats are frozen.
