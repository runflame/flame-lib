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
5. Data entry — for data logging that does not occupy permanent storage.

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

Cryptography types: Merlin transcript and MultiscalarMul evaluation.

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
| MultiscalarMul | Deferred linear combination of points (scalar-point multiplications). |

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

### Point

Ristretto255 group element. Stored as compressed 32-byte encoding. Used to represent public keys and Pedersen commitments.

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

# Instruction set

Layout of all the instructions:

|  | 00 | 10 | 20 | 30 | 40 | 50 | 60 | 70 | 80 | 90 | A0 | B0 | C0 | D0 | E0 | F0 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 0 | push0 | pushi8 | dup0 | roll0 | readbits | abs | dict | amount | break0 | input | callerid |  |  |  |  |  |
| 1 | push1 | pushi8s | dup1 | roll1 | readint | eq | put | issue | break1 | object | method |  |  |  |  |  |
| 2 | push2 | pushi16 | dup2 | roll2 | readstr | neg | replace | retire | break2 | output | gaslimit |  |  |  |  |  |
| 3 | push3 | pushi16s | dup3 | roll3 | readpoint | add | get | borrow | break3 | open | memlimit |  |  |  |  |  |
| 4 | push4 | pushi64 | dup4 | roll4 | writebits | mul | getopt | merge | break4 | send | newbytes |  |  |  |  |  |
| 5 | push5 | pushi64s | dup5 | roll5 | writeint | divmod | getdup | split | break5 | call | height |  |  |  |  |  |
| 6 | push6 | pushi128 | dup6 | roll6 | append | mod252 | first | mix | break6 | load | blockhash |  |  |  |  |  |
| 7 | push7 | pushi128s | dup7 | roll7 | writezeros | not | last | decrypt | break7 | save | blockburn |  |  |  |  |  |
| 8 | push8 | pushint | dup8 | roll8 | bitnot | and | next | issueflv | break8 | signtx | blockweight |  |  |  |  |  |
| 9 | push9 | pushstr | dup9 | roll9 | bitor | or | merlin | verify | break9 | signrun | blockrate |  |  |  |  |  |
| A | push10 | pushpoint | dup10 | roll10 | bitand | const | merlinwrite | fee | break10 | timelock | chainstate |  |  |  |  |  |
| B | push11 | pushtoken | dup11 | roll11 | bitxor | extvar | merlinread | run | break11 | version |  |  |  |  |  |  |
| C | push12 | drop | dup12 | roll12 | shiftleft | intvar | sha256 | loop | break12 | actorid |  |  |  |  |  |  |
| D | push13 | nop | dup13 | roll13 | shiftright | expr | sha512 | switch | break13 | anchor |  |  |  |  |  |  |
| E | push14 | dup | dup14 | roll14 | keccak256 | range | sha3 | return | break14 | gas |  |  |  |  |  |  |
| F | push15 | roll | dup15 | roll15 |  | size | sigverify | type | break15 | bytes |  |  |  |  |  |  |

### Stack operations

| Hex | Name | Stack diagram | Notes |
| --- | --- | --- | --- |
| 0k | push:k | ø → int | Pushes an integer in range 0..15 |
| 10, 11 | pushint8 | ø → int | Reads one more byte, sets the sign to s. |
| 12, 13 | pushint16 | ø → int | Reads 2 more bytes little-endian, sets the sign to `s`. |
| 14, 15 | pushint64 | ø → int | Reads 8 more bytes little-endian, sets the sign to `s`. |
| 16, 17 | pushint128 | ø → int | Reads 16 more bytes little-endian, sets the sign to `s`. |
| 18 | pushint | ø → int | Reads 32 more bytes; bytes are the sign-magnitude form (LE magnitude in bytes 0..31; highest bit of byte 31 is the sign). |
| 19 | pushstr | ø → string | Pushes a string on stack. |
| 1a | pushpoint | ø → point | Pushes a point on stack. |
| 1b | pushtoken | flv → token | Pops an Int253 flavor and pushes a 0-qty ClearToken with that flavor. |
| 1c | drop | x → ø | Drops any droppable item, including empty structs and zero-tokens. |
| 1d | nop | ø → ø | Does nothing. |
| 1e | dup | x… k → x ... x | Copies k-th item to the top of the stack, takes integer k from stack. |
| 1f | roll | x… k → ... x | Rolls over k-th item to the top of the stack, takes integer k from stack. |
| 2k | dup:k | x_k … x_0 → x_k ... x_0 x_k | Copies k-th item to the top of the stack. |
| 3k | roll:k | x_k … x_0 → x_{k-1} ... x_0 x_k | Rolls over k-th item to the top of the stack. |

