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

This document specifies the target architecture and the migration from the
current code. It is not a description of fully implemented behavior yet.
`docs/compression.md` records the motivation and broader experiments;
this document is the concrete design to implement.

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
  Snake
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
| `cells` | Cell construction, canonical records and IDs, bounded reading/writing, BoC validation, content lookup, Snake, and generic fixed-key Trie traversal |
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

The core API retains the useful parts of the current `Chunk` API:

```rust
impl CellRef {
    pub fn resident(cell: Cell) -> Self;
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

The current `Chunk` descriptor is retained:

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

`CellBuilder` never spills automatically into another Cell. The containing
type must decide whether overflow belongs in a `Snake`, Trie, or
another explicit child. This keeps encodings canonical.

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

## Snake

`Snake` stores an arbitrary byte string as a linear chain. It is the
byte-granular counterpart of TON's snake encoding.

```text
non-terminal Cell: exactly 8191 payload bytes, exactly one continuation ref
terminal Cell:     0..=8191 payload bytes, zero refs
```

The continuation is always reference 0. No other references are permitted.
Empty input has one empty terminal Cell. An exact multiple of 8191 bytes ends
in a full terminal Cell; it does not add an empty sentinel.

Examples:

```text
0 bytes:       [0]
8191 bytes:    [8191]
8192 bytes:    [8191] -> [1]
16382 bytes:   [8191] -> [8191]
```

There is no stored total length. Length is derived by walking the chain, so a
length query over a pruned tail may return `MissingCell`. This is preferable to
reducing every root's useful payload or adding a wrapper Cell solely for a
cached value. Execution charges for Cells as they are traversed.

Target API:

```rust
impl Snake {
    pub fn from_bytes(bytes: &[u8]) -> Self;
    pub fn from_root(root: CellRef) -> Self;
    pub fn root(&self) -> &CellRef;
    pub fn reader<'a, R: CellResolver + ?Sized>(
        &'a self,
        cells: &'a mut R,
    ) -> SnakeReader<'a, R>;
    pub fn to_bytes<R: CellResolver + ?Sized>(
        &self,
        cells: &mut R,
        limit: usize,
    ) -> Result<Vec<u8>, CellError>;
}

impl<'a, R: CellResolver + ?Sized> SnakeReader<'a, R> {
    pub fn read(&mut self, output: &mut [u8]) -> Result<usize, CellError>;
}

pub struct SnakeWriter { /* full segments plus current segment */ }

impl SnakeWriter {
    pub fn new() -> Self;
    pub fn write(&mut self, input: &[u8]);
    pub fn finish(self) -> Snake;
}
```

Construction splits input into complete payload Cells and builds the chain
iteratively from tail to head. Reading walks from head to tail without
recursion and can return bytes incrementally. The initial writer may retain
complete 8191-byte segments until `finish`; add a disk-backed spool only if a
real producer needs strings larger than practical RAM.

Decoding rejects a short non-terminal Cell, more than one reference, cycles,
and configured byte/cell/depth limit overruns. It does not flatten the entire
string unless the caller asks for `to_bytes`.

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
    len: usize,
    root: Option<CellRef>,
}
```

The raw trie root does not commit `key_bytes` or `len`. The owning type fixes
`key_bytes` when it is inherent in that type, and encodes it only when it truly
varies per value. It encodes `len` when the value must reconstruct an O(1)
entry count without loading the whole Trie. `into_root` (or `into_parts`)
preserves the existing ability to unwrap the bookkeeping layer.

`len` is authoritative committed metadata, not a hint used to preallocate. It
must equal the leaf count. `new`, `insert`, and `remove` maintain that invariant;
constructing from a pruned root is allowed only for a root that was previously
created by a valid state transition. A full-state import validates the count by
walking the graph. Untrusted witness data can supply bodies for an existing
root but cannot replace the committed `len`. If arbitrary untrusted Trie roots
are admitted later, subtree counts or a full validation proof will be required.

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

The low-level Trie knows nothing about `Int253`, VM Values, portability, or
linear types. `Dict2` remains in `flamevm`; it converts `Int253` to a 32-byte
ordered path and interprets the leaf Cells.

