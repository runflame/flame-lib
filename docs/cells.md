# Flame Cells

Cell is the data encoding format underpinning all data structures in Flame. Each cell is a data structure that can carry 0 to 8191 bytes of binary data (called "payload") and 0 to 4 references to other cells. Nested cells form an immutable directed acyclic graph (DAG).

Design of cells in Flame is heavily inspired by cells used within TON blockchain designed by Nikolai Durov, with some important differences:

1. Flame Cells store whole bytes instead of bits.
2. Payload maximum size is considerably larger (8191 bytes vs. 1023 bits).
3. Flame Cells do not store level information or depth.
4. Cells are not first-class types exposed in the FlameVM, but instead underpin Strings, Dicts, Contracts, Actors, and canonical encodings.
5. FlameVM permits transparent loading of pruned branches from an externally provided data source. This is used in decoding of Contracts, pruned Actors, Taproot branches and compressed Dicts.
6. Bag-of-Cells (BoC) format is much simpler.

## Status

The Cell crate, VM/chain codecs other than the deferred Utreexo formats,
unified Dict, witness-backed predicate and actor reads, and per-external
execution closure are implemented. Exact typed
layouts are specified in [encoding.md](encoding.md). The architecture below
also describes constraints on future disk adapters and explicit partial
pruning APIs; those adapters/opcodes are not implemented.
`docs/compression.md` records the motivation and broader experiments.

Runtime `String` currently contains at most 8191 bytes. Its expected encoding
is one Cell with those raw bytes and no references or length prefix; the Cell
descriptor already supplies the length. Scripts, cryptographic proofs, and
other potentially longer protocol byte fields still use Snake encoding.
Replacing the VM String type with a first-class Cell is a proposal below,
not part of the implemented migration. Utreexo forest, proof, and path
serialization also remains unchanged for now.

The word **Cell** in this document always means the low-level encoding object.
It does not reintroduce the old FlameVM UTXO `Cell` type, which has been renamed
to `Contract`.

### Relationship to `compression.md`

This design replaces several provisional mechanisms in
`docs/compression.md`:

- Cell graphs and the radix-4 Trie replace the generic `Tree`/`Link`/`Node`
  proposal.
- A stored graph's exact `BoCID` replaces a separate availability tree.
- The transaction BoC replaces request-keyed witness manifests and tables.
- Committing the complete provided bag makes an exact-used-data manifest
  unnecessary; unused bodies are allowed and paid for.
- Immediate draining of each external transaction's actor descendants replaces
  per-message witness submanifests.

The freezing, linear-value, deterministic-availability, and typed-decoding
requirements from that document still apply. Where the two documents differ,
this document is the implementation target; `compression.md` should be reduced
to rationale and cross-references as this plan lands.

## Design goals

1. One small byte-granular primitive underlies canonical encoding, object
   identity, persistent graphs, authenticated pruning, and witness transport.
2. A Cell has the same identity whether its descendants are loaded, stored, or
   pruned.
3. Missing data is resolved only from consensus-visible sources. A node's
   private cache or network access must never change execution.
4. External transactions commit the exact BoC made available to their entire
   execution, including synchronous calls and descendant asynchronous sends.
5. Cells remain a serialization mechanism. FlameVM, not the `cells` crate,
   decides whether decoded values are portable, linear, valid in a Contract,
   or valid in actor state.
6. The first format stays deliberately small. It has no TON levels, depths,
   exotic cell kinds, cache bits, optional indexes, or checksums.

## Architecture

The dependency direction is:

```text
cells
  Cell, CellID, CellRef
  CellBuilder, CellSlice, CellEncode, CellDecode
  BagOfCells
  Trie

flamechain --depends on--> flamevm --depends on--> cells
     |
     +--Utreexo only-----> merkle
```

The `cells` crate must not depend on `flamevm`, `flamechain`, `merkle`, or
`readerwriter`. Its only non-standard dependencies should be `sha2` and the
error derive crate.

Responsibilities are split as follows:

| Layer | Responsibility |
| --- | --- |
| `cells` | Cell construction, canonical records and IDs, bounded reading/writing including snake strings, BoC validation, content lookup, and generic fixed-key Trie traversal |
| `flamevm` | Cell encodings for VM values and instructions; Contract, Actor, Predicate, and Dict semantics; portability and linearity checks; execution-time Cell access |
| `flamechain` | Persistent actor Cell stores, transaction BoC commitment, per-external execution scheduling, block limits, and state commitments |
| `merkle` | The specialized Utreexo accumulator only; it is not a dependency of `cells` or `flamevm` |

There is no generic storage framework in `cells`. Persistent databases and
network retrieval remain outside the crate. The crate exposes the IDs and
bounded lookup interface those layers need.

## Cell model

### Logical structure

```rust
pub type CellID = [u8; 32];

pub enum CellRef {
    Resident(Arc<Cell>),
    Pruned(CellID),
}

pub struct Cell {
    id: CellID,
    payload: Box<[u8]>,
    refs: Box<[CellRef]>,
}
```

`Resident` and `Pruned` are runtime representations of the same reference.
They are not different Cell kinds and do not hash differently. `Pruned` means
that the child body is not attached to this in-memory object; whether it is
available in actor storage or the transaction BoC is determined by the
execution context.

The term is scoped rather than global: pruning is relative to a particular
stored graph or BoC. A body can be absent from actor storage but present in the
current transaction BoC.

Cells are immutable after construction. Resolving a `Pruned` reference returns
an `Arc<Cell>` and may populate an execution-local cache, but it does not mutate
the parent Cell or change persistent availability. Explicit pruning and
persistent restoration build a new stored graph/frontier.

The core immutable API is:

```rust
impl CellRef {
    pub fn resident(cell: impl Into<Arc<Cell>>) -> Self;
    pub fn pruned(id: CellID) -> Self;
    pub fn id(&self) -> CellID;
    pub fn as_resident(&self) -> Option<&Cell>;
}

impl Cell {
    pub fn new(payload: Vec<u8>, refs: Vec<CellRef>) -> Result<Self, CellError>;
    pub fn id(&self) -> CellID;
    pub fn payload(&self) -> &[u8];
    pub fn refs(&self) -> &[CellRef];
    pub fn into_parts(self) -> (Vec<u8>, Vec<CellRef>);
    pub fn encoded_size(&self) -> usize;
}
```

`Cell` and `Arc<Cell>` implement conversion into `CellRef`: `cell.into()`
creates a resident reference. Moving a `Cell` preserves its payload/reference
allocations; converting an `Arc<Cell>` reuses that allocation without copying
the body.

There is no mutating `hydrate` method. Hydration belongs to the resolver cache,
while explicit pruning returns/rebuilds a `Pruned` reference.

