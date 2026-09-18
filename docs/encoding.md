# Data encoding

FlameVM values, actor storage, and transaction/block transport use the
byte-granular [Cells format](cells.md). Utreexo remains an isolated exception:
its accumulator, Merkle paths, and legacy codecs are unchanged.
Expected types carry no schema tag or version. Sum types have one discriminant;
existing transaction/block protocol versions remain in their headers.

A Cell record is `descriptor:u16-le || payload || child_ids`, with
`descriptor = (ref_count << 13) | payload_length`, 0..8191 payload bytes,
and 0..4 ordered 32-byte references. Its ID is plain SHA-256 of that record.
Pruning never changes the ID. Standalone transport is a `CellEnvelope`:
root ID followed by a canonical, strictly CellID-sorted Bag of Cells (BoC).
Bodies omitted from the bag remain pruned.

## Transactions and execution witnesses

An ExternalTx root has this layout:

| Payload | Ordered child references |
| --- | --- |
| version:u32 LE, locktime:u32 LE, execution BoCID:32 bytes | script container, signature, R1CS proof, **pruned TxLog** |

The script container stores the script in length-prefixed snake encoding.
Its next unused reference points to a snake containing the canonical bytes of
the execution BoC. A long script uses the first reference for its continuation;
the execution-bag reference follows it. The signature Cell has either zero
bytes (no TxID-bound authorization) or 64 bytes, and no references. The proof
is a dedicated snake. UnsignedTx omits the signature and proof references,
leaving script-container and pruned-TxLog references.

There are deliberately two bags:

- The **transport bag** reconstructs the envelope, script, signature, proof,
  and nested execution-bag bytes. Its bodies are not implicitly VM witnesses.
- The **execution bag** is the immutable set available to the external VM and
  every synchronous call and asynchronous descendant it initiates. It contains
  Contract bodies, predicate paths, and optional actor/Dict bodies.

External execution emits `Header`, then `CellWitness(execution_boc.id())`.
That entry participates in TxID and therefore the signature and R1CS transcript.
Removing even an unused execution body changes the signed statement. The bag
is fixed before proving; it never includes its own transaction envelope,
avoiding a circular commitment. Private commitment openings and prover-only
instructions do not belong in either public bag.

### Prover-to-verifier API

Keep witnesses in `ScriptBuilder` until `build_tx`; raw `to_bytecode()` is not
a standalone transaction. `PredicateTree::from_scripts` retains typed branch
programs, including their allocation assignments and nested witnesses.
`push_taproot_proof(&tree, logical_index)` emits the short selector and attaches
the selected public path plus that program's private overlay. Unselected
programs' witnesses are not collected. Code already present in a literal or
predicate leaf is not duplicated as a separate code Cell in the execution bag.

For a Contract protected by `tree`, the public workflow is:

```rust
let unsigned = ScriptBuilder::new()
    .push_str(String::contract(contract))
    .input()
    .push_taproot_proof(&tree, 0)?
    .push_int(child_gas)
    .push_int(0u64) // no explicit arguments
    .open()
    .verify()
    .drop_() // discard the zero return count from this branch
    .build_tx(header, limits)?;

// When signtx is used, sign unsigned.signing_instructions() instead.
let tx = unsigned.without_signature()?;
let bytes = tx.to_envelope()?.encode();
let decoded = ExternalTx::from_bytes_bounded(
    &bytes, 1, max_script_bytes, max_proof_bytes,
)?;
let verified_log = decoded.verify(limits)?;
```

This example expects the selected branch to consume its payload and return no
values. `UnsignedTx::witnesses()` exposes the frozen public bag for inspection;
both `sign` and `without_signature` preserve it. `ExternalTx` carries the bag
through transport and passes it to the verifier automatically. There is no
separate witness sidecar for the caller to assemble. The bag contains all
explicitly attached paths, including those in conditional code, and freezes
before proving; it is not trimmed according to later execution outcomes.

