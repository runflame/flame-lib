# Flame Cells

Cell is the data encoding format underpinning all data structures in Flame. Each ordinary cell is a data structure that can carry 0 to 4095 bytes of binary data (called "payload") and 0 to 4 references to other cells. Nested cells form an immutable directed acyclic graph (DAG).

Design of cells in Flame is heavily inspired by cells used within TON blockchain designed by Nikolai Durov, with some important differences:

1. Flame Cells store whole bytes instead of bits.
2. Payload maximum size is considerably larger (4095 bytes vs. 1023 bits).
3. Flame Cells use TON-style significant hash levels and depths, with a 15-bit level mask and two-byte little-endian depths.
4. Cells are not first-class types exposed in the FlameVM, but instead underpin Strings, Dicts, Contracts, Actors, and canonical encodings.
5. FlameVM permits transparent loading of unloaded bodies from an externally provided data source. This is distinct from an explicit pruning record: virtualization cannot recover the omitted body.
6. Graph transport is a single root-last list with short backward reference indexes. Only ordinary and pruned Cells exist; there are no MerkleProof, MerkleUpdate, or library-reference wrappers.

## Status

The Cell crate, VM/chain codecs other than the deferred Utreexo formats,
unified Dict, witness-backed predicate and actor reads, and per-external
execution closure are implemented. Exact typed
layouts are specified in [encoding.md](encoding.md). The architecture below
also describes constraints on future disk adapters and explicit partial
pruning VM opcodes; those adapters/opcodes are not implemented.
`docs/compression.md` records the motivation and broader experiments.