### Limits

```text
payload length: 0..=8191 bytes
reference count: 0..=4
maximum Cell record: 2 + 8191 + 4*32 = 8321 bytes
```

The four references are ordered. Reordering them changes the Cell ID.
Duplicate references are valid because a type may use the same child in two
positions.

### Canonical Cell record

The compact descriptor is:

```text
descriptor: u16 little-endian
  bits  0..12: payload length
  bits 13..15: reference count

record:
  descriptor                         2 bytes
  payload                            payload_length bytes
  child CellIDs, in reference order  32 * reference_count bytes
```

The descriptor is computed as:

```text
descriptor = payload_length | (reference_count << 13)
```

Reference counts 5, 6, and 7 are invalid, even though the descriptor can
represent them. A record contains child IDs, never child bodies or loading
metadata, and is therefore self-delimiting.

### Identity

```text
CellID = SHA256(canonical_cell_record)
```

This is one ordinary SHA-256 invocation: no transcript, namespace, prefix,
double hash, or out-of-band type label. The descriptor and fixed-width child
IDs make the record unambiguous and self-delimiting. Encoding changes are
consensus changes; the hash layer has no separate version.
Consequently identical canonical Cell bytes always have the same ID regardless
of which higher-level type refers to them.

Child IDs make the identity recursive without making record decoding
recursive. A parent can be validated before any child body is available.

The operation or containing type that reads a Cell already determines which
decoder to invoke. An ordinary typed root therefore begins directly with its
fields:

```text
typed root payload = fields...
typed object ID    = root CellID
```

There is no global type-tag registry and no per-type schema version. A
discriminant is encoded only when the expected type is a sum type and its
variants are not already unambiguous from their required fields or reference
shape. `flamevm::Value` and `TxEntry` need discriminants; an optional reference
can use zero-versus-one references without another byte. A discriminant is not
a namespace for the hash. Any future encoding change is activated by the outer
consensus protocol rather than dormant version fields repeated throughout the
Cell graph.

## Builder and Slice API

The API follows the useful shape of ton-swift's `Builder`, `Slice`, and
`CellCodable`, translated to whole bytes. Here, **streaming** means incremental
construction and a consuming in-memory cursor. It does not mean network I/O.

```rust
pub struct CellBuilder {
    payload: Vec<u8>,
    refs: Vec<CellRef>,
}

impl CellBuilder {
    pub fn new() -> Self;
    pub fn used_bytes(&self) -> usize;
    pub fn remaining_bytes(&self) -> usize;
    pub fn used_refs(&self) -> usize;
    pub fn remaining_refs(&self) -> usize;

    pub fn store_u8(&mut self, value: u8) -> Result<&mut Self, CellError>;
    pub fn store_u16(&mut self, value: u16) -> Result<&mut Self, CellError>;
    pub fn store_u32(&mut self, value: u32) -> Result<&mut Self, CellError>;
    pub fn store_u64(&mut self, value: u64) -> Result<&mut Self, CellError>;
    pub fn store_bytes(&mut self, value: &[u8]) -> Result<&mut Self, CellError>;
    pub fn store_snake(&mut self, value: &[u8]) -> Result<&mut Self, CellError>;
    pub fn store_ref(&mut self, value: CellRef) -> Result<&mut Self, CellError>;
    pub fn store<T: CellEncode + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<&mut Self, CellError>;

    pub fn build(self) -> Cell;
}
```

Integers are little-endian, matching current Flame encodings. Store methods
check capacity before mutation. The generic `store` checkpoints the two vector
lengths and restores them on error, so a failed compound write is atomic.
Writing to a Cell can fail only because the payload or reference capacity is
exhausted; business validation happens before encoding. Once all stores have
succeeded, `build` is infallible.

Primitive `CellBuilder` stores never spill automatically into another Cell.
The containing type explicitly chooses `store_snake` for length-prefixed byte
strings, a Trie, or another child layout. Only `store_snake` creates continuation
Cells, under the canonical rules below.

```rust
#[derive(Clone)]
pub struct CellSlice<'a> {
    cell: &'a Cell,
    byte_offset: usize,
    ref_offset: usize,
}

impl CellSlice<'_> {
    pub fn remaining_bytes(&self) -> usize;
    pub fn remaining_refs(&self) -> usize;

    pub fn load_u8(&mut self) -> Result<u8, CellError>;
    pub fn load_u16(&mut self) -> Result<u16, CellError>;
    pub fn load_u32(&mut self) -> Result<u32, CellError>;
    pub fn load_u64(&mut self) -> Result<u64, CellError>;
    pub fn load_bytes(&mut self, len: usize) -> Result<&[u8], CellError>;
    pub fn load_snake<R: CellResolver + ?Sized>(
        &mut self,
        cells: &mut R,
        limit: usize,
    ) -> Result<Vec<u8>, CellError>;
    pub fn load_ref(&mut self) -> Result<CellRef, CellError>;

    pub fn preload<T>(
        &self,
        f: impl FnOnce(&mut CellSlice<'_>) -> Result<T, CellError>,
    ) -> Result<T, CellError>;

    pub fn try_load<T>(
        &mut self,
        f: impl FnOnce(&mut CellSlice<'_>) -> Result<T, CellError>,
    ) -> Result<T, CellError>;

    pub fn finish(self) -> Result<(), CellError>;
}
```

Payload and references have independent cursors, as they do in TON. `preload`
parses a cloned cursor without consuming the original. `try_load` commits the
clone only on success. `finish` rejects either trailing payload bytes or
trailing references.

The core typed traits are intentionally small:

```rust
pub trait CellEncode {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError>;

    fn to_cell(&self) -> Result<Cell, CellError> { /* default */ }
}

pub trait CellDecode: Sized {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        cells: &mut R,
    ) -> Result<Self, CellError>;
}
```

`from_cell` is an exact-decoding helper: it calls `decode` and then `finish`.
Primitive payload operations do not need a resolver; composite decoders use it
when following references. There is no runtime codec factory or type registry.

All crate operations use one `CellError`. It distinguishes payload/reference
capacity, insufficient input, trailing payload, trailing references, malformed
type encoding, `MissingCell(CellID)`, and exhausted gas/resource budget. This
lets a composite decoder propagate resolution failures without converting
between unrelated reader and resolver errors. Builder methods only produce the
two capacity variants.

## Proposal: replace the VM String with a first-class Cell

This section is a design proposal, not an instruction to introduce new VM
types yet. The current `CellBuilder` and `CellSlice` remain codec helpers.
The goal is to expose their byte/reference operations to programs without
adding a second graph format or weakening the ownership rules of typed Values.

### Two alternatives