### Transaction identity

The claimed TxID is the **TxLog root CellID**, not the ExternalTx root ID.
Verification re-executes and checks the claimed effect root. Bounded transport
decoders reject trailing bytes and unused outer bodies; unused bodies inside
the committed execution bag are allowed.

## TxLog

The root payload is entry_count:u64 LE. A nonempty log has one reference to
a Trie keyed by consecutive u64 big-endian indices. Each leaf references a
separate TxEntry Cell. The sum tag is one byte:

| Tag | Entry | Remaining payload | References |
| ---: | --- | --- | --- |
| 0 | Header | version:u32, locktime:u32 | — |
| 1 | Data | — | snake bytes |
| 2 | Input | ContractID:32 | — |
| 3 | Receive | MessageID:32 | — |
| 4 | Output | — | Contract |
| 5 | IssuePub | qty:Scalar, flavor:Scalar | — |
| 6 | IssuePriv | qty point:32, flavor point:32 | — |
| 7 | Retire | qty point:32, flavor point:32 | — |
| 8 | Fee | sparks:u64 | — |
| 9 | ActorSave | actor ID:32 | state Value |
| 10 | SetCode | actor ID:32 | code snake |
| 11 | Send | — | Message |
| 12 | StoragePurchase | actor ID:32, bytes:u64, expiry:u64, fee:Scalar | — |
| 13 | ActorDestroy | actor ID:32 | — |
| 14 | ActorDeploy | actor ID:32 | constructor code snake |
| 15 | CellWitness | execution BoCID:32 | — |

Integers in payloads are little-endian unless explicitly stated otherwise.

## Values

| Expected type | Payload | References |
| --- | --- | --- |
| Scalar (formerly Int253) | canonical residue, 32 LE bytes, strictly below the group order | — |
| Point / Predicate | 32-byte compressed Ristretto representation | — |
| Token | qty commitment:32, flavor commitment:32 | — |
| ClearToken | qty Scalar:32, flavor Scalar:32 | — |
| String | raw bytes, 0..8191; no length prefix | — |
| Script / proof / arbitrary blob field | snake: u32 LE total length followed by bytes | next continuation when needed |
| Dict | count:u64 LE, flags:u8 | Trie root unless empty |

Only `Value` adds a tag: Scalar=0, String=1, Dict=2, Point=3,
Token=4, ClearToken=6. Fixed-width contents follow inline; String and Dict
contents are in one child Cell. Other tags are rejected. WideToken, the
Contract stack handle, Merlin, Variable, Expression, Constraint, and MSM
have no Value encoding.

The runtime String API is retained temporarily; replacing it with a Cell value
is a later change. Every String fits in one payload-only Cell: its descriptor
supplies the length, and references or snake continuations are forbidden.
The limit also applies to witness-bearing Strings' public bytes. Literal
parsing and VM string growth reject lengths above 8191 with `StringTooLong`,
before allocating the result. Programs, proofs, and other schema-defined blob
fields remain snakes and may span many Cells; they are not runtime Strings.

Dict has a single implementation over `cells::Trie`, including small and
sequential dictionaries. Keys are always present in the trie path: reverse
the canonical 32-byte scalar encoding to obtain big-endian paths, preserving
unsigned numeric order. There is no separate list-mode Dict format.
Flags are sticky portable (bit 0) and droppable (bit 1); other bits are invalid.
An empty Dict has no root and is droppable even if its sticky droppable bit
is false. Pruning does not reset either flag.

Ordinary typed imports validate counts, keys, flags, and reachable values.
Authenticated Contract/Actor reads may trust previously admitted summaries
and leave branches pruned. Access resolves only the required path. Private
prover values can supply commitment openings, but cannot manufacture public
availability. Nested typed imports have an explicit depth bound.

