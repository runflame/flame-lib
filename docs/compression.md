# Authenticated compression

## Status and scope

This is an exploratory subproject. It does not yet define consensus rules or
describe implemented behavior.

Here, **compression** means authenticated pruning: replace resident data with a
small cryptographic commitment while keeping enough witness data elsewhere to
restore selected contents later. **Decompression** means verifying that witness
data against the commitment and materializing it for execution. This is not an
ordinary byte-compression codec and does not, by itself, guarantee that the
witness data remains available.

The immediate scope is actor state, Dict branches, and Cells. Utreexo and
Taproot program trees may share the same primitives, but need not be part of the
first implementation.

## Core insights

- An actor whose storage lease expires can be **frozen instead of destroyed**.
  Its code and state are replaced by commitments, so its identity and ownership
  relationships survive without keeping the complete state resident.
- Freezing must not retire linear values hidden in actor state. Tokens and other
  non-droppable values cannot be destroyed in bulk; they remain committed under
  the frozen state root until the actor explicitly accesses and disposes of
  them according to normal VM rules.
- A Dict can be committed as a whole while being materialized piece by piece.
  An operation need only provide the branches it reads or changes.
- An actor may eventually be allowed to prune selected Dict branches while it
  is still active, reducing resident storage without freezing all of its state.
- Opening a Cell and restoring a frozen actor are instances of the same basic
  operation: resolve a short authenticated reference using witness data.
- A Cell may itself contain a Dict with pruned branches. Restoring the Cell does
  not require eagerly restoring every branch of every nested Dict.
- Witness data may be supplied explicitly as program data, implicitly through
  the transaction execution context, or through both interfaces.
- These cases may use one content-addressed witness format, one verifier, and
  one set of availability and charging rules.

## Actor lifecycle

The proposed lifecycle is:

```text
resident actor
    | storage expiry or explicit freeze
    v
frozen actor: ActorID + code/state commitments + capability summaries
    | transaction supplies the witnesses needed by this execution
    v
transiently materialized actor
    | successful save
    v
resident, partially pruned, or frozen actor state
```

A frozen actor remains addressable. A transaction that supplies sufficient
witnesses can execute it and save a new state. Missing witness data causes that
execution to fail; it does not erase the actor.

Explicit permanent actor destruction, if retained, is a separate operation. It
must prove that no linear value is being discarded rather than inheriting the
old lease-expiry bulk-destruction behavior.

Freezing changes the meaning of storage rent: rent pays for resident,
witness-free availability, not for logical existence. The permanent actor
record still has a global cost, so consensus must eventually price or
accumulate those records rather than assuming that a 32-byte root is free.

## Partially materialized Dicts

A Dict should have one canonical **content root** independent of which branches
are currently resident. A branch reference is either:

```text
Resident(node)
Pruned(commitment, entry_count, logical_size, portable, droppable)
```

The exact tree shape is open. Binary, quaternary, and higher-fanout trees trade
proof size against update and lookup costs. The first design should choose the
simplest canonical representation that gives bounded proofs.

Each pruned branch needs enough authenticated summary data to preserve the
rules that would apply if it were resident:

- subtree commitment;
- entry count and logical encoded size;
- `portable` summary; and
- `droppable` summary.

The parent commitment must commit to these summaries. A branch hiding a token
must therefore remain non-droppable, while a proven empty branch may be
droppable. Pruning and hydration do not copy or move VM values; they change only
the representation of the same logical Dict.

A lookup that reaches a pruned branch asks the current execution's witness
provider for that branch. A valid proof of absence produces normal Dict
not-found behavior. A missing proof is a distinct hard `MissingWitness`
failure. An update consumes the old authenticated path and commits a new one,
so a witness for an earlier root cannot be replayed after mutation.

Optional partial compression should be built on this same representation:
pruning a chosen branch changes residency and storage charges, not the content
root. Because residency controls whether execution requires a witness, the
actor record must separately commit its residency map (or an equivalent
availability root). Local caches never change that committed map. There is no
need for a second compression format.

## Cells and frozen actors

Cells and frozen actors differ in ownership but can share restoration
machinery:

| Property | Cell | Frozen actor |
| --- | --- | --- |
| Identity | Cell ID and anchor | Actor ID and state version/root |
| State | Immutable, single-use | Mutable after authenticated restoration |
| Witness result | Cell payload/program | Actor code and selected state branches |
| Consumption | Cell is opened or spent | Actor remains and receives a new state root |

Both can use content-addressed witness blobs and authenticated paths. Cells may
also contain compressible Dicts, so restoration should be lazy across nested
objects rather than recursively materializing everything.