The implemented transport is specified in
[Root-last graph transport](#root-last-graph-transport). It packs a
single rooted DAG using backward reference indexes and commits through the
root's highest hash. Sparse indexes and typed-object snapshots are ordinary
Cell hierarchies carried through that same transport.

The first [🎻 Cell Type Language (CTL) prototype](../cells/ctl.md) compiles
byte-oriented `.ctl` schemas into Rust Cell codecs. Its illustrative schemas
do not replace the existing VM/chain encodings or define a new transaction
layout.

The crate implements explicit pruning, significant-level hashing, and
read-only `CellView` virtualization. VM codecs continue using physical Cells
and the signed execution-witness snapshot; they do not automatically virtualize
Taproot roots or accept virtual views as typed values. Selecting logical versus
factual roots in those higher-level protocols is a separate integration step.

This format is consensus-breaking: descriptors, reference depths/summaries,
payload limits, and hash preimages change downstream identities and require
coordinated activation, not mixed decoding of the old and new formats.

Runtime `String` currently contains at most 4095 bytes. Its expected encoding
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
- A Cell-based snapshot's highest root hash commits to exact stored availability.
- The transaction witness hierarchy replaces request-keyed witness manifests and tables.
- Committing the complete provided index makes an exact-used-data manifest
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
2. Residency does not affect identity. Explicit proof pruning changes factual
   identity while preserving selected lower-level commitments.
3. Missing data is resolved only from consensus-visible sources. A node's
   private cache or network access must never change execution.
4. External transactions commit the exact Cell hierarchy made available to their entire
   execution, including synchronous calls and descendant asynchronous sends.
5. Cells remain a serialization mechanism. FlameVM, not the `cells` crate,
   decides whether decoded values are portable, linear, valid in a Contract,
   or valid in actor state.
6. Keep only ordinary and pruned Cell kinds, with absolute-level virtualization.
   There are no proof/update/library wrappers, cache bits, optional indexes,
   checksums, or per-type versions.

## Architecture

The dependency direction is:

```text
cells
  Cell, CellID, CellRef, CellCommitment, CellView
  CellBuilder, CellSlice, CellEncode, CellDecode
  CellResolver (ID lookup), CellReader (logical access)
  CellIndex
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
| `cells` | Cell construction, canonical records and IDs, bounded reading/writing including snake strings, Cell hierarchy validation, content lookup, and generic fixed-key Trie traversal |
| `flamevm` | Cell encodings for VM values and instructions; Contract, Actor, Predicate, and Dict semantics; portability and linearity checks; execution-time Cell access |
| `flamechain` | Persistent actor Cell stores, transaction witness hierarchy commitment, per-external execution scheduling, block limits, and state commitments |
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
    Unloaded(Arc<CellCommitment>),
}

pub struct Cell {
    data: Arc<CellData>,
}
```

There are two physical Cell kinds:

- **Ordinary:** payload and ordered references.
- **Pruned:** no references; retained lower-level hashes and depths stand in
  for an omitted subtree. The pruning record has its own factual hash.

`Resident` and `Unloaded` describe physical availability, not these kinds.
Both retain the same `CellCommitment`: a level mask, significant hashes, and
matching depths, including the highest factual pair. Detaching a child keeps
this summary and does not change any ancestor hash. A reference to a pruned
Cell can itself be resident or unloaded.

A bare CellID is a lookup key, not a CellRef. `resolve_cell(reader, id)` loads
and verifies its body without inventing hash/depth metadata. Every CellRef
already has a complete commitment; `commitment()` and `to_unloaded()` are
infallible. Inventing depth zero for a bare ID would make parent identity
depend on whether the child happened to be loaded.

Cells are immutable and `Cell::clone()` shares `CellData`; it does not copy the
payload or descendants. Resolving an unloaded reference returns an `Arc<Cell>`
and validates its complete summary. It does not mutate a parent, make an
explicitly pruned subtree available, or persist a fetched body into actor
storage. `into_parts()` reuses the payload/reference allocations when the body
is uniquely owned, otherwise it copies the bounded payload and reference
handles; it is not always a constant-time unwrap of shared storage.

The core immutable API is:

```rust
impl CellRef {
    pub fn resident(cell: impl Into<Arc<Cell>>) -> Self;
    pub fn to_unloaded(&self) -> Self;
    pub fn id(&self) -> CellID;
    pub fn commitment(&self) -> &CellCommitment;
    pub fn as_resident(&self) -> Option<&Cell>;
}

impl Cell {
    pub fn new(payload: Vec<u8>, refs: Vec<CellRef>) -> Result<Self, CellError>;
    pub fn from_pruned(mask: u16, hashes: Vec<CellID>, depths: Vec<u16>)
        -> Result<Self, CellError>;
    pub fn id(&self) -> CellID;
    pub fn level_mask(&self) -> u16;
    pub fn level(&self) -> u8;
    pub fn hash(&self, level: u8) -> Result<CellID, CellError>;
    pub fn depth(&self, level: u8) -> Result<u16, CellError>;
    pub fn prune(&self, level: u8) -> Result<Self, CellError>;
    pub fn virtualize(&self, level: u8) -> Result<CellView, CellError>;
    pub fn is_pruned(&self) -> bool;
    pub fn payload(&self) -> &[u8];
    pub fn refs(&self) -> &[CellRef];
    pub fn into_parts(self) -> (Vec<u8>, Vec<CellRef>);
    pub fn record_size(&self) -> usize;
    pub fn encode_record(&self) -> Vec<u8>;
    pub fn decode_record_exact(bytes: &[u8]) -> Result<Self, CellError>;
    pub fn encode(&self) -> Result<Vec<u8>, CellError>;
    pub fn encode_transport<R: CellReader + ?Sized>(
        &self, resolver: &mut R,
    ) -> Result<Vec<u8>, CellError>;
    pub fn decode_transport(
        bytes: &[u8], max_bytes: usize, gas: &mut impl GasMeter,
    ) -> Result<Self, CellError>;
}
```

`Cell` and `Arc<Cell>` implement conversion into `CellRef`: `cell.into()`
creates a resident reference. Converting an `Arc<Cell>` reuses that allocation;
both conversions preserve the shared body without copying its contents.

There is no mutating `hydrate` method. Hydration belongs to the resolver cache,
while explicit pruning creates a new Cell with a different factual identity.

### Limits

```text
ordinary payload length: 0..=4095 bytes
ordinary reference count: 0..=4
level mask: 15 bits, levels 0..=15
depth: 0..=65535, measured in child edges
pruned Cell: 1..=15 retained hash/depth pairs, no references
maximum ordinary record: 2 + 4095 + 4*(2 + 16*34) = 6281 bytes
maximum pruned record: 2 + 15*34 = 512 bytes
```

A level-zero child summary is 36 bytes: mask plus one hash and one depth.
The maximum ordinary record assumes every child has all 16 significant pairs.
An ordinary parent cannot reference a child whose depth is 65535 at any level:
adding its edge would overflow. Level nesting and tree depth are independent.

The four references are ordered. Reordering them changes the Cell ID.
Duplicate references are valid because a type may use the same child in two
positions.

### Canonical Cell record

This standalone summary record is used for storage accounting, debugging,
and independent metadata reconstruction. `encode_record` and
`decode_record_exact` operate on it. Network transport instead uses the
[indexed rooted DAG](#root-last-graph-transport); it does not repeat child
commitment summaries.

The compact descriptor is:

```text
descriptor: u16 little-endian
  bit 15 = 0: ordinary
    bits 12..14: reference count, 0..4
    bits  0..11: payload length, 0..4095
  bit 15 = 1: pruned
    bits 0..14: nonzero level mask

ordinary record:
  descriptor                         2 bytes
  payload                            payload_length bytes
  child commitment summaries        one per reference, in reference order

each child summary:
  child level mask                   2 bytes LE, bit 15 must be zero
  hashes                             32 * (popcount(mask) + 1) bytes
  depths                             2 * (popcount(mask) + 1) bytes LE

pruned record:
  descriptor = 0x8000 | mask          2 bytes LE
  retained hashes                    32 * popcount(mask) bytes
  retained depths                    2 * popcount(mask) bytes LE
```

Ordinary descriptors are computed as:

```text
descriptor = payload_length | (reference_count << 12)
```

Ordinary reference counts 5..7 are invalid; all twelve payload-length bits are
used. Pruned descriptor `0x8000` is invalid because its mask is zero. All hashes
precede all depths within each summary or pruning
record. Depths are little-endian, unlike TON's encoding.

The summaries include every significant pair, including the factual one; a
pruning record stores only the lower pairs and computes its own factual pair.
Records are self-delimiting and contain no child bodies, residency flags,
schema tags, or versions. Ordinary masks are the OR of their child masks and
are therefore derived rather than repeated in the ordinary descriptor.

This redundancy in standalone summaries permits independent record decoding and
parent hashing before child bodies arrive. When a body is resolved,
`resolve_cell` compares the whole summary, not only its final
ID. Retained hashes/depths are claims until authenticated against the expected
root; serialization alone does not prove them.

### Significant levels

Level zero always exists. Mask bit `i` marks another significant hash at level
`i + 1`. A query in a gap uses the preceding significant pair; queries above a
Cell's highest level, but no higher than 15, use its highest pair.

For example, mask `0b101` has significant levels 0, 1, and 3:

```text
query level:   0    1    2    3..15
selected pair: 0    1    1    2
```

An ordinary Cell derives its mask by OR-ing its children. At each significant
level its depth is zero if it has no children, otherwise one plus the maximum
child depth at that level. A pruned Cell retains lower depths; its own highest
depth is zero because its physical record has no references. A mask is not a
history of pruning passes: thinning one proof can repeatedly use the same
level without adding another bit.

### Identity

```text
CellID = highest significant hash = cell.hash(cell.level())
```

**Wire records and hash preimages are different.** Ordinary records carry all
child summaries; an ordinary hash selects one child pair per level. Do not
compute an ordinary `CellID` by hashing either `encode_record()` or the graph
transport returned by `encode()`, even at level zero.

For each significant ordinary level `l`, in ascending order:

```text
applied_mask = mask & ((1 << l) - 1)
data = original payload at level 0; previous significant hash otherwise

H(l) = SHA256(
    ordinary_descriptor:u16 LE
    || applied_mask:u16 LE
    || data
    || each child depth(l):u16 LE, in reference order
    || each child hash(l):32 bytes, in reference order
)
```

The ordinary descriptor always encodes the original payload length, even when
`data` is the previous 32-byte hash. At level zero the applied mask is zero.
`hash_preimage(l)` exposes the exact bytes for debugging; a gap selects the
same preimage as its preceding significant level.

A pruned Cell's highest hash is `SHA256(pruned_record)`; lower hashes are its
retained claims and have no available preimage. Its highest depth is zero.
There are no transcript domains, extra namespaced prefixes, double hashes, or
schema versions. The kind/descriptor and applied mask are structural encoding,
not application-specific hash namespaces. Identical canonical physical Cell
records have identical IDs regardless of the higher-level type using them.

Residency never changes these hashes. Explicit pruning preserves selected
lower hashes while changing the factual hash; callers must specify which
commitment their protocol expects.

### Pruning and virtualization

`cell.prune(level)` replaces a whole subtree with a pruning record preserving
its commitments through `level - 1`. It accepts levels 1..15 no lower than the
source's factual level. Its new mask is the source mask truncated below that
level, with bit `level - 1` set. Retained pairs come from the corresponding
source levels. To prune selected descendants, rebuild the ordinary ancestors
with those replacements. Choose one target level for the whole proof:

- Continue thinning a level-1 proof using `prune(1)`; preserve its level-zero
  target, not the old proof's factual identity.
- Prove that level-1 proof as factual data using `prune(2)` for new omissions;
  preserve its level-1 target. Retained old pruning records are unchanged.

`virtualize(level)` creates a cheap, read-only `CellView` capped at the absolute
level requested. It accepts zero through the current Cell or view level;
virtualizing an existing view can only lower its cap. It neither decrements
every node nor resets masks through a wrapper. Child views inherit the cap;
lower-level children naturally alias their highest pair.

```rust
let view = cell.virtualize(level)?;
let logical_id = view.id();
let payload = view.payload()?;
let child = view.reference(index, resolver)?;
```

Views expose checked payload/reference access, not a raw underlying Cell.
`reference` resolves using the physical factual ID, checks all committed
pairs, then returns another view with the inherited cap. If a pruning record's
physical level exceeds the cap, its hash and depth are known but its contents
are unavailable: payload/reference access returns `PrunedCell`. At its factual
level that record is inspectable as a pruning record, not as the hidden
application payload. A physical `Cell::payload()` intentionally exposes raw
stored bytes for inspection; `CellSlice` rejects pruning records as ordinary
data. Virtual views have no physical serialization method.

For a transaction containing a level-1 pruned Taproot tree, an outer block proof
can introduce level-2 omissions. Its level-1 view preserves the transaction's
factual commitment and leaves the old level-1 pruning records intact. The
Taproot consumer can separately request level zero. This needs no wrapper
Cell, but the business protocol must explicitly select the expected level.
Virtualization never restores an omitted body, and another resolver cannot
silently substitute the hidden subtree for the pruning record.

### Expected typed encodings

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
Writing checks payload/reference capacity, requires complete reference
metadata, and rejects child depths that would overflow in the new parent.
Snake construction checks its continuation depth before allocation. These are
structural checks, not portability or business validation. Once all stores
have succeeded, `build` is infallible.

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
    pub fn load_snake<R: CellReader + ?Sized>(
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
trailing references. Reading bytes or refs, or finishing, on an explicit
pruning record returns `PrunedCell`; retained hashes must not be parsed as
application data. `CellSlice` is over a physical Cell, not a `CellView`.

The core typed traits are intentionally small:

```rust
pub trait CellEncode {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError>;

    fn to_cell(&self) -> Result<Cell, CellError> { /* default */ }
}

pub trait CellDecode: Sized {
    fn decode<R: CellReader + ?Sized>(
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
type encoding, `MissingCell(CellID)`,
`CellCommitmentMismatch`, `DepthOverflow`, `InvalidLevel`, `PrunedCell`, and
exhausted gas/resource budget. This
lets a composite decoder propagate resolution failures without converting
between unrelated reader and resolver errors.

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
4095 payload bytes and four reference handles. Avoiding that copy requires an
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
Cell value:     CellRef — resident Arc<Cell> or unloaded CellCommitment
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
failure/rollback behavior. Copying an unloaded reference does not assert that its
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
4095 bytes; storing a child reference is a different operation.

`CellID` is the factual hash defined in [Identity](#identity), not SHA256 of
payload bytes or of an ordinary wire record. Advancing a Slice changes
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

This proposal does not change the current Cell record, Cell hierarchy, Trie, actor
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
   overflow, the parent payload must be exactly 4095 bytes and its next
   reference in serialization order points to the continuation. A missing
   builder reference slot returns `ReferenceCapacity` without mutation.
3. Continuation Cells belong only to this string. Each nonterminal continuation
   has exactly 4095 payload bytes and one reference at index zero; the terminal
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
4091 bytes:    [len=4091 | 4091 bytes]
4092 bytes:    [len=4092 | 4091 bytes] -> [1 byte]
8186 bytes:    [len=8186 | 4091 bytes] -> [4095 bytes]
8187 bytes:    [len=8187 | 4091 bytes] -> [4095 bytes] -> [1 byte]
```

A parent with other references can encode a 6000-byte string as:

```text
parent payload: [70 17 00 00 | 4091 string bytes]
parent refs:    [earlier object, continuation, later object]
                                     |
                                     v
                         [1909 string bytes; no refs]
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
It follows continuation refs through `CellReader`, checking each resolved
body's commitment and exact payload/reference shape. Short nonterminals, extra
continuation refs, incorrectly sized terminal payloads, explicit pruning
records, missing bodies, depth overflows, and limit overruns fail. Each
continuation makes positive progress toward the
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

`Trie::from_cell(root, key_bytes)` accepts `impl Into<CellRef>`: a resident,
unloaded, or unresolved `CellRef`, a `Cell`, or an `Arc<Cell>`. It checks the configured key width
but does not load or inspect the root. Every visited node, including the root,
is resolved and its child mask and compressed path label validated on access,
regardless of residency. One constructor therefore supports a wholly unloaded
Dict without extra resident/unloaded bookkeeping in Dict. An explicit pruning
record cannot be parsed as an ordinary Trie node: access fails with
`PrunedCell`. Bare unresolved IDs can name a lookup root, but cannot be inserted
as value references without first resolving their metadata.

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

`get`, `insert`, and `remove` take a `CellReader`. They resolve only the path
being traversed. Mutations rebuild the affected path and swap the root only
after the operation succeeds, so `MissingCell` and malformed-node errors leave
the Trie unchanged. Resolver-backed `get_ref` and `get` return an owned
`CellRef` and `Arc<Cell>` respectively; they cannot return borrows tied to a
temporary resolution cache.

There is no separate Merkle path object. The committed root `CellID`
authenticates the root body; each verified body contains the commitment of the
next child. Loading and hashing the Cells on the requested path therefore
authenticates the leaf and its position. Sibling subtrees remain as full
mask/hash/depth summaries in their parent Cells and do not need a separately
encoded proof path or the `merkle` crate. The current Trie traverses physical Cells; traversal through
`CellView` with an explicit virtualization cap is not yet integrated.

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


## Root-last graph transport

Hashing and graph transport are separate encodings. The highest significant
hash commits to the factual Cell graph, including the explicit pruning
records it contains. Lower hashes describe virtualized views whose hidden
contents are represented by retained commitments. A submitted graph is
identified by its root's highest hash; packet indexes never enter a hash
preimage.

This codec is implemented in `cells/src/transport.rs` and exposed on `Cell`.
`encode()` packs its resident graph, while `encode_transport(resolver)` can
also resolve unloaded references from a caller-supplied source.

### One root, children before parents

The packet contains one nonempty list of distinct physical Cell records.
References run from a Cell to its children, so reverse topological order
places children before their parents. The last record is the sole root.
There is no separate root index, root hash, root list, or bag commitment.

All listed Cells must be reachable from that root. To send several top-level
objects, join them with an ordinary envelope Cell or Cell tree before packing.
The envelope's shape belongs to its application schema; the transport needs
no multi-root mode.

```text
graph:             root -> [A, B]
                   A -> [leaf]
                   B -> [leaf]

record order:      leaf, A, B, root
record indexes:      0   1  2     3
```

A shared child is emitted once. References retain their original order,
including repeated references to the same child.

### Indexed wire format

```text
packet:
  cell_count                          V128, at least 1
  cell records                        cell_count records
  root                                implicit: final record

ordinary record at index i:
  descriptor                          U16 LE, existing ordinary descriptor
  payload                             descriptor's payload_length bytes
  backward reference distances        V128, one per ordered reference

pruned record:
  descriptor                          U16 LE, 0x8000 | nonzero level_mask
  retained hashes                     32 * popcount(mask) bytes
  retained depths                     2 * popcount(mask) bytes, U16 LE
```

For an ordinary record at position `i`, each distance `d` names record
`i - d`, and must satisfy `1 <= d <= i`. `V128` uses canonical unsigned
LEB128. A distance is a relative index, not a hash or an instruction to find
data outside this packet. Relative indexes keep immediate-child references
at one byte even in a long Snake or large graph.

The example above has these references:

```text
record 0: leaf       no references
record 1: A          distance 1       -> record 0
record 2: B          distance 2       -> record 0
record 3: root       distances 2, 1   -> records 1, 2
```

An ordinary wire reference contains no child mask, hashes, or depths. Those
are reconstructed from the earlier child record. Pruned Cells retain their
hash/depth payload because that information cannot be recovered from their
hidden subtrees.

### Hash encoding remains independent

The [hash preimage](#identity) continues to use the ordinary descriptor,
applied level mask, original payload or previous significant hash, and the
selected child depths and hashes. Hashes never use table indexes or their
variable-length representation. Pruned Cells' highest hashes use their
descriptor and retained hash/depth payload as already specified.

Decoding proceeds from the first record to the last:

1. Decode the descriptor and payload.
2. Resolve ordinary references by backward index into the decoded list.
3. Derive the ordinary level mask and compute every significant hash/depth
   pair, or reconstruct the retained pairs and highest hash of a pruned Cell.
4. Append the completed Cell to the list.
5. Return the final Cell as the root, together with a lookup index if the
   application needs one.

No forward-reference placeholders or second hashing pass are needed. The
root and its metadata arrive last, so execution begins after the packet has
been read and validated.

### Canonical ordering and validation

The encoder uses iterative, ordered depth-first postorder: visit child
references in their declared order, emit each Cell after its children, and
deduplicate by highest CellID. First occurrence determines the position of
a shared subtree. The decoder enforces this order as well as reachability,
so there is one packing of the supplied rooted graph.

The decoder rejects zero count, overlong or overflowing `V128`, invalid
descriptors, zero/out-of-range distances, duplicate CellIDs, unreachable
records, noncanonical order, trailing bytes, and depth overflow. Backward
references guarantee physical acyclicity by construction. Record counts,
bytes, references, and hashing work are bounded and charged before allocating
or performing the corresponding work.

Every physical child in the submitted view must have a record. A missing
ordinary body is malformed transport, not a permissible omission that can
change execution results. To hide a subtree, transmit an explicit pruned
Cell in its place; that changes the factual root while retaining the selected
lower commitments. Storage residency outside a submitted packet remains a
separate concern.

The protocol chooses the root and commitment level it expects. The signed
transaction body includes every execution witness through its program graph.
The Tx root references that body and carries the signature and R1CS proof
inline. The decoder never
grants execution access to extra uncommitted records.

### Integration

`hash_preimage(level)` and hash/depth computation are independent of transport.
The flat bag codec, independent bag hash, and root-ID packet prefix are
removed. Transactions carry their execution witness hierarchy by Cell
reference, rather than embedding serialized bag bytes in a Snake.

The in-memory `CellIndex` retains exact lookup scopes; snapshots of its bodies
are normal Cell hierarchies. Standalone summary records retain their existing
role in actor rent and logical-resolution gas accounting. Their `record_size`
is independent of index compression and transport order.

## Cell indexes and typed snapshots

### In-memory availability

`CellIndex` is a lookup map from original CellID to a detached Cell body.
It has no independent wire format. Its `collect` method visits attached
resident descendants and unions resident frontiers of equivalent Cells.
Insertion detaches references so an indexed body cannot silently grant
access to descendants absent from the index.

```rust
pub struct CellIndex { /* CellID -> Arc<Cell> */ }

impl CellIndex {
    pub fn new() -> Self;
    pub fn collect(root: Arc<Cell>) -> Result<Self, CellError>;
    pub fn insert(&mut self, cell: Arc<Cell>) -> Result<(), CellError>;
    pub fn extend(&mut self, other: &Self) -> Result<(), CellError>;
    pub fn get(&self, id: &CellID) -> Option<Arc<Cell>>;
    pub fn contains(&self, id: &CellID) -> bool;
    pub fn iter(&self) -> impl Iterator<Item = (&CellID, &Arc<Cell>)>;
    pub fn to_cell(&self) -> Result<Cell, CellError>;
    pub fn id(&self) -> Result<CellID, CellError>;
    pub fn from_cell<R: CellReader + ?Sized>(
        root: &Cell, resolver: &mut R,
    ) -> Result<Self, CellError>;
}
```

The index is used for actor-owned residency and for the initiating
transaction's immutable execution witnesses. Those two scopes remain
separate. Cache residency does not expand either scope.

### A snapshot is an ordinary Cell hierarchy

When an application needs to commit or carry an exact set of bodies, it
constructs a normal snapshot hierarchy:

```text
snapshot root:
  payload: body_count:U32 LE, original_level_cap:U8
  refs:    one Trie root for a nonempty index, none for an empty index

Trie:
  key:     original CellID, 32 bytes
  value:   reference to that body's physical proof tree
```

The highest hash of this root is the availability commitment. There is no
separate bag hash and no flat list embedded as a byte blob. The hierarchy
travels through the same indexed Cell transport as any other root.

Only bodies explicitly indexed are supplied in the proof trees. A reference
to an absent body becomes an explicit pruning record at one fresh level
above the original cap. Its retained pairs preserve the original child
commitment. Existing pruning records at or below that cap remain literal
data. The resulting physical DAG is complete.

Snapshot decoding projects each listed proof body back to its original cap,
verifies its ID against the Trie key, and recreates detached original bodies.
New cuts are not indexed as available original data. As a result, restoring
a snapshot preserves exactly which lookups succeed or fail; it does not
hydrate missing descendants from other storage or from a transport cache.
The reconstructed snapshot must have the same factual root ID.

The fresh level consumes one of the available fifteen pruning levels when
there are missing bodies; a sparse snapshot already at the maximum level
fails explicitly. Complete graphs can always use direct Cell transport.
Actor commitment and block processing propagate these encoding errors through
their rollback paths rather than panicking while computing a state root.

### Typed-object snapshots

The existing typed-object helpers retain a small ordinary snapshot root:

```text
typed snapshot root:
  payload: original_level_cap:U8
  refs:    one physical proof root of the object
```

`CellEnvelope` is a convenience object for this schema, not a packet header.
Its `encode()` packs that ordinary root-last DAG. Its `transport_root()`
exposes the factual root being carried; `root()` names the original content
view used by typed decoders. It contains no root-ID prefix and no rootless
bag. Unreachable extras are rejected.

`CellEncode::to_envelope()` remains useful for transporting partial actor or
Dict views while preserving the original content IDs. For a complete graph,
`cell.encode()` transports the Cell itself directly. Multiple roots require
an ordinary application-defined envelope hierarchy.

### Transactions

Tx has one body reference, a fixed 64-byte signature field, and a U16-length
inline R1CS proof. The body holds the header, a program reference, and a
canonical mask-one pruned log reference containing the log's hash(0)/depth(0).
TxID is the body's factual ID; WitnessID is the Tx root's factual ID.

The program contains bytecode and its supplied execution subcells. In the
current bytecode API these subcells use an ordinary witness snapshot hierarchy
referenced from the Snake container. No separate body witness field or
`CellWitness` effect is needed: the program's factual hash commits availability.
Only the decoded execution
index is exposed to VM witness resolution. Its snapshot is frozen before
proving and is shared unchanged with all calls and asynchronous descendants.

## Resolution and witness context

### Allowed sources

Lookup providers have one ID-only operation. A separate read context covers
both attached resident bodies and ID lookups:

```rust
pub trait CellResolver {
    fn resolve(&mut self, id: CellID) -> Result<Arc<Cell>, CellError>;
}

pub trait CellReader {
    fn read(&mut self, id: CellID, resident: Option<&Arc<Cell>>)
        -> Result<Arc<Cell>, CellError>;
}
```

Every CellResolver automatically implements unmetered CellReader: attached
bodies are used directly, otherwise `resolve(id)` supplies them. `()` is an
empty lookup provider and still supports resident reads. Metered and recording
contexts implement CellReader instead, covering every logical access, even
when no lookup is needed. `read_cell(reader, reference)` checks the returned
ID and full expected commitment; `resolve_cell(reader, id)` checks the ID.
Codecs, Trie, Snake, and CellView use this shared read path. Pure lookups do
not determine gas prices or witness-recording policy.

The concrete VM read context may use exactly these sources:

1. a body already attached to the value in RAM;
2. a body present in the current Contract or Actor's consensus-committed
   persistent Cell store;
3. a body present in the initiating external transaction's committed Cell hierarchy.

It must not use an unrelated actor's store, a node-wide content cache, an
archive, a database record not committed as resident for the current object,
or the network. A physical cache may avoid decoding a body again only after
the resolver has established that the ID belongs to source 2 or 3.

The current `ExecutionCells` implementation already combines attached bodies,
the current actor's retained `CellIndex`, and the external transaction's Cell hierarchy,
in that order. `ActorStore` is currently RAM-backed. There is no disk backend
or general-purpose resolver-composition adapter yet. The `CellResolver` trait
allows a composite implementation to consult actor-scoped disk storage and
then the transaction witness hierarchy. Such a fallback must continue only on a missing
requested ID, not suppress integrity, resource-limit, or storage errors. The
read context must charge logical access once, regardless of which source supplies the body,
and disk lookup must enforce the same committed-residency scope as RAM lookup.

### Crossing ownership domains

Persistent-store authority does not travel with a value. While actor A is
running, its own state may resolve from A's committed store. If A passes a
String or Dict into actor B, only recursively attached `Resident` Cells travel
with the value. A remaining `Unloaded` reference may resolve from the external
transaction witness hierarchy (or from B's own store if B independently retained that exact
Cell), but never from A's store merely because A originated the argument.

The same rule applies to synchronous calls and asynchronous sends. A Message's
collected Cell graph includes all resident Cell bodies carried by its portable
payload; unloaded descendants retain summaries and require the shared execution
Cell hierarchy. On return, the caller's own store scope is restored and returned resident
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
against the same CellID and, for an unloaded reference, its full commitment
summary. The initial gas rule charges every logical resolution
from the canonical Cell size, including a physical cache hit. It can be refined
later, but consensus cost must never depend on wall-clock cache behavior.

`MissingCell(CellID)` means no permitted source contains the requested body.
A malformed generic Cell record invalidates the Cell hierarchy before VM execution. A
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
  `CellIndex`.
- A verifier resolver reads from committed actor/Contract storage and the
  immutable Cell hierarchy carried by the external transaction.

The candidate Cell hierarchy must be frozen **before the proof-producing execution**, not
merely before signing. `Prover::prove` derives the TxLog/TxID and binds TxID
into the R1CS transcript, so learning `availability CellID` during that same run would be
circular. Transaction construction is therefore:

1. assemble a conservative Cell hierarchy directly, or run an optional discovery pass;
2. freeze the canonical snapshot and its `availability CellID`;
3. run the real prover against that frozen index using verifier-equivalent
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
    pub cells: CellIndex,
}

// Actor commitment includes (stored.root, stored.cells.id()).
```

The actor-state commitment includes both fields. The root says *what* the
state is; the availability CellID says *which bodies remain resident and rent-bearing*.
Evicting a branch's body changes the stored Cell hierarchy and storage charge but not the
content root. This is residency pruning, not construction of an explicit
pruning Cell, which would change the factual root. A fully frozen actor can
retain its code/state commitments while retaining no corresponding bodies.

Resolving a body from a transaction witness hierarchy only puts it in the execution cache.
It does not silently add it to actor storage or increase rent. A persistent
restore must be an explicit high-level operation. New or modified Trie paths
are resident because the actor created their Cell bodies; untouched unloaded
siblings remain unloaded.

Cells do not restrict their payloads to portable data. When a typed Cell is
decoded into a VM Value, the boundary that consumes it performs the same
portability and linearity checks it would perform for a fully resident value.
This permits frozen actor state to commit linear values without retiring them
and prevents those values from crossing a boundary that forbids them.

## VM-visible uses

In the implemented interface, Cells stay below the FlameVM Value layer:

| Use | Root held by | Body source when accessed |
| --- | --- | --- |
| Contract | `ContractID` / UTXO input | External transaction witness hierarchy |
| Actor code and state | Actor registry record | Actor's stored Cell hierarchy, then external transaction witness hierarchy for pruned bodies |
| Predicate program branch | Predicate commitment | External transaction witness hierarchy |
| Dict | Typed Dict wrapper with key width, length, and Trie root | Current resident graph/store, then external transaction witness hierarchy |
| Bounded VM String | Its zero-ref raw-byte Cell | Current resident graph/store, then external transaction witness hierarchy |
| Snake-encoded bytes | Typed program/protocol-proof/blob field | Current resident graph/store, then external transaction witness hierarchy |
| Utreexo proof data | Existing Utreexo format, unchanged | Explicit legacy proof input; specialized accumulator verification applies; Cell migration deferred |

Current opcodes do not load an arbitrary Cell Value. They perform a typed action
such as opening a Contract, reading a Dict key, or executing a Predicate
branch; that implementation follows Cells through the current resolver.

## Transaction commitment and actor scheduling

### Committing availability

Each external transaction's program carries one immutable execution `CellIndex`
as subcells. The body's factual CellID includes the program's supplied graph,
so availability is bound by TxID, every `signtx` signature, and the R1CS proof.
No separate witness hash field or log entry is needed. The program's ordinary
snapshot hierarchy preserves exactly which logical body lookups succeed.

The current `BlockTx::witness_hash` is not enough: it is a block-level
commitment assembled by the minter, while Cell availability must already be
bound to the submitted transaction.

Removing, adding, or changing an indexed body changes the snapshot's highest
root CellID. Consensus execution must expose every indexed body
to resolution. A minter cannot choose to ignore a present body and obtain
a different branch result.

Unused bodies are permitted. They increase transaction bytes and fees but do
not change execution unless accessed. This is simpler than committing the
exact data-dependent sequence of loads.

### One execution closure per external transaction

Block execution is:

```text
for each external transaction in block order:
    validate its Cell hierarchy and create one Cell execution context
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
copy a Cell hierarchy. Every asynchronous descendant is processed before the next
external transaction and therefore has exactly one unambiguous witness source.
The per-external queue is FIFO in Send-effect order. Witness indexes are never coalesced
at block scope.

The existing block-global gas, multiplication, and message-count limits remain
outside the loop. Each Cell hierarchy is bounded by the existing witness-byte limit and
gas-charged decoding/resolution. An infinite send chain or Cell walk is
therefore bounded by consensus costs and limits.

Failure behavior follows the existing execution boundary:

- missing data in the external root makes that external transaction invalid;
- missing data in a synchronous call returns the call's failure result and
  arguments according to the call rollback rules;
- missing data in an asynchronous message produces the ordinary failed-message
  bounce;
- effects and actor mutations from a failed scope roll back, while the shared
  read-only Cell hierarchy and its cache remain unchanged.

## Universal Flame encoding and hashing

The end state is:

1. Every consensus type has one expected `CellEncode`/`CellDecode` layout.
2. Every content object's identity is derived from its canonical root Cell.
3. Unbounded protocol byte fields use length-prefixed Snake encoding; keyed
   collections use `Trie`. VM String uses one zero-ref Cell with raw bytes;
   fixed and small fields stay in the parent payload or explicit child Cells.
4. A standalone graph is transported from its root Cell. Typed snapshots may
   use `CellEnvelope`; persistent types commit a content root `CellID` and the
   availability snapshot's root `CellID`.
5. Exact encoded size comes from Cell records and graph transport bytes, not a parallel
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

The graph record count and two-byte Cell descriptors are the transport framing,
implemented directly inside `cells`. Snapshot headers belong to normal Cell
payloads, not a separate packet format.
Above that boundary, the migrated types are typed Cell graphs with no generic
flat Reader/Writer layer. Utreexo's existing codec boundary is an explicit
temporary exception, not a second codec for these migrated types.

Cell hashing replaces serialization-to-`Vec<u8>` followed by a separate object
hash for IDs such as Contract, Actor state/code, Message, transaction, and
block objects. The caller's context selects the root type and commitment
level; `CellID` is the highest SHA-256 hash specified in [Identity](#identity),
not a direct hash of an ordinary wire record.

The external Tx root identifies WitnessID. Its body root identifies TxID and
commits the header, supplied program, and pruned log claim. EffectID is the
computed log's level-zero hash, verified together with its depth. Internal
execution IDs retain their factual effect-log root. See [encoding](encoding.md).

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

The overall Cell migration changes every downstream consensus hash whose preimage changes,
including ContractID, MessageID, actor code/state roots, TxID, block witness
and effects roots, and BlockHash. All formats must switch together in one
coordinated consensus activation with golden vectors; mixed old/new hashing is
not valid. The root-last transport replacement does not change Cell hash
preimages; it changes transaction witness and actor availability commitments
because those sets are now committed by ordinary snapshot Cell roots.

## Implementation map

| Area | Implementation |
| --- | --- |
| Primitive and streaming API | `cells/src/cell.rs`, `builder.rs`, `slice.rs`, `codec.rs`: ordinary/pruned Cells, level masks and depths, shared read-only CellViews, exact codecs, inline length-prefixed snake operations |
| Transport | `cells/src/transport.rs`: root-last ordered DAG, deduplication, backward indexes, bounded and metered decoding; `index.rs`: exact availability and typed Cell snapshots |
| Trie | `cells/src/trie.rs`: one radix-4 Patricia format; resolver-backed get/insert/remove/ordered navigation; raw root wrapping/unwrapping |
| Dict | `flamevm/src/dict.rs`: one Trie index, typed runtime witness cache, sticky summaries, lazy authenticated reads; no Dict2 |
| VM codecs | `flamevm/src/encoding.rs` plus type-owned codecs: fixed scalars/points/tokens, tagged Values, single-Value Contracts, actors, messages, scripts |
| Predicates | `flamevm/src/contract.rs`: program Trie, index selector, path-only witness collection and resolver-based branch opening |
| Execution | `flamevm/src/vm.rs`: fixed Cell hierarchy before proving, scoped and metered resolution, public-path validation before restoring private witnesses |
| Transactions | `flamevm/tx.ctl`, `build.rs`, `src/tx.rs`: generated Tx/body codecs, inline authorization, pruned log, body TxID, factual WitnessID, and program-scoped execution index |
| Actor storage | `flamechain/src/storage.rs`: content roots plus retained availability CellID, leases Trie, freeze instead of bulk destruction, explicit persistence only |
| Scheduling and blocks | `flamechain/src/block.rs`: external then FIFO descendants with one witness index, Cell-backed block/record commitments and exact transport decoding |
| Utreexo | `merkle` plus `flamechain/src/utreexo`: specialized accumulator and legacy forest/proof/path serialization retained; Cell migration deferred |

The old `flamevm/src/chunk.rs`, local Trie, and Dict2 are deleted.
`readerwriter/` remains only for the deferred Utreexo codec boundary.
`cells` and `flamevm` have no dependency on `merkle`.
Instruction bytecode keeps its own compact opcode operands; those bytes are
stored in script Cells, not routed through a generic serialization facade.

### Remaining extensions

- Explicit VM operations to prune selected Dict branches or persist selected
  witness bodies. Existing `save`/`setcode` are the persistence boundary.
- Integration of explicit virtualized proof views into Taproot/Trie and
  transaction/block proof consumers. The crate's `CellView` is not an implicit
  replacement for physical typed decoding or the current signed execution index.
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

Verification covers canonical Cell/Cell hierarchy boundaries, sparse and dense masks,
multilevel hash/depth commitments, pruning/virtualization, rejection of hidden
payload access, partial Trie traversal,
sticky Dict capabilities, public/private witness separation, call-failure
escrow, witness-snapshot commitment, actor freeze/recovery, and immediate
per-external queue draining. Tests should be run together: these encodings
change VM, proof, actor, and block commitments as one format.

## Security and consensus invariants

- Cell hierarchy decoding applies the outer byte bound and precharges declared element
  counts before allocating. Individual Cell records have fixed payload,
  reference, and significant-level bounds.
- All lengths, counts, and size sums use checked conversions/arithmetic.
- Cell identity never depends on residency or cache contents. Explicit proof
  pruning changes factual identity and preserves only its designated lower
  commitments.
- Level masks and all retained/child hash-depth summaries are validated.
  An unresolved lookup ID is not a serializable child reference.
- Virtualization can only lower the cap; unavailable proof data fails reads.
  It cannot be silently recovered from another record with a semantic hash.
- The persistent availability set is committed separately from the content
  root.
- A transaction's Cell hierarchy is immutable, signed through TxID, and never merged with
  another transaction's index.
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
- Merkle proof/update wrappers, library-reference Cells, or other Cell kinds
  beyond ordinary and pruned. Levels/masks and depths are implemented.
- Multiple packet variants, optional index tables, CRC, or cache bits.
- Implicit network/database retrieval during consensus execution.
- Implementing the proposed first-class raw Cell Value or Cell-manipulation
  opcodes before their separate design review.
- A generic storage engine or automatic persistence of hydrated data.
- A proc-macro codec framework or speculative generic collection family.

## References

- [TON cell overview and representation](https://docs.ton.org/foundations/serialization/cells)
- [TON Bag of Cells format](https://docs.ton.org/foundations/serialization/boc)
- [TON DataCell hashing, significant levels, and depths](https://github.com/ton-blockchain/ton/blob/master/crypto/vm/cells/DataCell.cpp)
- [TON CellBuilder pruning-record construction and retained-pair ordering](https://github.com/ton-blockchain/ton/blob/master/crypto/vm/cells/CellBuilder.cpp)
- [ton-swift Cell API](https://github.com/tonkeeper/ton-swift/blob/aacae64c40f40500a9485b6b2d8d060a4777cf72/Source/TonSwift/Cells/Cell.swift)
- [ton-swift Builder API](https://github.com/tonkeeper/ton-swift/blob/aacae64c40f40500a9485b6b2d8d060a4777cf72/Source/TonSwift/Cells/Builder.swift)
- [ton-swift Slice API](https://github.com/tonkeeper/ton-swift/blob/aacae64c40f40500a9485b6b2d8d060a4777cf72/Source/TonSwift/Cells/Slice.swift)
- [ton-swift typed Cell serialization](https://github.com/tonkeeper/ton-swift/blob/aacae64c40f40500a9485b6b2d8d060a4777cf72/Source/TonSwift/Cells/Serialization.swift)
- [ton-swift snake string encoding](https://github.com/tonkeeper/ton-swift/blob/aacae64c40f40500a9485b6b2d8d060a4777cf72/Source/TonSwift/Cells/SnakeEncoding.swift)
- [TON canonical BoC declarations](https://github.com/ton-blockchain/ton/blob/master/crypto/tl/boc.tlb)

TON's algorithms inform the level/hash rules; Flame's descriptors, byte payloads,
15-bit masks, little-endian depths, and backward-index graph transport above
are adaptations, not TON wire compatibility.