Both alternatives keep an immutable Cell as the portable, content-addressed
result. The choice is whether temporary reading and writing use one VM type
or two.

| Property | Combined read/write object | Separate Builder and Slice |
| --- | --- | --- |
| Temporary VM types | One buffer/cursor type | Builder plus Slice |
| Basic layout | Payload, refs, and independent byte/ref read cursors | Builder has payload/ref buffers; Slice has a shared Cell and two cursors |
| Reading an existing Cell | Copy its bounded body into writable buffers, or add shared/read-only storage with copy-on-write | Share its immutable body; only cursors change |
| Writing | Append bytes/refs to the same object being read | Append only to Builder |
| Reading after writing | Direct, but must define whether already-read prefixes remain | Finalize Builder, then open a Slice |
| Editing after reading | Convenient; cursor, append position, and final identity need explicit rules | Explicit new Builder; no hidden edit or cursor invalidation |
| Identity | No stable ID while writable; finalization hashes the complete result | Cell ID is always stable; Builder is unhashed; Slice still refers to its original Cell |
| Copying | Copying must copy buffers or define copy-on-write; sharing a mutable cursor is unsafe | Cell/Slice copies share immutable storage with independent cursors; Builder need not be copyable |
| Implementation | Fewer opcode type distinctions, more object states and corner cases | Closely matches the existing crate; one additional VM variant, simpler invariants |

The simplest combined object would have mutable `payload`, mutable `refs`,
`byte_offset`, and `ref_offset`. Reads advance offsets without deleting data;
writes append, never overwrite. Finalizing includes the entire buffers,
including already-read prefixes. Keeping only the unread remainder would be
a separate, explicit operation. Opening an existing Cell would copy at most
8191 payload bytes and four reference handles. Avoiding that copy requires an
additional shared-versus-writable representation and detachment on first
write; merging the public types does not eliminate that internal distinction.

Recommendation: keep **immutable Cell, separate Builder, separate Slice** for
the initial VM interface. Ordinary decoding stays zero-copy, Cell identity
never depends on a cursor, and the current crate already has the necessary
operations. Add a combined convenience interface later only if actual
contract code frequently reads and appends to the same bounded buffer.

### Proposed values and transitions

An illustrative runtime layout is:

```text
Cell value:     CellRef — resident Arc<Cell> or pruned CellID
Builder value: owned CellBuilder — payload buffer and reference buffer
Slice value:   Arc<Cell> + byte cursor + reference cursor

new builder --store bytes/refs--> builder --finalize--> immutable Cell
immutable Cell --resolve/open--> Slice --load bytes/refs--> advanced Slice
```

`CellSlice<'a>` currently borrows a Cell and therefore cannot itself live on
the VM stack independently of that borrow. A VM Slice would own an `Arc<Cell>`
and its offsets; it should not use self-referential pointers or unsafe lifetime
extensions. Offset updates can reuse the existing checked parsing logic.
Small cursor fields fit the established payload/ref limits; the canonical
encoding has no cursor fields.

Payload and reference cursors remain independent. Reading bytes does not
implicitly consume refs. Reading a child reference yields another Cell value;
opening that child resolves its body using the *current* execution context.
It neither transfers the parent's cursor nor captures the sender's actor
storage authority. Missing permitted witness data has the existing hard
failure/rollback behavior. Copying a pruned reference does not assert that its
body is available.

Finalization consumes the Builder and yields a new immutable Cell. Failed
stores leave the Builder unchanged. Failed reads leave both Slice offsets
unchanged, but work already charged is not refunded. `finish` checks exact
consumption of both streams; it is distinct from simply dropping a cursor.
There is no implicit Builder-to-Slice conversion and no automatic spill on
ordinary stores. Explicit Snake operations remain available for schemas that
actually require a byte chain.

### Ownership and portability

| Proposed Value | Copyable | Droppable | Portable |
| --- | --- | --- | --- |
| Raw immutable Cell/reference | Yes; share the graph, not its bodies | Yes | Yes |
| Slice | Yes; independent offsets over shared immutable data | Yes | No; transient execution state |
| Builder | No initially; moves avoid implicit buffer copies | Yes | No; finalize before crossing a storage/call boundary |

These are plain binary-data capabilities, **not ownership of whatever their
bytes might encode**. A copyable raw Cell can contain the serialization of a
Token, Contract, or actor record without granting permission to instantiate
that linear object. Repeatedly parsing those bytes must not create assets.

Consequently the initial interface should expose primitive byte/ref reads,
not an unrestricted `Cell -> Value` decoder. `input` still authenticates and
consumes an existing ContractID; actor `load` still checks out that actor's
typed state; Dict operations still transfer owned typed values. Only these
business-logic boundaries may decode linear types. Likewise there must be no
generic `Token -> copyable Cell` conversion that consumes a live token and
forgets its ownership. Debug serialization is not a transferable asset claim.

Dropping a Slice or Builder therefore discards only a cursor or plain-data
draft. This does not relax the non-droppability of Tokens, Contracts, or typed
Dicts containing them. Slice/Builder Values inserted into a Dict would clear
its sticky portability flag, just like other transient non-portable Values.
They may return upward through a synchronous call, but may not be arguments
moving downward or cross an asynchronous send boundary.

### Bytes, identity, and gas

Existing byte-oriented instructions need an explicit rule when their operand
can have references. Initially, operations such as signatures, fixed-size IDs,
byte hashing, and concatenation should require a **zero-reference Cell** of
the appropriate payload length. They must not silently ignore child refs or
flatten an arbitrary graph. Byte concatenation fails if its result exceeds
8191 bytes; storing a child reference is a different operation.

`CellID` hashes the complete canonical record, including child IDs. It is not
the same operation as SHA256 of payload bytes. Advancing a Slice changes
neither its source Cell nor its ID; computing a commitment to the unread tail
requires explicitly constructing a new Cell. Typed program loading should
continue to use the current program/Snake schema, not reinterpret an arbitrary
Cell graph as concatenated bytecode.

Charge before allocation, copying, hashing, or path rebuilding: bytes appended
or copied, refs stored, Cell finalization, and every logical child resolution.
Opening or copying a Slice shares its body and charges only cursor/reference
work plus the standard logical resolution charge. Any explicit conversion
from an existing Cell into a writable Builder charges for copying its bounded
payload and refs. A cached Cell ID is cheap to read; finalizing new content
pays for its hash. Physical residency, private prover metadata, and cache hits
must not change consensus gas or which bodies are visible.

### Minimal migration sequence

1. Agree on the separate-type interface and byte/ref semantics above. Specify
   the primitive stack transitions, capacities, error results, and capability
   table before assigning opcode numbers. Keep String unchanged meanwhile.