### String operations

**Failure principle (bit/int read/write opcodes).** Violations of constraints derived from *external data* — string length, canonical magnitude, negative-zero — are **soft fails**: the opcode returns the optional-`0` shape and leaves the source string on the stack untouched, letting the script branch on the failure. Violations of *author-controlled* bounds — e.g. a static bit count `n > 256` — are **hard fails**: the script aborts with a programmer-error. The same principle applies to other read/write opcodes in this section: a hard-fail signals "the script is wrong", a soft-fail signals "the input doesn't fit".

| Hex | Name | Stack diagram | Notes |
| --- | --- | --- | --- |
| 40 | readbits | s n → s’ x 1 | s 0 | Reads `n ≤ 256` bits **LSB-first within byte** into bits 0..n-1 of a new `Int253`. Sign at fixed bit position 255 — only set when `n = 256` AND input bit 255 is 1. Soft-fails (`s 0`, string untouched) on insufficient bytes, magnitude ≥ ℓ (possible when `n ≥ 253`), or negative zero (possible when `n = 256`). **Hard-fails the script if `n > 256`** (programmer error). |
| 41 | readint | s → s’ x 1 | s 0 | Equivalent to `readbits(s, 256)`. Reads the canonical 32-byte `Int253` (bit 255 = sign). Same soft-fail conditions: insufficient bytes, magnitude ≥ ℓ, or negative zero. |
| 42 | readstr | s n → s’ s’’ 1  | s 0 | Reads n bytes in a new string, consuming them from the first string. |
| 43 | readpoint | s → s’ point 1 | s 0 | Reads point |
| 44 | writebits | s x n → s’ | Appends the low `n` bits of `x`'s canonical 32-byte `Int253` representation as bytes (LSB-first). `n` must be a multiple of 8 and `≤ 256`; **hard-fails** otherwise. The sign bit (bit 255) is included iff `n = 256`. |
| 45 | writeint | s x → s’ | Appends the canonical 32-byte `Int253` representation of `x`. Equivalent to `writebits(s, x, 256)`. |
| 46 | append | s s’ → s’’ | Appends s’ to s: s’’ = s || s’ |
| 47 | writezeros | s n → s’ | Appends n zero-bytes. |
| 48 | bitnot | s → s’ | Inverts all bits in a string. |
| 49 | bitor | a b → c | Bitwise OR of two strings. Fails if strings are of different size. |
| 4a | bitand | a b → c | Bitwise AND of two strings. Fails if strings are of different size. |
| 4b | bitxor | a b → c | Bitwise XOR of two strings. Fails if strings are of different size. |
| 4c | shiftleft | a n → b c | Shifts bits left by n≤256 bits. Returns removed bits as string zero-padded on the left. |
| 4d | shiftright | a n → b c | Shifts bits right by n≤256 bits. Returns removed bits as string zero-padded on the right. |
| 4e |  |  |  |
| 4f |  |  |  |

### Ints, logic and constraints

| Hex | Name | Stack diagram | Notes |
| --- | --- | --- | --- |
| 50 | abs | int → int’ s | Removes the sign from int, turning it into a canonical scalar. Puts sign s on stack. |
| 51 | eq | x y → x y {0 | 1} | Checks equality of two types. |
| 52 | neg | x → –x | Negates integer, varexpr or pointexpr. |
| 53 | add | x y → z | Adds two integers modulo R255 group order; or adds two Expressions. |
| 54 | mul | x y → z | Multiplies two integers modulo R255 group order; or multiplies two Expressions. |
| 55 | divmod | x z → d r | Computes the quotient and the remainder for int. |
| 56 | mod252 | str → int | Interprets 0..64-byte as a little-endian unsigned integer and mod-reduces to R255 group order. |
| 57 | not | x → y | Numbers: 0 → 1, non-0 → 0; Constraints: returns inverse constraint. |
| 58 | and | a b → c | Logical AND of two ints (one zero ⇒ 0, both non-zeroes ⇒ 1); or the same for constraints. |
| 59 | or | a b → c | Logical OR of two ints (non-zero ⇒ 1, both zeroes ⇒ 0); or the same for constraints. |

All constraint operations available on external transactions only.

