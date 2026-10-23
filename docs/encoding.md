# Cell-based encoding

This document owns Flame's application data layouts and identifier definitions.
Schemas use [CTL](../cells/ctl.md#type-reference); generic Cell records, hashing
and transport are specified in [Cells](cells.md). VM behavior is specified in
[FlameVM](flamevm.md), and human-readable formats in [Addresses](address.md).

## Primitives and containers

| Type | Encoding |
| --- | --- |
| `U8 / U16 / U32 / U64`, `V128`, `[T; N]`, `^T` | [CTL type reference](../cells/ctl.md#type-reference). |
| `Cell` | [Canonical Cell record](cells.md#canonical-cell-record). |
| Scalar | Canonical 32-byte LE residue below the [Scalar field order](flamevm.md#scalar-values). |
| Point / Predicate | 32-byte CompressedRistretto representation; [VM validation rules](flamevm.md#point). |
| Byte blob | Dedicated zero-ref Cell containing raw bytes, without a length prefix, within the [payload limit](cells.md#limits). |
| Trie | [Patricia Cell format](cells.md#trie); the owning schema fixes key width. |

CellID, pruning levels and transport use the [Cell identity](cells.md#identity),
[virtualization](cells.md#pruning-and-virtualization) and
[root-last graph transport](cells.md#root-last-graph-transport) definitions.

Schemas below share preceding declarations. Conditional refs use handwritten
codecs; CTL does not automatically spill fields into children. Applications
define explicit layouts for larger pure-data transfers.

## Values and Contracts

```ctl
type Hash = [U8; 32];
type Scalar = [U8; 32];
type Point = [U8; 32];

struct Token { qty: Point, flavor: Point }
struct ClearToken { qty: Scalar, flavor: Scalar }

enum Value: U8 {
    Scalar { value: Scalar } = 0,
    Dict { dict: ^Cell } = 2,
    Point { point: Point } = 3,
    Token { token: Token } = 4,
    ClearToken { token: ClearToken } = 6,
    Cell { cell: ^Cell } = 13,
}

struct DictHeader { count: U64, flags: U8 }
// Dict: header plus one Trie ref iff count != 0.

struct Contract {
    predicate: Point,
    anchor: Hash,
    payload: Value,
}
```

Dict keys reverse canonical Scalar bytes into 32-byte BE ordered paths.
The count and sticky flags belong to the Dict, not the Trie: portable bit 0,
droppable bit 1; other bits are invalid. No special list encoding exists.
Runtime representations, capability rules and admission checks are specified
under [Dict](flamevm.md#dict) and [Types](flamevm.md#types).

Other Value tags and runtime-only types are rejected, including reserved tag 1
(the removed String variant); existing tags are not renumbered.
ContractID is the Contract root's factual CellID. Typed import validation is
specified in [VM encoding](flamevm.md#encoding), and private annotations in
[CellValue](flamevm.md#cell).

## Programs and predicates

Programs are raw bytecode in Cell payloads, without a length prefix or automatic
spilling. Bytecode semantics are specified in the [Instruction set](flamevm.md#instruction-set).
Predicate representation is listed above; its Taproot binding and execution
are specified by [`open`](flamevm.md#open), and program authorization by
[`signcall`](flamevm.md#signcall).

## Actor addresses, state and messages

[actor.ctl](../flamevm/actor.ctl) is the canonical address schema:

```ctl
type ActorID = [U8; 32];

enum ActorAddress: U8 {
    Init { ctor: ^Cell } = 0,
    Address { addr: ActorID } = 1,
    Restore { addr: ActorID, state: ^Cell } = 2,
}

struct ActorState { code: ^Cell, data: ^Cell }

enum Caller: U8 {
    None = 0,
    Some { actor: ActorID } = 1,
}

struct Message {
    anchor: Hash,
    target: ActorAddress,
    caller: Caller,
    refund: Point,
    gas: U64,
    payload: ^Value,
}

// Application routing address; distinct from the ActorAddress destination.
enum Address: U8 {
    Predicate { predicate: Point } = 0,
    MessageTarget { dst: ActorAddress, gas: U64, payload: ^Value } = 1,
}
```

ActorID is 32 bytes without a tag or reversal; its derivation and routing
behavior are specified under [Actors](flamevm.md#actors). ActorState has empty
payload and two refs: code and a portable Value root.

Message payload is one tagged portable Value, without an implicit argument-list
Dict. MessageID is the Message Cell's factual ID. Delivery, uniqueness and
failure handling are specified under [Messages](flamevm.md#messages).
The routing `Address::MessageTarget` also carries one portable Value directly;
its encoder and decoder both reject nonportable payloads.

Actor registry and lease shapes:

```ctl
struct ActorRegistryHeader { count: U64 }
// Optional Trie ref; 32-byte BE ActorID keys.

struct ActorEntry { availability: Hash, slot: ^Cell }

enum ActorSlot: U8 {
    LeasesOnly { leases: ^Cell } = 0,
    Present { state: ^Cell, protocol: ^ActorProtocol } = 1,
}
struct ActorProtocol { code_bytes: U64, state_bytes: U64, leases: ^Cell }

struct LeasesHeader { count: U32 }
// Optional Trie ref; U64 BE expiry keys, U64 LE unit-count leaves.
```

Present stores the content root and protocol cell as two refs. Registry lifecycle
and storage charging are specified in [Actor storage](storage.md).

## Transactions and execution witnesses

[tx.ctl](../flamevm/tx.ctl) generates the transaction codecs; ProgramGraph is
the handwritten program wrapper:

```ctl
struct Tx {
    body: ^TxBody,
    signature: [U8; 64],
    r1cs_length: U16,
    r1cs_proof: [U8; r1cs_length],
}
struct TxBody { header: TxHeader, program: ^Cell, log: ^TxLog }
struct TxHeader { version: U32, locktime: U32 }
struct TxLog { entry_count: U64, entries: ^Cell }
// External logs are nonempty; generic empty logs omit the entries ref.
struct ProgramGraph { code: ^Cell, witnesses: ^Cell }
```

ProgramGraph witnesses use the [CellIndex snapshot layout](cells.md#a-snapshot-is-an-ordinary-cell-hierarchy).
Execution scope and witness discovery are specified under
[Resolution and witness context](cells.md#resolution-and-witness-context).

The transmitted log is a canonical mask-one pruning Cell retaining the computed
log's hash(0) and depth(0). It is checked by re-execution, not decoded as a log.
TxID = TxBody.id(); WitnessID = Tx.id(); EffectID = computed TxLog.hash(0).
Internal execution IDs use their factual computed logs, including Receive.

An all-zero signature means absent authorization; otherwise it must parse as
Schnorr. The inline R1CS proof is at most 4029 bytes.

### TxLog

TxLog entries use consecutive U64 BE Trie keys:

```ctl
enum TxEntry: U8 {
    Header { header: TxHeader } = 0,
    Data { blob: ^Cell } = 1,
    Input { contract: Hash } = 2,
    Receive { message: Hash } = 3,
    Output { contract: ^Contract } = 4,
    IssuePub { qty: Scalar, flavor: Scalar } = 5,
    IssuePriv { qty: Point, flavor: Point } = 6,
    Retire { qty: Point, flavor: Point } = 7,
    Fee { sparks: U64 } = 8,
    ActorSave { actor: ActorID, data: ^Cell } = 9,
    SetCode { actor: ActorID, code: ^Cell } = 10,
    Send { message: ^Message } = 11,
    StoragePurchase { actor: ActorID, bytes: U64, expiry: U64, fee: Scalar } = 12,
    ActorDestroy { actor: ActorID } = 13,
    ActorDeploy { actor: ActorID, code: ^Cell } = 14,
    ActorRestore { actor: ActorID, state: ^Cell } = 15,
}
```

Data uses the byte-blob layout above. ActorSave data is a Value root;
ActorRestore state is the supplied ActorState proof. External Tx transport
carries its root directly. Other typed CellEnvelope snapshots use
[the generic snapshot layout](cells.md#typed-object-snapshots).

## Blocks and Utreexo

```ctl
struct Block { header: ^BlockHeader, transactions: ^Cell, mint: ^Cell }
struct BlockTx { gas: U64, tx: ^Tx, proofs: ^Cell }
struct BlockHeader {
    version: U32, height: U64, core_block: Hash, core_height: U64, time: U64,
    parent: Hash, witnesses: Hash, mint: Hash, effects: Hash, contracts: Hash,
    actors: Hash, available_storage_units: U64,
}
struct Burn { reward_predicate: Point, sats: U64 }
// mint: a sequence of core blocks, each a sequence of Burn.
enum ExecutionKind: U8 { External = 0, Internal = 1, Failed = 2, Mint = 3 }
struct ExecutionRecord { kind: ExecutionKind, txid: Hash }
struct SequenceHeader { count: U32 }
// Optional Trie ref; consecutive U32 BE keys, expected-type leaves.
```

BlockHash is the BlockHeader CellID.
`time` is the minter's clock in Unix seconds when it built the block, not the
core block's time; it may not be less than the parent's. Several consecutive
blocks (iblocks) may name the same `core_block`.
A block either stays on its parent's `core_block` and `core_height`, with an
empty mint proof, or moves to a later height and a different hash, with one
entry per core block it moves over: entry `i` lists the burns of core block
`parent.core_height + 1 + i`. Each burn has a nonzero `sats` and a valid
point. Every core block issues `allotment(h)` sparks: 50 FLM halved every
210,000 core blocks, split among its burns by `sats`, each share floored and
the leftover sparks given one each to the largest remainders (earlier burn
first on ties). A core block without burns issues nothing. The block runs one
`Mint` execution before its transactions, unless nothing is issued: a log of
a version-1, locktime-0 header and one cleartext Flame output per reward
predicate, in order of first appearance, summed over the proof. Output `i`'s
anchor is the Merlin transcript `flame.mint` with `core_height` (the
header's) and `index` (`i`) as U64s, challenged as 32 `anchor` bytes.
Whether the burns are the ones the core chain holds is checked by the node
against its core indexer, as `core_block` is.
Witness root is the transaction sequence's factual ID; mint root is the mint
proof's; effects root is the execution-record sequence's factual ID. Block transport uses CellEnvelope.

Utreexo proofs keep their existing `merkle / readerwriter` byte encoding,
stored as raw payloads in dedicated zero-ref Cells inside BlockTx, with no
additional length prefix. Each depth-63 proof fits the 4095-byte Cell limit.
The legacy Proof decoder must consume the entire blob, rejecting trailing bytes.
These Cell layouts change consensus identities; no legacy decoder or state
migration is provided.