2. Replace the plain byte-string VM variant with immutable Cell/reference.
   Preserve the existing zero-ref raw-byte encoding as that subset. Decide
   deliberately whether its existing Value discriminant can be reused in the
   coordinated consensus activation; do not add a schema version. Keep prover
   witness metadata outside the public Cell identity.
3. Add transient Builder and owned Slice Values with only the existing
   byte/ref operations, explicit finalize/open, independent cursors, and exact
   finish. Do not add editing, seeking, graph flattening, automatic spilling,
   or generic typed-value decoding in this first step.
4. Migrate byte-oriented opcodes to explicit zero-ref checks; migrate program,
   predicate, Contract-input, and actor readers through their expected typed
   Cell schemas and scoped resolver. Preserve all linear-domain gates.
5. Add gas and conformance tests: zero/max capacity, fifth ref, pruned child,
   cursor-copy independence, atomic failure, discarded drafts, stable identity,
   partial-witness reads, call rollback, prover/verifier equality, and attempts
   to manufacture or duplicate linear values through raw bytes. Only then
   remove the obsolete String API and update the VM specification.

This proposal does not change the current Cell record, BoC, Trie, actor
availability commitments, or Utreexo encoding. It changes the VM-facing data
and execution interface; those changes require their own reviewed activation.

## Snake encoding

Snake is a length-prefixed byte-string encoding operated directly by
`CellBuilder::store_snake` and `CellSlice::load_snake`, not a separate type or
buffering writer. It uses a linear chain inspired by TON, but requires full
nonterminal segments and records the total byte length.

The exact layout is:

1. A four-byte little-endian `u32` byte length, wholly within the current
   Cell's remaining payload. Insufficient prefix space, or a length above
   `u32::MAX`, returns `PayloadCapacity` without changing the builder.
2. String bytes occupy the available parent payload after the prefix. If they
   overflow, the parent payload must be exactly 8191 bytes and its next
   reference in serialization order points to the continuation. A missing
   builder reference slot returns `ReferenceCapacity` without mutation.
3. Continuation Cells belong only to this string. Each nonterminal continuation
   has exactly 8191 payload bytes and one reference at index zero; the terminal
   has exactly the remaining bytes and no references.
4. Stop when the declared length is satisfied. Empty strings and exact fits
   create/consume no continuation reference and no empty sentinel.

The parent reference is the reader's next unread reference, not necessarily
reference zero. Descent is local to this operation: the public builder/slice
stays on the parent, preserving its later references. When a string fits
inline, later payload fields are also available; after overflow, the parent's
payload is full, although it may still have reference slots.

Examples starting in an empty builder (`len` is the four-byte prefix):

```text
0 bytes:       [len=0]
8187 bytes:    [len=8187 | 8187 bytes]
8188 bytes:    [len=8188 | 8187 bytes] -> [1 byte]
16378 bytes:   [len=16378 | 8187 bytes] -> [8191 bytes]
16379 bytes:   [len=16379 | 8187 bytes] -> [8191 bytes] -> [1 byte]
```

A parent with other references can encode a 9000-byte string as:

```text
parent payload: [28 23 00 00 | 8187 string bytes]
parent refs:    [earlier object, continuation, later object]
                                     |
                                     v
                         [813 string bytes; no refs]
```

After loading the earlier reference and the string, the next `load_ref` returns
the later object. The length prefix has no type tag, version, or terminator.

```rust
builder.store_snake(bytes)?;
builder.store_ref(later_object)?;

let bytes = slice.load_snake(cells, max_length)?;
let later_object = slice.load_ref()?;
```

Writing copies input directly into the final payloads and constructs only the
overflow chain, iteratively from tail to head; it never stages a whole-string
buffer. Reading checks the declared length against the caller's limit before
allocation or resolution, then grows the result only from validated data.
It follows continuation refs through `CellResolver`, checking each resolved
body's identity and exact payload/reference shape. Short nonterminals, extra
continuation refs, incorrectly sized terminal payloads, missing bodies, and
limit overruns fail. Each continuation makes positive progress toward the
bounded length; no recursive walk or separate snake cycle set is needed.

A failed read leaves the parent cursor unchanged, but resolver charges and
read-only cache entries do not roll back. The successful result is a `Vec<u8>`.
Snake is used for program/proof blobs and other explicitly unbounded byte
fields, not for the bounded single-Cell VM String.

## Trie

The existing radix-4 Patricia Trie moves into `cells` and uses Cells directly.
Keys have one configured whole-byte width and are interpreted as 2-bit digits,
most-significant digit first.

Each trie node is an ordinary Cell:

```text
payload:
  child_mask: u8
  label_len:  u16 big-endian, measured in 2-bit digits
  label:      four 2-bit digits per byte, zero-padded

references:
  leaf:   one reference to the value Cell
  branch: two to four child references
```

`child_mask == 0` denotes a leaf. Otherwise the low four bits identify child
selectors `0..3`, and references occur in ascending selector order. Branches
with zero or one child are non-canonical; Patricia compression merges them into
the path label. `label_len` handles prefixes that are not byte-aligned without
creating intermediate Cells.

The wrapper remains small:

```rust
pub struct Trie {
    key_bytes: usize,
    root: Option<CellRef>,
}
```

`Trie::from_cell(root, key_bytes)` accepts `impl Into<CellRef>`: a resident or
pruned `CellRef`, a `Cell`, or an `Arc<Cell>`. It checks the configured key width
but does not load or inspect the root. Every visited node, including the root,
is resolved and its child mask and compressed path label validated on access,
regardless of residency. One constructor therefore supports a wholly pruned
Dict without extra pruned/resident bookkeeping in Dict.

Key width is a separate constant chosen by the owning schema. There is no
entry count in a raw Trie node, and lookup, insertion, deletion, and ordered
navigation do not need one. `Trie::new(key_bytes)` creates an empty Trie with
no root; `into_root()` unwraps the Trie to its raw `Option<CellRef>`.

Counts belong to the owning type when needed. Dict retains its O(1) entry count
in memory and commits it in its existing envelope, updating it atomically with
successful mutations. TxLog and block sequence envelopes likewise keep their
counts. `entries_exact(count, resolver)` validates the leaf count on full
import, stopping at the first extra leaf without preallocating from the claimed
count. Lazy Dict imports rely on prior admission of this metadata. Ordinary
`entries(resolver)` enumerates without a count; a metered resolver must bound
traversal of an untrusted graph. No extra root Cell or count field is added to
the Trie encoding.

`Trie::lookup(root, key, resolver)` needs no cached count or envelope. Its key
width comes from the key supplied by the owning schema, and it validates node
shape and content identity along the requested path. Predicate branch opening
uses this API: the Taproot commitment is directly to the raw eight-byte-key
Trie root, not an intermediate count Cell. Membership lookup does not need to
assert the total size or validate unvisited siblings.