| Hex | Name | Stack diagram | Notes |
| --- | --- | --- | --- |
| 5a | const | scalar → expression | Converts scalar into an expression with weight 1. |
| 5b | extvar | point → var | Allocates an external variable based on a Pedersen commitment. |
| 5c | intvar | ø → var | Allocates an internal variable in CS (non-committed via PC). |
| 5d | expr | var → expr | Converts variable into an expression. |
| 5e | range | expr n → expr | **[E]** Pops bit count `n: Int253` (must be in `[1, 64]`) and an `Expression`. For `Expression::Constant`, asserts the constant fits in `[0, 2ⁿ)` (cleartext check, no CS work). For `Expression::LinearCombination`, adds a bulletproofs range-proof gadget asserting `0 ≤ expr.value < 2ⁿ`. The Expression is pushed back unchanged. Hard-fails `BitCountOutOfRange` if `n ∉ [1, 64]`, `InvalidBitrange` on cleartext overflow, `R1CSError` on CS-construction failure. |
| 5f | size | x → x n | (Internal+External) Returns length of string in bytes, or struct’s number of entries. |

### Dicts

| Hex | Name | Stack diagram | Notes |
| --- | --- | --- | --- |
| 60 | dict | … val key val key n → dict | Creates a new dict with 2*n items as key-value pairs. |
| 61 | put | dict k v → dict’ | Inserts the value at key, fails if the slot is already occupied. |
| 62 | replace | dict k v → dict’ {prev 1 | 0 } | Set value at the key, returning previous value as optional. |
| 63 | get | dict k → dict’ k v | Takes out value at key `k`, fails if the value is missing. |
| 64 | getopt | dict k → dict’ {v 1 | 0} | Removes the value as optional. |
| 65 | getdup | dict k → dict {v 1 | 0} | Copies the value at key. Returns 0 if key is missing, fails if value exists but not copyable. |
| 66 | first | dict → dict {k 1 | 0} | First key in the dict |
| 67 | last | dict → dict {k 1 | 0} | Last key in the dict |
| 68 | next | dict k → dict {k’ 1 | 0} | Next key after the given one |

### **Cryptography**

| Hex | Name | Stack diagram | Notes |
| --- | --- | --- | --- |
| 69 | merlin | label → merlin | Creates a new transcript with a given label. |
| 6a | merlinwrite | merlin label str → merlin | Writes byte string with a given label. |
| 6b | merlinread | merlin label n → merlin str | Reads n bytes with a given label. |
| 6c | sha256 | str → x | Returns a 256-bit string with sha256 digest of an input |
| 6d | sha512 | str → x | Returns a 512-bit string with sha2-512 digest of an input |
| 6e | sha3 | str → x | Returns a 256-bit string with sha3-256 (FIPS-202) digest of an input |
| 4e | keccak256 | str → x | Returns a 256-bit string with Keccak-256 digest of an input (Ethereum compatibility). |
| 6f | sigverify | msg pk sig scheme → ø | Checks the signature or fails.  |

### Tokens

| Hex | Name | Stack diagram | Notes |
| --- | --- | --- | --- |
| 70 | amount | token → token qty flv | Peeks the top token-shaped value and pushes `(qty, flv)` above it. ClearToken: both as `Int253` (cleartext). Token: both as `Point` (the compressed commitment points; works without a live CS). WideToken: errors `TypeNotToken` until Phase 13 lands the constructor. |
| 71 | issue | qty tag → T | Cleartext branch (`qty: Int253`): builds `ClearToken(qty, flavor_from_actor(current_actor, tag))`, emits `TxEntry::Issue(unblinded_qty, unblinded_flv)`. Encrypted branch (`qty: Point`): defers to Phase 11/12 — hard-fails `TokenRequiresCS` until the constraint-system delegate is online. Requires actor context (`OpcodeRequiresActorContext` from `ExternalRoot`); to compute a flavor without actor context use `issueflv`. |
| 72 | retire | token → ø | Consumes a token; emits `TxEntry::Retire(qty_point, flv_point)`. ClearToken uses unblinded commitments; Token uses live commitment points. WideToken / other types: `TypeNotToken`. |
| 73 | borrow | qty flv → –T +T | Cleartext branch (both `Int253`): pushes `(ClearToken(-qty, flv), ClearToken(qty, flv))` — the negative is non-portable bottom, the positive is portable top. Encrypted branch (any operand is `Point`): defers to Phase 12 with a 64-bit range proof on `+T` — hard-fails `TokenRequiresCS` until then. |
| 74 | merge | a b → {c 1 | a b 0} | ClearTokens only: on flavor match, pushes `(ClearToken(a.qty+b.qty, flv), 1)`. On flavor mismatch, restores `(a, b, 0)` (soft-fail). Non-ClearToken inputs error `TypeNotClearToken`. |
| 75 | split | a q → a’ b | ClearTokens only: returns `(ClearToken(a.qty-q, flv), ClearToken(q, flv))`. Hard-fails `TokenSplitOutOfRange` if `q < 0`, `a.qty < 0`, or `q > a.qty`. Non-ClearToken inputs error `TypeNotClearToken`. |
| 76 | mix | anytokens... commitments… m n → values | (External) Performs merge and split of tokens, cleartokens and widetokens. |
| 77 | decrypt | token f f’ q q’ → cleartoken | Converts token to a cleartext one by providing cleartext flavor and quantity with their blinding factors. |
| 78 | issueflv | cid tag → int | Pops a `tag` String and a `cid` String (must be exactly 32 bytes — actor id); pushes `flavor_from_actor(cid, tag)` as `Int253`. Pure helper: no CS, no txlog effect, no actor-context requirement. Domain separator `flamevm.token.flavor.v1` (consensus-fixed). |