## Witness delivery

Two interfaces are useful:

1. **Explicit:** the program receives witness values and invokes a restore
   operation. This is simple and visible, but exposes proof plumbing to every
   contract.
2. **Implicit:** the transaction carries a witness bag outside the program.
   Dict lookup, Cell opening, or actor restoration asks the execution context
   for the required object. Programs use short IDs and keys.

The implicit form is the better default for ordinary programs. An explicit
form may remain useful when a contract needs to inspect or forward proof data.
Both forms should feed the same verifier.

Witness availability must be scoped to a particular execution. A block may
deduplicate identical bytes physically, but it must not make every block
witness visible to every transaction or message. Otherwise one transaction can
change from failure to success merely because an unrelated transaction happens
to carry the missing witness.

A possible block representation is:

```text
WitnessTable:       WitnessID -> bytes
ExecutionManifest: transaction/message -> ordered WitnessIDs
```

Consensus rules should include:

- an execution sees only its own manifest;
- there is no fallback to a node's private cache, archive, or network;
- a manifest reference with absent bytes or a hash mismatch makes the block
  invalid;
- a required witness absent from the manifest causes deterministic execution
  failure;
- valid witness data guarantees availability of that object, though execution
  may still fail for another reason;
- unused witnesses are allowed only if they are committed and charged; and
- asynchronous messages commit to the witness scope they will receive, so a
  message ID also commits to any witness-dependent behavior.

Synchronous calls can share the enclosing execution's witness scope. The actor
and state root against which a proof is checked must still be explicit in the
lookup key.

## Common and adjacent applications

| Application | Common pattern | Important difference |
| --- | --- | --- |
| Actor state | Root plus selectively restored state | Mutable identity and linear values |
| Dict | Root plus key-path proofs | Fine-grained reads and updates |
| Cell | ID plus payload/program witness | Immutable and single-use |
| Utreexo | Compact accumulator plus membership proof | Set membership and batch updates |
| Taproot program tree | Root plus revealed script path | Usually reveals one immutable branch |

Utreexo already demonstrates the compact-state-plus-witness model. Taproot
demonstrates selective program revelation. They are useful reference cases,
but sharing code with them should wait until the actor/Dict/Cell primitives are
stable and an actual common interface is evident.

## Safety requirements

Any design in this subproject must preserve these properties:

- **Canonical state:** resident and pruned representations have the same
  content root, while consensus separately commits any residency distinction
  that affects witness requirements.
- **Linearity:** hiding a value never makes it droppable or duplicable.
- **Scoped availability:** success cannot depend on witnesses supplied to an
  unrelated execution or retained privately by a node.
- **Deterministic failure:** missing, malformed, and valid absence proofs have
  distinct consensus outcomes.
- **Atomic rollback:** a failed execution restores the same actor root,
  residency metadata, and witness obligations it started with.
- **Bounded work:** witness bytes, proof verification, materialized nodes, and
  resulting resident storage are charged and limited.
- **Block self-containment:** validation does not require fetching data from the
  network during execution.
- **Data availability:** commitments preserve integrity, not availability;
  owners or archival services must retain the witness data needed for future
  restoration.

Witnesses included in a block also reveal their data to block observers.
Privacy, encryption, and zero-knowledge access are separate concerns.

## Open decisions

- Canonical Dict tree shape, node encoding, proof format, and size bounds.
- Whether actors freeze automatically on lease expiry, explicitly, or both.
- Which actor metadata remains resident and how its permanent cost is bounded.
- Whether actor code can be pruned separately from state.
- Exact explicit and implicit witness APIs.
- How witness manifests propagate into asynchronous messages and bounces.
- Whether partial Dict pruning is actor-controlled or only a storage-layer
  representation choice.
- Maximum String and program sizes, so large portable data has one canonical
  chunking path through Dicts.
- Which witness bytes are retained across reorganization and archival pruning.
- Whether Utreexo and Taproot merely inform the design or eventually share its
  implementation.

## Proposed work order

1. Specify commitments, capability summaries, witness scoping, and failure
   semantics independently of a tree implementation.
2. Prototype a canonical pruned Dict node and test lookup, absence, update,
   rollback, and hidden-token droppability.
3. Add a transaction-scoped witness provider and block manifest.
4. Freeze and restore actor state using the Dict mechanism, while treating a
   bounded non-Dict state as one witness blob.
5. Reuse the provider for Cell restoration and nested Dict payloads.
6. Only then evaluate whether Utreexo or Taproot reuse enough of the mechanism
   to justify a shared abstraction.