`get`, `insert`, and `remove` take a `CellResolver`. They resolve only the path
being traversed. Mutations rebuild the affected path and swap the root only
after the operation succeeds, so `MissingCell` and malformed-node errors leave
the Trie unchanged. Resolver-backed `get_ref` and `get` return an owned
`CellRef` and `Arc<Cell>` respectively; they cannot return borrows tied to a
temporary resolution cache.

There is no separate Merkle path object. The committed root `CellID`
authenticates the root body; each verified body contains the `CellID` of the
next child. Loading and hashing the Cells on the requested path therefore
authenticates the leaf and its position. Sibling subtrees remain as IDs in
their parent Cells and do not need sibling-hash vectors or the `merkle` crate.

The low-level Trie knows nothing about Scalar, VM Values, portability, or
linear types. FlameVM has one `Dict` over this Trie. It reverses the canonical
32-byte little-endian Scalar bytes to obtain big-endian key paths, so bytewise
Trie order agrees with unsigned numeric order. There is no separate Dict2,
small-dictionary representation, or list-mode encoding.

The expected Dict encoding has no repeated type tag or key width: payload
`count:u64 LE | flags:u8`, plus an optional Trie-root reference. The flags
are sticky `portable` (bit 0) and `droppable` (bit 1). An empty Dict has no
root and is droppable even when its stored droppable bit is false. Pruning
does not reset either flag. Generic imports validate summaries/counts; reads
of authenticated prior state may leave unrequested branches pruned.

Here "trusted" is about the previously validated count/capability summaries,
not a trusted sender and not permission to skip hashes or parsing checks.
`Contract::from_trusted_cell` contrasts with ordinary `CellDecode`, which walks
the complete typed payload; chain membership checks must still establish that
an input Contract actually exists. A Cell hash alone grants no linear ownership.


## Bag of Cells

### Purpose

A `BagOfCells` is a canonical, bounded lookup table from `CellID` to Cell body.
Unlike TON's general graph transport, Flame's first BoC has one job: carry a
set of bodies that may satisfy otherwise unloaded references.

It is deliberately rootless. Contracts, actor records, Predicate fields,
typed object envelopes, and Trie wrappers already carry their root IDs. A
helper that transports one standalone object returns the pair `(root_id,
bag_of_cells)` rather than putting another root table inside the bag.

### Canonical format

```text
cell_count:  u32 little-endian      4 bytes
records:     canonical Cell records, strictly sorted by computed CellID
```

Cell records do not repeat their own IDs: the decoder computes each ID from
the record. The descriptor makes every record self-delimiting. Identical Cells
are emitted once.

```text
BoCID = SHA256(canonical_boc_bytes)
```

This is also direct SHA-256 with no hash namespace. The caller already expects
a BoC, and its count plus sorted self-delimiting records form an unambiguous
exact-set preimage; a magic tag and dormant format version would add no
information.

The outer protocol supplies the BoC byte length. The decoder rejects trailing
bytes, invalid Cell descriptors, duplicate IDs, non-increasing record order,
cycles among included bodies, and any caller-provided bound violation.

BoC decoding receives the existing outer witness-byte bound and the
transaction gas meter:

```rust
pub trait GasMeter {
    fn charge(&mut self, amount: u64) -> Result<(), CellError>;
}

impl BagOfCells {
    pub fn decode(
        bytes: &[u8],
        max_bytes: usize,
        gas: &mut impl GasMeter,
    ) -> Result<Self, CellError>;
}
```

`GasMeter` is the minimal charge interface defined by `cells`; FlameVM adapts
its existing per-execution gas counter to it. It is not a memory limit or a
second resource budget. Per-Cell, per-byte, and per-reference rates belong to
the consensus gas schedule and are independent of the storage source.
The VM charges the same rates in external and internal execution; any 4x
internal-work weighting remains a transaction-ordering policy, not a Cell gas
rule.

The decoder validates `max_bytes`, then charges the declared Cell count before
allocating its lookup table. It also charges record bytes, references, cycle
validation, and resolution work. This prevents a small bag of two-byte empty
Cells from causing disproportionate allocation without introducing a separate
memory-limit knob. Total references are already bounded by four times the Cell
count. Traversals are iterative and terminate on exhausted gas. BoC validation
gas is deducted from the initiating external transaction's gas; later Cell
resolutions are charged to the VM execution or message that performs them.

### Pruned Cells

If a Cell references ID `X` and the current bag has no record whose computed
ID is `X`, that branch is pruned in this bag. No placeholder record, exotic
Cell tag, depth, proof level, or pruned bit is needed.

```text
parent record contains X

BoC contains body X     -> X can be resolved
BoC omits body X        -> X remains pruned
```

The same rule applies to a root ID held outside the bag: if its body is absent,
the root itself is pruned.

Adding body `X` later does not change the parent or any ancestor ID. A bag may
contain bodies not reachable from one particular root because a transaction's
bag can serve several Contracts and Actors. Unused bodies are allowed; they
are still committed and paid for. An exact-used-witness manifest is therefore
not needed.

The initial format stores 32-byte child IDs in every record rather than TON's
compact table indexes. With payloads up to 8191 bytes, the simpler single
record format is the better starting trade-off. Indexed reference compression
should be considered only after measurements show that child IDs dominate real
transactions.

### Minimal API

```rust
pub struct BagOfCells { /* sorted CellID -> Arc<Cell> */ }

pub type BoCID = [u8; 32];

impl BagOfCells {
    pub fn new() -> Self;
    pub fn collect(root: Arc<Cell>) -> Result<Self, CellError>;
    pub fn insert(&mut self, cell: Arc<Cell>) -> Result<(), CellError>;
    pub fn get(&self, id: &CellID) -> Option<Arc<Cell>>;
    pub fn contains(&self, id: &CellID) -> bool;
    pub fn id(&self) -> BoCID;
    pub fn encode(&self) -> Vec<u8>;
    pub fn decode(
        bytes: &[u8],
        max_bytes: usize,
        gas: &mut impl GasMeter,
    ) -> Result<Self, CellError>;
}
```

`collect` follows only attached `Resident` references and deduplicates them by
ID. A `Pruned` reference remains absent. BoC decoding creates Cells with
`Pruned` references and keeps resolved bodies in the bag's lookup table; it
need not rebuild the whole DAG eagerly. Construction and insertion reject a
count that does not fit the wire `u32`, so encoding an already valid bag to a
`Vec` and computing its ID are infallible.

### Root framing

A rootless bag still needs a root when transporting one typed object. The
canonical bootstrap wrapper is:

```rust
pub struct CellEnvelope {
    root: CellID,
    cells: BagOfCells,
}

impl CellEnvelope {
    pub fn new(root: CellID, cells: BagOfCells) -> Result<Self, CellError>;
    pub fn root(&self) -> CellID;
    pub fn cells(&self) -> &BagOfCells;
    pub fn encode(&self) -> Vec<u8>;
    pub fn decode(
        bytes: &[u8],
        max_bytes: usize,
        gas: &mut impl GasMeter,
    ) -> Result<Self, CellError>;
}
```

```text
CellEnvelope bytes = root CellID || canonical BagOfCells bytes
```

The enclosing network framing or containing type supplies the envelope byte
boundary.
Decoding a complete object requires the root body to be present in `cells` and
then runs the expected typed decoder. This small wrapper is sufficient to
replace the old flat top-level codecs; the BoC itself remains reusable as a
rootless witness lookup set.

An external transaction has two distinct graphs:

1. Its own canonical `CellEnvelope`, which identifies and transports the
   ExternalTx object.
2. Exactly one **execution BoC**, whose canonical bytes are a length-prefixed
   snake field in a Cell referenced by the ExternalTx root.

The execution BoC excludes Cells used solely to encode the ExternalTx
envelope. Its `BoCID` is stored in the root payload and checked against the
decoded bytes. Only this nested execution BoC is exposed to Contract and Actor
resolution. This avoids a circular attempt to place a BoC commitment inside a
root that is itself a member of the same BoC.

## Resolution and witness context

### Allowed sources

Every Cell access occurs through an execution-scoped resolver:

```rust
pub trait CellResolver {
    fn resolve(&mut self, reference: &CellRef) -> Result<Arc<Cell>, CellError>;
}
```

The concrete VM resolver may use exactly these sources:

1. a body already attached to the value in RAM;
2. a body present in the current Contract or Actor's consensus-committed
   persistent Cell store;
3. a body present in the initiating external transaction's committed BoC.

It must not use an unrelated actor's store, a node-wide content cache, an
archive, a database record not committed as resident for the current object,
or the network. A physical cache may avoid decoding a body again only after
the resolver has established that the ID belongs to source 2 or 3.

The current `ExecutionCells` implementation already combines attached bodies,
the current actor's retained `BagOfCells`, and the external transaction's BoC,
in that order. `ActorStore` is currently RAM-backed. There is no disk backend
or general-purpose resolver-composition adapter yet. The `CellResolver` trait
allows a composite implementation to consult actor-scoped disk storage and
then the transaction BoC. Such a fallback must continue only on a missing
requested ID, not suppress integrity, resource-limit, or storage errors. It
must charge logical access once, regardless of which source supplies the body,
and disk lookup must enforce the same committed-residency scope as RAM lookup.

### Crossing ownership domains

Persistent-store authority does not travel with a value. While actor A is
running, its own state may resolve from A's committed store. If A passes a
String or Dict into actor B, only recursively attached `Resident` Cells travel
with the value. A remaining `Pruned` reference may resolve from the external
transaction BoC (or from B's own store if B independently retained that exact
Cell), but never from A's store merely because A originated the argument.

The same rule applies to synchronous calls and asynchronous sends. A Message's
collected Cell graph includes all resident Cell bodies carried by its portable
payload; unresolved descendants remain IDs and require the shared execution
BoC. On return, the caller's own store scope is restored and returned resident
Cells travel with the returned value. This permits non-portable values to move
up the synchronous call chain without granting a callee access to caller
storage.

Cell encoding never bypasses the existing direction rules for linear types:
asynchronous sends and arguments moving down a call chain must be portable;
returns moving up the synchronous call chain may carry non-portable borrowed
values. A typed Dict keeps its authenticated sticky `portable` flag even when
some branches are pruned, so portability is never recomputed from only the
currently loaded subset.

Lookup order cannot affect the decoded value because every source is verified
against the same CellID. The initial gas rule charges every logical resolution
from the canonical Cell size, including a physical cache hit. It can be refined
later, but consensus cost must never depend on wall-clock cache behavior.

`MissingCell(CellID)` means no permitted source contains the requested body.
A malformed generic Cell record invalidates the BoC before VM execution. A
valid Cell whose payload is invalid for the type expected by the caller fails
at that typed operation; `cells` does not know the business meaning.

### Atomic access and linear values

Any helper or opcode that can discover a missing or malformed Cell must be
transactional with respect to VM values and effects. It first resolves and
typed-decodes through cloned `CellSlice`s or a call/actor checkpoint, then
commits stack pops, linear moves, Trie-root replacement, and effects only after
all required checks succeed.

In particular:

- Contract opening, Predicate selection, Dict access/mutation, and
  snake reads must not consume a linear argument before a possible
  Cell failure.
- A failed synchronous call returns exactly the caller's `k` arguments and
  rolls back callee effects.
- `send` validates portability and captures its payload before emitting the
  Message; a later internal failure follows the normal bounce path and returns
  that owned payload.
- Resolver cache entries are monotonic read-only facts and need not roll back,
  because they cannot change Cell identity or persistent availability.

### Prover and verifier

The same resolver boundary supports discovery and consensus execution:

- An optional recording resolver reads from the submitter's full local graph
  and inserts every externally needed body it loads into a candidate
  `BagOfCells`.
- A verifier resolver reads from committed actor/Contract storage and the
  immutable BoC carried by the external transaction.

The candidate BoC must be frozen **before the proof-producing execution**, not
merely before signing. `Prover::prove` derives the TxLog/TxID and binds TxID
into the R1CS transcript, so learning `BoCID` during that same run would be
circular. Transaction construction is therefore:

1. assemble a conservative BoC directly, or run an optional discovery pass;
2. freeze the canonical bag and its `BoCID`;
3. run the real prover against that frozen bag using verifier-equivalent
   lookup rules;
4. produce signing instructions from the resulting TxID.

Current external proving does not execute descendant actor messages. A wallet
that wants to discover their Cells must simulate the whole chain execution
closure against the relevant state, or include a conservative superset.
Otherwise a descendant that later reaches missing data deterministically
bounces. Repeated access to one ID reuses the execution cache and does not
duplicate the record.

### Persistent actor storage

Actor storage must commit both content and availability:

```rust
pub struct StoredGraph {
    pub root: CellID,
    pub cells: BagOfCells,
}

// Actor commitment includes (stored.root, stored.cells.id()).
```

The actor-state commitment includes both fields. The root says *what* the
state is; the BoCID says *which bodies remain resident and rent-bearing*.
Pruning a branch changes the stored BoC and storage charge but not the content
root. A fully frozen actor can retain its code/state root IDs while retaining
no corresponding bodies.