The eventual FlameVM Dict encoding does not repeat a Dict tag, version, or key
width: the caller expects a Dict and its `Int253` keys are always 32 bytes. It
contains only the entry count, semantic summary flags needed without loading
pruned branches, and the optional Trie-root reference. At minimum those
summaries include sticky `portable` and `droppable`; an empty Dict has no Trie
root and is droppable. Pruning changes availability, not these semantic fields.

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
2. Exactly one **execution BoC**, whose canonical bytes are a `Snake`
   field referenced by the ExternalTx root.

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
  Snake reads must not consume a linear argument before a possible
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

Cells stay below the FlameVM Value layer:

| Use | Root held by | Body source when accessed |
| --- | --- | --- |
| Contract | `ContractID` / UTXO input | External transaction BoC |
| Actor code and state | Actor registry record | Actor's stored BoC, then external transaction BoC for pruned bodies |
| Predicate program branch | Predicate commitment | External transaction BoC |
| Dict | Typed Dict wrapper with key width, length, and Trie root | Current resident graph/store, then external transaction BoC |
| Snake | Typed String/code/proof field | Current resident graph/store, then external transaction BoC |
| Utreexo proof data | Flamechain's Utreexo encoding | Its explicitly supplied Cell graph; specialized accumulator-proof verification still applies |

An opcode does not load an arbitrary Cell Value. It performs a typed action
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
3. Arbitrary byte fields use `Snake`; keyed collections use `Trie`;
   fixed and small fields stay in the parent payload or explicit child Cells.
4. A standalone graph is transported as a `CellEnvelope`; persistent types
   commit a root `CellID` and the `BoCID` of their retained bodies.
5. Exact encoded size comes from Cell records and BoC bytes, not a parallel
   `SizeWriter` pass.
6. No consensus code uses `serde` as a canonical format.
7. The `readerwriter` crate and `flamevm/src/encoding.rs` are removed after all
   callers migrate. VM-specific compact Value rules move into the relevant
   `CellEncode`/`CellDecode` implementations rather than into `cells`.

The Cell API does not prescribe one generic list encoding. Actual types use
the smallest of the three existing structures: payload for bounded items,
`Snake` for bytes, and a fixed-width Trie for unbounded keyed or
ordinal collections. Add a generic list wrapper only if several real types
would otherwise duplicate the same layout.

The two-byte Cell record and the small BoC/CellEnvelope headers are the
bootstrap framing for this system, implemented directly inside `cells`.
Everything above that boundary is a typed Cell graph; no generic flat
Reader/Writer layer remains.

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
Flamechain's Utreexo module; Flamechain-owned wrappers give its proof data Cell
encodings.

This migration changes every downstream consensus hash whose preimage changes,
including ContractID, MessageID, actor code/state roots, TxID, block witness
and effects roots, and BlockHash. All formats must switch together in one
coordinated consensus activation with golden vectors; mixed old/new hashing is
not valid.

## Implementation plan on the current codebase

### Current baseline

| Target | Current implementation |
| --- | --- |
| Cell | `flamevm/src/chunk.rs` already has immutable `Chunk`, 8191-byte/4-ref bounds, the two-byte descriptor, cached hash, and resident/pruned references |
| Trie | `flamevm/src/trie.rs` already has the fixed-width radix-4 Patricia trie, but it returns `ChunkReferencePruned` instead of resolving through context |
| Dict adapter | `flamevm/src/dict2.rs` owns the `Int253` ordered-key conversion and should remain VM-specific |
| Streaming codec | `readerwriter` reads/writes flat byte slices; it has no Cell/ref cursor |
| BoC | Not implemented |
| Witness context | The broader proposal exists in `docs/compression.md`; no Cell resolver is threaded through VM entry points |
| Actor scheduling | `flamechain/src/block.rs::execute_body` currently executes all external transactions, then drains one block-wide `VecDeque<Message>` |
| Canonical codecs | `readerwriter` is used by `flamevm`, `flamechain`, and `merkle`; `flamevm` publicly re-exports it |
| Generic Merkle commitments | `flamevm` and `flamechain` still use `merkle` for TxLog, block, and actor-state roots in addition to Utreexo; all non-Utreexo uses must become Cell graphs |

### Step 1: rename Chunks to Cells

- Rename `Chunk`, `ChunkRef`, `ChunkID`, limits, errors, tests, and comments to
  `Cell`, `CellRef`, and `CellID`.
