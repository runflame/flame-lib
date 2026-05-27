# FlameVM

# **Introduction**

FlameVM implements transaction verifications rules in the Flame blockchain in a form of a Forth-like stack machine.

FlameVM is used in two different contexts: external transactions and internal transactions. External transactions have user-defined results, zero-knowledge proof for confidential transfers and compute, and have access to Utreexo storage: scalable compressed set of unspent transaction outputs. Internal transactions operate on uncompressed storage and invoke multiplayer smart contracts that react to user-defined messages without pre-determined results.

FlameVM operates on multiple data types, including linear types for tokens and zero-knowledge expressions. Types can be “portable” and “non-portable”, “copyable” and “non-copyable”. Portable types can exist in the long-term blockhain state outside of VM execution. Copyable types can be duplicated during VM execution.

Successful VM execution equals to successful transaction verification. Therefore, VM execution encodes both built-in network rules, as well as enables authors to create custom rules within their applications.

# External transactions

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

# Internal transactions

Internal transaction is caused by a “message” sent to an “actor” by an external transaction. Such message does not control the outcome: internal transaction may produce undetermined result or even fail. As such, race conditions (when multiple users send messages to the same actor) can be resolved by the actor itself, accepting all the messages.

Internal transactions are best used for public multiplayer apps, where an actor can be accessed by anyone in any order.

Internal transactions do not have a pre-determined effect and therefore do not support signatures and ZK proofs bound to transaction ID.

Like external, internal transactions produce effects:

1. Receives — received messages from external and internal transactions
2. Outputs — creation of new entries in the Utreexo.
3. Sends — messages sent to actors that produce other internal transactions.
4. Issuance and retirement — creation and removal of tokens to/from circulation.
5. Actor-state mutations — `op_save` records the actor's post-save state hash, allowing a state machine to mutate the registry without re-running the script.
6. Data entry — for data logging that does not occupy permanent storage.

Note: calls themselves (the act of one actor invoking another) are intra-transaction control flow, not effects. Anything a callee does that the outer world cares about shows up via one of the effects above. See `../design.md` §"TxLog records effects, not control flow".

Internal transactions do not support fee payment: they operate within gas- and memory limits set by the external transaction. They also do not support inputs, as those can be consumed only by external transactions with a Utreexo proof and (most of the time), a transaction signature.

# Limits

**Gas limit:** maximum amount of gas used per block. Each tx and sum of all txs in a block cannot exceed that amount of gas.

**Script size limit:** maximum total size of scripts in each tx and all txs in a block.

**Multiplications limit:** maximum Bulletproofs multiplications per block and per tx.

**Gas credit:** maximum amount of gas used by external transaction without additional gas allocated for message sends.

**Added storage:** amount of storage virtual bytes introduced by each block.

# Fees

Transaction fees are paid by external transactions and are necessary to prioritize common resources on the open network and mitigate denial-of-service attacks. As blockchain imposes limits on storage and computation costs, transactions paying higher fees (per resource used) are prioritised over transactions paying lower fees.

Transaction fees are paid in *flames* and cover both the computation cost (”gas limit”) for the external transaction and subsequent calls, and storage costs (”bytes”).

External transaction pays for its own de-facto gas used and for additional gas requested for *sent messages*. Internal transactions cannot request or store gas beyond amount allocated at the “message send” operation at the internal transaction.

Unused gas in a message send is discarded: transaction commits to full amount of gas before doing a message send. Unused gas in a call (within internal transaction) remains with the caller and therefore not lost. 

Amount of gas available to each call can be limited by the caller. By default, the total remaining gas of the caller is available to the callee.

Each block makes available virtual bytes: (recycled from the existing actors + newly introduced) that can be purchased by external transaction and distributed towards any actor.

**Transaction prioritisation**

BFT consensus implies that the block candidate is progressively built and already included transactions are not pruned by higher-paying ones.

# Types

Ownership: every type on the stack is always owned. FlameVM does not allow reference-counting, borrowing, read-only access or implicit copies.

Plain data types: integers, byte strings, Ristretto points. These can be copied and ported.

Structured data types: dicts that are used as lists, dictionaries and enum variants. Dicts are as copyable/portable as the items they contain.

Token types: WideToken, Token, ClearToken.

Constraint types: Object, Variable, Expression and Constraint.

Cryptography types: Merlin transcript. (Batched scalar-point
checks are not user-visible — see the note on `MultiscalarMul`
below.)

Portable types: can be stored in a UTXO or permanent storage.

Copyable types: can be copied or dropped.

Tuples are not distinct types, but a name for passing multiple items on the stack and through calls. During VM execution any tuple is simply a number of items on stack.

Optionals are not distinct types, but a convention to return tuple (values…, 1) or (0). Instruction `verify` can be used to "unwrap optional" and fail immediately if the result is missing.

Encodable types (have a wire-format tag range):

| Type | Tag(s) | Description |
| --- | --- | --- |
| Int253 | 0..=67 | Signed sign-magnitude integer; magnitude is a canonical Ristretto scalar (< ℓ ≈ 2²⁵²) plus an explicit sign bit. The name reflects the effective conceptual width: ⌈log₂ ℓ⌉ = 253 bits of magnitude. |
| String | 68..=127 | Variable-length byte string. |
| Dict | 128..=247 | Map from Int253 keys to values. List-style encoding (sequential keys 0..n-1) uses 128..=187; explicit-key form uses 188..=247. Non-portable item poisons the dict with a “non-portable” flag. |
| Point | 248 | Element of the Ristretto255 group. |
| Token | 249 | Linear type (qty, flavor) representing an asset value, possibly encrypted. |
| ClearToken | 250 | Linear type (qty, flavor) with cleartext values; may be negative and therefore non-portable. |
| WideToken | 251 | Linear type representing a possibly-negative encrypted Token. |
| Object | 252 | Linear handle to a cell or external commitment. |
| Merlin | 253 | Instance of a Merlin transcript. |
| (reserved) | 254 | Reserved tag. |
| (extension) | 255 | Extension prefix; sub-tag follows. |

Stack-only types (non-portable, never encoded on the wire):

| Type | Description |
| --- | --- |
| Variable | Secret value in the constraint system, tied to a Pedersen commitment. |
| Expression | Linear combination of variables. |
| Constraint | Logical combination of boolean conditions. |
| MultiscalarMul | Lazy `sum(s_i · P_i)` accumulator; consumed by `verify` which appends it to the same batch as Schnorr/Musig sigs (assertion: `sum == identity`). |

**Encoding**