### Control flow

| Hex | Name | Stack diagram | Notes |
| --- | --- | --- | --- |
| 79 | verify | scalar → ø | Fails if scalar or int is zero. Otherwise pops the number off the stack. |
| 7a | fee | qty flv → widetoken | Records a fee and returns a corresponding debt token (with negative qty). |
| 7b | run | str → … | Runs bitstring as a program. |
| 7c | loop | ø → ø | Evaluates program from the beginning. |
| 7d | switch | x a b → … | Runs program a if x is non-zero, runs b if x is zero. |
| 7e | return | a_{k-1} … a_0 k → ø | Returns k items to the caller and finishes the **current call**. Pops `k`, asserts the callee's stack contains exactly `k` items, pops the call frame, refunds leftover gas to the parent, and pushes the `k` items onto the parent's stack — all atomically. **At the outermost call frame `return` always errors regardless of `k`** (`ReturnAtRoot`): there is no parent to receive values, even an empty tuple. Scripts that want to terminate early at root use `break:0` instead. |
| 7f | type | x → x typecode | Pushes typecode (as int) of the item on stack. |
| 8k | break:k | ø → ø | Stops execution of the current program and `k` more enclosing Runs. `break:0` stops only the current program. Attempting to break past the call boundary (`k` exceeds the run-stack depth) is a hard fail (`BreakOutOfCall`). When the cascade ends the entire call (e.g. `break:0` at the outermost Run of a frame), normal call-exit applies: the stack must be empty (`StackNotClean` otherwise); at root that ends the transaction cleanly. Unlike `return`, `break:0` is the safe way to short-circuit at the outermost frame: it has no recipient semantics and relies on the clean-stack invariant for correctness. |

### Actors & calls