- Preserve the current descriptor layout and limit tests.
- Replace the current Merlin transcript hash with direct
  `SHA256(canonical_cell_record)` and intentionally update the golden ID
  vector.
- Replace mutating `hydrate` with resolver/cache behavior so loading cannot
  accidentally alter persistent availability.
- Remove the term `Chunk` from this subsystem.

Steps 1-3 should land as one extraction change rather than rename a file in
place only to move it again.

Exit checks: boundary records round-trip; descriptors with ref count 5-7 are
rejected; reference order affects ID; resident and pruned references produce
the same parent ID; Cell IDs match direct SHA-256 test vectors.

### Step 2: create the `cells` crate

- Add `cells` beside `flamevm` in the workspace.
- Create only the modules needed by this design:

  ```text
  cells/src/lib.rs
  cells/src/cell.rs
  cells/src/builder.rs
  cells/src/slice.rs
  cells/src/codec.rs
  cells/src/snake.rs
  cells/src/boc.rs
  cells/src/trie.rs
  cells/src/error.rs
  ```

- Move Cell, Trie, codec, and BoC failures out of `VMError` into the single
  `cells::CellError`; map it into `VMError` only at the VM boundary.
- Do not depend on `readerwriter` during extraction. Decode the compact Cell
  record directly; Builder/Slice become its public codec.
- Add `cells` dependencies to `flamevm` and `flamechain`. Neither `cells` nor
  `flamevm` depends on `merkle`; Flamechain retains `merkle` only in its
  Utreexo implementation.

Exit checks: `cells` builds and tests alone; its dependency graph contains no
FlameVM, chain, `merkle`, or `merlin` crate; FlameVM's public Cell exports point
to the new crate.

### Step 3: move Trie to `cells`

- Move `flamevm/src/trie.rs` with its existing tests and replace `VMError` with
  `cells::CellError`.
- Change `root: Option<Cell>` to `root: Option<CellRef>` so a root may also be
  unloaded.
- Pass a resolver into traversal and mutation. Resolve only the path used.
- Keep the 32-byte current maximum and bounded recursive mutation for now; the
  existing recursion is at most 128 radix-4 digits.
- Keep `int253_to_ordered_key`, `ordered_key_to_int253`, and `Dict2` in
  `flamevm`; update their imports to the new crate.
- Require each owning type to fix Trie `key_bytes`, or encode it only when it
  varies per value. Encode `len` only where that type exposes an authoritative
  O(1) count.
- Return owned refs/Cells from resolver-backed lookups. Keep construction from
  pruned roots on the authenticated-state path; validate leaf count on a full
  untrusted import and never allocate from `len` alone.

Exit checks: move the current insert/get/remove, prefix-split, collapse, order,
malformed-node, and atomic-failure tests; add resolution tests where an accessed
unloaded path succeeds from a bag and an unrelated missing path is not loaded.

### Step 4: implement Builder, Slice, and typed codecs

- Implement the API specified above with separate payload/ref cursors.
- Make primitive stores atomic and little-endian.
- Implement transactional `preload`/`try_load` and exact `finish`.
- Add `CellEncode`/`CellDecode`, `to_cell`, and exact `from_cell` helpers.
- Define the shared `CellError` variants used by builders, slices, resolvers,
  Trie, and BoC.
- Implement codecs first for primitives, `CellID`, and test-only structs. Keep
  sum-type discriminants in the crate that owns the sum type.
- Do not add bit APIs, arbitrary-capacity builders, proc macros, a generic I/O
  facade, or TON's runtime `TypeCoder`.

Exit checks: one test covers atomic overflow, independent byte/ref cursors,
speculative read rollback/commit, and rejection of both trailing bytes and
trailing refs.

### Step 5: implement `Snake`

- Implement the strict full-nonterminal layout above.
- Build and read iteratively; never recurse with attacker-sized input.
- Expose incremental reading plus a bounded `to_bytes` convenience method.
- Resolve continuation refs through `CellResolver`.
- Keep its first implementation standalone in `cells`; Step 9 adopts it for
  scripts, actor code, proofs, and opaque byte-string fields. Changing
  `String::Opaque(Vec<u8>)` into a lazy runtime representation is a separate
  VM-integration change because current string opcodes and call frames assume
  contiguous slices and byte offsets.

