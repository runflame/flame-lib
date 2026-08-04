# Flame implementation plan

This list is ordered by dependency and risk. Resolve conservation and consensus
semantics before optimizing or extending the system. A task is complete only
when the specification, implementation, and focused regression tests agree.

1. [ ] Make synchronous call failure preserve ownership.

   - [ ] Define one rule for values moved into `call`, `open`, and `signcall`:
     a failed operation must restore each entry-owned value to the caller, or
     propagate failure to a transaction boundary that restores it. A successful
     enclosing transaction must never silently discard a `Token`, nonzero
     `ClearToken`, `WideToken`, `Cell`, or token-bearing `Dict`.
   - [ ] Fix pre-entry `call` failure. Target lookup, call-depth checks, vbyte
     credit, and other fallible setup currently happen after arguments and the
     byte token have been removed. A returned `0` must have a defined ownership
     result for all of them.
   - [ ] Fix entered-call failure. [`VM::fail_current_call`](../flamevm/src/vm.rs)
     currently discards the complete child stack; preserve the entry escrow for
     actor-call arguments and for the `Cell`, payload, and arguments consumed by
     `open`/`signcall`.
   - [ ] Apply the same rule to runtime errors, out-of-gas, out-of-memory,
     `StackNotClean`, `BadReturnArity`, and explicit verification failure.
   - [ ] Put every rollback checkpoint before its boundary side effects. In
     particular, a failed `signcall` must not retain the signature recorded
     before its snapshot, and actor vbyte credit must follow the chosen deposit
     rollback rule.
   - [ ] Make `credit_vbytes` participate in registry checkpoints. A synchronous
     call credit must not survive rollback of the frame or transaction that
     supplied its byte token; pin any deliberate root-message top-up exception
     separately.
   - [ ] Add a failure matrix covering pre-entry rejection, entered failure,
     dirty EOF, bad return arity, and nested failure for every bearer type, plus
     a byte-supply conservation test across nested and outer rollback.

2. [ ] Make asynchronous message failure conserve payload assets.

   - [ ] Implement the documented bounce: on failed delivery, seal the original
     payload into exactly one cell under `refund_predicate`, or replace the spec
     with another exactly-once recovery rule before implementing it.
   - [ ] Define whether a failed delivery consumes, refunds, or retains the
     attached vbytes, and whether failed constructor delivery leaves an actor
     deployed. Deployment, credit, message consumption, and bounce creation must
     share one atomic boundary.
   - [ ] Cover missing, frozen, and checked-out actors; malformed or failing
     code; dirty return stacks; repeated delivery; and failure while producing
     the bounce.
   - [ ] Prove with tests that a message carrying each portable bearer type can
     be delivered or recovered, but never lost or recovered twice.

3. [ ] Close internal signature authorization gaps.

   - [ ] Make `signtx` external-only, as internal transactions have no TxID-bound
     signature finalization.
   - [ ] Either make `signcall` external-only or verify its explicit signature
     during internal execution before entering the signed script.
   - [ ] Reject arbitrary 64-byte signatures in internal execution and test both
     invalid and valid signatures through the public transaction path.
   - [ ] Ensure a failed `signcall` removes its deferred signature together with
     the failed frame.

4. [ ] Enforce portability recursively at every wire and storage boundary.

   - [ ] Reject negative `ClearToken` quantities in both `read_value` and
     `write_value`; negative tokens remain stack-only intermediates.
   - [ ] Validate decoded `Dict` members recursively instead of relying on
     [`Dict::is_portable`](../flamevm/src/dict.rs) always returning `true`.
   - [ ] Apply the same recursive check to cells, messages, and actor state, and
     test nested negative tokens at more than one Dict depth.
   - [ ] Audit every public constructor and unchecked decoder so the portability
     invariant does not depend only on opcode callers.

