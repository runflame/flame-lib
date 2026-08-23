# FlameVM

This is the canonical design and specification of FlameVM. Explanatory sections
give rationale; exact encodings, stack diagrams, and statements using **must** or
**must not** define consensus behavior.

## Introduction

FlameVM implements transaction verifications rules in the Flame blockchain in a form of a Forth-like stack machine.

FlameVM is used in two different contexts: external transactions and internal transactions. External transactions have user-defined results, zero-knowledge proof for confidential transfers and compute, and have access to Utreexo storage: scalable compressed set of unspent transaction outputs. Internal transactions operate on uncompressed storage and invoke multiplayer smart contracts that react to user-defined messages without pre-determined results.

FlameVM operates on multiple data types, including linear types for tokens and zero-knowledge expressions. Types can be “portable” and “non-portable”, “copyable” and “non-copyable”. Portable types can exist in the long-term blockchain state outside of VM execution. Copyable types can be duplicated during VM execution.

Successful VM execution equals to successful transaction verification. Therefore, VM execution encodes both built-in network rules, as well as enables authors to create custom rules within their applications.


## Design overview

Flame combines single-use **cells** with persistent stateful **actors**. One
stack machine verifies both: external transactions consume and create cells and
may emit messages; internal transactions deliver messages and synchronously call
actors.

Values have explicit copy and drop capabilities. Bearer values—including tokens
and cells—are linear. Portable values may cross cell, message, and actor-state
boundaries; ephemeral verification values such as transcripts, expressions,
constraints, and MSMs may not.

The VM emits an ordered log of state effects. Inputs, outputs, issuance,
retirement, fees, sends, actor saves, storage purchases, actor destruction, and
explicit data are effects. Calls, branches, signatures, constraints, and batch
verification are execution machinery and do not appear in the log. TxID commits
to the ordered effects.

Every `open`, `signcall`, and actor `call` runs in an isolated frame with its own
stack, control flow, and gas budget. Successful calls return only explicitly
selected values. Failed calls restore state effects, constraints, deferred
signatures, fees, and batch-verification work to their entry checkpoints. They
also return entry-owned values: actor calls return their arguments, while cell
calls return the original locked Cell followed by their explicit arguments.
All downward call arguments must be portable, just like asynchronous `send`
payloads. Return values are unrestricted: non-portable liabilities and
VM-local values may travel upward so the caller can resolve them, but they may
not be delegated to another callee.

Actor state is its re-entrancy lock. `load` moves state out of the registry;
while checked out, another frame cannot enter or observe that actor. Calling
before `load` is safe because no partial state exists. Authors must still avoid
holding a stale loaded snapshot across a call and then saving it: the VM prevents
re-entrant observation, not application-level check-then-act mistakes.

Persistent actor storage is purchased in one-year leases by the actor itself.
Purchases burn Flame at a deterministic reserve price; storage is neither a
token nor transferable between actors. See [Actor storage](storage.md).

Execution memory is independent of persistent actor storage. There is no
separate memory-limit operand or storage-derived RAM allowance. Variable-size
allocation work is charged to the active frame's gas before allocation; these
charges are monotonic, so the gas cap bounds hostile allocation even after
values are dropped. Calls are bounded to 64 nested frames. Instruction fetch,
EOF checks, and forward label scans consume gas; a failed entered call burns its
grant, while an availability failure before entry refunds it.

External transaction effects are atomic and independently verifiable. Internal
transactions execute serially because they share actor state. Expired leases
return to the byte pool; an actor is destroyed when an expiry leaves it with
less capacity than its occupied storage.

All Flame-defined multi-byte integers are little-endian. Decoders reject
alternate-width integers, unordered or duplicate dictionary keys, excessive
nesting, unknown tags, and trailing data where a complete value is required.


## External transactions

External transactions are composed directly by users and have pre-determined outcome. They consume and produce transaction outputs, modifying the set of *unspent transaction outputs.* Each output can be unlocked by one or more signature and the entire transaction contains a zero-knowledge proof of confidential transfers and other constraints on encrypted data imposed by smart contracts.

External transactions are best used for private transactions between few parties.

External transactions produce the following effects:

1. Inputs — consumption of entries from Utreexo that produce *cells* on VM stack.
2. Outputs — creation of new entries in the Utreexo.
3. Sends — messages sent to actors that produce *internal transactions*.
4. Fee — payment of the transaction fees.
5. Issuance and retirement — creation and removal of tokens to/from circulation.
6. Data entry — for data logging that does not occupy permanent storage.

Transaction ID (”TxID”) is a hash (merkle root) of the list of all the effects. Transaction signature and ZK proofs both bind to TxID and therefore to all the effects of the transaction.

## Internal transactions

Internal transaction is caused by a “message” sent to an “actor” by an external transaction. Such message does not control the outcome: internal transaction may produce undetermined result or even fail. As such, race conditions (when multiple users send messages to the same actor) can be resolved by the actor itself, accepting all the messages.

Internal transactions are best used for public multiplayer apps, where an actor can be accessed by anyone in any order.

Internal transactions do not have a pre-determined effect and therefore do not support signatures and ZK proofs bound to transaction ID.

Like external, internal transactions produce effects:

1. `Receive(MessageID)` (txlog variant `TxEntry::Receive`) — the consumed Send's id. Emitted automatically as the first effect after `Header` by `VM::execute_internal`, committing the originating `MessageID` (canonical 32-byte hash of the whole Send: anchor, target, caller, payload, gas, refund predicate — analogous to `CellID` for cells) into the Internal TxID merkle root. Symmetric with `Input` for external transactions. The message **payload is delivered onto the recv frame's stack** in payload order before code runs — symmetric with `op_call` pushing its args; a conventional dispatch selector rides as the topmost payload arg.
2. Outputs — creation of new entries in the Utreexo.
3. Sends — messages sent to actors that produce other internal transactions.
4. Issuance and retirement — creation and removal of tokens to/from circulation.
5. Actor-state mutations — `op_save` records the actor's post-save state hash, allowing a state machine to mutate the registry without re-running the script.
6. Data entry — for data logging that does not occupy permanent storage.
7. Storage purchases — `addstorage` records the actor, purchased bytes, expiry
   height, and burned sparks.