Exit checks: golden layouts at lengths 0, 1, 8190, 8191, 8192, and 16382;
round-trip a long value; reject short nonterminal Cells, extra refs, cycles,
missing tails, and configured limit overruns.

### Step 6: implement `BagOfCells`

- Implement the one canonical rootless format and exact-set `BoCID` above.
- Collect attached graphs iteratively, hash-deduplicate, and sort records by
  `CellID`.
- Decode into a sorted lookup table without eagerly reconstructing child
  graphs.
- Check the outer witness-byte bound, then charge declared Cell-count gas
  before allocating. Charge record parsing and graph validation as they
  proceed; use checked arithmetic for sizes and aggregate counts.
- Keep pruned branches implicit as missing bodies.
- Implement `CellEnvelope` as the canonical root-plus-bag wrapper and require a
  complete envelope's root body to be present.
- Do not implement TON indexes, format tags/versions, CRC, cache flags, levels,
  depths, exotic Cells, or compact child indexes.

Exit checks: canonical output is independent of insertion/traversal order;
duplicate/non-sorted/malformed/cyclic records fail; a partial graph resolves
supplied bodies and reports omitted ones; prepaid gas stops a declared flood of
tiny Cells before lookup-table allocation; root framing round-trips and rejects
a missing root; BoCID golden vectors equal direct SHA-256 of the canonical bag.

### Step 7: thread Cell resolution through FlameVM

- Add a resolver parameter to external execution, internal message execution,
  synchronous calls, Contract opening, Predicate selection, actor code/state
  loading, Snake access, and Trie/Dict operations.
- Implement a recording resolver for the prover and a verifying resolver for
  transaction execution. The recording form is a pre-proving discovery tool;
  the proof-producing run uses the frozen bag.
- Scope persistent lookup to the current Contract/Actor's committed store; use
  the external transaction BoC as the only source for a pruned body.
- Add deterministic gas/accounting for every logical Cell resolution based on
  canonical size. A physical cache hit receives the same charge.
- Keep the execution cache separate from the actor's stored BoC. Add explicit
  persistent prune/restore operations only with the owning high-level type.
- Migrate actor code/state records to `StoredGraph`; include both content root
  and retained-body `BoCID` in the actor commitment and derive rent from the
  retained records rather than the execution cache.
- Map missing Cell errors through existing external, synchronous-call, and
  asynchronous bounce boundaries without weakening argument recovery.
- At call/send boundaries, carry attached resident graphs but never the source
  actor's storage authority. Make every Cell-loading operation commit stack,
  linear-value, and effect changes only after resolution and typed decoding
  succeed.

Exit checks: the same access succeeds from RAM, actor storage, and the
committed transaction bag; it fails when the body exists only in a node-local
or another actor's store; loaded witness data is not persisted by an unrelated
save; pruned data passed from actor A cannot borrow A's store in actor B;
call/send/return failures conserve their linear arguments; non-portable decoded
values are rejected at the correct business boundary.

### Step 8: bind one BoC to one complete execution closure

- Add one execution `BagOfCells` to `ExternalTx` and its bounded envelope
  codec. It is separate from the `CellEnvelope` that will transport ExternalTx
  itself after Step 9; the old outer codec may remain temporarily.
- Commit its `BoCID` as the mandatory second external TxLog entry, after the
  header, before proof construction, TxID finalization, or `signtx`
  instructions.
- Pass the frozen `BoCID` into external VM construction so its initial TxLog is
  `[Header, CellWitness]`; internal execution keeps its own existing
  `[Header, Receive]` prefix and inherits the resolver context.
- Extend `TxEntry` encoding and `flamechain::validate_log_shape` for that
  external-only prefix; include the full bag in `BlockTx` witness-size/hash
  accounting. If this step lands before the protocol-wide Cell migration in
  Step 9, the existing TxLog commitment may be extended temporarily; do not
  introduce a second Merkle abstraction in `cells`.
- Accept/freeze the bag before `Prover::prove`. An optional discovery pass may
  build it first, but the proof-producing run and verifier both use the frozen
  bag. Stateful tooling must simulate actor descendants if it wants to
  discover their requirements; missing descendant bodies retain normal bounce
  behavior.
- Decode/read the declared transaction gas budget before allocating the BoC
  lookup table, so Cell-count gas can be charged up front.
