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

Flame supports two forms of smart contract. **Contracts** are immutable,
single-use values held in Utreexo; each locks a payload under a **Predicate**.
**Actors** are persistent identities with state and directly executable code,
entered through asynchronous `send` or synchronous `call`. One stack machine
verifies both: external transactions consume and create Contracts and may emit
messages; internal transactions deliver messages and synchronously call Actors.

Values have explicit copy and drop capabilities. Bearer values—including tokens
and Contracts—are linear. Portable values may cross Contract, Message, and
actor-state boundaries; ephemeral verification values such as transcripts,
expressions, constraints, and MSMs may not.

The VM emits an ordered log of state effects. Inputs, outputs, issuance,
retirement, fees, sends, actor saves, storage purchases, actor destruction, and
explicit data are effects. Calls, branches, signatures, constraints, and batch
verification are execution machinery and do not appear in the log. TxID commits
to the ordered effects.

Every `open`, `signcall`, and actor `call` runs in an isolated frame with its own
stack, control flow, and gas budget. Successful calls return only explicitly
selected values. Failed calls restore state effects, constraints, deferred
signatures, fees, and batch-verification work to their entry checkpoints. They
also return entry-owned values: actor calls return their arguments, while Contract
predicate calls return the original locked Contract followed by their explicit
arguments.
All downward call arguments must be portable, just like asynchronous `send`
payloads. Return values are unrestricted: non-portable liabilities and
VM-local values may travel upward so the caller can resolve them, but they may
not be delegated to another callee.

Actor state is its re-entrancy lock. `load` moves state out of the registry;
while checked out, another frame cannot enter, observe, or mutate that actor.
The loaded value remains exclusively owned by the current frame across nested
interactions and may be saved afterward; it cannot become stale through
re-entrancy. The VM does not interpret actor policy, so the actor must still
account for a callee's status and effects when deciding what state to save.

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

1. Inputs — consumption of entries from Utreexo that produce *contracts* on VM stack.
2. Outputs — creation of new entries in the Utreexo.
3. Sends — messages sent to actors that produce *internal transactions*.
4. Fee — payment of the transaction fees.
5. Issuance and retirement — creation and removal of tokens to/from circulation.
6. Data entry — for data logging that does not occupy permanent storage.

Transaction ID (”TxID”) is the CellID of the ordered TxLog Trie envelope. External logs begin with `Header`, then `CellWitness(BoCID)` committing the exact initiating execution bag. Transaction signatures and ZK proofs bind to TxID and therefore to the effects **and witness availability**.

## Internal transactions

Internal transaction is caused by a “message” sent to an “actor” by an external transaction. Such message does not control the outcome: internal transaction may produce undetermined result or even fail. As such, race conditions (when multiple users send messages to the same actor) can be resolved by the actor itself, accepting all the messages.

Internal transactions are best used for public multiplayer apps, where an actor can be accessed by anyone in any order.

Internal transactions do not have a pre-determined effect and therefore do not support signatures and ZK proofs bound to transaction ID.

Like external, internal transactions produce effects:

1. `Receive(MessageID)` (txlog variant `TxEntry::Receive`) — the consumed Send's id. Emitted automatically as the first effect after `Header` by `VM::execute_internal`, committing the originating `MessageID` (canonical 32-byte hash of the whole Send: anchor, target, caller, payload, gas, refund predicate — analogous to `ContractID` for contracts) into the Internal TxID merkle root. Symmetric with `Input` for external transactions. The message **payload is delivered onto the recv frame's stack** in payload order before code runs — symmetric with `op_call` pushing its args; a conventional dispatch selector rides as the topmost payload arg.
2. Actor deployment — successful first delivery to a constructor-form target emits an immediate `ActorDeploy { actor, code }`; replay starts it with the canonical empty state.
3. Outputs — creation of new entries in the Utreexo.
4. Sends — messages sent to actors that produce other internal transactions.
5. Issuance and retirement — creation and removal of tokens to/from circulation.
6. Actor mutations — `ActorSave` carries the full replacement state and `SetCode` carries the full replacement code.
7. Data entry — for data logging that does not occupy permanent storage.
8. Storage purchases — `addstorage` records the actor, purchased bytes, expiry
   height, and burned sparks.