Every value has exactly one canonical wire byte sequence. The first byte is a type+width tag; within each type, width classes carve the value range into disjoint, offset-based sub-ranges so the encoder has no choice about which tag to use.

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
248        Point   (32-byte compressed Ristretto)
249        Token
250        ClearToken
251        WideToken
252        Object
253        Merlin
254        reserved
255        extension (sub-tag follows)
```

A list-style Dict (tags 128..=187) is used when keys are exactly `0..n-1`; the keys are omitted from the wire and reconstructed at decode. An explicit-keys Dict payload whose keys turn out to be `0..n-1` is rejected at decode time, since it has a shorter list-style encoding.

Sub-varint (used inside `STR_VAR` / `LIST_VAR` / `DICT_VAR`):

```
sub-tag 0  1 LE byte    value = b              range 0..=255
sub-tag 1  2 LE bytes   value = 256 + w        range 256..=65_791
sub-tag 2  4 LE bytes   value = 65_792 + w     range 65_792..≈4.3e9
sub-tag 3  8 LE bytes   value = 4_295_032_608 + w
```

Canonicality checks performed at decode time:

1. `INT_PFULL` / `INT_NFULL`: the 32-byte payload encodes the value as-is. The decoder rejects values that could have been encoded in a narrower width class.
2. `DICT_*` payload whose keys are `0..n-1` is rejected — the list-style encoding is shorter.

### Int253

Flame integers are signed sign-magnitude scalars: the magnitude is a canonical Ristretto255 scalar (strictly less than ℓ ≈ 2²⁵²), and the high bit of the 32-byte in-memory representation is the sign. The conceptual width is 253 bits of magnitude plus a sign.

In memory, integers are fixed 32-byte arrays with bit 255 (the high bit of byte 31) as the sign and the lower 255 bits as the canonical scalar magnitude. Negative zero is never representable.

Integers are used in logical operations: non-zero is “true”, zero is “false”. Logical results are always 0 or +1.

Dicts use Int253 keys; numeric ordering of keys is total and unambiguous.

### String

Binary byte-aligned strings.

Strings are used to represent arbitrary-length binary data, programs and cryptographic signatures and hashes. Other types can be parsed from strings.

Each string acts as a builder and a reader.

**Prover-side witness variants.** On the prover side a String may carry a typed witness payload that encodes to the same canonical wire bytes the verifier sees but preserves the underlying witness data through `dup` / `run` / payload-pour boundaries:

- `String::Point(Point)` — point-shaped witness; the inner [`Point`](#point) is `Opaque`, `Commitment(Open(value, blinding))` (used before `commit`, `scalar`, `expr`), or `Predicate(p)` (used before `signtx`, `signcall`, `cell`, `output`).
- `String::Scalar(int)` — used before `scalar`.
- `String::Script(instructions)` — used before `run`, `switch`, `signcall`.
- `String::Cell(c)` — used before `input`, carrying open commitments on Token payloads.

The verifier always sees `String::Opaque(bytes)`; the downcasts (`to_commitment`, `to_scalar`, `to_predicate`, `to_instructions`, `to_cell`) handle both shapes uniformly. There is no separate witness queue or per-opcode witness operand — witnesses ride on the pushed value itself.

**`pushpoint` also carries witnesses.** Because the `pushpoint` instruction's in-memory operand is a `Point` (not a raw `[u8; 32]`), the prover can attach a `Point::Commitment` / `Point::Predicate` witness to a literal point pushed via `pushpoint` — not only via `pushstr` + `String::Point`. The wire encoding stays the canonical 32 bytes regardless.

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

Keys are non-negative Ints because this way ordering is non-ambiguous. In case of strings we need to worry about keys of different length.

Values are: any other types. Portability and copyability flags are dynamic and poisoning: once non-portable item is added, the struct becomes non-portable. Same for non-copyable.

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

WideToken: encrypted token without a range proof on quantity (could be negative). Non-portable: cannot be stored.

Token: encrypted token with a proven non-negative qty, 

# Actors

Actors are entities stored in the common storage area defined by a dict:

```rust
ActorState = dict {
    public#0x00:  dict { ... };
    private#0x01: dict { ... };
}
```

`public` - a dict containing methods that can be called, each method identified by its key. Key 0 (`recv`) is reserved to process incoming messages.

`private` - a dict with private methods and data defined by the actor.

Each actor is identified by a unique Actor ID defined by its *constructor script* or its hash. Therefore, each unique constructor defines a unique actor.

Actor ID is either `enum{ 0x00: hash }` or `enum { 0x01: constructor }`.

Actors pay for their storage in vbytes each block (see Storage below). When an actor's vbyte balance reaches zero, it enters a frozen state with a grace period proportional to its prior activity (one block per four blocks of activity, capped at six months of blocks). A top-up restores it; without one the state is cleared and the vbytes are recycled (subject to 100-block maturity).

# Messages

Messages execute “method calls” asynchronously. Each message contains a predicate for bouncing its arguments in case of actor failure. If the method called through a message returns any values, the call fails and the original arguments are bounced.

Not only users, but also actors can send async messages to each other. This allows an actor to commit intermediate results between transactions.

# Addresses

Address is an entity that supports sending funds to predicates or actor methods.

```rust
Address = enum {
   0: Predicate,
   1: Message = struct {
      dst#0: ActorID,
      method#1: <method name>,
      args: struct{0:...,1:...},
      gas: Int253,
   }
}
```

# Storage

Each actor occupies a deterministic number of *virtual bytes* (vbytes) for its code and data. The vbyte metric is defined at the protocol level to remain consistent among implementations and may differ from actual bytes stored on disk.

Each block, after executing all transactions, every actor's vbyte balance is deducted by the number of vbytes it occupies. When an actor's balance reaches zero, it enters a *frozen* state: it stops accepting calls but its state is preserved. The frozen state lasts for a grace period equal to one block of grace per four blocks of prior activity, capped at six months of blocks. A [message send](#messages) that delivers vbytes restores the actor. If the grace period elapses without a top-up, the actor's state is cleared and its vbytes return to the pool.

Each block introduces 5000 new vbytes. New actors can “buy” (via tx fees) any amount of vbytes already available (recycled from previous blocks) plus newly introduced bytes.

A message send attaches vbytes to be deposited onto the destination actor. An empty message simply assigns vbytes, without running any code, and is guaranteed not to fail.

The amount of vbytes introduced per block can be adjusted by super-majority (up to 2× lower / 2× higher).

Why deduct after execution? So that a transaction can pre-pay for a single-use actor, let it do its job, and self-destruct.

Withdrawn vbytes are recycled into the total pool. Recycling is subject to 100-block maturity. With this design it should be hard to “trade” vbytes: any actor that allocates unnecessary storage continuously “bleeds” it in subsequent blocks.

**Transient memory.** In addition to persistent storage, an actor may use transient memory during a call (scratch space released when the call ends). The cap is fixed at 4× the actor's current persistent vbyte size; the `memlimit` opcode returns this cap. Allocations that would push live memory past the cap fail the call.


# Anchors

Every new cell and message is **anchored** by a unique 32-byte value embedded in its wire form, so two cells with the same predicate + payload but different anchors hash to different ids. Uniqueness is the core safety property — without it, cell ids collide across txs and authors can be tricked into operating on the wrong entity.

**Uniqueness source.** Anchors are unique only when they descend from a *spend-once source*: a UTXO consumed via [`input`](#input), or the `anchor` of a Message that triggered an internal tx (which is itself a split-child of an external tx's `op_send`). Cell ids are themselves anchored (`Cell::id = H(predicate, anchor, payload)`), so each input cell's id is unique on the network. Any anchor derived deterministically from such a source remains unique.

**Per-tx slot.** The VM carries a single `last_anchor: Option<Anchor>` for the entire transaction. It is:
- `None` at the start of an external tx — [`input`](#input) is the only way to seed it.
- `Some(M)` at the start of an internal tx, where `M` is the delivering Message's anchor (a split-child from the originating external tx's `op_send`).

Calls split the anchor at entry. `op_call`, `op_open`, and `op_signcall` each split `last_anchor` into `(left, right)` at frame entry. The child's frame starts with `last_anchor = left`; the parent frame stashes `right` in its `post_call_anchor` slot, and `last_anchor` is restored to `right` when control returns (success or failure). This keeps the caller's anchor chain independent of whatever the callee does with its own half, which is what makes the `0` failure marker semantics safe — a failed call cannot corrupt the caller's anchor state.

**Splitting.** Each opcode that produces a new unique-anchored *cross-tx* entity (cell-on-wire, message-to-actor) consumes `last_anchor` and replaces it with a fresh derived value. The split is a single Merlin transcript:

```
t = Transcript::new(b"flamevm.anchor.split");
t.append_message(b"parent", &last_anchor.0);
left  = t.challenge_bytes(b"left",  32);
right = t.challenge_bytes(b"right", 32);
```

The `left` half is embedded in the new entity (cell anchor / SendID); the `right` half replaces `last_anchor`. Authors never see `left` and `right` separately — the VM picks them automatically at the consume site.

**Sites that consume + split**: [`cell`](#cell), [`output`](#output), [`send`](#send). Each hard-fails `AnchorMissing` if no prior input claim has seeded the tx's anchor.

**Site that seeds without consuming**: [`input`](#input) sets `last_anchor = Anchor(cell.id())` directly (the spent UTXO's id is already unique on the wire — no split needed). Replacing any prior value is intentional: it lets a partial transaction depend only on its own input claim, not on what other parts of the tx contributed before it.

**Sites that split at call entry**: [`call`](#call), [`open`](#open), [`signcall`](#signcall). All three create new call frames and each splits `last_anchor` at entry. The `left` half seeds the child frame's `last_anchor`; the `right` half is held in the parent frame's `post_call_anchor` slot and replaces `last_anchor` when control returns (whether the call succeeded or returned the `0` failure marker). This makes anchor flow deterministic across call success/failure boundaries — the caller's anchor chain is independent of whatever the callee did with its left half.

**Locality across parties.** Each `op_input` *replaces* the anchor unconditionally rather than mixing into a chain. So a multi-party tx where party A claims input A_in and produces outputs, then party B claims input B_in and produces outputs, has each party's output anchors rooted only in their own input id. B's claim wipes A's residue; that's fine because B's outputs derive from B_in's split-children, not from anything A did. A party signing their portion can predict their own output anchors locally from their own input cell ids.

**Comparison with zkvm.** zkvm uses a single VM-level `Option<Anchor>`, ratchets only at `input`, and lets `output`/`contract` advance to the new contract's id directly (no extra hash step). The future tx that spends an output must ratchet on its own (an obligation enforced only by the next `input`). FlameVM's split-at-source removes that obligation: every consume site produces two cryptographically independent children at once, so neither the cell's wire-stored anchor nor the residual anchor can be reused without the matching half of the original split.

# Instruction set

Each instruction is a one-byte **opcode** optionally followed by **immediate data** encoded inline in the bytecode. Stack effects are written in left-to-right bottom-to-top order: in `a b → c`, `b` is the top of the stack on entry, `c` is the top on exit.

**Context column.** The **Ctx** column in the instruction table marks opcodes that fail outside their supported context:
- **ext.** — external-only. Hard-fails `ExternalOnly` from internal context. Covers `input`, the constraint-system opcodes (`scalar`, `commit`, `alloc`, `expr`, `range`, the CS branches of `borrow`), and the CS-consuming opcodes (`mix`, `decrypt`, `fee`).
- **int.** — internal-only. Needs a registry handle. Covers `call`, `load`, `save`, and the chain-info family.
- *(blank)* — works in either context. Most opcodes, including `send` (which emits messages from external txs too).

## Instruction table

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
| 4e | [keccak256](#keccak256) | | s → x | 32-byte Keccak-256 digest (Ethereum compatibility). |
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
| 80 | [merlin](#merlin) | | label → merlin | Open a new Merlin transcript seeded with `label`. |
| 81 | [merlinwrite](#merlinwrite) | | m label s → m | Append a labeled byte string to a transcript. |
| 82 | [merlinread](#merlinread) | | m label n → m s | Challenge `n` bytes from a transcript under `label`. |
| 83 | [sha256](#sha256) | | s → x | 32-byte SHA-256 digest. |
| 84 | [sha512](#sha512) | | s → x | 64-byte SHA-512 digest. |
| 85 | [sha3](#sha3) | | s → x | 32-byte SHA3-256 (FIPS-202) digest. |
| 86 | [log](#log) | | s → ø | Emit a byte string as a data entry into the transaction log. |
|    | **Tokens** | | | |
| 90 | [amount](#amount) | | t → t qty flv | Peek the quantity and flavor of a token without consuming it. |
| 91 | [issuepriv](#issuepriv) | ext. | qty tag → T | Mint a confidential token under the current predicate's identity + `tag`. CellOpen frames only. |
| 92 | [issuepub](#issuepub) | int. | qty tag → CT | Mint a cleartext token under the current actor's identity + `tag`. ActorCall frames only. |
| 93 | [issueflv](#issueflv) | | cid tag → int | Compute the canonical flavor scalar for an actor id + tag. |
| 94 | [retire](#retire) | | t → ø | Burn a token (emits a retire entry to the txlog). |
| 95 | [borrow](#borrow) | | qty flv → −T +T | Borrow balanced ±token pair; debt must be balanced before tx end. |
| 96 | [merge](#merge) | | a b → {c 1 \| a b 0} | Combine two same-flavor cleartokens; soft-fail on flavor mismatch. |
| 97 | [split](#split) | | a q → a' b | Split quantity `q` off a cleartoken. |
| 98 | [mix](#mix) | ext. | tokens… cmts… m n → tokens | Cloak: prove `m` input tokens balance `n` output commitments per flavor. |
| 99 | [decrypt](#decrypt) | ext. | T f' f q' q → CT | Open an encrypted Token to a ClearToken using cleartext openings. |
| 9a | [fee](#fee) | ext. | qty flv → −WT | Pay tx fee; push the balancing WideToken debt to net out via `mix`. |
|    | **Control flow** | | | |
| a0 | [verify](#verify) | | x → ø | Assert: hard-fail if int is zero, enforce a Constraint, or batch an MSM. |
| a1 | [run](#run) | | s → … | Execute a sub-program in the *same* call frame. |
| a2 | [loop](#loop) | | ø → ø | Rewind current Run to its start (loop body needs `break` to exit). |
| a3 | [switch](#switch) | | x a b → … | Pick the truthy branch: run `a` if `x ≠ 0`, else `b`. |
| a4 | [return](#return) | | a\_{k-1} … a\_0 k → ø | Exit current call frame, returning `k` items to the parent. |
| a5 | [type](#type) | | x → x code | Push the type code of the top value (peek). |
| bk | [break:k](#breakk) | | ø → ø | Exit current Run and `k` more enclosing Runs. |
|    | **Cells & predicates** | | | |
| c0 | [input](#input) | ext. | s → cell | Materialize a cell from a Utreexo-validated input encoding. |
| c1 | [cell](#cell) | | items… k pred → cell | Build a new cell from `k` portable items under predicate `pred`. |
| c2 | [output](#output) | | items… k pred → ø | Like `cell`, but emits the cell directly as a tx Output. |
| c3 | [open](#open) | | cell ik nbrs pos script gas bytes args… k → results… k' | Reveal a taproot leaf and run it in an isolated call frame. |
| c4 | [signtx](#signtx) | | cell → items… k | Authorize the tx with the cell predicate's signature; pour payload. |
| c5 | [signcall](#signcall) | | cell script sig gas bytes args… m → results… k' | Run a script signed by the cell predicate in an isolated frame. |
|    | **Actors** | | | |
| d0 | [send](#send) | | args… k refund gas bytes method addr → ø | Queue an asynchronous message to an actor. |
| d1 | [call](#call) | int. | args… k gas bytes method addr → results… k' | Synchronous actor-to-actor call (isolated frame, no re-entry). |
| d2 | [load](#load) | int. | ø → dict | Load the current actor's state dict (locks against re-entry). |
| d3 | [save](#save) | int. | dict → ø | Persist the actor's state dict (unlocks; required to survive). |
|    | **Frame introspection** | | | |
| e0 | [actorid](#actorid) | | ø → s | Push the current actor's id (32-byte string). |
| e1 | [anchor](#anchor) | | ø → s | Push the current frame's anchor (32-byte string). |
| e2 | [callerid](#callerid) | | ø → s | Push the caller actor's id (zero string if invoked externally). |
| e3 | [method](#method) | | ø → int | Push the method key the current call is dispatched under. |
| e4 | [gas](#gas) | | ø → n | Push remaining gas budget for the current call. |
| e5 | [gaslimit](#gaslimit) | | ø → n | Push the call's total gas budget cap. |
| e6 | [bytes](#bytes) | int. | ø → n | Push the actor's remaining persistent vbyte balance. |
| e7 | [memlimit](#memlimit) | | ø → n | Push the transient-memory cap (`4 × persistent_vbytes` for actor frames). |
| e8 | [newbytes](#newbytes) | | ø → n | Push vbytes delivered with the current call (0 at outermost frame). |
|    | **Tx & chain info** | | | |
| f0 | [timelock](#timelock) | | ø → n {0\|1} | Push tx locktime and a flag for height (`0`) vs. timestamp (`1`). |
| f1 | [version](#version) | | ø → n | Push tx version. |
| f2 | [height](#height) | int. | ø → n | Push the current block height. *planned* |
| f3 | [blockhash](#blockhash) | int. | h → s | Push the block hash at height `h`. *planned* |
| f4 | [blockburn](#blockburn) | int. | h → n | Push satoshis burned at height `h` (Bitcoin-coupled). *planned; maturity 100* |
| f5 | [blockweight](#blockweight) | int. | h → n | Push block weight at height `h`. *planned; maturity 100* |
| f6 | [blockrate](#blockrate) | int. | h → n | Push sparks-per-satoshi mint rate at height `h`. *planned; maturity 100* |
| f7 | [chainstate](#chainstate) | int. | n → dict | Push a dict of block stats at height `n`. *planned; maturity 100* |

Opcodes marked *planned* are reserved in the byte map; their handlers are not yet wired. Scripts using them error `UnknownOpcode` until the corresponding implementation phase lands (see `flamevm/plan.md`).

## Failure modes

A **hard fail** aborts the current call (and unwinds outwards on `op_break` boundaries).

A **soft fail** is an in-band signal: the opcode pushes an optional shape `{value 1 | 0}` and leaves the consumed value(s) on the stack untouched so the script can branch. The two kinds are noted per opcode.


## Stack instructions

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

Soft-fails on insufficient bytes, magnitude ≥ ℓ (reachable only when `n ≥ 253`), or negative zero (only when `n = 256`). **Hard-fails when `n > 256`** (programmer error).

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

Appends the low `n` bits of `x`'s canonical 32-byte representation as bytes (LSB-first). `n` must be a multiple of 8 and `≤ 256`; **hard-fails** `BitCountOutOfRange` otherwise. The sign bit (bit 255) is written iff `n = 256`.

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

Shifts bits of `a` left by `n ≤ 256`. `b` has the same length as `a`; `c` carries the displaced bits as a zero-padded-left string of `ceil(n/8)` bytes. Hard-fails `BitCountOutOfRange` if `n > 256`. Byte 0 is most significant.

### shiftright

_a n_ → _b c_

Mirror of `shiftleft`; displaced low-end bits land in `c` zero-padded on the right.

### keccak256

_s_ → _x_

Returns a 32-byte Keccak-256 digest (Ethereum compatibility). Distinct from [`sha3`](#sha3) (FIPS-202).

## Arithmetic & logic instructions

### abs

_x_ → _|x| s_

Pops `Int253` `x`; pushes the absolute value followed by the sign (`0` for non-negative, `1` for negative).

### eq

_a b_ **eq** → _a b {0|1}_ (cleartext) | → _constraint_ (CS branch)

Two stack diagrams depending on operand types and context:

1. **Cleartext branch.** When both operands are `Int253` (or both are non-CS types), peeks at the top two values and pushes `1` if equal or `0` otherwise. Operands stay on the stack. Equality is type-aware via `Value::try_eq`.
2. **Lifted branch.** When at least one operand is `Expression` or `Variable` in external context, both operands are popped, lifted to `Expression` (Int253 → `Expression::Constant`), and the result is `Constraint::eq(a, b)`.

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
`decrypt`, `fee`) appends to the delegate's R1CS. On `call` /
`open` / `signcall` entry the VM checkpoints the R1CS via
`bulletproofs::r1cs::CheckpointableConstraintSystem::checkpoint`;
on call failure the child's CS contributions (witness vectors,
constraint vectors, deferred constraints, transcript state) are
rolled back to the parent's snapshot via `rollback`. Both Prover
and Verifier hit the same checkpoint/rollback sites because they
walk the same script — the transcript stays in lockstep across
the failure boundary. On clean return the snapshot is dropped and
the child's CS contributions remain in the final proof. See
`design.md` §"CS rollback under call failure".

### scalar

_s_ → _expr_

Pops a 32-byte String, downcasts to `Int253` via `String::to_scalar`, pushes `Expression::Constant(int)`. Witness-bearing `String::Scalar(i)` extracts the witness directly; `String::Opaque(bytes)` parses canonical sign-magnitude bytes.

### commit

_s_ → _var_

Pops a 32-byte String, downcasts to a [Commitment](#types), wraps in `Variable { commitment }`. Verifier: `String::Opaque(point bytes)` → `Commitment::Closed(point)`. Prover: `String::Point(Point::Commitment(Open(witness)))` preserves the witness. Downstream [`expr`](#expr) binds the variable into the CS.

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

[Dict](#dict) keys are always `Int253`; values are any [Value](#types) subject to per-opcode copy/portability constraints.

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

### merlin

_label_ → _merlin_

Creates a fresh [Merlin transcript](#types) seeded with `label`. The transcript is a linear value (never copyable, never droppable) consumed by `merlinwrite` / `merlinread` to build custom ZKP statements.

### merlinwrite

_merlin label s_ → _merlin_

Appends `(label, s)` to the transcript and returns the same transcript on top.

### merlinread

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

### log

_s_ → ø

Pops a String, emits `TxEntry::Data(bytes)` into the txlog. Visible to the outer verifier; does not occupy persistent storage. Mirrors zkvm's `log` (same byte). Witness-bearing String variants serialize via `to_bytes` so prover and verifier emit identical bytes.

## Token instructions

See [Token / ClearToken / WideToken](#tokens) for type semantics.

### amount

_t_ → _t qty flv_

Peeks the top token-shaped value and pushes `(qty, flv)` above it. `ClearToken`: both as `Int253` (cleartext). `Token`: both as `Point` (the compressed commitment points; works without a live CS). `WideToken`: hard-fails `TypeNotToken` — its quantity isn't yet range-proven.

### issuepriv

_qty tag_ → _T_

Pops `tag` (String) and `qty` (`Variable` — a Pedersen commitment lifted via [`commit`](#commit)). Allocates a 64-bit range proof on `qty`. Builds `Token(qty_commitment, unblinded(flavor))` where `flavor = flavor_from_predicate(current_predicate, tag)`. Emits a confidential `TxEntry::Issue` binding the issuance to `current_predicate`, `tag`, and the qty commitment point. Pushes the `Token`.

The current_predicate is the predicate stored on the enclosing `CallKind::CellOpen` frame — created by [`open`](#open) or [`signcall`](#signcall) against an empty cell whose predicate is the desired issuer. Hard-fails `OpcodeRequiresPredicateContext` from `ExternalRoot` (no enclosing predicate) and from any `ActorCall` frame (issuance binds to a predicate, not an actor; the two issuance domains are kept disjoint by construction). Hard-fails `ExternalOnly` in internal context (the CS lane is required for the range proof and the qty commitment).

`Int253` or `Point` operands hard-fail `TypeNotVariable` — lift to a `Variable` via [`commit`](#commit) first.

**Non-fungible tokens.** Mix the cell's [`anchor`](#anchor) into `tag` (e.g. `anchor … keccak256` against domain bytes) to derive a fresh flavor per issuance — the result is a non-fungible Token, since no other issuance will share the flavor.

### issuepub

_qty tag_ → _CT_

Pops `tag` (String) and `qty` (`Int253` — cleartext). Builds `ClearToken(qty, flavor_from_actor(current_actor, tag))` and emits a cleartext `TxEntry::Issue` binding the issuance to `current_actor`, `tag`, and the cleartext `qty`. Pushes the `ClearToken`.

The current_actor is the actor stored on the enclosing `CallKind::ActorCall` frame. Hard-fails `OpcodeRequiresActorContext` from `ExternalRoot` and from any `CellOpen` frame (issuance binds to an actor, not a predicate; the two issuance domains are kept disjoint by construction). Runs without CS — internal context only.

`Variable` or `Point` operands hard-fail `TypeNotInt253` — `issuepub` is the cleartext path; for confidential qty, use [`issuepriv`](#issuepriv) from a `CellOpen` frame.

**Non-fungible tokens.** Mix the call's [`anchor`](#anchor) into `tag` to derive a fresh flavor per call — yields a unique non-fungible token. Use [`issueflv`](#issueflv) on the consumer side to recompute the same flavor scalar for verification.

### retire

_t_ → ø

Consumes a token; emits `TxEntry::Retire(qty_point, flv_point)`. `ClearToken` uses unblinded commitments; `Token` uses the live commitment points. Other types hard-fail `TypeNotToken`.

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

Hard-fails `MixDegenerate` if `m == 0` or `n == 0`. Mirrors zkvm's `cloak:m:n`.

### decrypt

_T f' f q' q_ → _CT_

Pops the cleartext blinding/value pairs (`q' q` for quantity, `f' f` for flavor) and the encrypted `Token`. Verifies that the supplied openings reconstruct the Token's commitment points; pushes a `ClearToken(q, f)` on success. Hard-fails on commitment mismatch.

### issueflv

_cid tag_ → _int_

Pops `tag` (String) and `cid` (String, exactly 32 bytes — an actor id). Pushes `flavor_from_actor(cid, tag)` as `Int253`. Pure helper: no CS, no txlog entry, no actor-context requirement. Domain separator is `flamevm.token.flavor` (consensus-fixed).

## Control-flow instructions

### verify

_x_ → ø

- `Int253`: hard-fails `VerifyFailed` if zero; otherwise consumes the value.
- `Constraint`: enforces the constraint via the delegate's CS (external context only).
- `MultiscalarMul`: appends `sum(s_i · P_i) == identity` to the delegate's `BatchVerifier` alongside any Schnorr/Musig sigs (external context only). Returns success immediately; the batched check runs at finalize and on failure surfaces as `BatchSignatureVerificationFailed`. See [MultiscalarMul](#multiscalarmul).
- Other types hard-fail `TypeNotInt253`.

### fee

_qty flv_ → _−WT_

Pops `qty: Int253` (non-negative, must fit `u64` and be `≤ MAX_FEE = 2²⁴`) and `flv: Int253`. Emits `TxEntry::Fee(qty as u64)` and bumps the per-tx [`CheckedFee`](#fees) accumulator (also capped at `MAX_FEE`). Allocates a fresh `WideToken` debt with `q = −qty`, `f = flv` (both cleartext-constrained) and pushes it. The script must balance the debt against real tokens, typically via [`mix`](#mix).

Hard-fails: `FeeQtyNegative`, `FeeTooHigh` (per-arg or aggregate overflow), `TypeNotInt253`, `ExternalOnly`. The blinded-fee branch is reserved for a future phase.

### run

_prog_ → _…_

Pops a String, decodes it as bytecode (or extracts instructions directly from `String::Script(instrs)` on the prover side), suspends the current Run onto the run-stack, and switches to a fresh Run over the new instructions.

**Same call frame** — see [`open`](#open) / [`signcall`](#signcall) for predicate-bound execution that creates a new frame.

### loop

ø → ø

Rewinds the current Run's cursor to the start. Without a `break` or `return` reachable from inside, this is an unbounded loop; gas metering is the long-term cap.

### switch

_x a b_ → _…_

Pops three values; if `x` is non-zero, enters `a` as the new Run, otherwise `b`. Same Run-level semantics as [`run`](#run).

### return

_a_{k-1} … a_0 k_ → ø

Atomic cross-frame return:

1. Pops `k` (Int253, non-negative).
2. Asserts an enclosing call frame exists (otherwise `ReturnAtRoot`).
3. Asserts the callee stack has exactly `k` items left (otherwise `BadReturnArity` or `StackNotClean`).
4. Pops the call frame.
5. Refunds leftover gas to the parent.
6. Pushes the `k` items onto the parent's stack.

At the outermost call frame, `return` always errors regardless of `k`. Use [`break:0`](#breakk) for early termination at root.

### type

_x_ → _x typecode_

Pushes the type code of the top value as `Int253`, leaving the value on the stack. Type codes match the wire-tag column in [Types](#types).

### break:k

ø → ø

Stops execution of the current program and `k` more enclosing Runs. `break:0` stops only the current Run. Hard-fails `BreakOutOfCall` if `k` exceeds the run-stack depth.

When the cascade ends the entire call (e.g. `break:0` at the outermost Run of a frame), normal call-exit applies: the stack must be empty (`StackNotClean` otherwise); at root that ends the transaction cleanly. Unlike `return`, `break:0` is the safe way to short-circuit at the outermost frame — no recipient semantics, relies on the clean-stack invariant.

## Cell, actor, and send instructions

[`open`](#open), [`signcall`](#signcall), and [`call`](#call) all create isolated CallFrames — see the [Calls and isolation](../design.md) section of `design.md` and [ADR 0013](../decisions/0013-predicate-call-isolation.md) for the unified-call model.

### input

_s_ → _cell_

Materializes a `cell` handle from the String on top of the stack. Seeds the frame's `last_anchor` to `Anchor(cell.id())` (the input cell's id is a spend-once unique source — see §Anchors), unconditionally replacing any prior value. Emits `TxEntry::Input(cell_id)`.

**Witness path (prover).** The prover pushes `String::Cell(c)` whose Token payloads still carry `Commitment::Open` quantities and flavors. `to_cell()` extracts the cell directly — open commitments survive into downstream `mix`/`commit` without any separate witness queue. Same pattern as zkvm's `String::Output` / `to_output`.

**Opaque path (verifier).** The verifier pushes `String::Opaque(cell_bytes)`. `to_cell()` runs `Cell::decode`, producing `Commitment::Closed` everywhere. The verifier-side CS rebuilds the commitments from points only.

Both paths produce the same `cell.id()` and the same `TxEntry::Input` (the txlog is byte-canonical regardless of which String variant the prover chose).

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

_cell internal_key neighbors position script gas bytes args… k_ → _results… k'_

Verifies the Taproot call-proof against the cell's predicate:

1. Pops `k` (Int253) and `args` (k portable values).
2. Pops `bytes` and `gas` as `Int253` (vbyte and gas allotments).
3. Pops `script` (String) — the revealed leaf bytes (or witness-bearing `String::Script` on the prover).
4. Pops `position` (String, bit-packed path), `neighbors` (list-Dict of 32-byte Strings, leaf-to-root), `internal_key` (Point).
5. Pops `cell`.
6. Constructs a `CallProof` and verifies `predicate.verify_callproof` — checks the Merkle root and the tweaked-key relation `P = X + h(X, M)·B`.
7. On success, creates a new isolated `CallKind::CellOpen { anchor: Anchor(cell.id()), predicate: cell.predicate, external_context }` frame with the popped `gas` / `bytes` allotments, pours the cell's payload then the `args` onto the new frame's stack, and enters the unlocked `script`. The tx's `last_anchor` is **not** touched — `op_open` is intra-tx and doesn't mint anything cross-tx. The `CellOpen.anchor` field is metadata (a reference to the opened cell's id), not a separate anchor slot.

The new frame has **no actor identity** by default — `op_load`/`op_save`/`op_call`/`op_send` all error from inside. The frame inherits CS access from the caller's context (external root → CS available; internal → not). Results return via `return k'`; leftover gas refunds to the parent.

