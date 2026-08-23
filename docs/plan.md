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

9. [x] Reconcile value capabilities and public data models.

   - [x] Define droppability by asset ownership: MSM, Merlin, Variables,
     Expressions, Constraints, zero-quantity `ClearToken`, and non-empty Dicts;
     the pure-computation values and zero token are droppable, while Dict
     droppability is sticky until the Dict is fully drained; an empty Dict is
     always droppable. Align `drop` and its tests.
   - [x] Define Dict keys as signed `Int253` values in total numeric order;
     list-style encoding still requires the exact nonnegative range `0..n-1`.
   - [x] Remove unsupported `WideToken`, `Object`, and `Merlin` entries from the
     encodable-types table. Generic value tags `251..=254` are unassigned;
     `Cell` has a separate top-level encoding rather than a generic Value tag.
   - [x] Clarify that MSM point decompression happens during `verify` even though
     batch acceptance is deferred.
   - [x] Remove the obsolete `method` field from exported
     `Address::MessageTarget`; a selector, when used, is an ordinary argument.

10. [x] Make committed effects complete, replayable, and canonically committed.

    - [x] Complete the context-specific effect inventory. Add `SetCode` to the
      exhaustive internal-effect list, and add an `ActorDeploy { actor, code }`
      effect for successful first delivery to a constructor-form actor. Initial
      state is the canonical empty state; append its wire tag without
      renumbering existing effects.
    - [x] Define and enforce canonical log shapes: external logs start with
      `Header`; successful internal logs start with `Header, Receive` and an
      optional immediate `ActorDeploy`; failed delivery is `Header, Receive,
      Output(refund)`; system destruction is `Header, Data(height), Retire...,
      ActorDestroy`.
    - [x] Implement one ordered effect applier for inputs, outputs, sends, actor
      deployment, state and code replacement, storage purchases, and actor
      destruction. Utreexo membership proofs remain an application sidecar;
      every actual state mutation must be represented by an effect.
    - [x] Make replay the production application path. Execute actor code under
      an existing registry checkpoint, capture the resulting commitment and
      log, roll the direct mutations back, then consume and apply the log. The
      replayed actor root, storage pool, cell root, and queued messages must
      equal the execution result.
    - [x] Cover constructor deployment; token-bearing state; ordered
      `ActorSave` and `SetCode`; storage pool, lease, and expiry-index changes;
      explicit and expiry-driven destruction; output/send combinations; nested
      actor calls; and atomic rollback when execution or replay fails.
    - [x] Specify decoder and trust-boundary ownership. `Block`, external
      transactions, and Utreexo proofs require bounded network decoders;
      FlameVM `TxLog`, `TxEntry`, and `Message` remain encode-only because
      consensus re-derives them. An archival decoder must never create an
      application bypass.
    - [x] Freeze the formats whose fields are already settled: `TxEntry` and
      `TxLog`, state and code roots, actor leaves/root, Utreexo `Forest`, and
      transient/committed Utreexo proofs. Serde encodings of accumulator working
      state are explicitly non-consensus.
    - [x] Pin those encodings and commitments with versioned hard-coded vectors
      generated independently of the production encoders. Remove the production
      `REGEN` path and cover every effect, ordered aggregate logs, actor roots,
      accumulator shapes, proof shapes, and malformed/non-canonical inputs.

11. [x] Pin actor-state checkout and re-entrancy semantics.

    - [x] Keep holding loaded state across a nested interaction legal. Checkout
      gives the current frame exclusive ownership until `save` or rollback, so
      the loaded state cannot become stale through re-entrancy and no mandatory
      checks-effects-interactions ordering is needed.
    - [x] Keep the regression `reentrant_view_of_mid_update_state_is_blocked`,
      which proves that a nested call cannot enter or observe a checked-out
      actor, even through a read-only method.
    - [x] Distinguish that VM guarantee from ordinary actor logic: an actor must
      still account for a callee's success status and effects when deciding what
      state to save, but the VM cannot infer or enforce that application policy.

12. [ ] Enforce complete transaction and block resource limits.

    - [ ] Resolve and implement the documented block gas, script-size,
      multiplication, gas-credit, and added-storage limits; current `Limits`
      covers only per-execution gas and memory.
    - [ ] Once those fields are settled, define bounded canonical encoders and
      decoders for `ExternalTx`, `BlockTx`, `BlockHeader`, and `Block`; reject
      unknown versions and tags, oversized lengths, malformed proofs, and
      trailing bytes before application.
    - [ ] Pin external-transaction and block encodings, witness/effect roots,
      state commitments, and block IDs with independently generated vectors.
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
    - [ ] After selecting the BFT protocol, define its canonical proposal, vote,
      and certificate formats, bounded decoders, domain separation, and
      independently generated conformance vectors. Do not invent a placeholder
      certificate before the signer set, quorum rule, and signature scheme are
      known.
    - [ ] Rebuild the threat model around the selected design: equivocation,
      long-range attacks, proposer withholding and censorship, MEV, bribery,
      vote stalling, partitions, eclipse and Sybil attacks, Bitcoin reorgs, key
      rotation, resource-market manipulation, governance capture, unsafe
      upgrades, and proof-update denial of service.

15. [ ] Validate the finished design adversarially.

    - [ ] Fuzz every network decoder and state transition with explicit memory,
      recursion, and work bounds.
    - [ ] Run differential tests against an independent implementation using the
      vectors from tasks 5, 10, 12, and 14.
    - [ ] Commission fresh cryptographic, VM, chain, and consensus audits only
      after the preceding semantics and formats are frozen.