| Hex | Name | Stack diagram | Notes |
| --- | --- | --- | --- |
| 90 | input | string → cell | **[E]** Decodes a wire-encoded cell from `string` and materializes a `cell` handle on the stack. Seeds the VM's `last_anchor` from the cell's identity (via `Cell::to_anchor()`, which ratchets internally) and emits a `TxEntry::Input(cell_id)` effect into the txlog committing the consumed cell's id. **The VM does not consult any Utreexo accumulator**: the caller is expected to have validated the supplied bytes against the Utreexo proof outside the VM before invoking the script. From the VM's perspective the bytes simply assert "this cell existed as a UTXO"; the txlog entry commits the script's reliance on that assertion so the outer verifier can cross-check it against Utreexo state. Hard-fails on a non-`String` top, on bytes that don't decode as a canonical cell (`MalformedCellEncoding`, including trailing bytes after the cell), or when invoked from internal context (`ExternalOnly`). |
| 91 | cell | args… k pred → cell | Wraps a tuple of portable items into a linear `cell` handle. Consumes the VM's `last_anchor` for the new cell's anchor and advances it to the next anchor via `Cell::to_anchor()`. |
| 92 | output | args… k pred → ø | Same construction as `cell` but emits an `Output` effect into the txlog instead of pushing the handle. |
| 93 | open | cell internal_key neighbors position program args… k → results… | Verifies the Taproot call-proof formed by `internal_key` (a Point), `neighbors` (list-style Dict of 32-byte Strings, leaf-to-root order), `position` (String of bit-packed sides), and `program` (String) against the cell's predicate. On success, pours the cell's payload then the `k` args onto the current call's stack and enters a new Run over `program`. Cell-open is **Run-level, not Call-level**: the program shares the current call's stack, gas, memory cap, identity, and control-flow scope — `return k` from inside the opened program exits the enclosing call frame, and `break:k` past the run-stack errors `BreakOutOfCall`. Any error inside the Run hard-fails the current call. Position bits are read LSB-first within byte, zero-extended past the end; bit value `0` = current hash on left / neighbor on right, `1` = swap. |
| 94 | send | args… k gas bytes method addr → ø | Send message; similar to call, but does not expect results. |
| 95 | call | args… k gas bytes method addr → results… k | (Internal) Calls a method on an actor, transferring control. |
| 96 | load | ø → dict | (Internal) Loads actor state and marks the actor for destruction (re-entry blocked until `save`). |
| 97 | save | dict → ø | (Internal) Saves the actor state and unmarks it for destruction. |
| 98 | signtx | cell → items… k | Pops the cell, defers a TxID-bound signature record (verification key = cell's predicate point; signature comes from the tx envelope at finalize), pours the cell's payload onto the current call's stack and pushes the count `k`. |
| 99 | signrun | cell prog sig args… m → items… k | Records a deferred signature commitment over `prog` only (verification key = cell's predicate point; signature = the popped `sig` String, must be 64 bytes); pours the cell's payload then the `m` args onto the current call's stack; enters a new Run over `prog`. Same Run-level isolation rules as `open`. Programs bind themselves to context (anchor, actor identity, etc.) via explicit checks inside the program. |
| 9a | timelock | ø → n {0 | 1} | Pushes timelock integer and a flag: 0 for block height, 1 for timestamp. |
| 9b | version | ø → n | Version bits of the external transaction invoking this call. |
| 9c | actorid | ø → string | Pushes actor ID. |
| 9d | anchor | ø → string | Returns the unique 256-bit anchor for the current actor invocation. |
| 9e | gas | ø → int | Remaining gas after execution of this instruction. |
| 9f | bytes | ø → int | Remaining persistent storage (vbytes) after execution of this instruction. |
| a0 | callerid | ø → string | Actor ID of the caller. All-zero for a message from external tx. |
| a1 | method | ø → int | (Internal) Invoked method. |
| a2 | gaslimit | ø → int | Maximum amount of gas for this call. |
| a3 | memlimit | ø → int | Maximum amount of memory that can be used during the call. |
| a4 | newbytes | ø → int | Received storage units during this call. |

### Chain info (Internal only)

| Hex | Name | Stack diagram | Notes |
| --- | --- | --- | --- |
| a5 | height | ø → int | Current block height. |
| a6 | blockhash | h → string | Hash of the block at a given height. |
| a7 | blockburn | h → int | Total amount of satoshis burned at height h. Fails for h > current-100 (maturity period). |
| a8 | blockweight | h → int | Weight of the block |
| a9 | blockrate | h → int | Average mint rate in terms of sparks per satoshi at block height h. |
| aa | chainstate | n → dict | Pushes struct with block stats at height n. n must be 100 blocks behind the current block. |
|  |  |  |  |

---

# Discussion

**Bit-oriented vs byte-oriented strings:** we choose byte-oriented strings as simpler and less error-prone.

**Varlength encoding:** using the same CompactSize format as in Bitcoin.

**Cell-open trust model.** `open` and `signrun` are Run-level — the unlocked program runs inside the caller's call frame, sharing its stack, gas, memory cap, identity, and (when called from within an actor) its actor authority. The caller is choosing to delegate full local authority to the unlocked program. This is safe in external transactions because the *creator* of the external tx is the same party choosing which cell to open and which predicate to satisfy — the program is, by construction, code the tx author has accepted by accepting the predicate. The same reasoning applies to internal-context cell-opens but with finer granularity: an actor opening a cell is granting the cell's program full access to the actor's frame. Authors of methods that accept cells from untrusted callers must therefore treat the cell's predicate as **the** authorization filter for the entire program-side authority. (Future review: tighter sandboxing for cell-opens in internal context may be desirable.)

**`signrun` binding policy.** The deferred signature for `signrun` commits to the program bytes only. The program is expected to bind itself to further context (anchor, actor identity, tx-level data) by including explicit checks such as `anchor <expected> eq verify` inside the program. This shifts the binding policy into the program author's hands — flexibility at the price of footgun.

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
    return issue(“btc”, q)
  }
  
  pub fn revert(btc: Token) -> Token {
    verify(btc.flv == issuance_flavor("btc"))
    // TBD: burn btc, return flames at current price
    ...
  }
}
```