Position bits are read LSB-first within byte, zero-extended past the end; bit `0` = current hash on left, neighbor on right; bit `1` = swap.

Hard-fails: `CallProofMismatch`, `MalformedCallProof`, plus the type errors from each pop.

### send

_args… k refund gas bytes method addr_ → ø

Asynchronous message-send. Pops operands top-first:

1. `addr` (32-byte String) — target [`ActorID::Hash`](#addresses).
2. `method` (`Int253`) — target method key.
3. `bytes` (`Int253`) — vbyte allotment.
4. `gas` (`Int253`) — gas allotment.
5. `refund` (32-byte String) — bounce predicate point.
6. `k` (`Int253`) — args count.
7. `args…` — k portable values, delivery payload.

Splits the frame's `last_anchor` (see §Anchors): the `left` half becomes the message's `anchor` (= SendID, known at broadcast time), the `right` half replaces `last_anchor`. Hard-fails `AnchorMissing` if no anchor has been claimed yet. Emits `TxEntry::Send { anchor, target, caller, method, refund_predicate, gas, vbytes, payload }`. The full message lives in the entry — there is no separate "sends queue"; the block builder reads `TxEntry::Send` records from the TxLog when constructing internal-tx deliveries. The originator's actor id (if any) becomes the entry's `caller`.

Available in both contexts. Hard-fails `MalformedAddress` on wrong-size addr or refund, `NonPortableInSend` on non-portable args, `InvalidBitrange` on negative/overflowing allotments.

On internal-tx failure during delivery, consensus seals the message payload into a fresh cell under `refund_predicate` and emits it as an Output effect — see [ADR 0011](../decisions/0011-send-id-and-internal-txid.md).

### call

_args… k gas bytes method addr_ → _results… k'_

Synchronous actor-to-actor call. Same operand shape as [`send`](#send) minus `refund`.

Verifies the **re-entrancy guard** — the target actor must not already appear on the current call stack ([ADR 0003](../decisions/0003-forbid-reentrancy.md); on failure, returns the `0` failure marker rather than aborting the tx). Resolves the callee's method bytes via the registry, then splits the parent's anchor (left to the child frame, right held in `post_call_anchor` for restoration on return).

**Emits no txlog entry.** Calls are intra-tx control flow; the structural effects produced inside the callee (`Output`, `Send`, `ActorSave`, `Issue`, `Retire`, `Fee`, `Data`) are what the state machine reads. The `(External TxID, Internal TxID)` of a tx is a merkle root over effects only — see `design.md` §"TxLog records effects, not control flow".

Creates an isolated `CallKind::ActorCall { actor, method, caller, anchor }` frame with the popped `gas` / `bytes` allotments. The frame has the callee's actor identity — `op_load`/`op_save`/`op_call`/`op_send` operate on the callee.

Returns via `return k'`. Hard-fails `RegistryUnavailable` outside an internal-tx execution.

### load

ø → _dict_

Loads the current actor's `ActorState` from the registry, marks the actor as locked (re-entry blocked until `save`), and pushes the wrapper Dict.

Hard-fails: `OpcodeRequiresActorContext`, `RegistryUnavailable`, `LoadAlreadyMarked`, `ActorNotFound`, `ActorFrozen`.

**Self-destruct.** A frame that returns without a matching `save` leaves the registry mark set; the tx-end commit hook removes the actor and recycles its vbytes through the maturity queue (100 blocks). See [ADR 0012](../decisions/0012-load-save-reentry-lock.md).

### save

_dict_ → ø

Pops a Dict, parses it as an `ActorState` (the two-entry wrapper shape with keys `0x00` public and `0x01` private), persists it against the current actor, and clears the mark. Emits `TxEntry::ActorSave { actor, post_state_root }` — the canonical hash of the post-save state is what a thin state machine consumes from the txlog to replay the actor-state mutation without re-running the script. See `design.md` §"TxLog records effects, not control flow".

Hard-fails: `SaveWithoutLoad`, `MalformedActorState`, `OpcodeRequiresActorContext`, `RegistryUnavailable`.

### signtx

_cell_ → _items… k_

Pops the cell, records a `DeferredSig::TxBound { verification_key: cell.predicate.point, cell_id }` for the delegate to verify at finalize against the eventual TxID, pours the cell's payload onto the current frame's stack, and pushes the count `k`.

**No new frame** — the cell-holder is authorizing the existing transaction in place.

The deferred signature is verified at finalize: the prover aggregates all `TxBound` keys via MuSig and supplies the envelope signature; the verifier batches all `TxBound` items against the `flamevm.signtx` transcript bound to TxID. Errors `BatchSignatureVerificationFailed` or `MissingTxBoundSignature` at finalize.

### signcall

_cell script sig gas bytes args… m_ → _results… k'_

Same call-frame mechanics as [`open`](#open) — taproot reveal is replaced by signature verification:

1. Pops `m` (Int253), `args` (m portable values), `bytes`, `gas`.
2. Pops `sig` (String, exactly 64 bytes — Schnorr signature).
3. Pops `script` (String) and `cell`.
4. Records `DeferredSig::Explicit { verification_key: cell.predicate.point, message: signcall_message(script_bytes), signature }`. The message is built via a Merlin transcript labelled `flamevm.signcall` over the script bytes only — scripts bind themselves to further context (anchor, actor identity, tx data) via explicit checks inside the script body.
5. Creates a new isolated `CallKind::CellOpen` frame matching [`open`](#open), pours payload + args, enters the signed script.

The deferred signatures are batch-verified at finalize alongside any `signtx` items.

### timelock

ø → _n {0|1}_

Pushes the transaction's `locktime` (as `Int253`) and a unit flag: `0` for block height, `1` for Unix timestamp. The split follows Bitcoin's BIP-65 convention — `flag = 1` iff `locktime ≥ 500_000_000` (`LOCKTIME_TIMESTAMP_THRESHOLD`). Values below the threshold are block heights; values at or above are Unix timestamps (the threshold corresponds to ~1985-11-05, before any practical timestamp range). Available in either context.

### version

ø → _n_

Pushes `TxHeader::version` as a non-negative `Int253`. Available in either context.

### actorid

ø → _s_

Pushes the current frame's actor id as a 32-byte String. Hard-fails `OpcodeRequiresActorContext` from `ExternalRoot` or `CellOpen` (no actor identity).

### anchor

ø → _s_

Pushes the frame's *current* `last_anchor` as a 32-byte String — the value the next consume site would split. Hard-fails `AnchorMissing` if no anchor has been claimed yet (same rule as `cell` / `output` / `send` / `call`). Available in either context.

### gas

ø → _n_

Pushes the current call's remaining gas budget — i.e. `gaslimit − gas_used` (saturating). Available in either context.

### bytes

ø → _n_

Pushes the current actor's remaining persistent vbyte balance, read from the registry. Internal-only: hard-fails `RegistryUnavailable` from external context and `OpcodeRequiresActorContext` from any frame without an actor identity (`ExternalRoot` / `CellOpen`).

### callerid

ø → _s_

Pushes the caller actor id as a 32-byte String. For `InternalRoot` triggered by an external send (caller = None), pushes the all-zero String. Hard-fails from `ExternalRoot` / `CellOpen` (no actor context).

### method

ø → _int_

Pushes the dispatched method key as `Int253`. Hard-fails from non-actor frames.

### gaslimit

ø → _n_

Pushes the current call's total gas budget cap (the value set at frame creation, not the remaining amount). Available in either context.

### memlimit

ø → _n_

Pushes the current call's transient-memory cap — `4 × persistent_vbytes` for actor frames per [ADR 0002](../decisions/0002-arena-memory-cap.md), or the caller-specified `bytes` operand for `CellOpen` frames, or the explicit limit passed at the outermost frame. Available in either context.

### newbytes

ø → _n_

Pushes the vbyte allotment delivered with the current call — the parent's `bytes` operand at the `call` / `send` / `open` / `signcall` site that created this frame. Zero at `ExternalRoot` (no parent). Available in either context.

## Chain-info instructions  *(all planned, internal-only)*

These opcodes read from the consensus-supplied `BlockContext`. All height-parameterized opcodes enforce the 100-block maturity window — querying `h > current_height − 100` hard-fails `BlockHeightImmature`.

### height

ø → _n_

Pushes the current block height.

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

# Discussion

**Bit-oriented vs byte-oriented strings:** we choose byte-oriented strings as simpler and less error-prone.

**Varlength encoding:** using the same CompactSize format as in Bitcoin.

**Cell-open trust model.** `open`, `signcall`, and `call` all create
isolated call frames. The unlocked / signed / called script runs in
its own stack, gas budget, memory cap, and identity scope. No
implicit access to the host's actor state, gas pool, or identity.
This eliminates the confused-deputy class of bugs for both
external- and internal-context cell-opens: an actor accepting an
untrusted-source cell does not need to audit the predicate as a
global authorization filter, because the script can't reach the
actor's state regardless of what the predicate authorizes. See
design.md §Calls and isolation.

**`signcall` binding policy.** The deferred signature for `signcall`
commits to the script bytes only. The script is expected to bind
itself to further context (anchor, actor identity, tx-level data)
by including explicit checks such as `anchor <expected> eq verify`
inside the script. This shifts the binding policy into the script
author's hands — flexibility at the price of footgun.

### Actor structure

Actor is a dict:

```rust
ActorState = Dict {
  public#0x00: Dict;
  private#0x01: Dict;
}
```

`public` - a Dict of methods that can be called, each method identified by its key. The “recv” key is reserved to process incoming messages; other keys are for custom callable methods by other actors.

`private` - a Dict with private methods and data defined by the actor.

The actor has full access to all its data, can redefine methods, change state etc.

Actor is stored in the following structure:

```rust
// All actors are stored as (ActorID, Actor) pairs.
// ActorID = Hash(initial ActorState);

struct Actor {
  state: ActorState;
  bytes_remaining: u64; // vbytes remaining
}
```

### Async messages vs Sync calls

Should the ctVM work as a sync stop-the-world machine like in Ethereum, or an async actor network like in TON?

Pros of sync:

- Simpler programming model - easier for developer adoption, less error prone.
- Contracts can easily return data to each other.
- Full picture at once, no delays to be exploited by traders and validators.

Pros of async:

- Isolated concurrently verifiable transactions - better scalability options.

Cons of both models: 

- Need to store most of the state in memory to execute transactions.

Additional considerations:

- Well-designed massive multiplayer contracts operate at constant cost (variable cost works only for limited amount of data and/or users).

Flame architecture offloads most of the accounts and payments to UTXO model that keeps the state compressed. Most of the computation costs are concentrated in zkVM and are easily parallelizable.

zkVM communicates with ctVM via async messages: instead of an *output*, a zkVM transaction may create a message that’s added to the ctVM message queue. When ctVM is invoked to process the message, the entire state of all deployed contracts is available for the execution — ctVM contracts can call each other in a synchronous manner.

### **Interface**

zkVM emits messages - this allows parallelizable verification of zkVM transactions & allows committing payment for gas (because zkvm tx by itself does not pay fees when it fails).

ctVM asynchronously executes messages, processing queue in FIFO order. All messages emitted during the block of zkvm txs are processed within that block.

ctVM may emit both outputs (utxos) and messages (async calls) — for returning values to the users and scheduling messages for other contracts. 

ctVM asymmetric crypto operations (based on point-scalar multiplications) are for verification only: all such operations are assumed to be valid during computation and batched in the end of it. These include verification of signatures and encrypted values.

### **Messages**

- zkVM transaction pre-pays fee for a portion of gas for each emitted message.
- All prepaid gas is consumed regardless of execution result, gas is never refunded upon failure.
- ctVM contract has two results: failure or success. In case of failure, the message payload is returned to the predicate specified in the message, ctVM state is unchanged.
- ctVM may fail for (1) exceeding gas limit, (2) exceeding space limit, (3) triggering failure explicitly during execution.

Missing public method - call to private fn missing().
Destination = Either(Address, InitialState).
Empty bounce predicate = no bounce, value is burned on error.
Can port zero qty token of any flavor for InitialState.

Bounce: only makes sense for predicates since we cannot know who pays the gas to process the bounce and if it succeeds. Bounces only make sense for offchain invocations, since cross-contract calls are all atomic.

Idea: no need to have context-free program execution only to call a method on a deployed contract. Simply call on a contract (empty string - no call). 

Bytes transfer: contract has balance of storage limit and can transfer it to any other contract.

```rust
Call {
	destination: Enum {
		a: Address,
		i: Constructor
	},
	method: String, // name of the code to be called
	payload: (T…),   // tuple of items 
	gas: Int,        // prepurchased amount of gas
	bytes: Int,      // amount of storage that can be allocated to the contract
	bounce: Optional<Predicate>,
	anchor:          // unique anchor generated by the transaction
}
```

Call arguments:

- **k** items as payload, all portable.
- gas limit, 0 if inherits all parent’s limit.
- blockbytes, 0 if none to add to the balance.
- method name
- address (ctor or hash)

Send:

- in addition: bounce
- anchor.

Note: if calls failure can be handled, then sigchecks must be batched per call. This also means that sometimes sigchecks can fail and that is allowed behavior. Meaning, when we transfer the block of txs to another node we may signal the status and reason for execution of each call, so failed sigchecks are verified

```rust
send:
args... k method gas bbytes addr -> {results... m 1 | args... k 0}

call:
args... k method gas bbytes addr -> {results... m 1 | args... k 0}

```

**Questions:**

- Reentrancy? How the stored state mutates?
- What is address? Is the address really a constructor code, but it can be compressed into a hash? What if user provides hash, but the contract is not even deployed yet? What if we don’t have a hash, but have a short name instead? Meaning, we send to another id which is DNS contract and it routes the call to the destination contract.
- Call failures w.r.t. linear types:

- When deployed, ctVM contracts allocate power-of-two space and pre-pay for N blocks.
    - Note: we cannot give out prepayment all at once to current minters - then the rest of the minters are not getting the rent. It should be parked
- Operations that exceed prepaid bounds fail. When prepayment expires, contract state is frozen and compressed (id→state hash pair), and can be resurrected by anyone paying for its deployment. Network therefore guarantees that the state is not lost when the rent expires for the extra N blocks into the future.
    - TODO: we can try to use utxos to offload state perpetually, but then we cannot protect against fresh state re-deployment. What do we do to permanently erase the state?
- each ctVM contract is allowed to consume transiently as much space as it has rented for its storage. E.g. if rented 128 bytes, then only up to extra 128 bytes can be used by a contract during calls and re-entrant calls.

### Execution

Layers:

1/ VM run: process message, memlimit, gas limit, verify point ops, emit outputs.

2/ Contract call: isolated storage access. Call must end with empty stack. Pass in and returns are explicit. Call is mem-bounded and gas-bounded. In-place contracts provide isolation and inherit remaining mem/gas limits and affect the parent’s limit.

3/ Program run: executing a specific string of instructions.

### Design questions

- how do we name two VMs, do we name them as one VM? How we name each context: outside the deployed contract and inside?
    - “FlameVM”, external transaction and internal transactions.
- How to claim new flames?
    - (a) insert utxo with out-of-vm rules upon maturity; (b) explicit “claim” opcode - more complicated.
- how does anchor work in all contracts?
    - Provided in context. Cannot be user-affected.
- how does issuance work?
    - both zk and ctvm can issue. Flavor tied to contract identity, anchor is optionally used by the user to make issuance
- how do contracts with preds and with methods work?
    - “open:k” shows merkle path to a program that executes within scope of the contract with arguments passed in.
- how opening of taproot works?
    - open:k is like call:k, but without selector.
    - predicate signature simply returns root item in the contract.
- how deployment works?
    - we need on-the-fly deploy
- how rent payment is subtracted / spread out?
- how isolated program runs work?
- how encrypted vs unencrypted values work and their ops?
    - Token - possibly encrypted supertype of ClearToken.
    - Can typecast up, must prove typecast down.
- how to pay fee in any asset (softly enforced by the network elsewhere)?

idea: stateinit = constructor code - then we can verify zero token init w/o scanning data structs

idea: message is a in-vacuum call. also: after wrap contract runs in vacuum.

idea: issue from utxo to prevent replays - if deployed contract offloads, then it can reissue tokens. And tokens are permeating other contracts - making this ecosystem risk.

Issuance should be possible to tie to an anchor if it’s programmable and tied to global state.

**Example: DEX AMM**

```jsx
type Message: Dict {

}

contract AMM {
  var X: Token
  var Y: Token
  
  fn liquidity(msg: ) {
     // determine which token is in excess and add it as a swap.
  }
  
  fn receive(message: Message) {
     if message.code {
     }
  }
  
  // sells x for y, returns y
  fn sell(x: Token, price_slippage: f64) -> Token {
     // X*Y = k
     // (X+x)*(Y-y) = k
     // ~~XY~~ + xY - y *(X+x) = ~~k~~
     // y = xY / (X+x)
     verify(x.flv == self.X.flv);
     let y = x*self.Y.qty / (self.X.qty + x);
     self.X = self.X + x;
     self.Y = self.Y - y;
     return y;
  }
  
  fn buy(y: Token) {
     
  }
}
```

**Example: Bitcoin Resurrection**

```rust
contract Prime {
  var btc_issued: u64;
  var flames_deposited: Token;
  
  pub fn resurrect(f: Token) -> Token {
    verify(f.flv == FlameFlavor);
    let circulation = Flame.flames_issued - self.flames_deposited
    let q = f.qty * Flame.btc_burned / circulation
    self.btc_issued += q
    self.flames_deposited = merge(self.flames_deposited, f)
    return issuepub(“btc”, q)
  }
  
  pub fn revert(btc: Token) -> Token {
    verify(btc.flv == issuance_flavor("btc"))
    // TBD: burn btc, return flames at current price
    ...
  }
}
```