Resolving a body from a transaction BoC only puts it in the execution cache.
It does not silently add it to actor storage or increase rent. A persistent
restore must be an explicit high-level operation. New or modified Trie paths
are resident because the actor created their Cell bodies; untouched pruned
siblings remain pruned.

Cells do not restrict their payloads to portable data. When a typed Cell is
decoded into a VM Value, the boundary that consumes it performs the same
portability and linearity checks it would perform for a fully resident value.
This permits frozen actor state to commit linear values without retiring them
and prevents those values from crossing a boundary that forbids them.

## VM-visible uses

In the implemented interface, Cells stay below the FlameVM Value layer:

| Use | Root held by | Body source when accessed |
| --- | --- | --- |
| Contract | `ContractID` / UTXO input | External transaction BoC |
| Actor code and state | Actor registry record | Actor's stored BoC, then external transaction BoC for pruned bodies |
| Predicate program branch | Predicate commitment | External transaction BoC |
| Dict | Typed Dict wrapper with key width, length, and Trie root | Current resident graph/store, then external transaction BoC |
| Bounded VM String | Its zero-ref raw-byte Cell | Current resident graph/store, then external transaction BoC |
| Snake-encoded bytes | Typed program/protocol-proof/blob field | Current resident graph/store, then external transaction BoC |
| Utreexo proof data | Existing Utreexo format, unchanged | Explicit legacy proof input; specialized accumulator verification applies; Cell migration deferred |

Current opcodes do not load an arbitrary Cell Value. They perform a typed action
such as opening a Contract, reading a Dict key, or executing a Predicate
branch; that implementation follows Cells through the current resolver.

## Transaction commitment and actor scheduling

### Committing availability

Each external transaction carries one immutable execution `BagOfCells`. Its
`BoCID` must be included in the transaction's canonical effect prefix and
therefore in the `TxID` and every `signtx` signature. A concrete migration is a
mandatory `TxEntry::CellWitness(BoCID)` immediately after `TxEntry::Header` for
external execution. The full canonical BoC remains in the external transaction
envelope.

The current `BlockTx::witness_hash` is not enough: it is a block-level
commitment assembled by the minter, while Cell availability must already be
bound to the submitted transaction.

Because the BoC encoding is canonical, removing, adding, or changing a body
changes `BoCID`. Consensus execution must expose every body in that committed
bag to resolution. A minter cannot choose to ignore a present body and obtain
a different branch result.

Unused bodies are permitted. They increase transaction bytes and fees but do
not change execution unless accessed. This is simpler than committing the
exact data-dependent sequence of loads.

### One execution closure per external transaction

Block execution is:

```text
for each external transaction in block order:
    validate its BoC and create one Cell execution context
    execute and apply the external transaction
    enqueue its Send effects

    while that queue is not empty:
        execute the next actor message with the same Cell context
        apply success or the normal failure/bounce result
        enqueue descendant Send effects into the same queue

    discard the Cell context

perform block-boundary actor maintenance
```

Synchronous calls naturally borrow the same context. Messages do not carry or
copy a BoC. Every asynchronous descendant is processed before the next
external transaction and therefore has exactly one unambiguous witness source.
The per-external queue is FIFO in Send-effect order. BoCs are never coalesced
at block scope.

The existing block-global gas, multiplication, and message-count limits remain
outside the loop. Each BoC is bounded by the existing witness-byte limit and
gas-charged decoding/resolution. An infinite send chain or Cell walk is
therefore bounded by consensus costs and limits.

Failure behavior follows the existing execution boundary:

- missing data in the external root makes that external transaction invalid;
- missing data in a synchronous call returns the call's failure result and
  arguments according to the call rollback rules;
- missing data in an asynchronous message produces the ordinary failed-message
  bounce;
- effects and actor mutations from a failed scope roll back, while the shared
  read-only BoC and its cache remain unchanged.

## Universal Flame encoding and hashing

The end state is:

1. Every consensus type has one expected `CellEncode`/`CellDecode` layout.
2. Every content object's identity is derived from its canonical root Cell.
3. Unbounded protocol byte fields use length-prefixed Snake encoding; keyed
   collections use `Trie`. VM String uses one zero-ref Cell with raw bytes;
   fixed and small fields stay in the parent payload or explicit child Cells.
4. A standalone graph is transported as a `CellEnvelope`; persistent types
   commit a root `CellID` and the `BoCID` of their retained bodies.
5. Exact encoded size comes from Cell records and BoC bytes, not a parallel
   `SizeWriter` pass.
6. No consensus code uses `serde` as a canonical format.
7. Cell-based VM/chain formats replace `readerwriter`; the legacy crate remains
   only for the separately deferred Utreexo forest/proof/path formats.
   `flamevm/src/encoding.rs` now contains
   only VM-owned `CellEncode`/`CellDecode` implementations and typed byte-string
   helpers. It is not a flat Reader/Writer compatibility layer.

The Cell API does not prescribe one generic list encoding. Actual types use
the smallest of the three existing structures: payload for bounded items,
snake encoding for bytes, and a fixed-width Trie for unbounded keyed or
ordinal collections. Add a generic list wrapper only if several real types
would otherwise duplicate the same layout.

The two-byte Cell record and the small BoC/CellEnvelope headers are the
bootstrap framing for this system, implemented directly inside `cells`.
Above that boundary, the migrated types are typed Cell graphs with no generic
flat Reader/Writer layer. Utreexo's existing codec boundary is an explicit
temporary exception, not a second codec for these migrated types.

Cell hashing replaces serialization-to-`Vec<u8>` followed by a separate object
hash for IDs such as Contract, Actor state/code, Message, transaction, and
block objects. The caller's context selects the root type; the Cell ID remains
the plain SHA-256 content address of that root record.

The ExternalTx envelope root identifies the submitted encoding, while `TxID`
continues to identify the typed TxLog/effect graph. Under the Cell format that
TxLog root is itself a CellID; it is not silently replaced by the ExternalTx
root.

Do not mechanically hash an enum's encoded view when the type already has a
normalized identity. In particular,
`ActorID::Constructor(code).to_hash() == ActorID::Hash(h).to_hash()` must remain
true when `h` is the canonical code-root CellID. The constructor form carries
the code graph as a preimage; the hash form carries only that same root ID.
Their field encodings differ, but their actor identity does not. Apply the same
rule to any later preimage/hash pair.

Merlin remains where it is cryptographically meaningful: signature challenge
transcripts, Bulletproof/R1CS transcripts, and other interactive protocol
domains are not serialization APIs. Their domain separators bind protocol
statements and are unrelated to Cell content addressing.