Lease expiry freezes actor code/state bodies instead of destroying their
linear contents. Actor roots and lease metadata survive. A later execution
may recover data from its committed BoC; reading does not implicitly persist
it. Explicit dismantling can still emit `ActorDestroy`. See
[Actor storage](storage.md#global-state-and-block-order).

Each external transaction immediately drains its FIFO message descendants
before the next external transaction, with the same immutable execution BoC.
Witness bodies from another external transaction are never added to that bag.

Calls themselves are intra-transaction control flow, not effects. Anything a callee does that the outer world cares about appears through one of the effects above.

Canonical internal log shapes are enforced by the state machine. Success is `Header, Receive, [ActorDeploy], effects...`, where deployment is permitted only immediately after `Receive` and any `ActorDestroy` entries form a suffix. Failed delivery is exactly `Header, Receive, Output(refund)`. Lease expiry freezes bodies without emitting retirement/destruction effects. External logs start with `Header, CellWitness(BoCID)` and cannot contain internal-only actor effects.

**TxLog transport.** Effects are **re-derived by re-executing** the bytecode under the proof/signature binding; a supplied log does not authorize effects. `TxEntry` and `TxLog` implement `CellEncode` and `CellDecode` for archival and light-client transport. The log envelope holds a u64-LE count and a Trie of consecutive u64 big-endian indices, whose leaves reference tagged TxEntry Cells. Existing tags 0–14 are retained and tag 15 is `CellWitness`. Blob fields use snakes; typed fields use their expected Cell layouts. See [Data encoding](encoding.md#txlog).

Internal transactions do not pay transaction-prioritization fees. Actors may
nevertheless burn Flame for storage through `addstorage`. Internal transactions
do not support inputs, as those can be consumed only by external transactions
with a Utreexo proof and, most of the time, a transaction signature.

## Limits

**Transaction gas limit:** the declared `Limits.gas` is the execution budget for
one external transaction. It includes gas permanently reserved by direct
`send` effects. The VM hard-fails before actual use can exceed the declaration,
and the network envelope caps the declaration itself.

**Gas credit:** for one verified external transaction:

```text
direct_send_gas = sum(gas grant of each Send in the derived TxLog)
gas_credit      = gas_used - direct_send_gas
```

`gas_credit` is the actual external execution work. It has a per-transaction
limit and contributes to the block's external-gas total. Direct send grants
contribute instead to the block's internal-gas total. Nested sends are not
counted again: they partition gas already granted to their ancestor message.

**Script size limit:** counts canonical external-transaction script bytes, both
per transaction and across a block. Dynamically loaded actor code is persistent
state and its execution/allocation work is charged in gas; it is not counted a
second time as external witness script.

**Multiplications limit:** `TxMetrics.multiplications` is the exact number of
Bulletproofs multiplication gates in the final constraint system, including
randomized constraints and excluding failed-child work rolled back at a call
boundary. It is limited per transaction and in aggregate per block.

**Issued storage:** `StorageParams.issued_units_per_block` adds exactly that
many units to the global pool at each core block. Storage purchases do not
issue bytes; they move already-issued units from the pool into actor leases.
The storage supply invariant checks initial plus per-block issuance against the
pool and all live leases.

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

Plain data types: scalars, byte strings, Ristretto points. These can be copied and ported.

Structured data types: dicts that are used as lists, dictionaries and enum variants. Dicts are **never copyable** and cache sticky portability and droppability capabilities (see §Dict).

Bearer types: Contract, WideToken, Token and ClearToken.

Constraint-system types: Variable, Expression and Constraint.

Cryptography types: Merlin transcript. (Batched scalar-point
checks are not user-visible — see the note on `MultiscalarMul`
below.)

Portable types can cross a Contract, Message, actor-call, or actor-state boundary.

Copyable types can be duplicated by the VM. Every copyable value is also
droppable, but non-copyable pure-computation values may be droppable too.

| Type | Copyable | Droppable | Portable |
| --- | --- | --- | --- |
| Scalar, String, Point | yes | yes | yes |
| Dict | no | empty or sticky flag | sticky flag |
| Token | no | no | yes |
| ClearToken | no | quantity is zero | centered quantity is non-negative |
| WideToken, Contract | no | no | no |
| Merlin, Variable, Expression, Constraint, MultiscalarMul | no | yes | no |

Droppability tracks asset ownership, not whether a value is mutable or useful.
Dropping a transcript, expression, constraint, variable, or unscheduled MSM
abandons computation but cannot destroy bearer value. A Dict starts droppable;
inserting any non-droppable value clears its sticky flag. A fully drained Dict
is nevertheless droppable, allowing the empty container shell to be discarded.

Portability is a business-logic capability, not a serialization property.
Generic value codecs describe representation only. For example, a negative
`ClearToken`, or a Dict containing one, may be encoded and decoded for
diagnostics even though Contract, Message, call, and actor-state admission reject
it. A type may also be non-portable and have no implemented value encoding;
these are separate questions.

Tuples are not distinct types, but a name for passing multiple items on the stack and through calls. During VM execution any tuple is simply a number of items on stack.

Optionals are not distinct types, but a convention to return tuple (values…, 1) or (0). Instruction `verify` can be used to "unwrap optional" and fail immediately if the result is missing.

**Implementation layout (not consensus).** Heap-indirection keeps the Rust
`Value` enum compact without changing VM semantics: `Merlin` boxes its
`Transcript`, `Value::Contract` boxes the otherwise unchanged `Contract`, secret
equality constraints box both `Expression`s, and `String` boxes all typed
`StringWitness` variants while leaving `Opaque(Vec<u8>)` inline, and
`spacesuit::AllocatedValue` boxes its optional cleartext assignment. On the
current 64-bit build this makes `Value` 80 bytes, equal to its largest remaining
inline variant (`Token`), while `WideToken` is 40 bytes. Rust layout and byte
counts are implementation details; the canonical wire representation is
unchanged.

Implemented generic `Value` Cell encodings:

| Type | Tag | Representation after the tag |
| --- | ---: | --- |
| Scalar | 0 | canonical 32-byte LE residue |
| String | 1 | one reference to a payload-only Cell of 0..8191 raw bytes |
| Dict | 2 | one reference to count/flags/Trie envelope |
| Point | 3 | compressed Ristretto:32 |
| Token | 4 | qty commitment:32, flavor commitment:32 |
| ClearToken | 6 | qty Scalar:32, flavor Scalar:32 |

Other Value tags are rejected. An expected concrete type does not repeat a tag.
See [Data encoding](encoding.md) for the complete Cell layouts.

Stack-only types (non-portable and without an implemented `Value` encoding):

| Type | Description |
| --- | --- |
| Contract | Linear contract handle. `Contract` has its own top-level encoding, but `Value::Contract` has no generic value tag. |
| WideToken | Possibly-negative encrypted token tied to the current constraint system. |
| Merlin | Mutable transcript state tied to the current execution. |
| Variable | Secret value in the constraint system, tied to a Pedersen commitment. |
| Expression | Linear combination of variables. |
| Constraint | Logical combination of boolean conditions. |
| MultiscalarMul | Lazy `sum(s_i · P_i)` accumulator; consumed by `verify` which appends it to the same batch as Schnorr/Musig sigs (assertion: `sum == identity`). |

### Encoding

FlameVM values use `CellEncode` / `CellDecode`, not the flat Reader/Writer
layer. Cell IDs commit payload and ordered child IDs, independently of
residency. Expected types have no schema/version prefixes. A String occupies
one Cell's raw payload, with no length prefix or references. Dict uses one fixed-width Trie format for all
key patterns, with big-endian Scalar paths preserving numeric order.

The encoder does not impose portability. Negative ClearTokens and Dicts with
encodable non-portable members may be encoded. Business-logic admission into
Contracts, actor state, messages, and downward calls checks portability.
Unsupported computation-only variants have no Value encoding.

Typed imports reject malformed tags, noncanonical Scalar residues, invalid
counts/flags/paths, trailing payload, and trailing references. Generic imports
validate the whole supplied value graph, with a depth bound. Authenticated
Contract/Actor reads can retain pruned Dict branches and trust previously
admitted count/capability summaries; a requested missing branch is an error,
not an absent key.

The VM resolves resident references first, then the current actor's explicitly
retained code/state bodies, then the initiating transaction's immutable BoC.
Actor-layout and lease metadata are included in registry storage exports, not
in the `actor_cells` execution fallback. No other actor or global cache supplies
implicit witnesses.

Group-element validation remains lazy: decoding a Point or Token preserves its
32-byte compressed representation without decompression. Cryptographic use
performs that validation. This permits byte-identical public/private encoding;
it does not make invalid points usable.

The compact **instruction** grammar is separate from data encoding.
`pushint` still chooses the smallest operand width, and label numbers retain
their offset sub-varint encoding. These instruction bytes are carried inside
snake-encoded script Cells.

Utreexo is outside this migration: its accumulator, paths, hashing, and legacy
`merkle`/`readerwriter` codecs remain unchanged. Outer block transport merely
wraps the existing Utreexo `Proof` bytes in snake Cells.

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
| `-2^128` | `18edd3f55c1a631258d69cf7a2def9de14ffffffffffffffffffffffffffffff0f` |

### Scalar values

Flame scalars are elements of the field modulo the Ristretto255 group order
`ℓ = 2^252 + 27742317777372353535851937790883648493`. Their canonical
representation is the unsigned residue in `[0, ℓ)`, stored as 32 little-endian
bytes. There is no stored sign. Addition, multiplication, and negation are
ordinary field operations modulo ℓ; equality compares canonical residues.

`abs` and `divmod` interpret a residue `r` through its centered lift:
`r` when `r ≤ H`, otherwise `r-ℓ`, where `H = (ℓ-1)/2`. This gives the
integer interval `[-H, H]`. For example, the scalar `ℓ-1` has centered value
`-1`. This interpretation does not change the stored value or impose checked
integer semantics on field arithmetic. Financial code must establish its
own bounds to prevent unintended modular wraparound.

Scalars are used in logical operations: non-zero is “true”, zero is “false”.
Logical results are always 0 or 1. Dict keys are ordered by unsigned canonical
residue, so `ℓ-1` follows all smaller residues rather than preceding zero.

Canonical full-width scalar bytes and Dict key ordering are consensus-visible.
The fixed-width Scalar Cell encoding is not wire-compatible with the former
compact value-tag encoding. Existing state and
bytecode must be regenerated or explicitly migrated before using this format.

### String

Binary byte-aligned strings of 0..8191 bytes (`String::MAX_LEN`).

The runtime String type and its opcodes remain temporarily; replacing String
with a Cell value is a later change. Its canonical encoding is one payload-only
Cell: raw bytes, no length prefix, no references, no snake continuation. Strings
can hold bounded data, short programs, signatures, and hashes. Schema-defined
program/proof/blob fields can still use multi-Cell snakes independently of this
runtime limit. Other types can be parsed from strings.

Each string acts as a builder and a reader.

Literal parsing and VM growth reject results over the limit with
`StringTooLong` before allocating them. Witness-bearing Strings obey the same
limit on their public bytes, before witness serialization.

**Prover-side witness variants.** `String` has two in-memory shapes: `String::Opaque(Vec<u8>)` and `String::Witness(Box<StringWitness>)`. Boxing the uncommon witness shape keeps every `String` the size of a `Vec`. The witness encodes to the same canonical bytes the verifier sees while preserving typed data through `dup` / `open` / `signcall` / `call` / payload-pour boundaries:

- `StringWitness::Point(Point)` — point-shaped witness; the inner [`Point`](#point) is `Opaque`, `Commitment(Open(value, blinding))` (used before `commit`, `scalar`, `expr`), or `Predicate(p)` (used before `signtx`, `signcall`, `contract`, `output`).
- `StringWitness::Scalar(scalar)` — used before `scalar`.
- `StringWitness::Script(instructions)` — bounded inline programs used before `run` / `switch` / `signcall`; predicate-branch overlays are supplied separately.
- `StringWitness::Contract(c)` — used before `input`, carrying open commitments on Token payloads.

The verifier sees `String::Opaque(bytes)`. Atomic point/scalar downcasts preserve the same public bytes. A Contract witness emits only its 32-byte ContractID; ScriptBuilder collects its public Cell bodies and keeps private openings separately. `input` must resolve the authenticated public path before restoring private atomic witnesses. Predicate programs likewise use authenticated Cell paths; a private script overlay may only replace matching public bytecode.

**`pushpoint` also carries witnesses.** Because the `pushpoint` instruction's in-memory operand is a `Point` (not a raw `[u8; 32]`), the prover can attach a `Point::Commitment` / `Point::Predicate` witness to a literal point pushed via `pushpoint` — not only via `pushstr` + `String::point`. The wire encoding stays the canonical 32 bytes regardless.

### Point

Ristretto255 group element. Stored as compressed 32-byte encoding. Used to represent public keys, Pedersen commitments, and Taproot predicates.

**Prover-side variants.** On the prover side a Point may carry typed witness data while still serializing to the same canonical 32 bytes:

- `Point::Opaque(CompressedRistretto)` — verifier's view; no witness.
- `Point::Commitment(Commitment::Open(value, blinding))` — Pedersen commitment with cleartext opening (used by [`commit`](#commit) / [`expr`](#expr) to skip re-opening).
- `Point::Predicate(PredicateTree)` — Taproot predicate with a Cell Trie (used by [`contract`](#contract) / [`output`](#output) to attach an unlock witness, and by [`signtx`](#signtx) / [`signcall`](#signcall) for the verification key).

A Point on the value stack downcasts via `to_commitment` / `to_predicate` to extract its witness (preserved through `Point::Commitment` / `Point::Predicate`) or to wrap an `Opaque` as the verifier's `Closed` / `Opaque` form.

### MultiscalarMul

Lazy multi-scalar-multiplication: a vector of `(scalar_i, point_i)` pairs that the VM defers as the assertion `sum(s_i · P_i) == identity`. Non-copyable but droppable, stack-only, and **not** wire-encodable — exactly like [Expression](#types) and [Constraint](#types). Dropping it abandons an assertion that has not been scheduled; only `verify` commits it to the batch.

**Purpose: custom Sigma-protocols.** Together with [Merlin](#cryptography-instructions) transcripts (Fiat–Shamir challenges), `MultiscalarMul` lets contract authors express any Schnorr-style relation over Pedersen-committed data — proof of knowledge of discrete log, equality of two encryptions, proof of correct re-encryption, etc. The verification equation always reduces to "this weighted sum of group elements is the identity point".

**Batched verification.** `verify` on an MSM attempts to decompress every point while scheduling the statement and appends each result as `Some(point)` or `None` to the same `BatchVerifier` that holds the transaction's Schnorr / Musig signatures (with `basepoint_scalar = 0` so the MSM contributes only its dynamic terms). Scheduling does not accept or reject the assertion. At finalize, `optional_multiscalar_mul` rejects the entire batch if any point failed decompression or if the weighted sum is not the identity, while still amortising batch work across every Sigma-protocol assertion and signature in the transaction.

**Witness-gated failures are fail-closed.** A few opcodes can fail on the *prover* over data the verifier lacks (e.g. an out-of-`u64` range-proof assignment, a `commit`-then-`expr` over an opaque commitment with no witness). Because the proof binds the **entire** constraint system through Fiat–Shamir, any prover/verifier control-flow or CS divergence — including one caused by such a prover-only failure being caught as a sub-call `0` marker — makes the proof **fail to verify**: the verifier rejects, it can never *accept* an invalid transaction. The effect is a self-inflicted liveness edge (a prover that commits to a malformed witness produces an unverifiable tx), not a soundness break. Making such failures tx-level (uncatchable) so they never reach a marker is a deliberate liveness-hardening item, tracked for ZK review.

**Batch rollback under call failure.** The batch is a verifier-side optimization, not part of the consensus semantics — the on-chain `TxLog` and proof shapes are unchanged. The delegate owns a single `BatchVerifier` for the whole tx, with its RNG. Every opcode that appends to it (today: `op_verify` on a `MultiscalarMul`) calls `BatchVerification::append`, which multiplies the appended statement by a fresh random scalar drawn from the delegate's RNG — Schwartz–Zippel safety against canceling failures. For per-frame rollback the VM snapshots the batch state (basepoint scalar + dyn-arrays length) on the *parent* frame at every call entry (`call` / `open` / `signcall`), via `BatchCheckpoint::snapshot`. On call failure, `fail_current_call` restores the batch via `BatchCheckpoint::restore` — truncating the dyn-arrays and resetting the basepoint scalar — so any MSM appended by the failed callee is dropped. On clean return the snapshot is discarded; the child's appends stay. The RNG state is *not* rewound; random factors sampled between snapshot and restore are simply lost, which is harmless — the remaining batch terms still carry the random factors that were sampled for them.

**Construction.** MSM has no dedicated constructor opcode. Instead, the arithmetic opcodes lift Point/MSM operands implicitly:

| Operation | Result |
|---|---|
| `Point + Point` | MSM with two unit-scalar terms |
| `Point + MSM` / `MSM + Point` | MSM with the point appended (coefficient 1) |
| `MSM + MSM` | concatenated term lists |
| `Scalar * Point` / `Point * Scalar` | MSM with one term `(scalar, point)` |
| `Scalar * MSM` / `MSM * Scalar` | MSM with all coefficients scaled |
| `-Point` | MSM with one term `(-1, point)` |
| `-MSM` | MSM with all coefficients negated |

Quadratic group-element products (`Point * Point`, `MSM * Point`, `MSM * MSM`) hard-fail `TypeNotScalar` — no Sigma-protocol semantics.

**Random factor.** The `BatchVerifier` multiplies each appended statement (MSM, single-sig, multi-sig) by a fresh random scalar before summing, so a failing MSM cannot be cancelled out by other batch members (probability `< 2^-252` per statement). See `starsig::BatchVerification` for the exact construction.

### Dict

Dict is a versatile data structure for representing lists, dictionaries and even sum-type (aka “enum”) values. One-key struct is used to encode a single variant of a sum-type.

Keys are `Scalar` values. Canonical iteration follows unsigned numeric order
of residues in `[0, ℓ)`: a near-order key such as `ℓ-1` sorts after small keys.
All dictionaries use the same `cells::Trie`, with fixed 32-byte big-endian
paths. Sequential keys `0..n-1` are a list convention, not another encoding.

**Dicts are never copyable**: `dup`/`getdup` of a Dict value always fails `TypeNotCopyable`. Each Dict carries independent sticky `portable` and `droppable` flags. A newly constructed empty Dict starts with both flags true. Every successful insertion applies `dict.portable &= value.is_portable()` and `dict.droppable &= value.is_droppable()`. Removal and replacement never restore a cleared flag; a rejected strict insertion does not change either flag. Emptiness overrides only droppability: after every member is extracted, the empty Dict can be dropped while the extracted bearer values remain owned by the script. Portability remains sticky because it governs domain admission rather than disposal.

The `dict` / `put` / `replace` opcodes may insert any Value. A non-portable Dict remains usable on the stack, but `contract`, `output`, `send`, `call`, `open`, `signcall`, and actor-state storage reject it at their portability boundary. Checking a Dict is O(1), including when it is nested: inserting a nested Dict reads that Dict's already-cached flag. Dict values are owned and cannot be mutated through an alias, so the cached parent flag cannot become stale.

The Cell envelope serializes count:u64 LE and flags:u8 (portable bit 0,
droppable bit 1), followed by a Trie reference unless empty. Sticky flags
survive encode/decode, including on an empty Dict; emptiness still permits
dropping it. Generic imports validate the count, paths, and all members against
claimed capabilities. Previously authenticated storage can preserve pruned
branches and use the committed summaries without loading every member.

Drilling down the nested dict preserving ownership with `get` and `put` instructions: 

```rust
// Given a struct like this want to drill down "a",
// then "b" and update balance.
{ 1: {2: balance } }

// Program:
1 get 2 get ... put put
```

### Tokens

All token types are non-copyable. `Token`, `WideToken`, and a nonzero
`ClearToken` are non-droppable; a zero-quantity `ClearToken` is droppable
because it carries only a flavor nameplate and no balance.

The native Flame flavor is `FLAME_FLAVOR = 0`. One Flame is exactly
`100_000_000` sparks; all native-token quantities on the stack and wire are
integral sparks.

For clear token quantities, “negative” and “non-negative” refer to the scalar's
centered interpretation. This is a token-domain rule: a centered-negative
`ClearToken` represents debt and remains non-portable until balanced. Flavor
scalars use their full canonical residue range and have no sign restriction.

Clear token merging preserves integer quantities: the sum of the centered
quantities must remain in `[-H, H]`. A merge that exceeds this interval
soft-fails without consuming either token. This token-domain check does not
change ordinary `Scalar` addition, which remains modular.

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

- **state** — **any portable `Value`** (Scalar, String, Point, Token, Dict, …), with no mandated shape and no methods-in-state. [`load`](#load) checks it out, [`save`](#save) moves it back; the author structures it however they like.

Each actor is identified by a unique Actor ID derived from its constructor
script. `Hash(h)` and `Constructor(bytes)` route to the same registry key when
`h = code_root(bytes)`, so the identity commits to the initial code's snake
Cell. Their expected ActorID Cell layouts are distinct:

```text
Hash(h)            = payload: 0x00 || h[32]; refs: none
Constructor(code)  = payload: 0x01; refs: [snake(code)]
```

The constructor form carries the code needed for deployment; the hash form is
the compact address used afterward. A VM actor-destination operand is a String
containing a CellEnvelope for either form, subject to String's 8191-byte limit.
It also accepts a bare 32-byte String as `Hash(bytes)`.

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
produces one Contract containing the original payload under `refund_predicate`; it
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
      gas: Scalar,
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

Every new contract and message is **anchored** by a unique 32-byte value embedded in its wire form, so two contracts with the same predicate + payload but different anchors hash to different ids. Uniqueness is the core safety property — without it, contract ids collide across txs and authors can be tricked into operating on the wrong entity.

**Uniqueness source.** Anchors are unique only when they descend from a *spend-once source*: a UTXO consumed via [`input`](#input), or the `anchor` of a Message that triggered an internal tx (which is itself a split-child of an external tx's `op_send`). Contract ids are themselves anchored (`Contract::id = H(predicate, anchor, payload)`), so each input contract's id is unique on the network. Any anchor derived deterministically from such a source remains unique.

**Per-tx slot.** The VM carries a single `last_anchor: Option<Anchor>` for the entire transaction. It is:
- `None` at the start of an external tx — [`input`](#input) is the only way to seed it.
- `Some(M)` at the start of an internal tx, where `M` is the delivering Message's anchor (a split-child from the originating external tx's `op_send`).

Calls split the anchor at entry. `op_call`, `op_open`, and `op_signcall` each split `last_anchor` into `(left, right)` at frame entry. The child's frame starts with `last_anchor = left`; the parent frame stashes `right` in its `post_call_anchor` slot, and `last_anchor` is restored to `right` when control returns (success or failure). This keeps the caller's anchor chain independent of whatever the callee does with its own half.

**Splitting.** Each opcode that produces a new unique-anchored *cross-tx* entity (contract-on-wire, message-to-actor) consumes `last_anchor` and replaces it with a fresh derived value. The split is a single Merlin transcript:

```
t = Transcript::new(b"flamevm.anchor.split");
t.append_message(b"parent", &last_anchor.0);
left  = t.challenge_bytes(b"left",  32);
right = t.challenge_bytes(b"right", 32);
```

The `left` half is embedded in the new entity (contract anchor / MessageID); the `right` half replaces `last_anchor`. Authors never see `left` and `right` separately — the VM picks them automatically at the consume site.

**Sites that consume + split**: [`contract`](#contract), [`output`](#output), [`send`](#send). Each hard-fails `AnchorMissing` if no prior input claim has seeded the tx's anchor.

**Site that seeds without consuming**: [`input`](#input) sets `last_anchor = Anchor(contract.id())` directly (the spent UTXO's id is already unique on the wire — no split needed). Replacing any prior value is intentional: it lets a partial transaction depend only on its own input claim, not on what other parts of the tx contributed before it.

**Sites that split at call entry**: [`call`](#call), [`open`](#open), [`signcall`](#signcall). All three create new call frames and each splits `last_anchor` at entry. The `left` half seeds the child frame's `last_anchor`; the `right` half is held in the parent frame's `post_call_anchor` slot and replaces `last_anchor` when control returns. This makes anchor flow deterministic across call success/failure boundaries — the caller's anchor chain is independent of whatever the callee did with its left half.

**Locality across parties.** Each `op_input` *replaces* the anchor unconditionally rather than mixing into a chain. So a multi-party tx where party A claims input A_in and produces outputs, then party B claims input B_in and produces outputs, has each party's output anchors rooted only in their own input id. B's claim wipes A's residue; that's fine because B's outputs derive from B_in's split-children, not from anything A did. A party signing their portion can predict their own output anchors locally from their own input contract ids.

FlameVM splits anchors at the source: every consume site produces two cryptographically independent children, so neither the contract's stored anchor nor the residual anchor can be reused without the matching half of the original split.

## Instruction set

Each instruction is a one-byte **opcode** optionally followed by **immediate data** encoded inline in the bytecode. Stack effects are written in left-to-right bottom-to-top order: in `a b → c`, `b` is the top of the stack on entry, `c` is the top on exit.

**Context column.** The **Ctx** column in the instruction table marks opcodes that fail outside their supported context:
- **ext.** — external execution, including an external `ContractOpen`. Hard-fails
  `ExternalOnly` from internal context. Covers `input`, the constraint-system
  opcodes (`scalar`, `commit`, `alloc`, `expr`), and the CS-consuming
  opcodes (`mix`, `fee`). Branch-polymorphic opcodes (`borrow`, `eq`, `add`,
  `and`, `or`, `range`) keep a blank marker; their CS-branch restriction is in the
  per-opcode prose. `decrypt` also has a blank marker: it batches externally and
  checks immediately internally.
- **actor** — requires a current actor identity. Available only in
  `InternalRoot` and `ActorCall`, never `ExternalRoot` or `ContractOpen`.
- **pred.ext.** — requires an external `ContractOpen` predicate context.
- **caller** — requires a called frame (`InternalRoot`, `ActorCall`, or
  `ContractOpen`) with caller attribution; excludes `ExternalRoot`.
- **int.** — internal chain context. Currently used only for planned chain-info
  operations.
- *(blank)* — works in either context. Most opcodes, including `send`.

| Hex | Name | Ctx | Stack | Description |
| --- | --- | --- | --- | --- |
|     | **Stack**  | | | |
| 0k | [push:k](#pushk-and-friends) | | ø → scalar | Push a small literal scalar 0–15 inline. |
| 10–18 | [pushint8/16/64/128 \[s\], pushint](#pushk-and-friends) | | ø → scalar | Push a scalar with a canonical width-class payload. |
| 19 | [pushstr](#pushstr) | | ø → str | Push a literal byte string with a length prefix. |
| 1a | [pushpoint](#pushpoint) | | ø → point | Push a literal 32-byte Ristretto point. |
| 1b | [pushtoken](#pushtoken) | | flv → token | Mint a zero-qty `ClearToken` of the given flavor (placeholder). |
| 1c | [drop](#drop) | | x → ø | Discard a droppable value off the top of the stack. |
| 1d | [nop](#nop) | | ø → ø | Do nothing — useful as a padding / alignment hook. |
| 1e | [dup](#dup) | | x\_k … x\_0 k → x\_k … x\_0 x\_k | Copy the value at depth `k` onto the top (`k` popped as scalar). |
| 1f | [roll](#roll) | | x\_k … x\_0 k → x\_{k-1} … x\_0 x\_k | Move the value at depth `k` to the top (`k` popped as scalar). |
| 2k | [dup:k](#dupk) | | x\_k … x\_0 → x\_k … x\_0 x\_k | One-byte `dup` with `k` ∈ 0..=15 baked into the opcode. |
| 3k | [roll:k](#rollk) | | x\_k … x\_0 → x\_{k-1} … x\_0 x\_k | One-byte `roll` with `k` ∈ 0..=15 baked into the opcode. |
|    |  **String**  | | | |
| 40 | [readbits](#readbits) | | s n → s' x 1 \| s 0 | Pull `n` bits off the head of a string as a scalar (soft-fail if short). |
| 41 | [readint](#readint) | | s → s' x 1 \| s 0 | Pull a canonical 32-byte scalar off the head of a string. |
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
| 50 | [abs](#abs) | | x → \|x\| s | Push the centered magnitude and derived sign (`s` ∈ {0,1}). |
| 51 | [eq](#eq) | | a b → a b {0\|1} or constraint | Equality test — cleartext peek, or lifted Constraint in the CS. |
| 52 | [neg](#neg) | | x → −x | Modular negation of Scalar / Expression / MSM (Point lifts to MSM). |
| 53 | [add](#add) | | x y → z | Add scalars (or Expressions); Point/MSM operands lift to MSM. |
| 54 | [mul](#mul) | | x y → z | Multiply scalars (or CS gate); Scalar·Point or Scalar·MSM lift to MSM. |
| 55 | [divmod](#divmod) | | x z → d r | Centered integer division toward zero — push quotient and remainder. |
| 56 | [mod252](#mod252) | | s → scalar | Reduce a ≤64-byte LE string modulo ℓ. |
| 57 | [not](#not) | | x → y | Logical NOT for scalars; structural negation for Constraints. |
| 58 | [and](#and) | | a b → c | Logical AND for scalars; lifts to Constraint conjunction in CS. |
| 59 | [or](#or) | | a b → c | Logical OR for scalars; lifts to Constraint disjunction in CS. |
| 5a | [size](#size) | | x → x n | Push length of a String / entry count of a Dict (peek). |
|    | **Constraints**  | | | |
| 60 | [scalar](#scalar) | ext. | s → expr | Lift a 32-byte scalar string to a constant Expression. |
| 61 | [commit](#commit) | ext. | s → var | Wrap a 32-byte Pedersen-commitment point as a CS Variable. |
| 62 | [alloc](#alloc) | ext. | ø → expr | Allocate a fresh R1CS variable and push it as a one-term Expression. |
| 63 | [expr](#expr) | ext. | var → expr | Bind a Variable into the CS and push it as a one-term Expression. |
| 64 | [range](#range) | | x n → x | Check a Scalar in either context; range-prove an Expression externally. `n` ∈ 1..=64. |
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
| 92 | [issueprivflv](#issueprivflv) | | pred tag → scalar | Consumer-side helper: recompute `flavor_from_predicate(pred, tag)`. |
| 93 | [issuepub](#issuepub) | actor | qty tag → CT | Mint a cleartext token under the current actor's identity + `tag`. |
| 94 | [issuepubflv](#issuepubflv) | | cid tag → scalar | Consumer-side helper: recompute `flavor_from_actor(cid, tag)`. |
| 95 | [retire](#retire) | | t → ø | Burn a token (emits a retire entry to the txlog). |
| 96 | [borrow](#borrow) | | qty flv → −T +T | Borrow balanced ±token pair; debt must be balanced before tx end. |
| 97 | [merge](#merge) | | a b → {c 1 \| a b 0} | Combine two same-flavor cleartokens; soft-fail on flavor mismatch or centered quantity overflow. |
| 98 | [split](#split) | | a q → a' b | Split quantity `q` off a cleartoken. |
| 99 | [mix](#mix) | ext. | tokens… cmts… m n → tokens | Cloak: prove `m` input tokens balance `n` output commitments per flavor. |
| 9a | [decrypt](#decrypt) | | T f f' q q' → CT | Open an encrypted Token to a ClearToken using cleartext openings. |
| 9b | [fee](#fee) | ext. | qty → −WT | Pay tx fee in the native Flame flavor; push the balancing WideToken debt to net out via `mix`. |
|    | **Control flow** | | | |
| a0 | [verify](#verify) | | x → ø | Assert: hard-fail if scalar is zero, enforce a Constraint, or batch an MSM. |
| a1 | [label](#label) | | ø → ø | Mark a jump target (operand: label number); labels number 0,1,2… in order. |
| a2 | [jump](#jump) | | ø → ø | Unconditional jump to a label (operand: label number). |
| a3 | [jumpif](#jumpif) | | x → ø | Pop a scalar; jump to a label iff non-zero (operand: label number). |
| a4 | [return](#return) | | a\_{k-1} … a\_0 k → ø | Exit current call frame, returning `k` items to the parent. |
| a5 | [type](#type) | | x → x code | Push the type code of the top value (peek). |
|    | **Contracts & predicates** | | | |
| c0 | [input](#input) | ext. | id → contract | Resolve a Utreexo-validated ContractID through the execution BoC. |
| c1 | [contract](#contract) | | payload pred → contract | Lock one portable Value under the predicate. |
| c2 | [output](#output) | | payload pred → ø | Like `contract`, but emits an Output. |
| c3 | [open](#open) | | contract ik root index gas args… k → {results… k' 1 \| contract args… k 0} | Resolve a predicate program through its Cell Trie and run an isolated frame. |
| c4 | [signtx](#signtx) | ext. | contract → payload | Authorize TxID and unwrap the single payload Value. |
| c5 | [signcall](#signcall) | | contract script sig gas args… m → {results… k' 1 \| contract args… m 0} | Run a script signed by the contract predicate in an isolated frame. |
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
| `merge`: `CT(H,7), CT(1,7)` | `CT(H,7), CT(1,7), 0` | neither token (`H = (ℓ-1)/2`) |
| unavailable `addstorage` / `quotestorage`: `q` | `0` | request `q` |
| failed entered `call`: `A, B` with `k=2` | `A, B, 2, 0` | address and gas; arguments are restored and count is re-emitted |
| failed entered `open`: `Contract, A, B` with `k=2` | `Contract, A, B, 2, 0` | proof/script/gas operands; Contract and arguments are restored, count excludes Contract |
| failed entered `signcall`: `Contract, A, B` with `k=2` | `Contract, A, B, 2, 0` | script/signature/gas operands; Contract and arguments are restored, count excludes Contract |

Thus count and lookup-key operands do not need restitution: they are copyable
control data, not linear values. The source String, Dict, tokens, Contract, and call
arguments follow the exact shapes above.

### Stack instructions

### push:k and friends

ø → _scalar_

Pushes an [`Scalar`](#scalar-values). The encoder picks the narrowest of these forms:

- `0x00..=0x0f` — immediate `push:k`, value `k ∈ 0..=15`, no payload.
- `0x10`/`0x11` — `pushint8`, 1-byte LE payload `m`, producing `m` / `-m mod ℓ`.
- `0x12`/`0x13` — `pushint16`, 2-byte LE payload `m`, producing `m` / `-m mod ℓ`.
- `0x14`/`0x15` — `pushint64`, 8-byte LE payload `m`, producing `m` / `-m mod ℓ`.
- `0x16`/`0x17` — `pushint128`, 16-byte LE payload `m`, producing `m` / `-m mod ℓ`.
- `0x18` — `pushint` full form, the 32-byte canonical residue.

The decoder rejects values that fit a narrower class, including full-width
values with a compact modular-negation form. Zero has only the immediate
encoding; the negating compact forms do not admit a second encoding for it.

### pushstr

ø → _string_

Reads a sub-varint length prefix + payload bytes; pushes them as a [String](#string).
The bytecode length prefix is not part of the String's Cell encoding. A complete
literal longer than 8191 bytes hard-fails `StringTooLong` before allocation.

### pushpoint

ø → _point_

Reads 32 more bytes and pushes them as a [Point](#point). Bytes are not decompressed eagerly — invalid Ristretto encodings only fail when consumed by a downstream opcode.

### pushtoken

_flv_ → _token_

Pops a `Scalar` flavor; pushes a zero-quantity `ClearToken { qty: 0, flv }`. Convenience for downstream `merge`/`mix` shapes that need a typed placeholder.

### drop

_x_ → ø

Drops a [droppable](#types) value. Hard-fails `TypeNotDroppable` for
asset-bearing values and Dicts whose sticky droppable flag is false. A
non-empty Dict of droppable members and pure-computation values may be dropped.

### nop

ø → ø

No effect.

### dup

_x\_k … x\_0 k_ → _x\_k … x\_0 x\_k_

Pops `k` as `Scalar`, copies the value at depth `k` (zero-indexed from the top) onto the stack. Source must be a [copyable](#types) type — hard-fails `TypeNotCopyable` otherwise. Hard-fails `IndexOutOfRange` when the canonical residue is outside the stack's index range.

Prefer the immediate [`dup:k`](#dupk) form for `k ∈ 0..=15` (one byte instead of two).

### roll

_x\_k … x\_0 k_ → _x\_{k-1} … x\_0 x\_k_

Pops `k` as `Scalar`, moves the value at depth `k` to the top of the stack. Any type works (no copy required). Hard-fails `IndexOutOfRange` when the canonical residue is outside the stack's index range.

Prefer the immediate [`roll:k`](#rollk) form for `k ∈ 0..=15`.

### dup:k

_x\_k … x\_0_ → _x\_k … x\_0 x\_k_

Immediate-encoded `dup` with `k ∈ 0..=15` taken from the low nibble of the opcode byte (`0x2k`). One-byte equivalent of `pushint8 k; dup` — saves a byte over [`dup`](#dup) for shallow depths. Same copyability and bounds rules as `dup`.

### roll:k

_x\_k … x\_0_ → _x\_{k-1} … x\_0 x\_k_

Immediate-encoded `roll` with `k ∈ 0..=15` taken from the low nibble of the opcode byte (`0x3k`). One-byte equivalent of `pushint8 k; roll`. Same bounds rules as `roll`.

## String instructions

**Failure principle.** Insufficient source bytes and noncanonical scalar residues are **soft fails**. Invalid operation parameters (e.g. `n > 256`) and exceeding String's 8191-byte limit are **hard fails**. `writebits`, `writeint`, `append`, and `writezeros` check prospective growth before allocation and fail with `StringTooLong` if it would exceed the limit.

### readbits

_s n_ → _s' x 1_ | _s 0_

Reads `n ≤ 256` bits **LSB-first within each byte** into bits `0..n-1` of a fresh
`Scalar`, with all higher bits zero. The resulting unsigned residue must be
strictly less than ℓ; no input bit is interpreted as a sign.

Soft-fails on insufficient bytes or a residue ≥ ℓ (reachable only when
`n ≥ 253`). A count outside `[0, 256]` hard-fails `IndexOutOfRange`.

### readint

_s_ → _s' x 1_ | _s 0_

Equivalent to `readbits(s, 256)`. Reads the canonical 32-byte little-endian `Scalar` residue. Soft-fail conditions match `readbits`.

### readstr

_s n_ → _s' s'' 1_ | _s 0_

Reads `n` bytes into a new String, consuming them from `s`. Soft-fails if `s` has fewer than `n` bytes.

### readpoint

_s_ → _s' p 1_ | _s 0_

Reads 32 bytes as a [Point](#point). Soft-fail on insufficient bytes.

### writebits

_s x n_ → _s'_

Appends the low `n` bits of `x`'s canonical 32-byte residue as bytes (LSB-first). `n` must be a multiple of 8 and `≤ 256`. A count outside `[0, 256]` hard-fails `IndexOutOfRange`; a non-byte-aligned count hard-fails `BitCountOutOfRange`.

### writeint

_s x_ → _s'_

Equivalent to `writebits(s, x, 256)`. Appends the canonical 32-byte little-endian residue.

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

Shifts bits of `a` left by `n ≤ 256`. `b` has the same length as `a`; `c` carries the displaced bits as a zero-padded-left string of `ceil(n/8)` bytes. A count outside `[0, 256]` hard-fails `IndexOutOfRange`. Byte 0 is most significant.

### shiftright

_a n_ → _b c_

Mirror of `shiftleft`; displaced low-end bits land in `c` zero-padded on the right.

## Arithmetic & logic instructions

### abs

_x_ → _|x| s_

Pops `Scalar` `x`; interprets it through the [centered lift](#scalar-values)
and pushes its magnitude followed by the derived sign (`0` for a non-negative
centered value, `1` for a negative one). Thus `abs(0) = (0, 0)`,
`abs(H) = (H, 0)`, and `abs(ℓ-1) = (1, 1)`. The magnitude is at most `H`.

### eq

_a b_ **eq** → _a b {0|1}_ (cleartext) | → _constraint_ (CS branch)

Two stack diagrams depending on operand types and context:

1. **Cleartext branch.** When neither operand is a CS type (`Variable` / `Expression`), or in internal context, peeks at the top two values and pushes `1` if equal or `0` otherwise. Operands stay on the stack. Equality is type-aware via `Value::try_eq`: `Scalar` / `String` / `Point` compare by value; **same-variant `Dict` and all linear types (`Token`, `ClearToken`, `Contract`, …) hard-fail `TypeNotComparable`** (linear values have no equality; Dicts would cost unbounded recursion).
2. **Lifted branch.** When at least one operand is `Expression` or `Variable` *and* the context is external, both operands are popped, lifted to `Expression` (Scalar → `Expression::Constant`), and the result is `Constraint::eq(a, b)`.

### neg

_x_ → _−x_

`Scalar` is negated modulo ℓ (`0` stays `0`, and nonzero `x` becomes `ℓ-x`). `Expression` negates the linear combination. `Point` / `MultiscalarMul` lift to MSM (see [MultiscalarMul](#multiscalarmul)) with negated coefficients. Other types hard-fail `TypeNotScalar`.

### add

_x y_ → _z_

Dispatch by operand types:
- `Scalar + Scalar` → addition mod ℓ.
- `Point + Point` / `Point + MSM` / `MSM + Point` / `MSM + MSM` → [MultiscalarMul](#multiscalarmul) with concatenated terms (works in either context).
- Mixed Scalar/Expression in external context → lifts to `Expression` LC sum.

### mul

_x y_ → _z_

Dispatch by operand types:
- `Scalar * Scalar` → multiplication mod ℓ.
- `Scalar * Point` / `Point * Scalar` → [MultiscalarMul](#multiscalarmul) with one `(scalar, point)` term.
- `Scalar * MSM` / `MSM * Scalar` → MSM with scaled coefficients.
- Mixed Scalar/Expression in external context → may add a CS multiplier gate (or constant-fold when one side is `Expression::Constant`).
- `Point * Point`, `MSM * Point`, `MSM * MSM` → hard-fail `TypeNotScalar` (quadratic in group elements, no Sigma-protocol semantics).

### divmod

_x z_ → _d r_

Both operands must be raw `Scalar` values. Let `X` and `Z` be their
[centered lifts](#scalar-values). Computes integer quotient `D` truncated toward
zero and remainder `R`, so `X = D·Z + R`, `|R| < |Z|`, and nonzero `R` has
the same sign as `X`. Returns `D mod ℓ` and `R mod ℓ` as scalars. For example,
`divmod(ℓ-7, 3) = (ℓ-2, ℓ-1)`. Hard-fails `DivByZero` on a zero divisor.
This is integer division, not multiplication by a field inverse; no R1CS
division gadget is allocated. Available in external and internal contexts.

### mod252

_s_ → _scalar_

Reads up to 64 bytes of `s` as a little-endian unsigned integer, reduces modulo ℓ, and pushes the canonical `Scalar` residue. Hard-fails `StringTooLongForModReduction` when `s.len() > 64`.

### not

_x_ → _y_

`Scalar`: `0 → 1`, non-zero → `0`. `Constraint`: structural negation via `Constraint::not(c)`.

### and

_a b_ → _c_

Cleartext logical AND when both operands are `Scalar`. When at least one operand is a `Constraint` (external context only), both lift to `Constraint` (Scalar → `Cleartext(v != 0)`) and the result is `Constraint::and(a, b)`.

### or

_a b_ → _c_

Mirror of [`and`](#and) for disjunction.

## Constraint system instructions

These instructions require external context except `range` on a raw `Scalar`,
which performs an ordinary range check in either context.

**Rollback on call failure.** Every CS-touching opcode (`scalar`,
`commit`, `alloc`, `expr`, `range` on a linear combination, `eq` via `verify`, `mix`,
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

Pops a 32-byte String, downcasts to `Scalar` via `String::to_scalar`, pushes `Expression::Constant(scalar)`. Witness-bearing `StringWitness::Scalar(i)` extracts the witness directly; `String::Opaque(bytes)` parses a canonical little-endian residue strictly less than ℓ.

### commit

_s_ → _var_

Pops a 32-byte String, downcasts to a [Commitment](#types), wraps in `Variable { commitment }`. Verifier: `String::Opaque(point bytes)` → `Commitment::Closed(point)`. Prover: `StringWitness::Point(Point::Commitment(Open(witness)))` preserves the witness. Downstream [`expr`](#expr) binds the variable into the CS.

### alloc

ø → _expr_

Allocates a low-level R1CS variable. The prover-side `Instruction::Alloc(Some(scalar))` carries the cleartext witness; the verifier sees `Alloc(None)` and the variable is left unassigned, constrained later by `eq`/`verify`. Pushes a one-term `Expression::LinearCombination([(var, 1)], witness?)`.

### expr

_var_ → _expr_

Pops a `Variable`, commits it via the delegate's `commit_variable`, pushes a one-term Expression bound to the resulting R1CS variable.

### range

_x n_ → _x_

Pops bit count `n: Scalar` (must be in `[1, 64]`) and either a raw `Scalar`
or an `Expression`. A raw scalar is checked immediately against `[0, 2ⁿ)` in
both external and internal contexts. It remains a raw scalar on the stack;
the check does not lift it into the constraint system.

Expressions remain external-only, including `Expression::Constant`.
For a constant, checks its canonical residue against `[0, 2ⁿ)`.
For `Expression::LinearCombination`, adds the existing Bulletproofs
range-proof gadget. The original value is pushed back unchanged.
Hard-fails `BitCountOutOfRange`, `InvalidBitrange`, or `R1CSError`;
an Expression in internal context hard-fails `ExternalOnly`.

### size

_x_ → _x n_

Peeks at the top value and pushes its length: byte count for `String`, entry count for `Dict`. Hard-fails `TypeHasNoLength` for other types. Available in both contexts (CS-system grouping is for byte adjacency).

## Dict instructions

[Dict](#dict) keys are always `Scalar`; values may be any [Value](#types). Insertion updates the Dict's sticky capability flags; portability is enforced only when the Dict crosses a storage or transfer boundary.

### dict

_… val\_{n-1} key\_{n-1} … val\_0 key\_0 n_ → _dict_

Pops `n` (Scalar), then `n` `(value, key)` pairs (key on top of each pair). Builds a Dict. Hard-fails `DictKeyOccupied` on duplicate keys.

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

Iteration helpers. Push the first/last key of the dict, or the key after `k`, or `0` if the dict is empty / `k` is the last entry. Keys are visited in canonical `Scalar` order.

## Cryptography instructions

### transcript

_label_ → _merlin_

Creates a fresh [Merlin transcript](#types) seeded with `label`. The transcript
is never copyable but is droppable; `twrite` / `tread` consume and return it
while building custom ZKP statements.

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

Peeks the top token-shaped value and pushes `(qty, flv)` above it. `ClearToken`: both as `Scalar` (cleartext). `Token`: both as `Point` (the compressed commitment points; works without a live CS). `WideToken`: hard-fails `TypeNotToken` — its quantity isn't yet range-proven.

### issuepriv

_qty tag_ → _T_

Pops `tag` (String) and `qty` (`Variable` — a Pedersen commitment lifted via [`commit`](#commit)). Allocates a 64-bit range proof on `qty`. Builds `Token(qty_commitment, unblinded(flavor))` where `flavor = flavor_from_predicate(current_predicate, tag)`. Emits `TxEntry::IssuePriv(qty_commitment_point, unblinded_flv_point)` — the confidential-issuance txlog effect. Pushes the `Token`.

The current_predicate is the predicate stored on the enclosing `CallKind::ContractOpen` frame — created by [`open`](#open) or [`signcall`](#signcall) against an empty contract whose predicate is the desired issuer. Hard-fails `OpcodeRequiresPredicateContext` from `ExternalRoot` (no enclosing predicate) and from any `ActorCall` frame (issuance binds to a predicate, not an actor; the two issuance domains are kept disjoint by construction). Hard-fails `ExternalOnly` in internal context (the CS lane is required for the range proof and the qty commitment).

`Scalar` or `Point` operands hard-fail `TypeNotVariable` — lift to a `Variable` via [`commit`](#commit) first.

**Non-fungible tokens.** Mix the contract's [`anchor`](#anchor) into `tag` (e.g. `anchor … keccak256` against domain bytes) to derive a fresh flavor per issuance — the result is a non-fungible Token, since no other issuance will share the flavor. Use [`issueprivflv`](#issueprivflv) on the consumer side to recompute the same flavor scalar for verification.

### issueprivflv

_pred tag_ → _scalar_

Pops `tag` (String) and `pred` (String, exactly 32 bytes — a compressed Ristretto predicate point). Pushes `flavor_from_predicate(pred, tag)` as `Scalar`. Pure helper: no CS, no txlog entry, no predicate-context requirement. Domain separator is `flamevm.issuepriv.flavor` (consensus-fixed). Hard-fails `IndexOutOfRange` if `pred` is not exactly 32 bytes.

The confidential token's flv commitment is unblinded, so its point uniquely determines this scalar — meaning a consumer who knows the issuing predicate's point and the tag can recompute the flavor and check that an incoming Token belongs to the expected issuance domain.

### issuepub

_qty tag_ → _CT_

Pops `tag` (String) and `qty` (`Scalar` — cleartext). Builds `ClearToken(qty, flavor_from_actor(current_actor, tag))` and emits `TxEntry::IssuePub(qty, flv)` carrying the cleartext `(qty, flv)` pair directly as `Scalar`s — publicly auditable on the wire without commitment indirection. Pushes the `ClearToken`.

The current actor is stored on either `CallKind::InternalRoot` or
`CallKind::ActorCall`. Both may issue. The opcode hard-fails
`OpcodeRequiresActorContext` from `ExternalRoot` and `ContractOpen` (issuance binds
to an actor, not a predicate). It runs without CS in internal execution.

`Variable` or `Point` operands hard-fail `TypeNotScalar` — `issuepub` is the cleartext path; for confidential qty, use [`issuepriv`](#issuepriv) from a `ContractOpen` frame.

**Non-fungible tokens.** Mix the call's [`anchor`](#anchor) into `tag` to derive a fresh flavor per call — yields a unique non-fungible token. Use [`issuepubflv`](#issuepubflv) on the consumer side to recompute the same flavor scalar for verification.

### issuepubflv

_cid tag_ → _scalar_

Pops `tag` (String) and `cid` (String, exactly 32 bytes — an actor id). Pushes `flavor_from_actor(cid, tag)` as `Scalar`. Pure helper: no CS, no txlog entry, no actor-context requirement. Domain separator is `flamevm.issuepub.flavor` (consensus-fixed). Hard-fails `IndexOutOfRange` if `cid` is not exactly 32 bytes.

### retire

_t_ → ø

Consumes a non-negative token and emits `TxEntry::Retire(qty_point, flv_point)`.
`ClearToken` uses unblinded commitments; `Token` uses the live commitment
points. A negative `ClearToken` hard-fails `NegativeTokenRetirement`, preventing
storage-fee debt and other liabilities from being discarded. Other types
hard-fail `TypeNotToken`.

### borrow

_qty flv_ → _−T +T_

Cleartext branch (both `Scalar`): pushes `(ClearToken(-qty, flv), ClearToken(qty, flv))`. The negative half is non-portable until balanced.

Encrypted branch (both `Variable`, external context): commits both to the CS, range-proves the positive `qty` 64-bit, allocates `-qty`, constrains the sum to zero, pushes `(WideToken(-qty, flv), Token(qty, flv))`.

Raw `Point` operand hard-fails `TokenRequiresCS` — lift to `Variable` via [`commit`](#commit) first.

### merge

_a b_ → _{c 1 | a b 0}_

`ClearTokens` only. On flavor match, adds the centered integer quantities.
If their sum lies in `[-H, H]`, pushes `(ClearToken(a.qty+b.qty, flv), 1)`.
Flavor mismatch or centered quantity overflow restores both original tokens
as `(a, b, 0)` (soft-fail). Overflow occurs exactly when both centered inputs
have the same sign but their modular sum has the opposite sign.
Non-`ClearToken` operands hard-fail `TypeNotClearToken`.

### split

_a q_ → _a' b_

`ClearTokens` only. Returns `(ClearToken(a.qty − q, flv), ClearToken(q, flv))`. Hard-fails `TokenSplitOutOfRange` when either `q` or `a.qty` is centered-negative, or when their canonical residues satisfy `q > a.qty`.

### mix

_tokens… commitments… m n_ → _tokens_

Pops `n` (output count) and `m` (input count) as `Scalar`; then `n` output Pedersen commitment pairs (qty/flv Strings); then `m` input token-shaped values (any of `Token`, `WideToken`, `ClearToken`). Invokes the [spacesuit cloak gadget](../spacesuit/spec.md) to constrain that inputs balance outputs per flavor and to 64-bit range-prove each output. Pushes `n` output `Token`s.

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

- `Scalar`: hard-fails `VerifyFailed` if zero; otherwise consumes the value.
- `Constraint`: enforces the constraint via the delegate's CS (external context only).
- `MultiscalarMul`: appends `sum(s_i · P_i) == identity` to the delegate's `BatchVerifier` alongside any Schnorr/Musig sigs (external context only). Returns success immediately; the batched check runs at finalize and on failure surfaces as `BatchSignatureVerificationFailed`. See [MultiscalarMul](#multiscalarmul).
- Other types hard-fail `TypeNotScalar`.

### fee

_qty_ → _−WT_

Pops `qty: Scalar` in sparks (non-negative, `≤ MAX_FEE = 2²⁴`). Fees always use the canonical native flavor `FLAME_FLAVOR = Scalar::ZERO`. The `2²⁴`-spark cap is chosen so fee-rate arithmetic stays within `u64`: even a `2⁴⁰`-byte (~1 TB) transaction leaves 24 bits of headroom. Emits `TxEntry::Fee(qty as u64)` and bumps the per-tx [`CheckedFee`](#fees) accumulator (also capped at `MAX_FEE`). Allocates a fresh `WideToken` debt with `q = −qty`, `f = FLAME_FLAVOR` (both cleartext-constrained) and pushes it. The script must balance the debt against native Flame tokens, typically via [`mix`](#mix).

Hard-fails: `FeeQtyNegative`, `FeeTooHigh` (per-arg or aggregate overflow), `TypeNotScalar`, `ExternalOnly`. The blinded-fee branch is reserved for a future phase.

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

Pops `x: Scalar`. If non-zero, behaves as [`jump`](#jump) to label `N`; if
zero, falls through to the next instruction. Hard-fails `TypeNotScalar` if
the top value is not a scalar.

### return

_a_{k-1} … a_0 k_ → ø

Atomic cross-frame return:

1. Pops `k` (Scalar, whose canonical residue must fit the argument-count limit).
2. Asserts an enclosing call frame exists (otherwise `ReturnAtRoot`).
3. Asserts the callee stack has exactly `k` items left (otherwise `BadReturnArity` or `StackNotClean`).
4. Pops the call frame.
5. Refunds leftover gas to the parent.
6. Pushes the `k` items onto the parent's stack, then the count `k`, then a **success marker `1`** — the parent observes `results… k 1`. A clean run-off-the-end exit pushes `0 1`. A failed child instead restores its entry escrow followed by the escrow count and `0`; callers branch on this trailing status. Actor calls escrow their arguments. `open` and `signcall` escrow the original locked Contract plus their explicit arguments, never the Contract payload separately.

`return` deliberately performs no portability check. Non-portable values,
including negative `ClearToken`s and `WideToken`s, may move upward to the
caller, which owns responsibility for balancing or consuming them. Every
downward boundary (`call`, `open`, `signcall`, and asynchronous `send`) accepts
portable values only.

At the outermost call frame, `return` always errors regardless of `k`; a script terminates cleanly by running off the end of its instructions with an empty stack (jump to a trailing label to short-circuit).

### type

_x_ → _x typecode_

Pushes the type code of the top value as `Scalar`, leaving the value on the stack. Type codes are a stable sequential enumeration of the value types — **independent of the wire-encoding tags** in [Types](#types), which keep their own compact scheme (so the `type` opcode also covers non-serializable stack-only types):

| Code | Type | Code | Type |
| --- | --- | --- | --- |
| 0 | Scalar | 7 | Contract |
| 1 | String | 8 | Merlin |
| 2 | Dict | 9 | Variable |
| 3 | Point | 10 | Expression |
| 4 | Token | 11 | Constraint |
| 5 | WideToken | 12 | MultiscalarMul |
| 6 | ClearToken | | |

### Contract, actor, and send instructions

[`open`](#open), [`signcall`](#signcall), and [`call`](#call) all create isolated call frames as described in [Design overview](#design-overview).

### input

_contract_id:string32_ → _contract_

Resolves the 32-byte ContractID from the initiating execution BoC and decodes
the authenticated Contract: predicate, anchor, and one payload Value. Dict
branches can remain pruned. Sets `last_anchor = Anchor(contract_id)` and
emits `TxEntry::Input(contract_id)`.

A prover may push `String::contract(c)`; the emitted bytecode still contains
only the ID. Public Cell bodies are collected before proving. Private atomic
openings are restored only after the same public path/value has resolved.
A private witness cannot supply a publicly missing branch. Generic untrusted
Contract imports remain fully validated; the VM input path relies on the
already-admitted UTXO identity.

The VM does not verify accumulator membership: the chain must check the
spend-once Utreexo proof and reject duplicate inputs. Missing/malformed witness
bodies or a non-32-byte ID hard-fail. `input` is external-only.

### contract

_payload pred_ → _contract_

Pops `pred: Point` and one portable payload Value. Splits `last_anchor`:
the left half becomes the new Contract anchor and the right half remains
the continuation. Use a Dict for multiple payload values; there is no count
operand. Hard-fails `AnchorMissing` or `NonPortableInOutput`.

### output

_payload pred_ → ø

Same construction as [contract](#contract), but emits an Output effect.

### open

_contract internal_key root_id index gas args… k_ →
_{results… k' 1 | contract args… k 0}_

Pops the explicit argument count and portable arguments, gas grant, branch
index (u64 Scalar), root ID (32-byte String), internal key (Point), and Contract.
Checks the tweaked-key relation `P = X + h(X, root_id)·B`, resolves the
program at that index through the predicate's Cell Trie, and reads its snake
bytes. There are no sibling hashes, position bits, or caller-supplied public
program bytes. Private program instructions must compile to the resolved bytes.
`root_id` is the raw eight-byte-key Trie root; there is no intermediate
count-envelope Cell. Lookup validates the visited path without relying on
or scanning a total program count.

The prover can build branches with `PredicateTree::from_scripts` and emit a
selector with `ScriptBuilder::push_taproot_proof(&tree, logical_index)`. This
attaches the selected path, private assignments, and nested program witnesses
to the builder. `build_tx` packages public bodies into the frozen execution BoC;
`UnsignedTx` preserves it through signing and `ExternalTx::verify` consumes it
directly, including after a CellEnvelope transport roundtrip.

On success, an isolated ContractOpen frame receives the **single payload**
followed by the explicit arguments. Its starting anchor is split from the
parent. The frame has only the immediate caller's actor ID for attribution,
never actor-state authority. Actor state/storage/code, public issuance,
`selfid`, and synchronous `call` hard-fail. Nested ContractOpen callerid
does not transitively inherit an earlier actor. Anonymous async `send` remains
available. CS access follows external/internal execution context.

Successful return yields `results… k' 1`; clean fall-through yields `0 1`.
An entered child's failure rolls back effects and returns the original locked
Contract followed by the explicit arguments, their original count `k`, and
status zero. The Contract is not counted and its payload is not returned
separately. Leftover child gas is refunded.

Bad operands/proofs, missing pre-entry path data, non-portable arguments,
insufficient caller gas, and depth rejection hard-fail the current frame.
Missing data after entry follows normal child-failure escrow recovery.

### send

_args… k refund gas addr_ → ø

Asynchronous message-send. Pops operands top-first:

1. `addr` (String) — a bare 32-byte hash or a CellEnvelope for ActorID.
   The constructor variant has a reference to its snake-encoded code.
2. `gas` (`Scalar`) — gas allotment.
3. `refund` (32-byte String) — bounce predicate point.
4. `k` (`Scalar`) — args count.
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
`Message.caller`; `ExternalRoot` and `ContractOpen` contribute `None`. `None` means
“no authenticated actor principal,” not necessarily “originated in an external
transaction.”

The payload is admitted by `Message::new`, not by `Message::encode`.
`Message::new` scans top-level arguments and uses the sticky Dict flag for O(1)
nested checks; once constructed, the payload cannot be replaced.

The send's identity is the canonical Message root CellID. Its payload contains
anchor:32, ActorID, caller-presence:u8 and optional caller hash:32,
refund Predicate:32, and gas:u64 LE. Its argument list is a referenced Dict
with consecutive keys; constructor code, when present, uses an earlier ref.
The Send TxEntry references that Message Cell. Anchor ratcheting makes every
distinct send unique by construction. See [Data encoding](encoding.md).
Available in both contexts. Hard-fails `MalformedAddress` when `addr` is neither
a bare hash nor one complete canonical `ActorID`, or when `refund` has the wrong
size; `NonPortableInSend` on non-portable args; and `InvalidBitrange` on a
gas allotment outside the `u64` range. It hard-fails `OutOfGas` when the active
frame cannot prepay the requested grant.

On delivery failure, consensus seals the original argument list as a Dict in one fresh Contract under `refund_predicate` and emits an Output effect.

### call

_args… k gas addr_ → _{results… k' 1 | args… k 0}_

Synchronous actor-to-actor call. Same operand shape as [`send`](#send) minus
`refund` and, likewise, no method selector. A caller that wants the callee to
purchase storage passes Flame among the ordinary arguments; allocation remains
the callee's explicit decision. A constructor-form address is accepted but
canonicalized to its hash for lookup: unlike first `send` delivery, `call` does
not deploy an absent actor.

Only `InternalRoot` and `ActorCall` may invoke `call`. `ExternalRoot` and
`ContractOpen` hard-fail `OpcodeRequiresActorContext` before consuming operands;
caller attribution stored in a `ContractOpen` is read-only and is never used to
fabricate an actor caller.

Before entering the callee, `call` rejects every non-portable argument with
`NonPortableInCall`. This is a top-level scan; a nested Dict is checked in O(1)
through its sticky flag.

**Re-entrancy:** state is reachable **only** via `load`, which acquires the
state-checkout lock, and there is no peek-state opcode. A re-entrant `call` or
`load` into an actor that has already loaded its state cannot enter, so no other
frame can make that loaded value stale. Holding it across a `call`, `send`, or
`open` is legal, and it may be saved afterward. Correctly accounting for the
interaction's status and effects is ordinary actor logic rather than a
concurrency guarantee the VM can infer. Nested call depth is capped at
`MAX_CALL_DEPTH` (64).

**Emits no txlog entry.** Calls are intra-tx control flow; the structural effects produced inside the callee (`Output`, `Send`, `ActorSave`, `SetCode`, `StoragePurchase`, `Issue`, `Retire`, `Fee`, `Data`, and actor destruction) are what the state machine reads. The `(External TxID, Internal TxID)` of a tx is a merkle root over effects only — see the effect model above.

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

Pops the state `Value`, **validates portability**, measures the prospective actor usage according to [Actor storage](storage.md), and **moves it back** into the current actor — which must be **checked out** by a prior `load`, else `SaveWithoutLoad` (saving to a non-checked-out actor would clobber, and silently drop the tokens of, live state). `save` hard-fails `StorageCapacityExceeded` if prospective usage exceeds capacity at the current core-block height. Emits `TxEntry::ActorSave { actor, state }` carrying the **full** post-save state — symmetric with `Output(Contract)` which carries the full Contract. The entry Cell contains the actor ID and a reference to the state Value Cell. The TxID commits to that root without flattening its graph.

**State is any portable Value.** The author structures state however they like (a Dict, a Scalar, a Token, …).

**Portability is the canonical storage gate.** Portable values: `Scalar`, `String`, `Point`, `Dict` of portable, non-negative `ClearToken`, `Token`. Non-portable values (`Contract`, `Merlin`, `Variable`, `Expression`, `Constraint`, `MultiscalarMul`, `WideToken`, negative `ClearToken`) hard-fail `NonPortableInState`.

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

Pops `q` as a `Scalar` whose canonical residue is a positive byte count. The request must be at least
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
nothing. Type and context violations hard-fail: `TypeNotScalar`,
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

_contract_ → _payload_

Pops the Contract, records a `DeferredSig::TxBound { verification_key: contract.predicate.point, contract_id }` for final authorization against TxID, and pushes its single payload Value. No arity is pushed.

**No new frame** — the contract-holder is authorizing the existing transaction in place.

`signtx` is external-only and checks its context before consuming the Contract.
Internal transactions have no signature envelope and their final TxID does not
exist when the opcode executes, so the TxID-bound signature cannot be verified
immediately. Internal authorization uses `signcall` instead.

The deferred signature is verified at finalize: the prover aggregates all `TxBound` keys via MuSig and supplies the envelope signature; the verifier batches all `TxBound` items against the `flamevm.signtx` transcript bound to TxID. Errors `BatchSignatureVerificationFailed` or `MissingTxBoundSignature` at finalize.

### signcall

_contract script sig gas args… m_ →
_{results… k' 1 | contract args… m 0}_

Same call-frame mechanics as [`open`](#open) — taproot reveal is replaced by signature verification:

1. Pops `m` (Scalar), `args` (m portable values), and `gas`. A non-portable
   argument hard-fails `NonPortableInCall` before child entry.
2. Pops `sig` (String, exactly 64 bytes — Schnorr signature).
3. Pops `script` (String) and `contract`.
4. Builds `signcall_message(script_bytes)` via a Merlin transcript labelled
   `flamevm.signcall`. Scripts bind themselves to further context (anchor,
   actor identity, tx data) through explicit checks inside the signed program.
5. In external execution, records `DeferredSig::Explicit` for final batch
   verification. In internal execution, parses and verifies the signature
   immediately against the Contract predicate; malformed bytes fail
   `BadSignatureBytes`, while a validly encoded but incorrect signature fails
   `SignatureVerificationFailed`. Internal execution records no deferred item.
6. After verification or deferral, snapshots rollback state, creates the
   isolated `ContractOpen` frame, and pours payload + args into the signed script.

External deferred signatures are batch-verified at finalize alongside any
`signtx` items. Entered-child failure removes an external deferred signature
and returns the original Contract plus explicit arguments using the same failure
shape as `open`; the count is `m`, excluding the contextual Contract. Internal
signature failure occurs before child entry and hard-fails the current frame.

### timelock

ø → _n {0|1}_

Pushes the transaction's `locktime` (as `Scalar`) and a unit flag: `0` for block height, `1` for Unix timestamp. The split follows Bitcoin's BIP-65 convention — `flag = 1` iff `locktime ≥ 500_000_000` (`LOCKTIME_TIMESTAMP_THRESHOLD`). Values below the threshold are block heights; values at or above are Unix timestamps (the threshold corresponds to ~1985-11-05, before any practical timestamp range). Available in either context.

### version

ø → _n_

Pushes `TxHeader::version` as a `Scalar`. Available in either context.

### selfid

ø → _s_

Pushes the current frame's actor id as a 32-byte String. Hard-fails `OpcodeRequiresActorContext` from `ExternalRoot` or `ContractOpen` (no actor identity).

### anchor

ø → _s_

Pushes the frame's *current* `last_anchor` as a 32-byte String — the value the next consume site would split. Hard-fails `AnchorMissing` if no anchor has been claimed yet (same rule as `contract` / `output` / `send` / `call`). Available in either context.

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
`ActorCall` use their recorded caller; `ContractOpen` uses its compact read-only
`caller_id`. It pushes the all-zero String when that caller is absent and
hard-fails only from `ExternalRoot`, which has no caller frame. A `ContractOpen`
opened by another `ContractOpen` sees zero: actor attribution is not propagated
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

The query is read-only and does not expose the lease list. A canonical residue
outside the `u64` height range hard-fails `InvalidBitrange`; a height below the current
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
throughout an external transaction, including its nested `ContractOpen` frames.

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

**Contract-open trust model.** `open`, `signcall`, and `call` all create isolated call frames: the unlocked / signed / called script runs in its own stack, gas budget, and identity scope, with no implicit access to the host's actor state, gas pool, or identity. This eliminates the confused-deputy class of bugs — an actor accepting an untrusted-source contract need not audit the predicate as a global authorization filter, because the script cannot reach the actor's state regardless of what the predicate authorizes .

**`signcall` binding policy.** The `signcall` signature commits to the script bytes only, whether it is verified immediately in internal execution or deferred in external execution. The script binds itself to further context (anchor, actor identity, tx-level data) via explicit checks such as `anchor <expected> eq verify`. Binding policy lives in the author's hands — flexibility at the price of footgun.