- Refactor `flamechain/src/block.rs::execute_body`: create a fresh send queue
  inside each external-transaction iteration and drain it completely before
  advancing to the next external transaction.
- Borrow the same immutable BoC/resolver scope for the external execution and
  all descendant messages. Do not add a BoC field to `Message` or `CallFrame`.
- Keep block-global gas, multiplication, message, and output-uniqueness state
  outside the per-external loop.

Exit checks: an actor descendant can load a body from its initiating BoC;
another external transaction cannot; supplied data cannot be ignored; removing
or adding a record changes TxID/signing instructions; the execution order is
external A, all A descendants, external B, all B descendants.

### Step 9: replace `readerwriter` and the old encoding module

This is a protocol migration, not a mechanical trait rename.

1. Inventory and freeze old/new golden vectors for every consensus type.
2. For every consensus use, document the expected root type and exact payload
   field/reference order. Do not add a root tag or per-type version; define a
   discriminant only for an actual sum type.
3. Give leaf VM types Cell layouts, then composite Values, instructions,
   Contract, Message, Actor state/code, ExternalTx, TxEntry/TxLog, Utreexo
   proof wrappers, BlockTx, and Block/Header.
4. Move the compact `Int253` and Value tag logic from
   `flamevm/src/encoding.rs` into VM-owned Cell codec implementations. Encoding
   remains capable of representing non-portable values; boundary validation
   stays outside the codec.
5. Replace unbounded flat fields with `Snake` or a Trie/explicit Cell
   sequence. Do not write a length followed by an unbounded allocation.
6. Replace Predicate program trees, TxLogs, actor-state collections, and block
   witness/effect collections with explicit Cell graphs. Their root Cell IDs
   are the commitments; access loads and verifies Cells along the path rather
   than decoding a `merkle::Path` or sibling list.
7. Replace ID functions that hash `encode_to_vec()` with canonical root Cell IDs,
   while preserving normalized preimage/hash identities such as `ActorID`.
   Keep cryptographic Merlin transcripts separate.
8. Replace `encoded_size`/`SizeWriter` accounting with canonical Cell/BoC
   record sizes.
9. Remove `merkle` from `flamevm`. In `flamechain`, remove its use from block
   witness/effect and actor-state commitments; retain it only behind the
   Utreexo module. Put Cell codecs on Flamechain-owned Utreexo wrappers so the
   algorithm crate remains independent of `cells` and serialization.
10. Switch all consensus formats together in one coordinated consensus
   activation and regenerate golden vectors.
11. Remove FlameVM's `readerwriter` re-exports, remove all crate dependencies,
   delete `readerwriter/`, and delete the old standalone encoding module after
   its last codec moves.
12. Update `docs/flamevm.md`, `docs/blockchain.md`, and `docs/compression.md` to
   reference this specification and remove superseded flat/witness formats.

The current production `readerwriter` users to migrate are:

```text
flamevm:
  actor.rs address.rs contract.rs encoding.rs message.rs ops.rs
  script.rs string.rs tx.rs vm.rs

flamechain:
  block.rs and Flamechain-owned Utreexo wrapper codecs

merkle:
  remove Path's Reader/Writer implementation; retain only Utreexo algorithms
```

The current non-Utreexo `merkle` users to remove are
`flamevm/src/tx.rs`, `flamechain/src/block.rs`, and
`flamechain/src/storage.rs`. The bespoke Predicate tree in
`flamevm/src/contract.rs` also becomes a Cell graph: its root `CellID` is used
in the key tweak and its selected branch is supplied as loaded Cells, not a
neighbor-hash proof.

Exit checks: repository search finds no `readerwriter` dependency or import;
all public decode entry points are bounded and exact; all consensus ID golden
vectors are updated in one coordinated change; `cargo tree -p cells` and
`cargo tree -p flamevm` contain no `merkle`; repository imports of `merkle`
are confined to Flamechain's Utreexo module and tests; `cargo test --workspace`
passes.

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

## Explicit non-goals

- Bit-level payload APIs.
- TON levels, depths, exotic/pruned-branch Cells, Merkle proof/update Cells,
  and library-reference Cells.
- Multiple BoC variants, optional indexes, CRC, cache bits, or reference-index
  compression.
- Implicit network/database retrieval during consensus execution.
- A first-class raw Cell Value or Cell-manipulation opcodes in FlameVM.
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