`Contract::from_trusted_cell` and `Value::decode_trusted` skip the complete
Dict walk performed by ordinary `CellDecode`. Their precondition is prior
admission of the owning state and its summaries, not merely knowledge of a
Cell hash. Hash equality proves content identity, not correct capability flags
or ownership. Input membership/spend-once checks remain the chain's job.
`Trie::from_cell(root, key_bytes)` accepts a resident or pruned `CellRef`, or a
`Cell`/`Arc<Cell>` converted into a resident reference. It needs no trusted
count and does not inspect the root; all visited nodes are resolved and
validated on access. Key width is fixed by the owning schema, not encoded in
the Trie. Dict owns its committed entry count;
`Trie::entries_exact(count, resolver)` checks that count on full import. The
constructor establishes neither VM ownership nor admission.

Encoding does not impose portability: a negative ClearToken is encodable.
Domain admission into outputs, actor state, downward calls, or sends enforces
the appropriate rules. Compressed points are validated at cryptographic use.

## Contracts

A UTXO Contract is a Cell whose ID is its ContractID:

```text
payload: predicate:32 | anchor:32 | one tagged Value
refs:    references used by that Value (e.g. one Dict root)
```

A Contract contains **one Value**, not an implicit list of stack arguments.
Use a Dict to hold several values. `contract` and `output` take
`payload predicate`; `signtx` returns that one payload, without an arity.

`input` consumes a 32-byte ContractID, resolves its body from the initiating
execution bag, and creates the linear Contract handle. The chain verifies
the spend-once Utreexo proof separately. ScriptBuilder can accept a private
Contract witness, emit its short ID in bytecode, and collect public Cell bodies
without revealing private token openings.

## Predicate branches

A Predicate remains a single 32-byte tweaked key. Its tree witness is an
ordered eight-byte-key Trie whose raw root CellID is used directly in the
Taproot tweak. There is no intermediate root/count envelope. Branch lookup
needs only the fixed key width and selected index, not the total leaf count.
Each logical program is blinded with a second leaf. A leaf is the sum
`0 | snake(program)` or `1 | blinding_bytes:32`.

A branch selector contains internal_key:32, root_CellID:32, index:u64 LE.
No sibling-hash array or bit-position string is encoded.
`PredicateTree::witness_for(logical_index)` returns this selector plus a
BoC containing only the selected path and its program/continuations.
`Predicate::open_branch(selector, resolver, max_program_bytes)` verifies
the tweaked-key relation and uses `Trie::lookup` to resolve the selected
program through Cells. Missing indices fail lookup; selecting a blinding leaf
fails the program-tag check. Unvisited sibling bodies are not decoded.

Removing the old count envelope changes predicate roots and tweaked points,
and thus IDs of Contracts using those predicates. This is a format change,
not a compatibility decoder for the previous wrapper layout.

VM stack form:

```text
contract internal_key root_id index gas args… k  open
  → results… k' 1
  | contract args… k 0       (entered child failed)
```

The child starts with the single payload followed by the explicit arguments.
Invalid selectors or missing pre-entry path bodies hard-fail the current
frame; failures after child entry retain the existing Contract/argument
escrow rules. Merlin is used for the Taproot tweak and signature/proof
binding, not for raw content addresses.

## Actors and messages

ActorID is a sum: `0 | hash:32`, or `1` with a reference to constructor
code as a snake. Both normalize to the constructor code's CellID.

A Message contains anchor:32, ActorID, caller-presence:u8 and optional raw
caller ID:32, refund Predicate:32, gas:u64, and a reference to a Dict of
arguments at consecutive keys. A constructor target uses the preceding
reference for its code. MessageID is this root CellID. The anchor is ratcheted
at every send, so identities are unique by construction.

The actor registry is a 32-byte-key Trie ordered by canonical actor ID.
Its envelope has count:u64 LE and an optional Trie reference. Each entry
contains resident_BoCID:32 and one pruned reference to this actor layout:

| Actor slot | Payload | Ordered references |
| --- | --- | --- |
| Live or frozen | 1:u8, code byte length:u64, state encoded byte size:u64 | code snake, state Value, lease envelope |
| Explicitly destroyed, leases remain | 0:u8 | lease envelope |

The lease envelope contains count:u32 LE and an optional Trie reference.
Keys are expiry height:u64 BE; leaves contain units:u64 LE. Same-expiry leases
are coalesced. `Blockchain::actor_storage` exposes a `StoredActor { root,
cells }` snapshot for witness construction.

The committed resident BoC contains actor/lease metadata and the explicitly
retained code/state graph. Rent counts unique resident code/state Cell record
bytes plus the existing per-lease charge; registry metadata is not charged
again. Frozen actors retain metadata, code/state IDs, lengths, and leases,
but discard code/state bodies. They are not literally represented by only
32 bytes. No tokens are retired by freezing.

Resolution uses attached resident Cells, then the **current actor's** retained
code/state bodies from `ActorRegistry::actor_cells`, then the initiating
execution bag. This VM fallback excludes actor-layout and lease metadata;
the registry's `StoredActor` export includes that metadata for storage/witness
construction. Resolution never consults another actor's store or a node-global
cache. Every logical Cell access is charged, including
resident/cache hits. Witness-only reads do not persist bodies; explicit
`save`/`setcode` retain reachable owned changes. General partial-pruning
opcodes and disk adapters are not introduced by this migration.

Existing block-boundary timing is retained: actors losing capacity are
unavailable during that block and have their bodies frozen at its end.
Subsequent execution can use supplied witnesses; persistent restoration must
still satisfy storage capacity.

## Blocks and Utreexo

Block transport is a CellEnvelope. The Block root has empty payload and
references [BlockHeader, transaction sequence]. A BlockTx root contains
gas:u64 LE and references [ExternalTx, Utreexo proof sequence].

Block sequences use count:u32 LE and an optional Trie root, keyed by consecutive
u32 BE indices. Their leaf Cells hold the expected type. The witness root is
the transaction-sequence CellID. An execution record is
`kind:u8 | TxID:32` (0 external, 1 internal, 2 failed/bounced internal);
the effects root is the execution-record-sequence CellID.

The BlockHeader has 213 payload bytes for a regular block or 221 for a Core block,
with no references:

```text
version:u32 | height:u64 | core_block_tag:u8
[core_height:u32 | target_btc_height:u32]
core_block_hash:32 | parent:32
| witness_root:32 | effects_root:32 | Utreexo_root:32 | actor_root:32
| available_storage_units:u64
```

`core_block_tag` is `0` for `None` or `1` for `Some(CoreBlockHeader)`; other values
are invalid. The two bracketed fields are present only for tag `1`. `core_height`
is the Core-block sequence height, separate from the ordinary Flame `height`.

BlockHash is its CellID. Each external transaction immediately drains its
FIFO actor-message closure before the next external transaction, reusing
only its own execution bag. A block-wide transport deduplication cannot add
witnesses to any transaction.

Utreexo's existing `Forest`, `Path`, and `Proof` algorithms, hashing, and byte
encodings are unchanged. The `merkle` and `readerwriter` crates remain for
that isolated subsystem. Only outer block transport wraps each unchanged
`Proof` byte encoding in a snake Cell, as a leaf of the block's proof sequence.
FlameVM no longer depends on those crates; its duplicate Chunk/Trie/Dict2
implementations are removed in favor of `cells`. Compact **instruction bytecode** is still
parsed directly and stored in snake-encoded script Cells; it is not a second
general-purpose serialization framework.

These are consensus-breaking changes to the migrated encodings and identities.
Old FlameVM/transport bytes and IDs must not be mixed with the new formats;
signatures and proofs bound to changed IDs must be regenerated. This does not
change Utreexo's own proof format.