5. [ ] Reconcile canonical wire and opcode contracts.

   - [ ] Change the sub-varint U64 base in [flamevm.md](flamevm.md) from
     `4_295_032_608` to the implemented contiguous value `4_295_033_088`, then
     pin the boundary with golden vectors.
   - [ ] Choose and specify one `decrypt` stack order. The document says
     `T f' f q' q`; the implementation and tests use `T f f' q q'`.
   - [ ] Decide whether invalid `decrypt` openings fail synchronously or only at
     final batch verification, then align code, rollback behavior, and prose.
   - [ ] Correct the `call`/`open`/`signcall` diagrams to include the success
     shape `results... k 1`, clean fall-through `0 1`, and failure `0`, or change
     the implementation.
   - [ ] Replace the global claim that soft failure preserves every operand with
     exact per-opcode stack shapes; decide whether count/key operands consumed by
     `readstr` and `getopt` should be restored.
   - [ ] Align documented error variants for bit-count bounds with the actual
     `IndexOutOfRange`/`BitCountOutOfRange` behavior.
   - [ ] Add cross-implementation vectors for all width boundaries, failure
     shapes, and supported value tags.

6. [ ] Make memory and gas accounting non-bypassable.

   - [ ] Remove the production meaning “`mem_limit == 0` is unlimited.” Zero
     must mean no allocation, or production entry points must reject it.
   - [ ] Charge `writebits`, `writeint`, Dict growth, stack growth, decoded
     payloads, constraint state, deferred signatures, and MSM/batch terms.
   - [ ] Audit every allocator reachable from hostile bytecode and place a hard
     bound where precise accounting is not worthwhile.
   - [ ] Decide whether EOF consumes gas. Code currently charges before finding
     EOF, so an `N`-instruction clean program needs `N + 1` gas.
   - [ ] Keep an immutable frame-creation gas cap for `gaslimit`; refunds must not
     make the reported cap grow.
   - [ ] Bound or debit the gas attached to `send`. An actor must not create
     arbitrary future execution budget from a small incoming grant.
   - [ ] Benchmark hashing, point decompression, signatures, proof operations,
     and MSM growth/finalization, then replace flat pricing or add hard caps.

7. [ ] Settle actor and call-context semantics.

   - [ ] Decide whether an empty message is a guaranteed code-free vbyte top-up
     or a normal delivery. The implementation currently always runs actor code.
   - [ ] Decide whether `CellOpen` may `send` and, during internal execution,
     `call` an actor with the zero caller identity. Align the context table and
     handlers.
   - [ ] Mark `selfid` and `callerid` as actor-context-only in the instruction
     table; the handlers reject ExternalRoot and CellOpen.
   - [ ] Remove the claim that `send` immediately fails for a checked-out actor,
     or add a registry check with well-defined asynchronous semantics.
   - [ ] Decide whether `open` and `signcall` arguments must be portable. The
     prose says yes; the implementation intentionally accepts arbitrary values.
   - [ ] Decide whether `issuepub` is valid in both `InternalRoot` and
     `ActorCall`, or only in the latter.
   - [ ] Remove the documented implicit default gas grant or add an operand form
     that actually uses the caller's remaining gas.
   - [ ] Make deploy-on-first-delivery reachable from bytecode or explicitly
     specify it as a host-only message-construction path. `send` currently emits
     only `ActorID::Hash` targets.
   - [ ] Build one context matrix test covering ExternalRoot, InternalRoot,
     ActorCall, and external/internal CellOpen for every restricted opcode.

8. [ ] Finish the vbyte token and actor-rent model.

   - [ ] Use one transient-memory formula everywhere. The implementation and the
     Design section use `4 * remaining vbyte balance`; the Storage section says
     `4 * occupied persistent size`.
   - [ ] Specify the acquisition mechanism for flavor-1 byte tokens. The VM pool
     delegates purchase to consensus, while the document still refers to buying
     bytes through fees.
   - [ ] State plainly that byte tokens held in cells or state are ordinary
     tradeable assets and do not bleed until deposited into actor rent.
   - [ ] Implement and report meaningful `vbytes_used`, including deposits,
     storage bleed, freeze/unfreeze, destruction, recycling, and maturity.
   - [ ] Resolve the failed-call and failed-delivery deposit rule from tasks 1
     and 2 before exposing the market mechanism.

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