Actor destruction caused by lease expiry is represented by a system internal
transaction. It has `ActorDestroy(actor)` instead of a
`Receive` as its identifying effect, binds the expiry height, and contains a
`Retire` effect for every token recursively removed from state. Destruction
transactions are ordered lexicographically by actor ID. See
[Actor storage](storage.md#global-state-and-block-order).

Calls themselves are intra-transaction control flow, not effects. Anything a callee does that the outer world cares about appears through one of the effects above.

**TxLog transport.** The TxLog (`Vec<TxEntry>`) is **re-derived by re-executing** the bytecode under the proof + signature binding — it is never trusted from the wire. `ExternalTx::verify` re-runs the script and rebuilds the log from scratch, so full-node consensus needs no TxLog decoding and a forged TxLog cannot be injected. For storage and light-client transport the crate provides a canonical **encode-only** serialization (`Encodable` for `TxEntry`/`TxLog`): a u64-LE entry count, then per entry a tag byte (sequential in declaration order: `0 Header, 1 Data, 2 Input, 3 Receive, 4 Output, 5 IssuePub, 6 IssuePriv, 7 Retire, 8 Fee, 9 ActorSave, 10 SetCode, 11 Send, 12 StoragePurchase, 13 ActorDestroy`) followed by the variant's fields in their existing canonical forms — `Cell`/`Message`/`ActorID` encoders, `write_value`/`write_int253`, u32/u64 little-endian integers, u64-LE length-prefixed byte blobs. `StoragePurchase` encodes actor, bytes (`u64`), expiry height (`u64`), and fee sparks (`Int253`); `ActorDestroy` encodes its actor.

Internal transactions do not pay transaction-prioritization fees. Actors may
nevertheless burn Flame for storage through `addstorage`. Internal transactions
do not support inputs, as those can be consumed only by external transactions
with a Utreexo proof and, most of the time, a transaction signature.

## Limits

**Gas limit:** maximum amount of gas used per block. Each tx and sum of all txs in a block cannot exceed that amount of gas.

**Script size limit:** maximum total size of scripts in each tx and all txs in a block.

**Multiplications limit:** maximum Bulletproofs multiplications per block and per tx.

**Gas credit:** maximum amount of gas used by external transaction without additional gas allocated for message sends.

**Issued storage:** amount added to the available storage pool by each core block.

## Fees

Transaction fees are paid by external transactions and are necessary to prioritize common resources on the open network and mitigate denial-of-service attacks. As blockchain imposes limits on storage and computation costs, transactions paying higher fees (per resource used) are prioritised over transactions paying lower fees.
BFT consensus implies that the block candidate is progressively built and already included transactions are not pruned by higher-paying ones.

Transaction fees are paid in Flame and cover computation for the external
transaction and subsequent calls within internal transactions. Actor storage is paid separately by burning
Flame through `addstorage`. 

An external transaction pays for its own execution and prepays every gas grant
attached to its direct `send` effects. An internal transaction executes inside
that grant and prepays any descendant sends from the same budget, so a message
tree can divide gas but cannot amplify it.

Unused gas in a message send is discarded: transaction commits to full amount of gas before doing a message send. Unused gas in a call (within internal transaction) remains with the caller and therefore not lost. 

Every `call`, `open`, and `signcall` has an explicit gas operand. There is no
implicit default grant; a caller that wants to delegate its remaining budget
must read `gas` and pass the desired amount explicitly.

One Flame equals `100_000_000` sparks. Native token quantities and fees are
integral sparks. 

Storage-purchase pricing and issuance are specified in
[Actor storage](storage.md).

## Types

Ownership: every type on the stack is always owned. FlameVM does not allow reference-counting, borrowing, read-only access or implicit copies.

Plain data types: integers, byte strings, Ristretto points. These can be copied and ported.

Structured data types: dicts that are used as lists, dictionaries and enum variants. Dicts are **never copyable** and cache sticky portability and droppability capabilities (see §Dict).

Token types: WideToken, Token, ClearToken.

Constraint types: Object, Variable, Expression and Constraint.

Cryptography types: Merlin transcript. (Batched scalar-point
checks are not user-visible — see the note on `MultiscalarMul`
below.)

Portable types: can be stored in a UTXO or permanent storage.

Copyable types: can be copied or dropped.

Portability is a business-logic capability, not a serialization property.
Generic value codecs describe representation only. For example, a negative
`ClearToken`, or a Dict containing one, may be encoded and decoded for
diagnostics even though Cell, Message, call, and actor-state admission reject
it. A type may also be non-portable and have no implemented value encoding;
these are separate questions.

Tuples are not distinct types, but a name for passing multiple items on the stack and through calls. During VM execution any tuple is simply a number of items on stack.

Optionals are not distinct types, but a convention to return tuple (values…, 1) or (0). Instruction `verify` can be used to "unwrap optional" and fail immediately if the result is missing.

**Implementation layout (not consensus).** Heap-indirection keeps the Rust
`Value` enum compact without changing VM semantics: `Merlin` boxes its
`Transcript`, `Value::Cell` boxes the otherwise unchanged `Cell`, secret
equality constraints box both `Expression`s, and `String` boxes all typed
`StringWitness` variants while leaving `Opaque(Vec<u8>)` inline, and
`spacesuit::AllocatedValue` boxes its optional cleartext assignment. On the
current 64-bit build this makes `Value` 80 bytes, equal to its largest remaining
inline variant (`Token`), while `WideToken` is 40 bytes. Rust layout and byte
counts are implementation details; the canonical wire representation is
unchanged.

Value tag allocation. A reserved tag does not imply that an encoder exists:

| Type | Tag(s) | Description |
| --- | --- | --- |
| Int253 | 0..=67 | Signed sign-magnitude integer; magnitude is a canonical Ristretto scalar (< ℓ ≈ 2²⁵²) plus an explicit sign bit. The name reflects the effective conceptual width: ⌈log₂ ℓ⌉ = 253 bits of magnitude. |
| String | 68..=127 | Variable-length byte string. |
| Dict | 128..=247 | Map from Int253 keys to values. List-style encoding (sequential keys 0..n-1) uses 128..=187; explicit-key form uses 188..=247. Its sticky `portable` flag starts true and is cleared by insertion of a non-portable value. |
| Point | 248 | Element of the Ristretto255 group. |
| Token | 249 | Linear type (qty, flavor) representing an asset value, possibly encrypted. |
| ClearToken | 250 | Linear type (qty, flavor) with cleartext values. Portable when non-negative. Both signs have the same representation; a negative `ClearToken` is a non-portable intermediate rejected at domain admission. |
| WideToken | 251 | Allocated tag; no current value encoding. The type is a CS-bound, possibly-negative token. |
| Object | 252 | Allocated for a future object value; no current `Value` variant or encoding. |
| Merlin | 253 | Allocated tag; no current value encoding for transcript state. |
| (reserved) | 254 | Reserved tag. |
| (extension) | 255 | Extension prefix; sub-tag follows. |

Stack-only types (non-portable and without an implemented `Value` encoding):

| Type | Description |
| --- | --- |
| WideToken | Possibly-negative encrypted token tied to the current constraint system. |
| Merlin | Mutable transcript state tied to the current execution. |
| Variable | Secret value in the constraint system, tied to a Pedersen commitment. |
| Expression | Linear combination of variables. |
| Constraint | Logical combination of boolean conditions. |
| MultiscalarMul | Lazy `sum(s_i · P_i)` accumulator; consumed by `verify` which appends it to the same batch as Schnorr/Musig sigs (assertion: `sum == identity`). |

### Encoding

Every value supported by the generic codec has exactly one canonical byte
sequence. The first byte is a type+width tag; within each type, width classes
carve the value range into disjoint, offset-based sub-ranges so the encoder has
no choice about which tag to use.

The generic codec does not enforce portability. It accepts representable
non-portable values, including negative `ClearToken`s and Dicts whose current
members are representable but non-portable. Admission into a Cell, Message,
synchronous actor call, or actor state is checked by that domain's
business-logic boundary. At the `readerwriter` layer, writes fail only when the
destination lacks capacity, so encoding an admitted, representable value into
a `Vec<u8>` is infallible. VM-only variants without an implemented encoding are
outside this codec's input domain rather than rejected for being non-portable.

**Group-element validation is lazy.** `Point` (`POINT_TAG`) and `Token` (`TOKEN_TAG`) bytes are accepted at decode **without** Ristretto decompression — they land as `Point::Opaque` / `Commitment::Closed` and are validated only at first cryptographic use (signature batch, `verify_taproot_proof`, CS decompression), where an invalid element fails the proof. A structurally-invalid group element may therefore sit inside a committed Cell / actor-state / Send until used; this is deliberate (it preserves byte-identical prover/verifier round-tripping and keeps decode allocation-free), not a malleability hole — the 32 bytes are canonical, and any non-decompressable element is unusable. **Length-prefixed counts (list/dict/string payloads) are always bounded against remaining input before allocation** (`Cell::decode` payload count, `pushstr`/`ActorID` lengths), so a small hostile prefix cannot force a large allocation.

Tag namespace (one byte, 256 values total):

```
0..=58     Int253 positive immediate (value = tag)
59         Int253 +U8     (1-byte payload b; value = 59 + b;  range 59..=314)
60         Int253 +U32    (4-byte payload w; value = 315 + w; range 315..≈4.3e9)
61         Int253 +U64    (8-byte payload w; value = 4_294_967_611 + w)
62         Int253 +FULL   (32-byte canonical scalar; value > U64 range)
63         Int253 -1
64         Int253 -U8     (value = -(2 + b);   range -2..=-257)
65         Int253 -U32    (value = -(258 + w))
66         Int253 -U64    (value = -(4_294_967_554 + w))
67         Int253 -FULL   (32-byte sign-magnitude; magnitude > U64 range)
68..=126   String immediate length (length = tag - 68; range 0..=58)
127        String VAR     (sub-varint v; length = 59 + v)
128..=186  List-style Dict immediate count (count = tag - 128; range 0..=58)
187        List-style Dict VAR (sub-varint v; count = 59 + v)
188..=246  Dict explicit-keys immediate count (count = tag - 188; range 0..=58)
247        Dict explicit-keys VAR (sub-varint v; count = 59 + v)
248        Point      (32-byte compressed Ristretto)
249        Token      (32-byte qty commitment point + 32-byte flv commitment point)
250        ClearToken (cleartext qty Int253 + flv Int253)
251        WideToken  (assigned tag; Value encoding not implemented)
252        Object     (assigned tag; Value encoding not implemented)
253        Merlin     (assigned tag; Value encoding not implemented)
254        reserved
255        extension (sub-tag follows)
```

A list-style Dict (tags 128..=187) is used when keys are exactly `0..n-1`; the keys are omitted from the wire and reconstructed at decode. An explicit-keys Dict payload whose keys turn out to be `0..n-1` is rejected at decode time, since it has a shorter list-style encoding.

Sub-varint (used inside `STR_VAR` / `LIST_VAR` / `DICT_VAR`):

```
sub-tag 0  1 LE byte    value = b              range 0..=255
sub-tag 1  2 LE bytes   value = 256 + w        range 256..=65_791
sub-tag 2  4 LE bytes   value = 65_792 + w     range 65_792..≈4.3e9
sub-tag 3  8 LE bytes   value = 4_295_033_088 + w
```

Canonicality checks performed at decode time:

1. `INT_PFULL` / `INT_NFULL`: the 32-byte payload encodes the value as-is. The decoder rejects values that could have been encoded in a narrower width class.
2. `DICT_*` payload whose keys are `0..n-1` is rejected — the list-style encoding is shorter.

#### Canonical encoding vectors

Hex strings below contain no separators and all multibyte integers are little-endian. `xx×N` means byte `xx` repeated `N` times; `||` means concatenation. These vectors are also pinned by the Rust tests.

Sub-varint width boundaries:

| Value | Hex |
| ---: | --- |
| 0 | `0000` |
| 255 | `00ff` |
| 256 | `010000` |
| 65,791 | `01ffff` |
| 65,792 | `0200000000` |
| 4,295,033,087 | `02ffffffff` |
| 4,295,033,088 | `030000000000000000` |
| `u64::MAX` | `03fffefefffeffffff` |

Container prefix boundary (payload omitted):

| Prefix | Count | Hex |
| --- | ---: | --- |
| String | 58 | `7e` |
| String | 59 | `7f0000` |
| List Dict | 58 | `ba` |
| List Dict | 59 | `bb0000` |
| Explicit Dict | 58 | `f6` |
| Explicit Dict | 59 | `f70000` |

`Int253` width boundaries:

| Value | Hex |
| ---: | --- |
| 0 | `00` |
| 58 | `3a` |
| 59 | `3b00` |
| 314 | `3bff` |
| 315 | `3c00000000` |
| 4,294,967,610 | `3cffffffff` |
| 4,294,967,611 | `3d0000000000000000` |
| `4,294,967,611 + 2^64 - 1` | `3dffffffffffffffff` |
| `4,294,967,611 + 2^64` | `3e3b01000001000000010000000000000000000000000000000000000000000000` |
| -1 | `3f` |
| -2 | `4000` |
| -257 | `40ff` |
| -258 | `4100000000` |
| -4,294,967,553 | `41ffffffff` |
| -4,294,967,554 | `420000000000000000` |
| `-(4,294,967,554 + 2^64 - 1)` | `42ffffffffffffffff` |
| `-(4,294,967,554 + 2^64)` | `430201000001000000010000000000000000000000000000000000000000000080` |

One vector for every currently supported value tag:

| Value | Hex |
| --- | --- |
| `Int253(7)` | `07` |
| `String(aabb)` | `46aabb` |
| list Dict `{0: 1, 1: 2}` | `820102` |
| explicit Dict `{2: 3}` | `bd0203` |
| `Point(11×32)` | `f8 || 11×32` |
| `Token(qty=22×32, flv=33×32)` | `f9 || 22×32 || 33×32` |
| `ClearToken(qty=-1, flv=0)` | `fa3f00` |

Canonical `pushint` opcode width boundaries:

| Value | Bytecode hex |
| ---: | --- |
| 0 | `00` |
| 15 | `0f` |
| 16 | `1010` |
| 255 | `10ff` |
| 256 | `120001` |
| 65,535 | `12ffff` |
| 65,536 | `140000010000000000` |
| `2^64 - 1` | `14ffffffffffffffff` |
| `2^64` | `1600000000000000000100000000000000` |
| `2^128 - 1` | `16ffffffffffffffffffffffffffffffff` |
| `2^128` | `180000000000000000000000000000000001000000000000000000000000000000` |
| -1 | `1101` |
| -255 | `11ff` |
| -256 | `130001` |
| -65,535 | `13ffff` |
| -65,536 | `150000010000000000` |
| `-(2^64 - 1)` | `15ffffffffffffffff` |
| `-2^64` | `1700000000000000000100000000000000` |
| `-(2^128 - 1)` | `17ffffffffffffffffffffffffffffffff` |
| `-2^128` | `180000000000000000000000000000000001000000000000000000000000000080` |

### Int253

Flame integers are signed sign-magnitude scalars: the magnitude is a canonical Ristretto255 scalar (strictly less than ℓ ≈ 2²⁵²), and the high bit of the 32-byte in-memory representation is the sign. The conceptual width is 253 bits of magnitude plus a sign.

In memory, integers are fixed 32-byte arrays with bit 255 (the high bit of byte 31) as the sign and the lower 255 bits as the canonical scalar magnitude. Negative zero is never representable.

Integers are used in logical operations: non-zero is “true”, zero is “false”. Logical results are always 0 or +1.

Dicts use Int253 keys; numeric ordering of keys is total and unambiguous.

### String

Binary byte-aligned strings.

Strings are used to represent arbitrary-length binary data, programs and cryptographic signatures and hashes. Other types can be parsed from strings.

Each string acts as a builder and a reader.

**Prover-side witness variants.** `String` has two in-memory shapes: `String::Opaque(Vec<u8>)` and `String::Witness(Box<StringWitness>)`. Boxing the uncommon witness shape keeps every `String` the size of a `Vec`. The witness encodes to the same canonical bytes the verifier sees while preserving typed data through `dup` / `open` / `signcall` / `call` / payload-pour boundaries:

- `StringWitness::Point(Point)` — point-shaped witness; the inner [`Point`](#point) is `Opaque`, `Commitment(Open(value, blinding))` (used before `commit`, `scalar`, `expr`), or `Predicate(p)` (used before `signtx`, `signcall`, `cell`, `output`).
- `StringWitness::Scalar(int)` — used before `scalar`.
- `StringWitness::Script(instructions)` — used before `open` / `signcall` / `call`.
- `StringWitness::Cell(c)` — used before `input`, carrying open commitments on Token payloads.

The verifier always sees `String::Opaque(bytes)`; the downcasts (`to_commitment`, `to_scalar`, `to_predicate`, `to_instructions`, `to_cell`) handle both shapes uniformly. There is no separate witness queue or per-opcode witness operand — witnesses ride on the pushed value itself.

**`pushpoint` also carries witnesses.** Because the `pushpoint` instruction's in-memory operand is a `Point` (not a raw `[u8; 32]`), the prover can attach a `Point::Commitment` / `Point::Predicate` witness to a literal point pushed via `pushpoint` — not only via `pushstr` + `String::point`. The wire encoding stays the canonical 32 bytes regardless.

### Point

Ristretto255 group element. Stored as compressed 32-byte encoding. Used to represent public keys, Pedersen commitments, and Taproot predicates.

**Prover-side variants.** On the prover side a Point may carry typed witness data while still serializing to the same canonical 32 bytes:

- `Point::Opaque(CompressedRistretto)` — verifier's view; no witness.
- `Point::Commitment(Commitment::Open(value, blinding))` — Pedersen commitment with cleartext opening (used by [`commit`](#commit) / [`expr`](#expr) to skip re-opening).
- `Point::Predicate(PredicateTree)` — Taproot predicate with merkle tree (used by [`cell`](#cell) / [`output`](#output) to attach an unlock witness, and by [`signtx`](#signtx) / [`signcall`](#signcall) for the verification key).

A Point on the value stack downcasts via `to_commitment` / `to_predicate` to extract its witness (preserved through `Point::Commitment` / `Point::Predicate`) or to wrap an `Opaque` as the verifier's `Closed` / `Opaque` form.

### MultiscalarMul

Lazy multi-scalar-multiplication: a vector of `(scalar_i, point_i)` pairs that the VM defers as the assertion `sum(s_i · P_i) == identity`. Linear (non-copyable, non-droppable), stack-only, **not** wire-encodable — exactly like [Expression](#types) and [Constraint](#types).

**Purpose: custom Sigma-protocols.** Together with [Merlin](#cryptography-instructions) transcripts (Fiat–Shamir challenges), `MultiscalarMul` lets contract authors express any Schnorr-style relation over Pedersen-committed data — proof of knowledge of discrete log, equality of two encryptions, proof of correct re-encryption, etc. The verification equation always reduces to "this weighted sum of group elements is the identity point".

**Batched verification.** `verify` on an MSM does **not** decompress or check anything immediately — it appends the term vector to the same `BatchVerifier` that holds the transaction's Schnorr / Musig signatures (with `basepoint_scalar = 0` so the MSM contributes only its dynamic terms). At finalize the entire batch is verified with a single Dalek `vartime_multiscalar_mul` (Strauss algorithm), amortising the ~4× speedup of batched MSM across every Sigma-protocol assertion and every signature in the transaction.

**Witness-gated failures are fail-closed.** A few opcodes can fail on the *prover* over data the verifier lacks (e.g. an out-of-`u64` range-proof assignment, a `commit`-then-`expr` over an opaque commitment with no witness). Because the proof binds the **entire** constraint system through Fiat–Shamir, any prover/verifier control-flow or CS divergence — including one caused by such a prover-only failure being caught as a sub-call `0` marker — makes the proof **fail to verify**: the verifier rejects, it can never *accept* an invalid transaction. The effect is a self-inflicted liveness edge (a prover that commits to a malformed witness produces an unverifiable tx), not a soundness break. Making such failures tx-level (uncatchable) so they never reach a marker is a deliberate liveness-hardening item, tracked for ZK review.

**Batch rollback under call failure.** The batch is a verifier-side optimization, not part of the consensus semantics — the on-chain `TxLog` and proof shapes are unchanged. The delegate owns a single `BatchVerifier` for the whole tx, with its RNG. Every opcode that appends to it (today: `op_verify` on a `MultiscalarMul`) calls `BatchVerification::append`, which multiplies the appended statement by a fresh random scalar drawn from the delegate's RNG — Schwartz–Zippel safety against canceling failures. For per-frame rollback the VM snapshots the batch state (basepoint scalar + dyn-arrays length) on the *parent* frame at every call entry (`call` / `open` / `signcall`), via `BatchCheckpoint::snapshot`. On call failure, `fail_current_call` restores the batch via `BatchCheckpoint::restore` — truncating the dyn-arrays and resetting the basepoint scalar — so any MSM appended by the failed callee is dropped. On clean return the snapshot is discarded; the child's appends stay. The RNG state is *not* rewound; random factors sampled between snapshot and restore are simply lost, which is harmless — the remaining batch terms still carry the random factors that were sampled for them.

**Construction.** MSM has no dedicated constructor opcode. Instead, the arithmetic opcodes lift Point/MSM operands implicitly:

| Operation | Result |
|---|---|
| `Point + Point` | MSM with two unit-scalar terms |
| `Point + MSM` / `MSM + Point` | MSM with the point appended (coefficient 1) |
| `MSM + MSM` | concatenated term lists |
| `Int253 * Point` / `Point * Int253` | MSM with one term `(int_as_scalar, point)` |
| `Int253 * MSM` / `MSM * Int253` | MSM with all coefficients scaled |
| `-Point` | MSM with one term `(-1, point)` |
| `-MSM` | MSM with all coefficients negated |

Quadratic group-element products (`Point * Point`, `MSM * Point`, `MSM * MSM`) hard-fail `TypeNotInt253` — no Sigma-protocol semantics.

**Random factor.** The `BatchVerifier` multiplies each appended statement (MSM, single-sig, multi-sig) by a fresh random scalar before summing, so a failing MSM cannot be cancelled out by other batch members (probability `< 2^-252` per statement). See `starsig::BatchVerification` for the exact construction.

### Dict

Dict is a versatile data structure for representing lists, dictionaries and even sum-type (aka “enum”) values. One-key struct is used to encode a single variant of a sum-type.

Keys are non-negative Ints to keep ordering non-ambiguous.

**Dicts are never copyable** (todo #5): `dup`/`getdup` of a Dict value always fails `TypeNotCopyable`. Each Dict carries independent sticky `portable` and `droppable` flags. A newly constructed empty Dict starts with both flags true. Every successful insertion applies `dict.portable &= value.is_portable()` and `dict.droppable &= value.is_droppable()`. Removal and replacement never restore a cleared flag; a rejected strict insertion does not change either flag.

The `dict` / `put` / `replace` opcodes may insert any Value. A non-portable Dict remains usable on the stack, but `cell`, `output`, `send`, `call`, `open`, `signcall`, and actor-state storage reject it at their portability boundary. Checking a Dict is O(1), including when it is nested: inserting a nested Dict reads that Dict's already-cached flag. Dict values are owned and cannot be mutated through an alias, so the cached parent flag cannot become stale.

The flags are runtime metadata and are not serialized. Both Dict byte forms
reconstruct them bottom-up from the members that are decoded. This preserves
the capability of a Dict that still contains a non-portable member, but not its
history: if a Dict was poisoned and the offending member was removed, a debug
encode/decode round-trip produces a fresh Dict whose flag reflects only the
remaining members. Persistent domains reject the sticky-false Dict before
encoding, so serialized Cell, Message, and actor-state values never rely on
historical taint surviving the byte representation.

Drilling down the nested dict preserving ownership with `get` and `put` instructions: 

```rust
// Given a struct like this want to drill down "a",
// then "b" and update balance.
{ 1: {2: balance } }

// Program:
1 get 2 get ... put put
```

### Tokens

All token types are linear types: non-copyable and non-droppable.

The native Flame flavor is `FLAME_FLAVOR = 0`. One Flame is exactly
`100_000_000` sparks; all native-token quantities on the stack and wire are
integral sparks.

WideToken: encrypted token without a range proof on quantity (could be negative). Non-portable: cannot be stored.

Token: encrypted token with a proven non-negative quantity. `Token` is portable
by construction: all construction paths must establish the range/provenance
invariant before creating the type. `Value::is_portable` therefore does not
inspect a commitment opening. In particular, portability cannot depend on
whether the prover holds `Commitment::Open` while the verifier holds the same
point as `Commitment::Closed`.

## Actors

An actor is **`(code, state)`**:

- **code** — a single bytecode blob, set at deploy and replaced by [`setcode`](#setcode). Dispatch is selector-agnostic: the registry returns the blob (`load_code`); by convention the blob dispatches on its **top-of-stack argument**. The VM imposes **no method layout** and reserves nothing.

- **state** — **any portable `Value`** (Int253, String, Point, Token, Dict, …), with no mandated shape and no methods-in-state. [`load`](#load) checks it out, [`save`](#save) moves it back; the author structures it however they like.

Each actor is identified by a unique Actor ID derived from its constructor
script. `Hash(h)` and `Constructor(bytes)` route to the same registry key when
`h = H(bytes)`, so the identity commits to the initial code. Their exact wire
forms are distinct:

```text
Hash(h)            = 0x00 || h[32]
Constructor(code)  = 0x01 || len(code):u64-le || code
```

The constructor form carries the code needed for deployment; the hash form is
the compact address used afterward. A VM actor-destination operand is a String
containing either of those encodings. For compatibility it also accepts a bare
32-byte String as `Hash(bytes)`.

**Deploy-on-first-delivery.** The first message delivered to a not-yet-deployed
`Constructor`-form target instantiates a provisional actor: code is the
constructor bytes (self-authorized because the id commits to them) and state is
empty. Its constructor may use `addstorage` to buy its first lease. The actor is
committed only if its storage capacity covers its usage when the transaction
ends. A `Hash`-form target that does not exist fails `ActorNotFound` because the
hash alone carries no code. Deployment occurs only on asynchronous delivery;
`call` canonicalizes a constructor-form destination for lookup but never deploys
it.

## Messages

Messages execute “method calls” asynchronously. Each message contains a predicate for bouncing its arguments in case of actor failure. If the method called through a message returns any values, the call fails and the original arguments are bounced.

Messages are unique by construction, not by a duplicate-message table. Each
accepted `send` embeds the left child of the current anchor ratchet and leaves
the right child for subsequent execution. Calls split disjoint callee and
caller-continuation subtrees. A failed delivery rolls back its actor effects and
produces one Cell containing the original payload under `refund_predicate`; it
does not enqueue another Message. Failure to apply that refund rejects the
whole candidate, so the originating Send does not commit.

A Message's payload is immutable and is created through a checked constructor.
Construction scans its top-level values and rejects any non-portable item;
checking a nested Dict is O(1) through its sticky `portable` flag. Message
encoding is representation-only and relies on this prior asynchronous-domain
admission.

Not only users, but also actors can send async messages to each other. This allows an actor to commit intermediate results between transactions.

## Addresses

Address is an entity that supports sending funds to predicates or actors.

```rust
Address = enum {
   0: Predicate,
   1: Message = struct {
      dst#0: ActorID,
      args: struct{0:...,1:...},   // selector (if any) rides as the top arg — the design
      gas: Int253,
   }
}
```

## Storage

Persistent-storage allocation, pricing, leases, expiry, destruction, and
introspection are specified in [Actor storage](storage.md).

**Execution memory.** Persistent storage capacity does not grant execution RAM.
The frame's gas cap is the only execution-resource budget: byte-sized and
collection-sized growth is charged before allocation, and freeing a value does
not refund allocation gas. This deliberately accounts logical work rather than
Rust allocator layout, while still proving a gas-derived upper bound on hostile
growth.


## Anchors

Every new cell and message is **anchored** by a unique 32-byte value embedded in its wire form, so two cells with the same predicate + payload but different anchors hash to different ids. Uniqueness is the core safety property — without it, cell ids collide across txs and authors can be tricked into operating on the wrong entity.

**Uniqueness source.** Anchors are unique only when they descend from a *spend-once source*: a UTXO consumed via [`input`](#input), or the `anchor` of a Message that triggered an internal tx (which is itself a split-child of an external tx's `op_send`). Cell ids are themselves anchored (`Cell::id = H(predicate, anchor, payload)`), so each input cell's id is unique on the network. Any anchor derived deterministically from such a source remains unique.

**Per-tx slot.** The VM carries a single `last_anchor: Option<Anchor>` for the entire transaction. It is:
- `None` at the start of an external tx — [`input`](#input) is the only way to seed it.
- `Some(M)` at the start of an internal tx, where `M` is the delivering Message's anchor (a split-child from the originating external tx's `op_send`).

Calls split the anchor at entry. `op_call`, `op_open`, and `op_signcall` each split `last_anchor` into `(left, right)` at frame entry. The child's frame starts with `last_anchor = left`; the parent frame stashes `right` in its `post_call_anchor` slot, and `last_anchor` is restored to `right` when control returns (success or failure). This keeps the caller's anchor chain independent of whatever the callee does with its own half.

**Splitting.** Each opcode that produces a new unique-anchored *cross-tx* entity (cell-on-wire, message-to-actor) consumes `last_anchor` and replaces it with a fresh derived value. The split is a single Merlin transcript:

```
t = Transcript::new(b"flamevm.anchor.split");
t.append_message(b"parent", &last_anchor.0);
left  = t.challenge_bytes(b"left",  32);
right = t.challenge_bytes(b"right", 32);
```

The `left` half is embedded in the new entity (cell anchor / MessageID); the `right` half replaces `last_anchor`. Authors never see `left` and `right` separately — the VM picks them automatically at the consume site.

**Sites that consume + split**: [`cell`](#cell), [`output`](#output), [`send`](#send). Each hard-fails `AnchorMissing` if no prior input claim has seeded the tx's anchor.

**Site that seeds without consuming**: [`input`](#input) sets `last_anchor = Anchor(cell.id())` directly (the spent UTXO's id is already unique on the wire — no split needed). Replacing any prior value is intentional: it lets a partial transaction depend only on its own input claim, not on what other parts of the tx contributed before it.

**Sites that split at call entry**: [`call`](#call), [`open`](#open), [`signcall`](#signcall). All three create new call frames and each splits `last_anchor` at entry. The `left` half seeds the child frame's `last_anchor`; the `right` half is held in the parent frame's `post_call_anchor` slot and replaces `last_anchor` when control returns. This makes anchor flow deterministic across call success/failure boundaries — the caller's anchor chain is independent of whatever the callee did with its left half.

**Locality across parties.** Each `op_input` *replaces* the anchor unconditionally rather than mixing into a chain. So a multi-party tx where party A claims input A_in and produces outputs, then party B claims input B_in and produces outputs, has each party's output anchors rooted only in their own input id. B's claim wipes A's residue; that's fine because B's outputs derive from B_in's split-children, not from anything A did. A party signing their portion can predict their own output anchors locally from their own input cell ids.

FlameVM splits anchors at the source: every consume site produces two cryptographically independent children, so neither the cell's stored anchor nor the residual anchor can be reused without the matching half of the original split.

## Instruction set

Each instruction is a one-byte **opcode** optionally followed by **immediate data** encoded inline in the bytecode. Stack effects are written in left-to-right bottom-to-top order: in `a b → c`, `b` is the top of the stack on entry, `c` is the top on exit.

**Context column.** The **Ctx** column in the instruction table marks opcodes that fail outside their supported context:
- **ext.** — external execution, including an external `CellOpen`. Hard-fails
  `ExternalOnly` from internal context. Covers `input`, the constraint-system
  opcodes (`scalar`, `commit`, `alloc`, `expr`, `range`), and the CS-consuming
  opcodes (`mix`, `fee`). Branch-polymorphic opcodes (`borrow`, `eq`, `add`,
  `and`, `or`) keep a blank marker; their CS-branch restriction is in the
  per-opcode prose. `decrypt` also has a blank marker: it batches externally and
  checks immediately internally.
- **actor** — requires a current actor identity. Available only in
  `InternalRoot` and `ActorCall`, never `ExternalRoot` or `CellOpen`.
- **pred.ext.** — requires an external `CellOpen` predicate context.
- **caller** — requires a called frame (`InternalRoot`, `ActorCall`, or
  `CellOpen`) with caller attribution; excludes `ExternalRoot`.
- **int.** — internal chain context. Currently used only for planned chain-info
  operations.
- *(blank)* — works in either context. Most opcodes, including `send`.

| Hex | Name | Ctx | Stack | Description |
| --- | --- | --- | --- | --- |
|     | **Stack**  | | | |
| 0k | [push:k](#pushk-and-friends) | | ø → int | Push a small literal int 0–15 inline. |
| 10–18 | [pushint8/16/64/128 \[s\], pushint](#pushk-and-friends) | | ø → int | Push an int with a width-class payload (signed, canonical). |
| 19 | [pushstr](#pushstr) | | ø → str | Push a literal byte string with a length prefix. |
| 1a | [pushpoint](#pushpoint) | | ø → point | Push a literal 32-byte Ristretto point. |
| 1b | [pushtoken](#pushtoken) | | flv → token | Mint a zero-qty `ClearToken` of the given flavor (placeholder). |
| 1c | [drop](#drop) | | x → ø | Discard a droppable value off the top of the stack. |
| 1d | [nop](#nop) | | ø → ø | Do nothing — useful as a padding / alignment hook. |
| 1e | [dup](#dup) | | x\_k … x\_0 k → x\_k … x\_0 x\_k | Copy the value at depth `k` onto the top (`k` popped as int). |
| 1f | [roll](#roll) | | x\_k … x\_0 k → x\_{k-1} … x\_0 x\_k | Move the value at depth `k` to the top (`k` popped as int). |
| 2k | [dup:k](#dupk) | | x\_k … x\_0 → x\_k … x\_0 x\_k | One-byte `dup` with `k` ∈ 0..=15 baked into the opcode. |
| 3k | [roll:k](#rollk) | | x\_k … x\_0 → x\_{k-1} … x\_0 x\_k | One-byte `roll` with `k` ∈ 0..=15 baked into the opcode. |
|    |  **String**  | | | |
| 40 | [readbits](#readbits) | | s n → s' x 1 \| s 0 | Pull `n` bits off the head of a string as an int (soft-fail if short). |
| 41 | [readint](#readint) | | s → s' x 1 \| s 0 | Pull a canonical 32-byte int off the head of a string. |
| 42 | [readstr](#readstr) | | s n → s' s'' 1 \| s 0 | Pull `n` bytes off the head of a string as a substring. |
| 43 | [readpoint](#readpoint) | | s → s' p 1 \| s 0 | Pull a 32-byte Ristretto point off the head of a string. |
| 44 | [writebits](#writebits) | | s x n → s' | Append the low `n` bits of `x` to a string. |
| 45 | [writeint](#writeint) | | s x → s' | Append the canonical 32-byte form of `x` to a string. |
| 46 | [append](#append) | | s s' → s'' | Concatenate two strings. |
| 47 | [writezeros](#writezeros) | | s n → s' | Append `n` zero-bytes to a string. |
| 48 | [bitnot](#bitnot) | | s → s' | Invert every bit of a string (length preserved). |
| 49 | [bitor](#bitor) | | a b → c | Bitwise OR of two equal-length strings. |
| 4a | [bitand](#bitand) | | a b → c | Bitwise AND of two equal-length strings. |
| 4b | [bitxor](#bitxor) | | a b → c | Bitwise XOR of two equal-length strings. |
| 4c | [shiftleft](#shiftleft) | | a n → b c | Shift a string left by `n` bits; `c` carries the displaced high bits. |
| 4d | [shiftright](#shiftright) | | a n → b c | Shift a string right by `n` bits; `c` carries the displaced low bits. |
|    | **Math & logic**  | | | |
| 50 | [abs](#abs) | | x → \|x\| s | Push magnitude and sign-bit of an int (`s` ∈ {0,1}). |
| 51 | [eq](#eq) | | a b → a b {0\|1} or constraint | Equality test — cleartext peek, or lifted Constraint in the CS. |
| 52 | [neg](#neg) | | x → −x | Flip sign of int / Expression / MSM (Point lifts to MSM). |
| 53 | [add](#add) | | x y → z | Add ints (or Expressions); Point/MSM operands lift to MSM. |
| 54 | [mul](#mul) | | x y → z | Multiply ints (or CS gate); Int·Point or Int·MSM lift to MSM. |
| 55 | [divmod](#divmod) | | x z → d r | Truncated division — push quotient and remainder. |
| 56 | [mod252](#mod252) | | s → int | Reduce a ≤64-byte LE string modulo ℓ and push as non-negative int. |
| 57 | [not](#not) | | x → y | Logical NOT for ints; structural negation for Constraints. |
| 58 | [and](#and) | | a b → c | Logical AND for ints; lifts to Constraint conjunction in CS. |
| 59 | [or](#or) | | a b → c | Logical OR for ints; lifts to Constraint disjunction in CS. |
| 5a | [size](#size) | | x → x n | Push length of a String / entry count of a Dict (peek). |
|    | **Constraints**  | | | |
| 60 | [scalar](#scalar) | ext. | s → expr | Lift a 32-byte scalar string to a constant Expression. |
| 61 | [commit](#commit) | ext. | s → var | Wrap a 32-byte Pedersen-commitment point as a CS Variable. |
| 62 | [alloc](#alloc) | ext. | ø → expr | Allocate a fresh R1CS variable and push it as a one-term Expression. |
| 63 | [expr](#expr) | ext. | var → expr | Bind a Variable into the CS and push it as a one-term Expression. |
| 64 | [range](#range) | ext. | expr n → expr | Range-prove an Expression to `n` ∈ 1..=64 bits. |
|    | **Dict**  | | | |
| 70 | [dict](#dict) | | …kv… n → dict | Build a Dict from `n` `(value, key)` pairs already on the stack. |
| 71 | [put](#put) | | dict k v → dict' | Insert `v` at fresh key `k`. |
| 72 | [replace](#replace) | | dict k v → dict' {prev 1 \| 0} | Set `v` at key `k`; push prior value if any. |
| 73 | [get](#get) | | dict k → dict' k v | Remove and return the value at `k` (fails if absent). |
| 74 | [getopt](#getopt) | | dict k → dict' {v 1 \| 0} | Optional `get` — soft-fail if key absent. |
| 75 | [getdup](#getdup) | | dict k → dict {v 1 \| 0} | Peek-copy the value at `k` without removing it. |
| 76 | [first](#first--last--next) | | dict → dict {k 1 \| 0} | Push the smallest key, or `0` for an empty dict. |
| 77 | [last](#first--last--next) | | dict → dict {k 1 \| 0} | Push the largest key, or `0` for an empty dict. |
| 78 | [next](#first--last--next) | | dict k → dict {k' 1 \| 0} | Push the smallest key strictly greater than `k`. |
|    | **Cryptography**  | | | |
| 80 | [transcript](#transcript) | | label → merlin | Open a new Merlin transcript seeded with `label`. |
| 81 | [twrite](#twrite) | | m label s → m | Append a labeled byte string to a transcript. |
| 82 | [tread](#tread) | | m label n → m s | Challenge `n` bytes from a transcript under `label`. |
| 83 | [sha256](#sha256) | | s → x | 32-byte SHA-256 digest. |
| 84 | [sha512](#sha512) | | s → x | 64-byte SHA-512 digest. |
| 85 | [sha3](#sha3) | | s → x | 32-byte SHA3-256 (FIPS-202) digest. |
| 86 | [keccak256](#keccak256) | | s → x | 32-byte Keccak-256 digest (Ethereum compatibility). |
| 87 | [log](#log) | | s → ø | Emit a byte string as a data entry into the transaction log. |
|    | **Tokens** | | | |
| 90 | [amount](#amount) | | t → t qty flv | Peek the quantity and flavor of a token without consuming it. |
| 91 | [issuepriv](#issuepriv) | pred.ext. | qty tag → T | Mint a confidential token under the current predicate's identity + `tag`. |
| 92 | [issueprivflv](#issueprivflv) | | pred tag → int | Consumer-side helper: recompute `flavor_from_predicate(pred, tag)`. |
| 93 | [issuepub](#issuepub) | actor | qty tag → CT | Mint a cleartext token under the current actor's identity + `tag`. |
| 94 | [issuepubflv](#issuepubflv) | | cid tag → int | Consumer-side helper: recompute `flavor_from_actor(cid, tag)`. |
| 95 | [retire](#retire) | | t → ø | Burn a token (emits a retire entry to the txlog). |
| 96 | [borrow](#borrow) | | qty flv → −T +T | Borrow balanced ±token pair; debt must be balanced before tx end. |
| 97 | [merge](#merge) | | a b → {c 1 \| a b 0} | Combine two same-flavor cleartokens; soft-fail on flavor mismatch. |
| 98 | [split](#split) | | a q → a' b | Split quantity `q` off a cleartoken. |
| 99 | [mix](#mix) | ext. | tokens… cmts… m n → tokens | Cloak: prove `m` input tokens balance `n` output commitments per flavor. |
| 9a | [decrypt](#decrypt) | | T f f' q q' → CT | Open an encrypted Token to a ClearToken using cleartext openings. |
| 9b | [fee](#fee) | ext. | qty → −WT | Pay tx fee in the native Flame flavor; push the balancing WideToken debt to net out via `mix`. |
|    | **Control flow** | | | |
| a0 | [verify](#verify) | | x → ø | Assert: hard-fail if int is zero, enforce a Constraint, or batch an MSM. |
| a1 | [label](#label) | | ø → ø | Mark a jump target (operand: label number); labels number 0,1,2… in order. |
| a2 | [jump](#jump) | | ø → ø | Unconditional jump to a label (operand: label number). |
| a3 | [jumpif](#jumpif) | | x → ø | Pop an int; jump to a label iff non-zero (operand: label number). |
| a4 | [return](#return) | | a\_{k-1} … a\_0 k → ø | Exit current call frame, returning `k` items to the parent. |
| a5 | [type](#type) | | x → x code | Push the type code of the top value (peek). |
|    | **Cells & predicates** | | | |
| c0 | [input](#input) | ext. | s → cell | Materialize a cell from a Utreexo-validated input encoding. |
| c1 | [cell](#cell) | | items… k pred → cell | Build a new cell from `k` portable items under predicate `pred`. |
| c2 | [output](#output) | | items… k pred → ø | Like `cell`, but emits the cell directly as a tx Output. |
| c3 | [open](#open) | | cell ik nbrs pos script gas args… k → {results… k' 1 \| cell args… k 0} | Reveal a taproot leaf and run it in an isolated call frame. |
| c4 | [signtx](#signtx) | ext. | cell → items… k | Authorize the tx with the cell predicate's signature; pour payload. |
| c5 | [signcall](#signcall) | | cell script sig gas args… m → {results… k' 1 \| cell args… m 0} | Run a script signed by the cell predicate in an isolated frame. |
|    | **Actors** | | | |
| d0 | [send](#send) | | args… k refund gas addr → ø | Queue an asynchronous actor message. |
| d1 | [call](#call) | actor | args… k gas addr → {results… k' 1 \| args… k 0} | Synchronously call an actor. |
| d2 | [load](#load) | actor | ø → value | Check out the actor's state (any portable Value; moves it out, locks re-entry). |
| d3 | [save](#save) | actor | value → ø | Move the state value back in (requires checkout; unlocks). |
| d4 | [setcode](#setcode) | actor | code → ø | Replace the actor's code blob (author-gated upgrade). |
| d5 | [addstorage](#addstorage) | actor | q → {debt 1 \| 0} | Buy `q` bytes for one storage year and return a negative Flame token. |
| d6 | [quotestorage](#quotestorage) | actor | q → {fee 1 \| 0} | Quote the positive integral storage fee in sparks without reserving bytes. |
|    | **Frame introspection** | | | |
| e0 | [selfid](#selfid) | actor | ø → s | Push the current actor's id (32-byte string). |
| e1 | [anchor](#anchor) | | ø → s | Push the current frame's anchor (32-byte string). |
| e2 | [callerid](#callerid) | caller | ø → s | Push the direct caller actor's id, or zero when none. |
| e4 | [gas](#gas) | | ø → n | Push remaining gas budget for the current call. |
| e5 | [gaslimit](#gaslimit) | | ø → n | Push the call's total gas budget cap. |
| e6 | [usage](#usage) | actor | ø → n | Push the actor's currently occupied storage bytes. |
| e8 | [capacity](#capacity) | actor | h → n | Push actor storage capacity available at current or future core-block height `h`. |
|    | **Tx & chain info** | | | |
| f0 | [timelock](#timelock) | | ø → n {0\|1} | Push tx locktime and a flag for height (`0`) vs. timestamp (`1`). |
| f1 | [version](#version) | | ø → n | Push tx version. |
| f2 | [height](#height) | | ø → n | Push the current core-block height, or zero in an external transaction. |
| f3 | [blockhash](#blockhash) | int. | h → s | Push the block hash at height `h`. *planned* |
| f4 | [blockburn](#blockburn) | int. | h → n | Push satoshis burned at height `h` (Bitcoin-coupled). *planned; maturity 100* |
| f5 | [blockweight](#blockweight) | int. | h → n | Push block weight at height `h`. *planned; maturity 100* |
| f6 | [blockrate](#blockrate) | int. | h → n | Push sparks-per-satoshi mint rate at height `h`. *planned; maturity 100* |
| f7 | [chainstate](#chainstate) | int. | n → dict | Push a dict of block stats at height `n`. *planned; maturity 100* |

Opcodes marked *planned* are reserved in the target byte map but are not yet
implemented with the specified behavior. The storage opcodes and `height` are
implemented; bytes `f3` through `f7` remain reserved and currently fail
`UnknownOpcode`.

### Failure modes

A **hard fail** aborts the current call.

If that call has a parent, the VM rolls the child back and converts the failure
to the call opcode's documented in-band failure shape. At the outermost frame,
the error aborts the transaction. Synchronous failure never exposes the failed
child's mutated stack: it exposes only the entry escrow after rollback.

A **soft fail** is an in-band signal: the opcode pushes an optional shape
`{value 1 | 0}` so the script can branch. Its stack diagram is authoritative
about whether failed operands are retained; storage request opcodes consume `q`
on either branch. The two kinds are noted per opcode.

Canonical failure-shape vectors (stack order is bottom-to-top):

| Opcode and absent/failing input | Output stack | Consumed on zero branch |
| --- | --- | --- |
| `readbits`: `String(aa), 16` | `String(aa), 0` | count |
| `readint`: `String(aa×31)` | `String(aa×31), 0` | nothing else |
| `readstr`: `String(01), 5` | `String(01), 0` | count |
| `readpoint`: `String(00×31)` | `String(00×31), 0` | nothing else |
| `replace`: `{ }, 5, 7` (no prior value) | `{5: 7}, 0` | key and new value enter the Dict |
| `getopt`: `{ }, 5` | `{ }, 0` | key |
| `getdup`: `{ }, 5` | `{ }, 0` | key |
| `first` / `last`: `{ }` | `{ }, 0` | nothing else |
| `next`: `{1: 7}, 1` | `{1: 7}, 0` | search key |
| `merge`: `CT(1,7), CT(2,8)` | `CT(1,7), CT(2,8), 0` | neither token |
| unavailable `addstorage` / `quotestorage`: `q` | `0` | request `q` |
| failed entered `call`: `A, B` with `k=2` | `A, B, 2, 0` | address and gas; arguments are restored and count is re-emitted |
| failed entered `open`: `Cell, A, B` with `k=2` | `Cell, A, B, 2, 0` | proof/script/gas operands; Cell and arguments are restored, count excludes Cell |
| failed entered `signcall`: `Cell, A, B` with `k=2` | `Cell, A, B, 2, 0` | script/signature/gas operands; Cell and arguments are restored, count excludes Cell |

Thus count and lookup-key operands do not need restitution: they are copyable
control data, not linear values. The source String, Dict, tokens, Cell, and call
arguments follow the exact shapes above.

### Stack instructions

### push:k and friends

ø → _int_

Pushes an [`Int253`](#int253). The encoder picks the narrowest of these forms:

- `0x00..=0x0f` — immediate `push:k`, value `k ∈ 0..=15`, no payload.
- `0x10`/`0x11` — `pushint8` positive/negative, 1-byte LE payload (magnitude 0..=255).
- `0x12`/`0x13` — `pushint16` positive/negative, 2-byte LE payload.
- `0x14`/`0x15` — `pushint64` positive/negative, 8-byte LE payload.
- `0x16`/`0x17` — `pushint128` positive/negative, 16-byte LE payload.
- `0x18` — `pushint` full form, 32-byte sign-magnitude payload.

Width classes carve disjoint, offset-based value ranges so the encoding is canonical. The decoder rejects values that fit a narrower class. Negative zero is never representable.

### pushstr

ø → _string_

Reads a sub-varint length prefix + payload bytes; pushes them as a [String](#string).

### pushpoint

ø → _point_

Reads 32 more bytes and pushes them as a [Point](#point). Bytes are not decompressed eagerly — invalid Ristretto encodings only fail when consumed by a downstream opcode.

### pushtoken

_flv_ → _token_

Pops an `Int253` flavor; pushes a zero-quantity `ClearToken { qty: 0, flv }`. Convenience for downstream `merge`/`mix` shapes that need a typed placeholder.

### drop

_x_ → ø

Drops a [droppable](#types) value. Hard-fails `TypeNotDroppable` for linear types and non-empty containers.

### nop

ø → ø

No effect.

### dup

_x\_k … x\_0 k_ → _x\_k … x\_0 x\_k_

Pops `k` as `Int253`, copies the value at depth `k` (zero-indexed from the top) onto the stack. Source must be a [copyable](#types) type — hard-fails `TypeNotCopyable` otherwise. Hard-fails `IndexOutOfRange` when `k < 0` or exceeds the stack depth.

Prefer the immediate [`dup:k`](#dupk) form for `k ∈ 0..=15` (one byte instead of two).

### roll

_x\_k … x\_0 k_ → _x\_{k-1} … x\_0 x\_k_

Pops `k` as `Int253`, moves the value at depth `k` to the top of the stack. Any type works (no copy required). Hard-fails `IndexOutOfRange` when `k < 0` or exceeds the stack depth.

Prefer the immediate [`roll:k`](#rollk) form for `k ∈ 0..=15`.

### dup:k

_x\_k … x\_0_ → _x\_k … x\_0 x\_k_

Immediate-encoded `dup` with `k ∈ 0..=15` taken from the low nibble of the opcode byte (`0x2k`). One-byte equivalent of `pushint8 k; dup` — saves a byte over [`dup`](#dup) for shallow depths. Same copyability and bounds rules as `dup`.

### roll:k

_x\_k … x\_0_ → _x\_{k-1} … x\_0 x\_k_

Immediate-encoded `roll` with `k ∈ 0..=15` taken from the low nibble of the opcode byte (`0x3k`). One-byte equivalent of `pushint8 k; roll`. Same bounds rules as `roll`.

## String instructions

**Failure principle.** Constraints derived from *external data* — string length, canonical magnitude, negative zero — are **soft fails**. Constraints on *author-controlled* immediates (e.g. `n > 256`) are **hard fails**.

### readbits

_s n_ → _s' x 1_ | _s 0_

Reads `n ≤ 256` bits **LSB-first within each byte** into bits `0..n-1` of a fresh `Int253`. The sign bit at position 255 is set only when `n = 256` and the input bit 255 is `1`; for `n < 256` the result is non-negative.

Soft-fails on insufficient bytes, magnitude ≥ ℓ (reachable only when `n ≥ 253`), or negative zero (only when `n = 256`). A negative or greater-than-256 count hard-fails `IndexOutOfRange`.

### readint

_s_ → _s' x 1_ | _s 0_

Equivalent to `readbits(s, 256)`. Reads the canonical 32-byte `Int253` (bit 255 = sign). Soft-fail conditions match `readbits`.

### readstr

_s n_ → _s' s'' 1_ | _s 0_

Reads `n` bytes into a new String, consuming them from `s`. Soft-fails if `s` has fewer than `n` bytes.

### readpoint

_s_ → _s' p 1_ | _s 0_

Reads 32 bytes as a [Point](#point). Soft-fail on insufficient bytes.

### writebits

_s x n_ → _s'_

Appends the low `n` bits of `x`'s canonical 32-byte representation as bytes (LSB-first). `n` must be a multiple of 8 and `≤ 256`. A negative or greater-than-256 count hard-fails `IndexOutOfRange`; a non-byte-aligned count hard-fails `BitCountOutOfRange`. The sign bit (bit 255) is written iff `n = 256`.

### writeint

_s x_ → _s'_

Equivalent to `writebits(s, x, 256)`. Appends the canonical 32-byte sign-magnitude form.

### append

_s s'_ → _s''_

Concatenates: `s'' = s || s'`.

### writezeros

_s n_ → _s'_

Appends `n` zero-bytes.

### bitnot

_s_ → _s'_

Inverts every bit of `s`. Output length matches input.

### bitor

_a b_ → _c_

Bitwise OR of two strings. Operands must have the same length — mismatch hard-fails `BitwiseSizeMismatch`.

### bitand

_a b_ → _c_

Bitwise AND of two strings. Same length rule and failure mode as [`bitor`](#bitor).

### bitxor

_a b_ → _c_

Bitwise XOR of two strings. Same length rule and failure mode as [`bitor`](#bitor).

### shiftleft

_a n_ → _b c_

Shifts bits of `a` left by `n ≤ 256`. `b` has the same length as `a`; `c` carries the displaced bits as a zero-padded-left string of `ceil(n/8)` bytes. A negative or greater-than-256 count hard-fails `IndexOutOfRange`. Byte 0 is most significant.

### shiftright

_a n_ → _b c_

Mirror of `shiftleft`; displaced low-end bits land in `c` zero-padded on the right.

## Arithmetic & logic instructions

### abs

_x_ → _|x| s_

Pops `Int253` `x`; pushes the absolute value followed by the sign (`0` for non-negative, `1` for negative).

### eq

_a b_ **eq** → _a b {0|1}_ (cleartext) | → _constraint_ (CS branch)

Two stack diagrams depending on operand types and context:

1. **Cleartext branch.** When neither operand is a CS type (`Variable` / `Expression`), or in internal context, peeks at the top two values and pushes `1` if equal or `0` otherwise. Operands stay on the stack. Equality is type-aware via `Value::try_eq`: `Int253` / `String` / `Point` compare by value; **same-variant `Dict` and all linear types (`Token`, `ClearToken`, `Cell`, …) hard-fail `TypeNotComparable`** (linear values have no equality; Dicts would cost unbounded recursion).
2. **Lifted branch.** When at least one operand is `Expression` or `Variable` *and* the context is external, both operands are popped, lifted to `Expression` (Int253 → `Expression::Constant`), and the result is `Constraint::eq(a, b)`.

### neg

_x_ → _−x_

`Int253` flips its sign bit (zero stays positive). `Expression` negates the linear combination. `Point` / `MultiscalarMul` lift to MSM (see [MultiscalarMul](#multiscalarmul)) with negated coefficients. Other types hard-fail `TypeNotInt253`.

### add

_x y_ → _z_

Dispatch by operand types:
- `Int253 + Int253` → signed-modular addition mod ℓ.
- `Point + Point` / `Point + MSM` / `MSM + Point` / `MSM + MSM` → [MultiscalarMul](#multiscalarmul) with concatenated terms (works in either context).
- Mixed Int/Expression in external context → lifts to `Expression` LC sum.

### mul

_x y_ → _z_

Dispatch by operand types:
- `Int253 * Int253` → signed-modular multiplication mod ℓ.
- `Int253 * Point` / `Point * Int253` → [MultiscalarMul](#multiscalarmul) with one `(scalar, point)` term.
- `Int253 * MSM` / `MSM * Int253` → MSM with scaled coefficients.
- Mixed Int/Expression in external context → may add a CS multiplier gate (or constant-fold when one side is `Expression::Constant`).
- `Point * Point`, `MSM * Point`, `MSM * MSM` → hard-fail `TypeNotInt253` (quadratic in group elements, no Sigma-protocol semantics).

### divmod

_x z_ → _d r_

Truncated division: `sign(d) = sign(x) XOR sign(z)`, `sign(r) = sign(x)`. Hard-fails `DivByZero` on a zero divisor. Both operands must be `Int253`.

### mod252

_s_ → _int_

Reads up to 64 bytes of `s` as a little-endian unsigned integer, reduces modulo ℓ, pushes the result as a non-negative `Int253`. Hard-fails `StringTooLongForModReduction` when `s.len() > 64`.

### not

_x_ → _y_

`Int253`: `0 → 1`, non-zero → `0`. `Constraint`: structural negation via `Constraint::not(c)`.

### and

_a b_ → _c_

Cleartext logical AND when both operands are `Int253`. When at least one operand is a `Constraint` (external context only), both lift to `Constraint` (Int253 → `Cleartext(v != 0)`) and the result is `Constraint::and(a, b)`.

### or

_a b_ → _c_

Mirror of [`and`](#and) for disjunction.

## Constraint system instructions  *(external-only)*

**Rollback on call failure.** Every CS-touching opcode (`scalar`,
`commit`, `alloc`, `expr`, `range`, `eq` via `verify`, `mix`,
`fee`) appends to the delegate's R1CS. On `call` /
`open` / `signcall` entry the VM checkpoints the R1CS via
`bulletproofs::r1cs::CheckpointableConstraintSystem::checkpoint`;
on call failure the child's CS contributions (witness vectors,
constraint vectors, deferred constraints, transcript state) are
rolled back to the parent's snapshot via `rollback`. Both Prover
and Verifier hit the same checkpoint/rollback sites because they
walk the same script — the transcript stays in lockstep across
the failure boundary. On clean return the snapshot is dropped and
the child's CS contributions remain in the final proof. See
the rollback model described above.

### scalar

_s_ → _expr_

Pops a 32-byte String, downcasts to `Int253` via `String::to_scalar`, pushes `Expression::Constant(int)`. Witness-bearing `StringWitness::Scalar(i)` extracts the witness directly; `String::Opaque(bytes)` parses canonical sign-magnitude bytes.

### commit

_s_ → _var_

Pops a 32-byte String, downcasts to a [Commitment](#types), wraps in `Variable { commitment }`. Verifier: `String::Opaque(point bytes)` → `Commitment::Closed(point)`. Prover: `StringWitness::Point(Point::Commitment(Open(witness)))` preserves the witness. Downstream [`expr`](#expr) binds the variable into the CS.

### alloc

ø → _expr_

Allocates a low-level R1CS variable. The prover-side `Instruction::Alloc(Some(int))` carries the cleartext witness; the verifier sees `Alloc(None)` and the variable is left unassigned, constrained later by `eq`/`verify`. Pushes a one-term `Expression::LinearCombination([(var, 1)], witness?)`.

### expr

_var_ → _expr_

Pops a `Variable`, commits it via the delegate's `commit_variable`, pushes a one-term Expression bound to the resulting R1CS variable.

### range

_expr n_ → _expr_

Pops bit count `n: Int253` (must be in `[1, 64]`) and an `Expression`. For `Expression::Constant`, asserts the constant fits in `[0, 2ⁿ)` (cleartext check). For `Expression::LinearCombination`, adds a Bulletproofs range-proof gadget. The Expression is pushed back unchanged. Hard-fails `BitCountOutOfRange`, `InvalidBitrange`, or `R1CSError`.

### size

_x_ → _x n_

Peeks at the top value and pushes its length: byte count for `String`, entry count for `Dict`. Hard-fails `TypeHasNoLength` for other types. Available in both contexts (CS-system grouping is for byte adjacency).

## Dict instructions

[Dict](#dict) keys are always `Int253`; values may be any [Value](#types). Insertion updates the Dict's sticky capability flags; portability is enforced only when the Dict crosses a storage or transfer boundary.

### dict

_… val\_{n-1} key\_{n-1} … val\_0 key\_0 n_ → _dict_

Pops `n` (Int253), then `n` `(value, key)` pairs (key on top of each pair). Builds a Dict. Hard-fails `DictKeyOccupied` on duplicate keys.

### put

_dict k v_ → _dict'_

Inserts `v` at key `k`. Hard-fails `DictKeyOccupied` if the key already exists.

### replace

_dict k v_ → _dict' {prev 1 | 0}_

Sets `v` at key `k`. If a prior value existed, pushes `prev 1`; otherwise pushes `0`.

### get

_dict k_ → _dict' k v_

Removes the value at `k` and pushes `(k, v)` above the modified dict. Hard-fails `DictKeyNotFound` if absent.

### getopt

_dict k_ → _dict' {v 1 | 0}_

Like [`get`](#get) but soft-fails when absent: pushes `0` instead of erroring.

### getdup

_dict k_ → _dict {v 1 | 0}_

Copies the value at `k` without modifying the dict. Pushes `0` if absent. Hard-fails `TypeNotCopyable` when the value exists but is a linear type.

### first / last / next

_dict_ → _dict {k 1 | 0}_  (first / last)  
_dict k_ → _dict {k' 1 | 0}_  (next)

Iteration helpers. Push the first/last key of the dict, or the key after `k`, or `0` if the dict is empty / `k` is the last entry. Keys are visited in canonical `Int253` order.

## Cryptography instructions

### transcript

_label_ → _merlin_

Creates a fresh [Merlin transcript](#types) seeded with `label`. The transcript is a linear value (never copyable, never droppable) consumed by `twrite` / `tread` to build custom ZKP statements.

### twrite

_merlin label s_ → _merlin_

Appends `(label, s)` to the transcript and returns the same transcript on top.

### tread

_merlin label n_ → _merlin s_

Challenges `n` bytes under `label`; pushes the result as a String.

### sha256

_s_ → _x_

Returns a 32-byte SHA-256 digest.

### sha512

_s_ → _x_

Returns a 64-byte SHA-512 digest.

### sha3

_s_ → _x_

Returns a 32-byte SHA3-256 (FIPS-202) digest. Distinct from [`keccak256`](#keccak256).

### keccak256

_s_ → _x_

Returns a 32-byte Keccak-256 digest (Ethereum compatibility). Distinct from [`sha3`](#sha3) (FIPS-202).

### log

_s_ → ø

Pops a String and emits `TxEntry::Data(bytes)` into the txlog. It is visible to the outer verifier and does not occupy persistent storage. Witness-bearing String variants serialize via `to_bytes` so prover and verifier emit identical bytes.

## Token instructions

See [Token / ClearToken / WideToken](#tokens) for type semantics.

### amount

_t_ → _t qty flv_

Peeks the top token-shaped value and pushes `(qty, flv)` above it. `ClearToken`: both as `Int253` (cleartext). `Token`: both as `Point` (the compressed commitment points; works without a live CS). `WideToken`: hard-fails `TypeNotToken` — its quantity isn't yet range-proven.

### issuepriv

_qty tag_ → _T_

Pops `tag` (String) and `qty` (`Variable` — a Pedersen commitment lifted via [`commit`](#commit)). Allocates a 64-bit range proof on `qty`. Builds `Token(qty_commitment, unblinded(flavor))` where `flavor = flavor_from_predicate(current_predicate, tag)`. Emits `TxEntry::IssuePriv(qty_commitment_point, unblinded_flv_point)` — the confidential-issuance txlog effect. Pushes the `Token`.

The current_predicate is the predicate stored on the enclosing `CallKind::CellOpen` frame — created by [`open`](#open) or [`signcall`](#signcall) against an empty cell whose predicate is the desired issuer. Hard-fails `OpcodeRequiresPredicateContext` from `ExternalRoot` (no enclosing predicate) and from any `ActorCall` frame (issuance binds to a predicate, not an actor; the two issuance domains are kept disjoint by construction). Hard-fails `ExternalOnly` in internal context (the CS lane is required for the range proof and the qty commitment).

`Int253` or `Point` operands hard-fail `TypeNotVariable` — lift to a `Variable` via [`commit`](#commit) first.

**Non-fungible tokens.** Mix the cell's [`anchor`](#anchor) into `tag` (e.g. `anchor … keccak256` against domain bytes) to derive a fresh flavor per issuance — the result is a non-fungible Token, since no other issuance will share the flavor. Use [`issueprivflv`](#issueprivflv) on the consumer side to recompute the same flavor scalar for verification.

### issueprivflv

_pred tag_ → _int_

Pops `tag` (String) and `pred` (String, exactly 32 bytes — a compressed Ristretto predicate point). Pushes `flavor_from_predicate(pred, tag)` as `Int253`. Pure helper: no CS, no txlog entry, no predicate-context requirement. Domain separator is `flamevm.issuepriv.flavor` (consensus-fixed). Hard-fails `IndexOutOfRange` if `pred` is not exactly 32 bytes.

The confidential token's flv commitment is unblinded, so its point uniquely determines this scalar — meaning a consumer who knows the issuing predicate's point and the tag can recompute the flavor and check that an incoming Token belongs to the expected issuance domain.

### issuepub

_qty tag_ → _CT_

Pops `tag` (String) and `qty` (`Int253` — cleartext). Builds `ClearToken(qty, flavor_from_actor(current_actor, tag))` and emits `TxEntry::IssuePub(qty, flv)` carrying the cleartext `(qty, flv)` pair directly as `Int253`s — publicly auditable on the wire without commitment indirection. Pushes the `ClearToken`.

The current actor is stored on either `CallKind::InternalRoot` or
`CallKind::ActorCall`. Both may issue. The opcode hard-fails
`OpcodeRequiresActorContext` from `ExternalRoot` and `CellOpen` (issuance binds
to an actor, not a predicate). It runs without CS in internal execution.

`Variable` or `Point` operands hard-fail `TypeNotInt253` — `issuepub` is the cleartext path; for confidential qty, use [`issuepriv`](#issuepriv) from a `CellOpen` frame.

**Non-fungible tokens.** Mix the call's [`anchor`](#anchor) into `tag` to derive a fresh flavor per call — yields a unique non-fungible token. Use [`issuepubflv`](#issuepubflv) on the consumer side to recompute the same flavor scalar for verification.

### issuepubflv

_cid tag_ → _int_

Pops `tag` (String) and `cid` (String, exactly 32 bytes — an actor id). Pushes `flavor_from_actor(cid, tag)` as `Int253`. Pure helper: no CS, no txlog entry, no actor-context requirement. Domain separator is `flamevm.issuepub.flavor` (consensus-fixed). Hard-fails `IndexOutOfRange` if `cid` is not exactly 32 bytes.

### retire

_t_ → ø

Consumes a non-negative token and emits `TxEntry::Retire(qty_point, flv_point)`.
`ClearToken` uses unblinded commitments; `Token` uses the live commitment
points. A negative `ClearToken` hard-fails `NegativeTokenRetirement`, preventing
storage-fee debt and other liabilities from being discarded. Other types
hard-fail `TypeNotToken`.

### borrow

_qty flv_ → _−T +T_

Cleartext branch (both `Int253`): pushes `(ClearToken(-qty, flv), ClearToken(qty, flv))`. The negative half is non-portable until balanced.

Encrypted branch (both `Variable`, external context): commits both to the CS, range-proves the positive `qty` 64-bit, allocates `-qty`, constrains the sum to zero, pushes `(WideToken(-qty, flv), Token(qty, flv))`.

Raw `Point` operand hard-fails `TokenRequiresCS` — lift to `Variable` via [`commit`](#commit) first.

### merge

_a b_ → _{c 1 | a b 0}_

`ClearTokens` only. On flavor match, pushes `(ClearToken(a.qty+b.qty, flv), 1)`. On flavor mismatch, restores `(a, b, 0)` (soft-fail). Non-`ClearToken` operands hard-fail `TypeNotClearToken`.

### split

_a q_ → _a' b_

`ClearTokens` only. Returns `(ClearToken(a.qty − q, flv), ClearToken(q, flv))`. Hard-fails `TokenSplitOutOfRange` when `q < 0`, `a.qty < 0`, or `q > a.qty`.

### mix

_tokens… commitments… m n_ → _tokens_

Pops `n` (output count) and `m` (input count) as `Int253`; then `n` output Pedersen commitment pairs (qty/flv Strings); then `m` input token-shaped values (any of `Token`, `WideToken`, `ClearToken`). Invokes the [spacesuit cloak gadget](../spacesuit/spec.md) to constrain that inputs balance outputs per flavor and to 64-bit range-prove each output. Pushes `n` output `Token`s.

Hard-fails `MixDegenerate` if `m == 0` or `n == 0`.

### decrypt

_T f f' q q'_ → _CT_

The stack is bottom-to-top `Token, f, f', q, q'`, so the VM pops `q'`, `q`, `f'`, `f`, then `Token`. It verifies that the supplied openings reconstruct the Token's commitment points and pushes `ClearToken(q, f)` on success.

External execution appends the two opening equations to the transaction's randomized batch; an invalid opening surfaces as `BatchSignatureVerificationFailed` at final verification. Internal execution checks both equations immediately because internal transactions have no deferred batch; an invalid point hard-fails `InvalidPoint`, and a mismatch hard-fails `CommitmentOpeningMismatch`. In a nested call, either immediate error follows the ordinary call rollback path and restores the caller's entry arguments.

## Control-flow instructions

Control flow is structured-by-convention over a flat instruction stream:
`label`/`jump`/`jumpif` carry a **label number**, not an offset, so bytecode
is written without computing positions and the prover and a future streaming
verifier resolve labels in their own cursor space (instruction index vs byte
offset — see the design). The high-level builder (`build_if` / `build_while` /
`build_loop` / `build_switch` / `build_break` / `build_continue`) emits these
with a running label counter and forward-jump backpatching. Code only ever
executes by becoming a CallFrame — there is no inline `run`; see
[`open`](#open) / [`signcall`](#signcall) / [`call`](#call).

### verify

_x_ → ø

- `Int253`: hard-fails `VerifyFailed` if zero; otherwise consumes the value.
- `Constraint`: enforces the constraint via the delegate's CS (external context only).
- `MultiscalarMul`: appends `sum(s_i · P_i) == identity` to the delegate's `BatchVerifier` alongside any Schnorr/Musig sigs (external context only). Returns success immediately; the batched check runs at finalize and on failure surfaces as `BatchSignatureVerificationFailed`. See [MultiscalarMul](#multiscalarmul).
- Other types hard-fail `TypeNotInt253`.

### fee

_qty_ → _−WT_

Pops `qty: Int253` in sparks (non-negative, `≤ MAX_FEE = 2²⁴`). Fees always use the canonical native flavor `FLAME_FLAVOR = Int253::ZERO`. The `2²⁴`-spark cap is chosen so fee-rate arithmetic stays within `u64`: even a `2⁴⁰`-byte (~1 TB) transaction leaves 24 bits of headroom. Emits `TxEntry::Fee(qty as u64)` and bumps the per-tx [`CheckedFee`](#fees) accumulator (also capped at `MAX_FEE`). Allocates a fresh `WideToken` debt with `q = −qty`, `f = FLAME_FLAVOR` (both cleartext-constrained) and pushes it. The script must balance the debt against native Flame tokens, typically via [`mix`](#mix).

Hard-fails: `FeeQtyNegative`, `FeeTooHigh` (per-arg or aggregate overflow), `TypeNotInt253`, `ExternalOnly`. The blinded-fee branch is reserved for a future phase.

### label

ø → ø — operand: label number (sub-varint)

Marks a jump target. Each CallFrame keeps a `labels` array of positions
(instruction index on the prover; byte offset on a streaming verifier), built
lazily as labels are reached. Labels must appear in strictly sequential order
— 0, 1, 2, … — so the array is indexed directly by label number. On reaching
`label N`:

- `N == labels.len()` → record the position after the label (first sight).
- `N < labels.len()` → a re-visit (a loop back-edge re-traversing a label in
  its own body): allowed iff the recorded position matches; otherwise
  hard-fails `LabelOutOfOrder`.
- `N > labels.len()` → hard-fails `LabelOutOfOrder` (gap / out of order).

Labels are frame-local: an opened/called sub-program numbers from 0 in its
own frame.

### jump

ø → ø — operand: label number (sub-varint)

Sets the cursor to label `N`. If `N` is already recorded (`N < labels.len()`),
jumps immediately — backward, or forward to an already-seen label. Otherwise
enters *skipping mode*: scans forward **without executing**, recording each
`label` passed (each must be the next sequential number), until `label N` is
reached, then resumes execution after it. Reaching end-of-program while
skipping hard-fails `LabelNotFound`.

### jumpif

_x_ → ø — operand: label number (sub-varint)

Pops `x: Int253`. If non-zero, behaves as [`jump`](#jump) to label `N`; if
zero, falls through to the next instruction. Hard-fails `TypeNotInt253` if
the top value is not an int.

### return

_a_{k-1} … a_0 k_ → ø

Atomic cross-frame return:

1. Pops `k` (Int253, non-negative).
2. Asserts an enclosing call frame exists (otherwise `ReturnAtRoot`).
3. Asserts the callee stack has exactly `k` items left (otherwise `BadReturnArity` or `StackNotClean`).
4. Pops the call frame.
5. Refunds leftover gas to the parent.
6. Pushes the `k` items onto the parent's stack, then the count `k`, then a **success marker `1`** — the parent observes `results… k 1`. A clean run-off-the-end exit pushes `0 1`. A failed child instead restores its entry escrow followed by the escrow count and `0`; callers branch on this trailing status. Actor calls escrow their arguments. `open` and `signcall` escrow the original locked Cell plus their explicit arguments, never the Cell payload separately.

`return` deliberately performs no portability check. Non-portable values,
including negative `ClearToken`s and `WideToken`s, may move upward to the
caller, which owns responsibility for balancing or consuming them. Every
downward boundary (`call`, `open`, `signcall`, and asynchronous `send`) accepts
portable values only.

At the outermost call frame, `return` always errors regardless of `k`; a script terminates cleanly by running off the end of its instructions with an empty stack (jump to a trailing label to short-circuit).

### type

_x_ → _x typecode_

Pushes the type code of the top value as `Int253`, leaving the value on the stack. Type codes are a stable sequential enumeration of the value types — **independent of the wire-encoding tags** in [Types](#types), which keep their own compact scheme (so the `type` opcode also covers non-serializable stack-only types):

| Code | Type | Code | Type |
| --- | --- | --- | --- |
| 0 | Int253 | 7 | Cell |
| 1 | String | 8 | Merlin |
| 2 | Dict | 9 | Variable |
| 3 | Point | 10 | Expression |
| 4 | Token | 11 | Constraint |
| 5 | WideToken | 12 | MultiscalarMul |
| 6 | ClearToken | | |

### Cell, actor, and send instructions

[`open`](#open), [`signcall`](#signcall), and [`call`](#call) all create isolated call frames as described in [Design](#design).

### input

_s_ → _cell_

Materializes a `cell` handle from the String on top of the stack. Seeds the frame's `last_anchor` to `Anchor(cell.id())` (the input cell's id is a spend-once unique source — see §Anchors), unconditionally replacing any prior value. Emits `TxEntry::Input(cell_id)`.

**Witness path (prover).** The prover pushes `StringWitness::Cell(c)` through `String::cell(c)`; its Token payloads still carry `Commitment::Open` quantities and flavors. `to_cell()` extracts the cell directly, so open commitments survive into downstream `mix`/`commit` without a separate witness queue.

**Opaque path (verifier).** The verifier pushes `String::Opaque(cell_bytes)`. `to_cell()` runs `Cell::decode`, producing `Commitment::Closed` everywhere. The verifier-side CS rebuilds the commitments from points only.

Both paths produce the same `cell.id()` and the same `TxEntry::Input` (the txlog is byte-canonical regardless of which String variant the prover chose). Cell payloads are immutable after construction. The public `Cell::new` constructor rejects any non-portable top-level item, including a Dict whose sticky `portable` flag has been cleared. `Cell::decode` applies the same admission rule after representation decoding. That top-level Cell-domain scan is intentional and sufficient: nested Dict portability is an O(1) lookup of the flag reconstructed while decoding, not a recursive walk. Generic `read_value` itself remains policy-neutral and may decode values that no Cell may contain.

**The VM does not consult any Utreexo accumulator.** The caller must validate the supplied bytes against the Utreexo proof outside the VM before invoking the script. The txlog entry commits the script's reliance on that external check.

Hard-fails on non-String top, malformed bytes (`MalformedCellEncoding`, including trailing bytes), or invocation from internal context.

### cell

_items… k pred_ → _cell_

Pops `pred: Point`, then `k` portable items. Consumes the frame's `last_anchor` via a split (see §Anchors): the `left` half goes into the new cell's `anchor` field, the `right` half replaces `last_anchor`. Wraps everything into a linear `cell` handle.

Hard-fails `AnchorMissing` if no anchor has been claimed yet, `NonPortableInOutput` if any item isn't portable.

### output

_items… k pred_ → ø

Same construction as [`cell`](#cell) but emits an `Output` effect into the txlog instead of pushing the handle.

### open

_cell internal_key neighbors position script gas args… k_ →
_{results… k' 1 | cell args… k 0}_

Verifies the Taproot proof against the cell's predicate:

1. Pops `k` (Int253) and `args` (k portable values). A non-portable argument
   hard-fails `NonPortableInCall` before child entry.
2. Pops `gas` as a non-negative `Int253` gas allotment.
3. Pops `script` (String) — the revealed leaf bytes (or witness-bearing `StringWitness::Script` on the prover).
4. Pops `position` (String, bit-packed path), `neighbors` (list-Dict of 32-byte Strings, leaf-to-root), `internal_key` (Point).
5. Pops `cell`.
6. Constructs a `TaprootProof` and verifies `predicate.verify_taproot_proof` — checks the Merkle root and the tweaked-key relation `P = X + h(X, M)·B`.
7. On success, creates a new isolated `CallKind::CellOpen { predicate,
   external_context, caller_id }` frame with the popped `gas` allotment, pours
   the cell payload then `args` onto its stack, and enters the unlocked script.
   `caller_id` is only the canonical 32-byte id of the directly invoking actor;
   it is `None` when the direct parent has no actor identity. The frame's
   starting anchor is the child-anchor split from the parent.

The new frame never has actor identity or actor authority. Actor-state,
storage, code, public issuance, `selfid`, and synchronous `call` operations
hard-fail. `callerid` exposes only the direct `caller_id`; a nested `CellOpen`
therefore sees zero instead of transitively inheriting an earlier actor.
Asynchronous `send` remains available but is anonymous (`Message.caller =
None`), so caller introspection cannot become delegated authority. The frame
inherits CS access from its execution context (external root → available;
internal → unavailable). Results return as `results… k' 1`; clean fall-through
returns `0 1`; leftover gas refunds to the parent.

Once the child is entered, any hard failure, out-of-gas condition, dirty EOF, or
bad return arity rolls back its effects and returns the original locked `cell`
followed by the explicit `args`, their count `k`, and status `0`. The Cell is
contextual and is not included in that count. The payload is not
returned separately: it remains sealed in the restored Cell. Invalid operands,
invalid proofs, non-portable arguments, insufficient caller gas, and call-depth
rejection occur before child entry and hard-fail the current frame.

Position bits are read LSB-first within byte, zero-extended past the end; bit `0` = current hash on left, neighbor on right; bit `1` = swap.

Hard-fails: `TaprootProofMismatch`, `MalformedTaprootProof`, plus the type errors from each pop.

### send

_args… k refund gas addr_ → ø

Asynchronous message-send. Pops operands top-first:

1. `addr` (String) — a bare 32-byte hash or an exact encoded `ActorID`:
   `0x00 || hash[32]` or `0x01 || code_len:u64-le || constructor_code`.
2. `gas` (`Int253`) — gas allotment.
3. `refund` (32-byte String) — bounce predicate point.
4. `k` (`Int253`) — args count.
5. `args…` — k portable values, delivery payload. A caller may include Flame
   here for the destination actor to spend with `addstorage`.

The full `gas` grant is debited from the active frame before the Send effect is
recorded. Insufficient remaining gas hard-fails `OutOfGas` and no message is
emitted. The grant is never refunded: asynchronous execution has no live caller
to receive a remainder. An internal transaction therefore cannot reserve more
gas across its own descendant sends than it received in its triggering Message.

Splits the frame's `last_anchor` (see §Anchors): the `left` half becomes
the message's `anchor`, the `right` half replaces `last_anchor`. Hard-fails
`AnchorMissing` if no anchor has been claimed yet. Emits
`TxEntry::Send(Message)`; the block builder reads these records when constructing
internal deliveries. An actor frame contributes its actor id as
`Message.caller`; `ExternalRoot` and `CellOpen` contribute `None`. `None` means
“no authenticated actor principal,” not necessarily “originated in an external
transaction.”

The payload is admitted by `Message::new`, not by `Message::encode`.
`Message::new` scans top-level arguments and uses the sticky Dict flag for O(1)
nested checks; once constructed, the payload cannot be replaced.

The send's identity is the canonical 32-byte `MessageID = H(b"flamevm.message.id" ‖ Message.encode())` — `Message::id()`. The wire encoding `Message.encode()` writes the fields in fixed order:

1. `anchor` — 32 raw bytes.
2. `target` — `ActorID::encode` preserving its Hash or Constructor variant.
   Constructor code must survive until first delivery, so the two forms that
   route to the same registry key intentionally have different message bytes.
3. `caller` — `0x00` for None, `0x01 ‖ ActorID::encode(canonical)` for Some.
4. `refund_predicate` — 32-byte compressed Ristretto.
5. `gas` — little-endian u64.
6. `payload` — little-endian u64 count, then each value's canonical `Value` encoding.

The merkle leaf for `TxEntry::Send` commits to this single 32-byte MessageID, just as `TxEntry::Output(Cell)`'s leaf commits to `Cell::id()`. Uniqueness is inherited from `anchor`: every distinct send carries a distinct anchor, hence a distinct MessageID.

Available in both contexts. Hard-fails `MalformedAddress` when `addr` is neither
a bare hash nor one complete canonical `ActorID`, or when `refund` has the wrong
size; `NonPortableInSend` on non-portable args; and `InvalidBitrange` on a
negative or overflowing gas allotment. It hard-fails `OutOfGas` when the active
frame cannot prepay the requested grant.

On internal-tx failure during delivery, consensus seals the message payload into a fresh cell under `refund_predicate` and emits it as an Output effect — see the design.

### call

_args… k gas addr_ → _{results… k' 1 | args… k 0}_

Synchronous actor-to-actor call. Same operand shape as [`send`](#send) minus
`refund` and, likewise, no method selector. A caller that wants the callee to
purchase storage passes Flame among the ordinary arguments; allocation remains
the callee's explicit decision. A constructor-form address is accepted but
canonicalized to its hash for lookup: unlike first `send` delivery, `call` does
not deploy an absent actor.

Only `InternalRoot` and `ActorCall` may invoke `call`. `ExternalRoot` and
`CellOpen` hard-fail `OpcodeRequiresActorContext` before consuming operands;
caller attribution stored in a `CellOpen` is read-only and is never used to
fabricate an actor caller.

Before entering the callee, `call` rejects every non-portable argument with
`NonPortableInCall`. This is a top-level scan; a nested Dict is checked in O(1)
through its sticky flag.

**Re-entrancy:** the state-checkout lock closes issues with re-entracy: state is reachable **only** via `load`, which acquires the lock, and there is no peek-state opcode, so a half-applied update is never observable — a re-entrant `call`/`load` into an actor that has already `load`ed its state cannot enter. The lock does **not** enforce checks-effects-interactions ordering *within* a load/save window: holding a loaded state across a `call`/`send`/`open` is legal, but anything `save`d afterward is a pre-call snapshot — authors must `save` (or fully discharge) before calling out. Nested call depth is capped at `MAX_CALL_DEPTH` (64).

**Emits no txlog entry.** Calls are intra-tx control flow; the structural effects produced inside the callee (`Output`, `Send`, `ActorSave`, `Issue`, `Retire`, `Fee`, `Data`) are what the state machine reads. The `(External TxID, Internal TxID)` of a tx is a merkle root over effects only — see the effect model above.

Creates an isolated `CallKind::ActorCall { actor, caller }` frame (the frame's starting anchor lives in `CallFrame.anchor` — todo #4) with the popped gas allotment. The frame has the callee's actor identity — `op_load`/`op_save`/`op_call`/`op_send` operate on the callee.

Successful return is `results… k' 1`; clean fall-through is `0 1`. Before
entry, missing/empty actors, depth rejection, and a grant too small to activate
the callee return the original `args… k 0`. Availability rejection refunds the
grant; an undersized child grant is burned. After entry, every runtime failure
rolls back the child and returns the same escrowed arguments and failure suffix;
the failed child's current stack is discarded. `RegistryUnavailable`, malformed
operands, non-portable arguments, or inability of the caller to fund the grant
hard-fail the current frame before this soft boundary.

### load

ø → _value_

**Checks out** the current actor's state: moves the state `Value` out of the registry (the actor goes empty/`None`) and pushes it onto the stack. State is **any portable `Value`** — the author chooses its structure; the VM imposes no shape . Code is separate (`setcode`) and stays put during the call.

While checked out, synchronous `call` and `load` against the actor fail
`ActorEmpty` — the **state's presence is the re-entrancy lock**. `send` is
asynchronous and does not inspect recipient state when queued; availability is
resolved later during delivery. The state is a linear resource: `load` moves it
(no copy); the frame must discharge it before returning, per the frame-end
clean-stack rule. Re-loading an already-checked-out actor → `ActorEmpty`.

Hard-fails: `OpcodeRequiresActorContext`, `RegistryUnavailable`, `ActorEmpty` (already checked out), `ActorNotFound`.

**Self-destruct.** A frame discharges a loaded state by either `save`ing it back
(the actor persists) or **explicitly dismantling it** — recursively reading
every item out, retiring/spending tokens, and dropping the droppable residue. A
fully dismantled state leaves the actor empty; the transaction emits
`ActorDestroy` and removes its code and state. Unexpired leases are not refunded
and recycle only at their original expiry heights. Forgetting to discharge is
caught loudly by `StackNotClean` and rolls the transaction back.

### save

_value_ → ø

Pops the state `Value`, **validates portability**, measures the prospective actor usage according to [Actor storage](storage.md), and **moves it back** into the current actor — which must be **checked out** by a prior `load`, else `SaveWithoutLoad` (saving to a non-checked-out actor would clobber, and silently drop the tokens of, live state). `save` hard-fails `StorageCapacityExceeded` if prospective usage exceeds capacity at the current core-block height. Emits `TxEntry::ActorSave { actor, state }` carrying the **full** post-save state — symmetric with `Output(Cell)` which carries the full Cell. The merkle leaf for this entry hashes `(actor.to_hash(), state_root(&state))`, so the TxID commits to the canonical state root while consumers reading the txlog directly get the bytes (no separate state-witness channel needed).

**State is any portable Value.** The author structures state however they like (a Dict, an Int, a Token, …).

**Portability is the canonical storage gate.** Portable values: `Int253`, `String`, `Point`, `Dict` of portable, non-negative `ClearToken`, `Token`. Non-portable values (`Cell`, `Merlin`, `Variable`, `Expression`, `Constraint`, `MultiscalarMul`, `WideToken`, negative `ClearToken`) hard-fail `NonPortableInState`.

The registry repeats this check on direct save and deploy entry points, so the
actor-state invariant does not depend on `op_save` being the caller. As at the
other domain boundaries, nested Dicts are checked through their cached flag and
state encoding performs no portability validation.

### setcode

_code_ → ø

Pops a String, measures prospective usage, and replaces the current actor's
**code blob** with its bytes. It hard-fails `StorageCapacityExceeded` if the new
code would exceed current capacity. On success it records
`TxEntry::SetCode { actor, code }`, symmetric with [`save`](#save)). It is
method-agnostic and independent of the state checkout lock. Requires actor
context.

**Upgrade is author policy over a VM mechanism.** The VM provides `setcode`; *who* may call it is gated in the actor's own code. Actors normally authenticate by **caller identity** — `require(callerid() == GOV)` — and may use `signcall` when immediate signature authorization is required. Internal execution has no deferred signature batch: `signcall` verifies synchronously before entering its child. A naked `setcode` with no caller gate is an unconditionally-upgradeable (i.e. rug-able) actor — deliberately the author's call. See the design.

Hard-fails: `OpcodeRequiresActorContext`, `RegistryUnavailable`, `TypeNotString`, `ActorNotFound`, `StorageCapacityExceeded`.

**Atomicity.** A `save` failure (`SaveWithoutLoad`, `NonPortableInState`,
`StorageCapacityExceeded`, `ActorNotFound`) propagates as a frame failure. The
frame's call boundary rolls back both the txlog and actor registry. At the
outermost frame, `execute_internal` applies the same rollback at transaction
level.

Hard-fails: `SaveWithoutLoad` (actor not checked out), `NonPortableInState`, `StorageCapacityExceeded`, `OpcodeRequiresActorContext`, `RegistryUnavailable`, `ActorNotFound`.

### addstorage

_q_ → _debt 1 | 0_

Pops `q` as a positive `Int253` byte count. The request must be at least
`MIN_LEASE_BYTES`, must be a multiple of `STORAGE_UNIT_BYTES`, and must leave at
least `MIN_REMAINING_POOL_BYTES` in the global pool. The constants, quote
formula, block ordering, and lease rules are defined in
[Actor storage](storage.md).

On success, atomically:

1. recomputes the fee against the current reserve;
2. removes `q` bytes from that reserve;
3. adds or coalesces a lease for the current actor at
   `current_height + LEASE_DURATION_CORE_BLOCKS`;
4. emits `TxEntry::StoragePurchase { actor, bytes, expiry_height, fee_sparks }`;
5. pushes `ClearToken(-fee_sparks, FLAME_FLAVOR)` followed by `1`.

The negative token must be balanced with actual Flame before transaction
finalization. The storage-purchase effect burns that amount rather than paying a
minter. A later failure rolls back the pool, lease, effect, and burn.

An unavailable or unrepresentable request consumes `q`, pushes `0`, and changes
nothing. Type and context violations hard-fail: `TypeNotInt253`,
`OpcodeRequiresActorContext`, or `RegistryUnavailable`.

### quotestorage

_q_ → _fee 1 | 0_

Performs the same request, reserve, arithmetic, and representation checks as
[`addstorage`](#addstorage). On success it consumes `q` and pushes the positive
integral total fee in sparks followed by `1`. On an unavailable quote it
consumes `q` and pushes `0`.

This opcode is read-only: it does not reserve bytes, add a lease, emit an effect,
or change the price. `addstorage` always recomputes its quote, so an intervening
purchase may change the fee or make the request unavailable. Type and context
violations hard-fail under the same conditions as `addstorage`.

### signtx

_cell_ → _items… k_

Pops the cell, records a `DeferredSig::TxBound { verification_key: cell.predicate.point, cell_id }` for the delegate to verify at finalize against the eventual TxID, pours the cell's payload onto the current frame's stack, and pushes the count `k`.

**No new frame** — the cell-holder is authorizing the existing transaction in place.

`signtx` is external-only and checks its context before consuming the Cell.
Internal transactions have no signature envelope and their final TxID does not
exist when the opcode executes, so the TxID-bound signature cannot be verified
immediately. Internal authorization uses `signcall` instead.

The deferred signature is verified at finalize: the prover aggregates all `TxBound` keys via MuSig and supplies the envelope signature; the verifier batches all `TxBound` items against the `flamevm.signtx` transcript bound to TxID. Errors `BatchSignatureVerificationFailed` or `MissingTxBoundSignature` at finalize.

### signcall

_cell script sig gas args… m_ →
_{results… k' 1 | cell args… m 0}_

Same call-frame mechanics as [`open`](#open) — taproot reveal is replaced by signature verification:

1. Pops `m` (Int253), `args` (m portable values), and `gas`. A non-portable
   argument hard-fails `NonPortableInCall` before child entry.
2. Pops `sig` (String, exactly 64 bytes — Schnorr signature).
3. Pops `script` (String) and `cell`.
4. Builds `signcall_message(script_bytes)` via a Merlin transcript labelled
   `flamevm.signcall`. Scripts bind themselves to further context (anchor,
   actor identity, tx data) through explicit checks inside the signed program.
5. In external execution, records `DeferredSig::Explicit` for final batch
   verification. In internal execution, parses and verifies the signature
   immediately against the Cell predicate; malformed bytes fail
   `BadSignatureBytes`, while a validly encoded but incorrect signature fails
   `SignatureVerificationFailed`. Internal execution records no deferred item.
6. After verification or deferral, snapshots rollback state, creates the
   isolated `CellOpen` frame, and pours payload + args into the signed script.

External deferred signatures are batch-verified at finalize alongside any
`signtx` items. Entered-child failure removes an external deferred signature
and returns the original Cell plus explicit arguments using the same failure
shape as `open`; the count is `m`, excluding the contextual Cell. Internal
signature failure occurs before child entry and hard-fails the current frame.

### timelock

ø → _n {0|1}_

Pushes the transaction's `locktime` (as `Int253`) and a unit flag: `0` for block height, `1` for Unix timestamp. The split follows Bitcoin's BIP-65 convention — `flag = 1` iff `locktime ≥ 500_000_000` (`LOCKTIME_TIMESTAMP_THRESHOLD`). Values below the threshold are block heights; values at or above are Unix timestamps (the threshold corresponds to ~1985-11-05, before any practical timestamp range). Available in either context.

### version

ø → _n_

Pushes `TxHeader::version` as a non-negative `Int253`. Available in either context.

### selfid

ø → _s_

Pushes the current frame's actor id as a 32-byte String. Hard-fails `OpcodeRequiresActorContext` from `ExternalRoot` or `CellOpen` (no actor identity).

### anchor

ø → _s_

Pushes the frame's *current* `last_anchor` as a 32-byte String — the value the next consume site would split. Hard-fails `AnchorMissing` if no anchor has been claimed yet (same rule as `cell` / `output` / `send` / `call`). Available in either context.

### gas

ø → _n_

Pushes the current call's remaining gas budget — i.e. `gaslimit − gas_used` (saturating). Available in either context.

**Gas metering.** Every execution attempt costs 1 gas before checking for EOF;
executed instructions and instructions scanned while seeking a forward label
cost the same. Thus a clean program with `N` instructions consumes at least
`N + 1` gas. Prover-side decoded instructions and verifier/internal byte streams
walk the same sequence. Label tables are collected afresh per frame, so gas is
a pure function of code and inputs rather than cache history.

Variable-size allocation work is additional gas: canonical code and decoded
payload activation, String byte growth, Dict/collection growth, rollback
escrows, actor-state activation and cloning, constraint terms, range-proof
terms, MSM/batch vectors, and `mix`'s quadratic work are charged before the
corresponding allocation. Fixed-size stack pushes are bounded by the base
instruction charge. Allocation charges are monotonic and are not refunded when
memory is freed; this is a deterministic logical-work bound, not a dependency
on Rust object sizes or allocator behavior. Prover-only witness presence never
changes gas. Exhaustion hard-fails `OutOfGas`.

Expensive deterministic work is prepaid where it is requested, even when the
actual cryptographic check is deferred until transaction finalization. External
roots prepay proof-finalization overhead; hashing is charged by compression
block; `open` charges point decompression; `signcall` and `signtx` charge
signature verification; R1CS-building instructions charge estimated
multipliers/items; and MSM/decrypt verification charges decompression plus the
scheduled final MSM. Prover and verifier take the same charges.

The current schedule was calibrated with
`cargo bench -p flamevm --bench gas` on 2026-08-23 on an arm64 machine. One gas
was conservatively treated as approximately 100 ns of reference verifier work.
Representative medians were:

| Work | Median |
|---|---:|
| SHA-256, 64 KiB | 132.45 µs |
| SHA-512, 64 KiB | 85.12 µs |
| SHA3-256 / Keccak-256, 64 KiB | 71.5 µs |
| valid Ristretto decompression | 2.45 µs |
| immediate signature verification | 33.4 µs |
| 64-signature batch | 1.108 ms |
| R1CS verification, 64 multipliers | 0.968 ms |
| R1CS verification, 512 multipliers | 4.97 ms |
| MSM finalization, 64 terms | 98.5 µs |
| MSM finalization, 1,024 terms | 0.997 ms |

The measured hash, R1CS, and MSM curves remained bounded by the selected linear
prices over the tested ranges, so no arbitrary crypto-size cap is added. Checked
gas multiplication and the enclosing transaction/block gas limits provide the
bound. The logical allocation prices remain intentionally more conservative
than measured allocator work.

Call entry (`call` / `open` / `signcall`) debits the full gas grant from the
caller. A caller that cannot afford the grant hard-fails; leftover gas is
refunded on clean return and `gaslimit` remains the immutable creation cap. A
failed entered call burns the grant. An actor call rejected for availability
before entry refunds it, while a child grant too small to activate its code is
burned. `send` debits its full grant from the sender and never refunds it;
delivery and every descendant send execute within that prepaid budget.

### usage

ø → _n_

Pushes the current actor's occupied persistent-storage bytes, including code,
canonical state encoding, and lease records, as defined in
[Actor storage](storage.md). While state is checked out, the result measures the
checked-out committed state until `save` supplies a replacement. Hard-fails
`RegistryUnavailable` or `OpcodeRequiresActorContext` outside actor execution.

### callerid

ø → _s_

Pushes the directly invoking actor's canonical 32-byte id. `InternalRoot` and
`ActorCall` use their recorded caller; `CellOpen` uses its compact read-only
`caller_id`. It pushes the all-zero String when that caller is absent and
hard-fails only from `ExternalRoot`, which has no caller frame. A `CellOpen`
opened by another `CellOpen` sees zero: actor attribution is not propagated
through predicate frames.

### gaslimit

ø → _n_

Pushes the current call's total gas budget cap (the value set at frame creation, not the remaining amount). Available in either context.

### capacity

_h_ → _n_

Pops a nonnegative current or future core-block height and pushes the current
actor's leased capacity in bytes at that height:

```text
capacity(h) = STORAGE_UNIT_BYTES
            * sum(lease.units where lease.expiry_height > h)
```

The query is read-only and does not expose the lease list. A negative or
non-`u64` height hard-fails `InvalidBitrange`; a height below the current
core-block height hard-fails `StorageHeightInPast`. Outside actor execution it
hard-fails `OpcodeRequiresActorContext` (or `RegistryUnavailable` when no actor
registry was supplied).

### Chain-info instructions

`height` reads the implemented consensus-supplied `BlockContext`. The historical
opcodes `blockhash`, `blockburn`, `blockweight`, `blockrate`, and `chainstate`
below remain planned: their bytes are reserved but currently fail
`UnknownOpcode`. Their proposed 100-block maturity rule is not active.

### height

ø → _n_

Pushes the current core-block height during an internal transaction. Pushes zero
throughout an external transaction, including its nested `CellOpen` frames.

### blockhash

_h_ → _s_

Pushes the 32-byte block hash at height `h`.

### blockburn

_h_ → _n_

Pushes the total satoshis burned at height `h` (Bitcoin-coupled metric).

### blockweight

_h_ → _n_

Pushes the block weight at height `h`.

### blockrate

_h_ → _n_

Pushes the average mint rate (sparks per satoshi) at height `h`.

### chainstate

_n_ → _dict_

Pushes a Dict of block stats at height `n`.

---


## Isolation & binding notes

**Cell-open trust model.** `open`, `signcall`, and `call` all create isolated call frames: the unlocked / signed / called script runs in its own stack, gas budget, and identity scope, with no implicit access to the host's actor state, gas pool, or identity. This eliminates the confused-deputy class of bugs — an actor accepting an untrusted-source cell need not audit the predicate as a global authorization filter, because the script cannot reach the actor's state regardless of what the predicate authorizes .

**`signcall` binding policy.** The `signcall` signature commits to the script bytes only, whether it is verified immediately in internal execution or deferred in external execution. The script binds itself to further context (anchor, actor identity, tx-level data) via explicit checks such as `anchor <expected> eq verify`. Binding policy lives in the author's hands — flexibility at the price of footgun.