Cell-backed commitments do not use the generic `merkle` crate. Contract
Predicates, TxLogs, actor state, block witnesses/effects, Dicts, and other
ordered structures commit an explicit Cell root. A verifier authenticates an
access by resolving and hashing the Cells from that root to the requested
item; there is no separately encoded sibling path. Utreexo is the sole
exception because its dynamic accumulator update proofs have semantics beyond
loading a persistent Cell path. Its algorithms remain in `merkle`, scoped to
Flamechain's Utreexo module. Its forest, proof, and path serialization is
deliberately deferred and retains the existing format and Reader/Writer API.

This migration changes every downstream consensus hash whose preimage changes,
including ContractID, MessageID, actor code/state roots, TxID, block witness
and effects roots, and BlockHash. All formats must switch together in one
coordinated consensus activation with golden vectors; mixed old/new hashing is
not valid.

## Implementation map

| Area | Implementation |
| --- | --- |
| Primitive and streaming API | `cells/src/cell.rs`, `builder.rs`, `slice.rs`, `codec.rs`: immutable Cells, exact expected-type codecs, inline length-prefixed snake operations |
| Transport | `cells/src/boc.rs`: strict CellID ordering, duplicate rejection, bounded parsing, exact body membership, CellEnvelope |
| Trie | `cells/src/trie.rs`: one radix-4 Patricia format; resolver-backed get/insert/remove/ordered navigation; raw root wrapping/unwrapping |
| Dict | `flamevm/src/dict.rs`: one Trie index, typed runtime witness cache, sticky summaries, lazy authenticated reads; no Dict2 |
| VM codecs | `flamevm/src/encoding.rs` plus type-owned codecs: fixed scalars/points/tokens, tagged Values, single-Value Contracts, actors, messages, scripts |
| Predicates | `flamevm/src/contract.rs`: program Trie, index selector, path-only witness collection and resolver-based branch opening |
| Execution | `flamevm/src/vm.rs`: fixed BoC before proving, scoped and metered resolution, public-path validation before restoring private witnesses |
| Transactions | `flamevm/src/tx.rs`: separate transport/execution bags, CellWitness effect, TxLog Trie and claimed TxID validation |
| Actor storage | `flamechain/src/storage.rs`: content roots plus retained BoCID, leases Trie, freeze instead of bulk destruction, explicit persistence only |
| Scheduling and blocks | `flamechain/src/block.rs`: external then FIFO descendants with one bag, Cell-backed block/record commitments and exact transport decoding |
| Utreexo | `merkle` plus `flamechain/src/utreexo`: specialized accumulator and legacy forest/proof/path serialization retained; Cell migration deferred |

The old `flamevm/src/chunk.rs`, local Trie, and Dict2 are deleted.
`readerwriter/` remains only for the deferred Utreexo codec boundary.
`cells` and `flamevm` have no dependency on `merkle`.
Instruction bytecode keeps its own compact opcode operands; those bytes are
stored in script Cells, not routed through a generic serialization facade.

### Remaining extensions

- Explicit VM operations to prune selected Dict branches or persist selected
  witness bodies. Existing `save`/`setcode` are the persistence boundary.
- Database adapters and archival retrieval outside consensus execution.
- The first-class raw Cell/Builder/Slice proposal above, replacing String
  opcodes only after review. Runtime String currently has a bounded zero-ref
  Cell encoding; no standalone Snake or buffering SnakeWriter is introduced.
- Utreexo forest, proof, and path encoding. Keep its current serialization
  until that separate migration is requested.
- A more compact frozen-actor registry layout. The current implementation
  retains code/state IDs, sizes, and lease metadata rather than just one hash.
- A coordinated deployment/activation strategy for the changed consensus
  identities; the code does not provide automatic legacy-state migration.

Verification covers canonical Cell/BoC boundaries, partial Trie traversal,
sticky Dict capabilities, public/private witness separation, call-failure
escrow, execution-bag commitment, actor freeze/recovery, and immediate
per-external queue draining. Tests should be run together: these encodings
change VM, proof, actor, and block commitments as one format.

## Security and consensus invariants

- Cell and BoC decoders apply the outer byte bound and precharge declared
  element counts before allocating.
- All lengths, counts, and size sums use checked conversions/arithmetic.
- Cell identity never depends on resident/pruned state or cache contents.
- The persistent availability set is committed separately from the content
  root.
- A transaction's BoC is immutable, signed through TxID, and never merged with
  another transaction's bag.
- A present valid body is always visible; an absent body is never supplied by
  private node state.
- Generic Cell decoding never decides portability or linear ownership.
- Typed decoding is exact: trailing payload and refs are rejected.
- Cell-loading operations resolve and validate before committing linear moves
  or effects.
- Storage authority is scoped to its owner and never follows an argument into
  another Actor.
- Traversal is iterative where depth may be attacker-controlled and otherwise
  has an explicit small bound.
- Hydration is a read cache operation, not a persistent state mutation.
- Consensus gas depends on canonical work, not physical cache hits.

## Non-goals of the implemented migration

- Bit-level payload APIs.
- TON levels, depths, exotic/pruned-branch Cells, Merkle proof/update Cells,
  and library-reference Cells.
- Multiple BoC variants, optional indexes, CRC, cache bits, or reference-index
  compression.
- Implicit network/database retrieval during consensus execution.
- Implementing the proposed first-class raw Cell Value or Cell-manipulation
  opcodes before their separate design review.
- A generic storage engine or automatic persistence of hydrated data.
- A proc-macro codec framework or speculative generic collection family.

## References

- [TON cell overview and representation](https://docs.ton.org/foundations/serialization/cells)
- [TON Bag of Cells format](https://docs.ton.org/foundations/serialization/boc)
- [ton-swift Cell API](https://github.com/tonkeeper/ton-swift/blob/aacae64c40f40500a9485b6b2d8d060a4777cf72/Source/TonSwift/Cells/Cell.swift)
- [ton-swift Builder API](https://github.com/tonkeeper/ton-swift/blob/aacae64c40f40500a9485b6b2d8d060a4777cf72/Source/TonSwift/Cells/Builder.swift)
- [ton-swift Slice API](https://github.com/tonkeeper/ton-swift/blob/aacae64c40f40500a9485b6b2d8d060a4777cf72/Source/TonSwift/Cells/Slice.swift)
- [ton-swift typed Cell serialization](https://github.com/tonkeeper/ton-swift/blob/aacae64c40f40500a9485b6b2d8d060a4777cf72/Source/TonSwift/Cells/Serialization.swift)
- [ton-swift snake string encoding](https://github.com/tonkeeper/ton-swift/blob/aacae64c40f40500a9485b6b2d8d060a4777cf72/Source/TonSwift/Cells/SnakeEncoding.swift)
- [TON canonical BoC declarations](https://github.com/ton-blockchain/ton/blob/master/crypto/tl/boc.tlb)
