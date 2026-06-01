# FlameVM design

FlameVM is the stack machine that verifies transactions in the Flame network. Each transaction instantiates a fresh VM that reads a [script](#script), executes it against the current blockchain state, and emits a set of [effects](#effect) that update that state. Successful VM execution implies successful transaction verification — VM rules therefore encode both built-in network constraints and any custom rules an author writes into their applications.

## State model

The Flame network's persistent state has two substrates:

- **Utreexo accumulator** — a compressed set of unspent transaction [outputs](#output). Mutated only by [external transactions](#external-transaction), which consume entries (as [cells](#cell)) and append new ones.
- **Actor registry** — a set of long-living, multi-user [actors](#actor) holding state and methods. Mutated by [internal transactions](#internal-transaction), which deliver messages and invoke actor methods.

[External transactions](#external-transaction) interact with both parts of the state: they spend and create outputs, and they emit [message sends](#message-send) that schedule internal transactions. [Internal transactions](#internal-transaction) primarily mutate the actor registry, though they may also append outputs and emit further sends.

## FlameVM

FlameVM is a Forth-like stack machine with linear types. Each transaction instantiates its own VM, isolated from any other concurrent VM instance. The VM runs in one of two contexts:

- **External context** — verifies an [external transaction](#external-transaction). Has access to Utreexo (consume and produce [outputs](#output)), to zero-knowledge proof primitives ([Bulletproofs](#bulletproofs)), and to the ability to schedule [message sends](#message-send).
- **Internal context** — verifies an [internal transaction](#internal-transaction). Has access to the actor registry (call methods, mutate state) but cannot use Bulletproofs.

External transactions can be verified concurrently because each one is bound by its own [TxID](#txid) and reads only from already-confirmed state. Internal transactions are verified serially within a block because they share the actor registry. See [Concurrency and privacy](#concurrency-and-privacy).

## Data types

### Stack discipline

Every value on the stack is owned by the script that produced it. FlameVM has no reference counting, borrowing, read-only access, or implicit copies. Values carry two dynamic properties:

- **Copyable** — can be duplicated. Non-copyable values (notably [tokens](#token)) must be moved.
- **Portable** — can exist in persistent blockchain state outside FlameVM execution. Non-portable values exist only on the stack and are forbidden inside [cells](#cell) or [actor](#actor) state.

A composite value (a `Dict`) inherits these flags from its members: once a non-portable item is inserted, the whole dict becomes non-portable. The same is true for non-copyable items.

### Primitive types

| Type      | Description |
|-----------|-------------|
| `Int253` | Signed sign-magnitude integer; magnitude is a canonical Ristretto scalar (`< ℓ ≈ 2²⁵²`), plus an explicit sign bit. The name reflects the effective conceptual width: `⌈log₂(ℓ)⌉ = 253` bits of magnitude. |
| `String`  | Variable-length byte string; also acts as a builder/reader for parsing. |
| `Point`   | Element of the Ristretto255 group; used for public keys and Pedersen commitments. |
| `Dict`    | Map from `Int253` keys to arbitrary values; used for lists, maps, and enum variants. |

### Tokens

A token is a linear value type representing an asset instance. Tokens are bearer instruments: they cannot be duplicated, created out of thin air, or destroyed except through explicit issuance, retirement, or token-balanced merge/split.

Each token has a [quantity](#quantity) (number of atomic units) and a [flavor](#flavor) (identifier of the asset kind, derived from the issuing [actor](#actor)'s id or [predicate](#predicate)'s point — see [Issuance and retirement](#issuance-and-retirement)). Both may be encrypted. The built-in currency — also called *flame* — has the same type as any user-defined token. Tokens are suitable for both financial instruments and capabilities (voting rights, access rights, and similar).

Three concrete variants:

| Type         | Quantity   | Flavor     | Sign            | Portable          |
|--------------|------------|------------|-----------------|-------------------|
| `Token`      | encrypted  | encrypted  | non-negative (range-proven) | yes |
| `ClearToken` | cleartext  | cleartext  | may be negative | only when non-negative |
| `WideToken`  | encrypted  | encrypted  | may be negative | no                |

`WideToken` and signed `ClearToken` exist to express intermediate values during merge/split operations that must net out by end of transaction.

### Issuance and retirement

Two opcodes mint tokens, split by execution context:

- **`issuepub`** — runs only in an [actor](#actor) frame (internal context). Cleartext `qty` (`Int253`); pushes a [`ClearToken`](#cleartoken). [Flavor](#flavor) is `flavor_from_actor(current_actor, tag)`.
- **`issuepriv`** — runs only in a [`CellOpen`](#predicate) frame (external context). Confidential `qty` (a `Variable` lifted from a Pedersen commitment), range-proven to 64 bits; pushes a `Token`. Flavor is `flavor_from_predicate(current_predicate, tag)`.

The split is structural, not policy: **privacy lives where the CS lives**. Predicate-opened frames execute in external context where R1CS and the batch verifier are available, so the confidential opcode lives there. Actor-call frames execute in internal context where there is no CS lane, so only the cleartext opcode is usable. There is no dispatch on operand type — wrong-type operands hard-fail at the opcode boundary, and each opcode rejects the wrong frame kind (`OpcodeRequiresActorContext` / `OpcodeRequiresPredicateContext`).

The two issuer domains are **disjoint by construction**. Both opcodes seed a single Merlin transcript labelled `flamevm.token.flavor` (consensus-fixed), but `issuepub` opens it with first message `b"actor"` while `issuepriv` opens it with `b"predicate"`. A predicate-point that happens to be byte-identical to an actor id still produces a different flavor scalar — actors and predicates cannot collide on the same flavor by accident, and a predicate cannot forge an actor's flavor (or vice versa) by colliding identities.

The `retire` opcode destroys a token regardless of how it was minted. Issuance and retirement are explicit transaction [effects](#effect) — `issuepub` emits `TxEntry::IssuePub(qty, flv)` (cleartext `Int253` pair on the wire), `issuepriv` emits `TxEntry::IssuePriv(qty_point, flv_point)` (Pedersen commitments), and `retire` emits `TxEntry::Retire(qty_point, flv_point)`. The split keeps the wire-level audit trail clean: a verifier can tell at a glance whether an issuance reveals its qty/flv directly or only via commitments.

### Constraint types

Used for confidential operations in external transactions. See [Confidentiality](#confidentiality).

| Type          | Description |
|---------------|-------------|
| `Object`      | Linear handle to a cell or external commitment. |
| `Variable`    | Secret value in the constraint system, tied to a Pedersen commitment. |
| `Expression`  | Linear combination of variables. |
| `Constraint`  | Logical combination of boolean conditions. |

### Cryptography types

| Type             | Description |
|------------------|-------------|
| `Merlin`         | Transcript for building custom Schnorr-style ZKP statements. |
| `MultiscalarMul` | Deferred batched point-scalar multiplication, used for signature batching. |

## Cells

A cell is the in-VM form of an [output](#output): a compressed, single-use container of values locked by a [predicate](#predicate).

When an [external transaction](#external-transaction) consumes an output via `input`, the output is materialized as a cell on the stack. The cell carries:

- A **payload** — any portable collection of values (tokens, integers, strings, dicts).
- A **predicate** — a [script](#script) compressed under a public key that gates access to the payload.

Cell is encoded as a list (sequential Dict) where first element is a predicate, second is an anchor and the rest are items in the same order as they are placed on stack (topmost goes last in the list).

```
Cell = Dict {
  0: Point,  // predicate
  1: String, // anchor
  2: Value,  // list of values
  3: ...
}
```


Cells are linear: once opened (with `open` or via transaction signature), they vanish, and their payload values are released onto the stack. New cells are produced by `output`, sealing portable values from the stack under a fresh predicate. Cells are stored compressed in the Utreexo accumulator.

Cells are designed for private covenants — payments, multisignature vaults, escrow agreements. Their compressed on-chain footprint and single-use semantics make them cheap to store and natural for few-party use cases.

## Actors

An actor is a long-living, addressable entity holding persistent state and methods. Actors are stored uncompressed in the actor registry and accessed by [address](#address). They are designed for autonomous, multi-user applications such as AMM DEXes, where users can send requests concurrently.

### Structure

```
Actor = Dict {
  public#0:  Dict<Int253, String>   // callable methods; key 0 = recv
  private#1: Dict<Int253, Value>    // internal state and helpers
}
```

The reserved key `0` (`recv`) in `public` handles incoming [message sends](#message-send). Other public-method keys are dispatched by [method call](#method-call) from other actors. The `private` dict is accessible only to the actor's own scripts.

### Identity

An actor's ID is the hash of its initial state, or — for on-the-fly deployment — the constructor script itself:

```text
ActorID = enum {
  0: Hash(initial ActorState)
  1: Constructor
}
```

Each unique constructor defines a unique actor. The constructor produces the initial `ActorState`; subsequent calls mutate it.

### Dispatch

Method dispatch is by integer key into `public`. A method invoked through `send` (asynchronous, from an external or another internal transaction) must not return values — if it does, the call is treated as failure and arguments bounce. A method invoked through `call` (synchronous, within an internal transaction) may return values to the caller.

Actors hold a balance of [vbytes](#vbyte) for their persistent storage; see [Resources](#resources).

## Addresses

An address routes a payment or message to a recipient. Addresses are a tagged enum:

```text
Address = enum {
  0: Predicate                       // unlock path for a cell
  1: MessageTarget = dict {
       dst:    ActorID
       method: MethodKey
       args:   Tuple
       gas:    Int253
     }
}
```

- **Predicate addresses** identify spending paths for [cells](#cell): a transaction directs funds to a predicate, which then locks the produced cell.
- **MessageTarget addresses** route [message sends](#message-send) to a specific actor method, carrying a payload and a gas allotment.

Each [message send](#message-send) also carries an [anchor](#anchor) — a 256-bit value unique to the originating transaction — that disambiguates two otherwise-identical sends and prevents replay across blocks.

## Authorization

Authorization is how a transaction proves its right to perform a state mutation.

### Predicate compression

A cell's predicate is compressed under a public key in the style of Taproot: the on-chain form is a single 32-byte point that hides the predicate's structure. To unlock, the spender presents either:

- **A signature** against the predicate's public key, closing the spend without revealing the predicate; or
- **A reveal** — the predicate script and a Merkle path proving inclusion — followed by execution of the revealed predicate.

The signature path is the cheap common case; the reveal path is used when custom logic must run.

### TxID binding

Transaction signatures and ZK proofs bind to the [TxID](#txid) — the merkle root over the transaction's ordered [effects](#effect) list. Once signed, the transaction's effects are atomically committed: no individual effect can be added, removed, or altered without invalidating the binding.

### Caller identity

Within actor calls, the callee identifies its caller via the `callerid` opcode. Authorization between actors — who may call which method, with what arguments — is enforced by the actor's own script logic, not by the VM.

## Transactions

A transaction is the unit of state change. It comes in two forms:

- **External** — composed and signed by users, broadcast into the network. Has user-determined effects (the user knows the transaction outcome at signing time). Pays a fee.
- **Internal** — triggered by a [message send](#message-send) from an external (or another internal) transaction. Effects depend on actor state at execution time, so the outcome is not predetermined at the time the originating send was authored.

### TxID

Each transaction has a unique 32-byte identifier — the TxID — computed as the merkle root over its ordered [effects](#effect) list. Signatures, ZK proofs, and the [anchor](#anchor) all bind to TxID, which fixes all effects atomically.

### Effects

An external transaction may emit:

| Effect       | Description |
|--------------|-------------|
| Input        | Consumes an entry from [Utreexo](#utreexo), materialized as a [cell](#cell) on the stack. |
| Output       | Appends a new entry to Utreexo. |
| Send         | Schedules an [internal transaction](#internal-transaction) against an [actor](#actor). |
| Fee          | Records a transaction fee in *flames*. |
| Issuance     | Creates [tokens](#token); flavor bound to the issuing [actor](#actor) (cleartext, `issuepub`) or [predicate](#predicate) (confidential, `issuepriv`). |
| Retirement   | Destroys tokens, removing them from circulation. |
| Data         | Arbitrary binary log entry; not stored persistently. |

An internal transaction may emit:

| Effect       | Description |
|--------------|-------------|
| Receive      | Consumes the [message send](#message-send) that triggered the internal transaction. Emitted as `TxEntry::Receive(send_id)` — the originating Send's anchor bytes — so the Internal TxID merkle root commits to the triggering Send. Symmetric with `Input` for external transactions. |
| Output       | Appends a new entry to Utreexo. |
| Send         | Schedules a further internal transaction. |
| Issuance     | Creates [tokens](#token); cleartext only (`issuepub`) — no CS in internal context. |
| Retirement   | Destroys tokens. |
| Data         | Log entry. |

Internal transactions have no Input effect (Utreexo proofs are an external-context capability) and no Fee effect (gas is committed by the originating send).

### Atomic fee payment

An external transaction either commits all of its effects or none: failure produces no state change and deducts no fee. This makes external-transaction results fully deterministic — verifiers can process external transactions concurrently because each one's outcome is fixed at signing time.

### Block layout

A block contains an ordered list of external transactions. Each external transaction may emit message sends; all sends emitted in a block are executed as internal transactions after all external transactions in the block have been verified. Minters choose the order of external transactions subject to BFT consensus; users do not directly control internal-transaction order beyond the ordering of their originating external transactions.

## External transaction lifecycle

Any interaction with Flame begins with authoring, signing, and broadcasting an external transaction. The transaction is a [script](#script) that consumes outputs, performs operations on the values they carry, and emits new outputs and message sends.

A typical external transaction follows a Bitcoin-like skeleton:

1. Claim one or more unspent [outputs](#output) as inputs.
2. Unlock each input with a signature against [TxID](#txid) (or a script reveal).
3. Merge and split token values into new quantities.
4. Create new outputs — for the destination payment and for the remaining balance (the "change output").

External transactions differ from Bitcoin transactions in several structural ways:

1. **No rigid input/output layout.** The transaction is a [script](#script) of instructions (`input`, `output`, `merge`, `split`, ...) executable in any order.
2. **Encrypted values, in-VM ZK proof.** Token quantities and flavors may be encrypted; operations on them happen in zero-knowledge. The transaction carries a single Bulletproofs proof attesting that all constraints generated during script execution are satisfied. See [Confidentiality](#confidentiality).
3. **Cells, not raw outputs.** Each input is materialized as a [cell](#cell) carrying both a payload of values and a [predicate](#predicate). Outputs are produced by sealing portable stack values into a new cell under a fresh predicate.
4. **Message sends.** In addition to producing outputs, the script can emit [message sends](#message-send) that schedule internal transactions targeting [actors](#actor).

When a transaction appears in a new block, each network node verifies its validity per FlameVM rules, locates and destroys the inputs, appends the outputs, and queues any emitted sends for internal-transaction processing.

## Internal transaction lifecycle

An internal transaction is triggered by a [message send](#message-send). It runs in the internal FlameVM context against the live actor registry.

When a node processes a send, it instantiates an internal transaction uniquely identified by the send's [anchor](#anchor). The internal transaction:

1. **Receives** the send: payload, gas allotment, [vbyte](#vbyte) allotment, target actor ID, method key.
2. **Dispatches** to the target actor's `recv` method.
3. **Loads** the actor's state with `load`, mutates it, persists it with `save`.
4. **May emit** further sends, outputs, issuances, retirements, or data entries.
5. **Returns** success or failure.

The internal context permits most external-context operations except [Bulletproofs](#bulletproofs), which is bound to TxID-level results that cannot be predetermined when an internal transaction's outcome depends on live actor state.

### Send vs call

- **`send`** is asynchronous: emitted by an external or internal transaction, processed when its turn comes in the block. A send-triggered method must not return values. If a send fails, its arguments are sealed into a [cell](#cell) under the sender's [refund predicate](#refund-predicate); the gas and vbyte allotments are consumed regardless.
- **`call`** is synchronous: invoked between actors within the same internal transaction. A call may return values to its caller. If a call fails, the entire enclosing internal transaction fails.

### Re-entrancy

An [actor](#actor) cannot be entered via [`call`](#method-call) while it already has an unfinished invocation on the call stack. Attempts fail deterministically and abort the enclosing internal transaction. This applies to both direct self-calls and indirect call chains (A → B → A).

Recursion within a single method is unrestricted — the boundary is the actor, not the method. Cross-actor recursion or any mutual interaction that would require re-entry is expressed via asynchronous [`send`](#message-send), which executes in a separate internal transaction.

The rule eliminates the class of re-entrancy hazards by construction: no concurrent write sessions per actor, no broken mid-call invariants, no mid-transition reads. Validators enforce it with a single check — is the target actor's ID present on the current call stack? — at the cost of disallowing some patterns (synchronous callbacks, cross-actor mutual recursion) that must be re-expressed via explicit argument passing or async sends.

## Confidentiality

Confidentiality lets [token](#token) quantities, flavors, and other secret values flow through a transaction without being revealed on chain.

### Bulletproofs

External transactions carry a single Bulletproofs R1CS proof at the end of the transaction. The proof attests that the conjunction of all [constraints](#constraint) generated by the script during execution is satisfied. Constraints accumulate through the constraint type hierarchy:

- A confidential token's quantity is bound to a `Variable` tied to a Pedersen commitment.
- Variables combine into `Expression`s (linear combinations).
- Expressions feed into `Constraint`s (logical predicates).
- The transaction's final proof asserts the conjunction of all Constraints.

Because the proof binds to the outcome of the transaction, Bulletproofs are unavailable in internal transactions — an internal result depends on actor state that the prover cannot fix in advance.

### Schnorr ZKP

Discrete-log-based ZKP protocols can be evaluated in both external and internal transactions via the `Merlin` transcript and `MultiscalarMul` types. Users construct custom proofs of knowledge bound to arbitrary data.

### Privacy boundaries

An on-chain observer sees different information depending on the transaction type.

**External transactions reveal:**

- The transaction [script](#script). Taproot-style predicates remain compressed unless revealed.
- Pedersen commitments to encrypted quantities and flavors.
- The Bulletproofs proof.
- Schnorr signatures over TxID and any custom transcripts.

**External transactions hide:**

- The unexecuted scripts in [cells](#cell) (via Taproot).
- Variable assignments inside Bulletproofs.
- The plaintext quantities and flavors of encrypted tokens.

**Internal transactions reveal:**

- Target actor IDs, method keys, and arguments of every send and call.
- All mutations to actor state.
- Outputs emitted to Utreexo.

Internal transactions have no native confidentiality story: they operate on uncompressed actor state in the clear. Custom Schnorr ZKPs may be used by an actor to attest to facts about secret data.

## Concurrency

External transactions are verifiable in parallel because each one is bound only to its own set of consumed UTXOs. Two external transactions in the same block cannot observe each other's effects.

Internal transactions are verified serially within a block becuase they share the global state of all actors: an actor's state mutation in one internal transaction is visible to the next. The serial order is fixed by the order minters chose for the originating external transactions.

## Resources

FlameVM execution is metered by two resources: **gas** (compute cost) and **virtual bytes** (persistent storage cost). Both are committed up front via the external-transaction fee and are not refunded on failure.

### Gas

Gas is the unit of [script](#script) execution cost. The network enforces per-block limits on parallel and serial gas separately (see [Block limits](#block-limits)); the sum of transactions in a [block](#block) may not exceed either.

[External transactions](#external-transaction) pay for gas in *flames* via the transaction fee. A single fee covers two categories of gas:

1. Gas consumed by the external transaction's own [script](#script) execution.
2. Gas allotted to each [message send](#message-send) the external transaction emits.

Each external transaction is granted a baseline *gas credit* — an amount sufficient to execute a typical transaction without message sends. Additional gas required by message sends is purchased explicitly above the credit.

[Internal transactions](#internal-transaction) cannot request or hold gas beyond what was allotted at their originating [message send](#message-send). The full allotment is committed up front; gas left over at the end of the internal transaction is discarded, not refunded.

Within an internal transaction, a [method call](#method-call) may pass a gas limit to its callee; by default the callee inherits the caller's remaining gas. Unlike a message send, gas left over after a method call returns to the caller.

Transactions paying higher fees per unit of gas are prioritized by minters during block construction.

The opcodes `gas` (remaining gas) and `gaslimit` (current call's cap) are available during execution. The opcode `fee` records a fee, emitting a debt [`WideToken`](#widetoken) that must be balanced against tokens consumed in the transaction.

### Gas limits

The block-level gas budget is partitioned into two independent caps:

- **Parallel pool** (`B_par`): the sum of gas consumed by all external transactions in the block.
- **Serial pool** (`B_ser`): the sum of gas allotments forwarded by all [send](#message-send) effects in the block's external transactions.

The ratio `B_par / B_ser` is initially set to **4**, reflecting the wall-clock cost differential between parallel and serial execution on a reference validator with 4 effective verification threads. Both caps are protocol parameters published separately from the opcode table.

Sub-sends emitted by [internal transactions](#internal-transaction) consume their originator's already-allotted budget and do not contribute additional charges to the serial pool; `B_ser` accounting sums only originating allotments from external sends.

Because the serial pool is `1/4` the size of the parallel pool, serial-gas usage is the scarcer resource by construction. Under congestion, minters prioritize transactions paying higher fees per unit of serial gas, and the scarcity premium emerges from the market without requiring a per-send multiplier in the gas cost model.

Both caps may be tightened by supermajority soft fork. Supermajority vote may also raise either cap explicitly, subject to the same governance threshold as the [vbyte introduction rate](#storage).

### Storage

[Actor](#actor) state is metered in *virtual bytes* (vbytes). The vbyte metric is defined at the protocol level so all implementations agree on storage cost regardless of on-disk representation.

Each actor holds a balance of vbytes representing prepaid storage. At the end of every block, after all [transactions](#transaction) are processed, each actor's balance is decremented by the number of vbytes it occupies.

The protocol introduces 5000 new vbytes per block, on top of any vbytes recycled from cleared actors. Network supermajority can adjust the per-block supply by up to 2× in either direction. Recycled vbytes rejoin the global pool after a 100-block maturity. This makes hoarding storage costly: an actor that holds unused vbytes continuously bleeds them with each block.

Vbytes are purchased by [external transactions](#external-transaction) through fees and deposited onto actors via [message sends](#message-send). A message send attaches a vbyte allotment that is credited to the destination actor; an empty message send (no method, no arguments) is the dedicated form for transferring vbytes alone and is guaranteed not to fail. Actors may also transfer vbytes to each other through the allotment carried by a [method call](#method-call).

Instruction `bytes` returns the actor's remaining persistent vbyte balance. `newbytes` returns the vbytes received during the current call. `memlimit` returns the transient memory cap for the current call.

#### Depletion and grace period

When an actor's vbyte balance reaches zero, it does not vanish immediately. Each actor records the block in which it was most recently activated (initial deployment, or a top-up from zero). When the balance is depleted, the actor enters a *frozen* state: it stops accepting calls, but its state is preserved.

The frozen state lasts for a grace period equal to one block of grace per four blocks of prior activity, capped at six months of blocks. During the grace period, any [message send](#message-send) that delivers vbytes restores the actor and clears the frozen flag. If the grace period elapses without a top-up, the actor's state is cleared and its vbytes rejoin the pool (subject to the 100-block maturity).

This bounds the freeloading risk of short-lived actors — which earn little grace — while giving operators of long-lived actors months of headroom to notice a depleted balance and refill it.

#### Transient memory

In addition to its persistent state, an [actor](#actor) may use *transient memory* during a call — scratch space released when the call ends. The cap is fixed at **4× the actor's current persistent state size in vbytes**, returned by the `memlimit` opcode.

An actor occupying N vbytes can use up to 4N vbytes of working memory: enough headroom to `load` its state, mutate it in place, and `save` a new version without exceeding the cap. Allocations that would push live memory past the cap fail the call.

## Transaction lifecycle & API

#### Step 1: building an external transaction.

User prepares a program. Program is a `Vec<Instruction>` bearing witness data, including cells to be spent via `input`, blinding factors for the encrypted tokens, taproot branches etc.

```
let prog = Program::build().input().mix().output()...;
let bp_gens = BulletproofGens::new(256, 1);
let unsigned_tx = prog.build_tx(header, limits, &bp_gens)?; // UnsignedTx
```

#### Step 2: signing transaction

Unsigned transaction provides `signing_instructions` for each key that was triggering the transaction-scoped signature.
User produces the necessary signature and transforms the unsigned transaction into a signed one of type `ExternalTx`.

```
let signing_instructions = unsigned_tx.signing_instructions();
...
let sig = ...;
let signed_tx = unsigned_tx.sign(sig); // ExternalTx
```

#### Step 3: transaction broadcast

User broadcasts transaction to the network, Flame nodes verify it before inclusion in the mempool:

```
let bp_gens = BulletproofGens::new(256, 1);
let txlog = signed_tx.verify(limits, &bp_gens)?; // TxLog
```


#### Step 4: internal transaction processing

If `txlog` contains "send" entries, those are processed by minters during block creation and verified by the nodes receiving a block from the minters.
Each "send" entry produces `InternalTx` via validation process.

```
let tx = send.execute_tx(limits, &env)?; // InternalTx
env.apply_changes(tx.log.iter());
```

`execute_tx` takes read-only access to the current blockchain state, keeps track of modifications internally and emits the net changes in the form of TxLog.
Before the next internal transaction is executed, txlog must be applied to the state.

Any two transactions may be executed concurrently: if their txlog effects do not overlap, the effects could be applied in any order.

#### Integration notes

All the high-level APIs (`Program::build_tx`, `UnsignedTx::sign`, `SignedTx::verify` and `Send::execute_tx`)
internally allocate VM instance with necessary parameters and keep track of the state that's distilled into TxLog entries.

Given every txlog (external or internal), the node applies txlog changes to its state before processing the next transaction or the next block.


## Examples

The two sketches below are illustrative pseudocode, not literal scripts. They show the conceptual flow of values and effects; consult the FlameVM specification for exact opcode signatures and stack effects.

### Confidential payment

Alice spends a [cell](#cell) holding 250 *flames* to pay Bob 100 flames and keep the 150-flame change confidential.

1. **Input.** Alice's transaction claims her UTXO and materializes her cell on the stack (`input`). Signature verification against [TxID](#txid) is deferred (`signtx`).
2. **Split.** The 250-flame token is split into a 100-flame token (for Bob) and a 150-flame token (change) via `split`.
3. **Outputs.** Each token is sealed under its destination [predicate](#predicate) (`output`): one for Bob, one for Alice's change address.
4. **Fee.** A small fee in flames is recorded via `fee`, balanced against the inputs.

The transaction's [Bulletproofs](#bulletproofs) proof attests that the split balanced and that the change-output token's quantity is non-negative. An on-chain observer sees the input and outputs as commitments but learns no cleartext amounts.

### AMM swap

A user swaps 10 *X-tokens* for *Y-tokens* at an AMM [actor](#actor).

**External transaction (composed by the user):**

1. Claim a UTXO containing X-tokens; split off 10 X-tokens to send.
2. Emit a [message send](#message-send) to the AMM's `swap` method, carrying the 10 X-tokens as payload, a gas allotment, a [vbyte](#vbyte) allotment of zero, and a [refund predicate](#refund-predicate).

**Internal transaction (executed later; AMM's `recv` dispatches to `swap`):**

1. `load` the AMM's [state](#actor).
2. Compute the output quantity from the constant-product invariant; update reserves.
3. Emit an [output](#output) carrying Y-tokens under the user's destination predicate.
4. `save` the updated state.

If the swap fails — for example, slippage exceeds tolerance, or reserves are insufficient — the AMM script triggers explicit failure. The 10 X-tokens bounce back as a cell under the user's [refund predicate](#refund-predicate). The gas and vbyte allotments are consumed regardless.

## Glossary

Terms are listed in dependency order: each entry uses concepts introduced above it.

### Transaction
A unit of state change in the Flame network. Comes in two forms: [external](#external-transaction) and [internal](#internal-transaction).

### Block
Ordered batch of [transactions](#transaction) appended to the chain by consensus.

### FlameVM
Stack machine that verifies a [transaction](#transaction) by executing its [script](#script). Each transaction instantiates its own VM.

### Script
Sequence of [FlameVM](#flamevm) instructions; the executable body of a [transaction](#transaction).

### Value
Typed item on the [FlameVM](#flamevm) stack. May be linear, copyable, portable, or non-portable.

### Portable
Property of a [value](#value): may exist in persistent blockchain state outside [FlameVM](#flamevm) execution.

### Copyable
Property of a [value](#value): may be duplicated during [FlameVM](#flamevm) execution.

### Effect
A single state-mutating action emitted by a [transaction](#transaction). External effects: input, output, send, fee, issuance, retirement, data. Internal effects: receive, output, send, issuance, retirement, data.

### TxID
Unique 32-byte identifier of a [transaction](#transaction); merkle root over its ordered [effects](#effect) list. Signatures and ZK proofs bind to TxID, fixing all effects atomically.

### Token
Linear [value](#value) representing an asset instance. Bearer: cannot be duplicated, created, or destroyed except by explicit operations. Three concrete types: `Token`, [`ClearToken`](#cleartoken), [`WideToken`](#widetoken).

### Quantity
Number of atomic units in a [token](#token). May be encrypted.

### Flavor
Asset-kind identifier of a [token](#token), derived from the issuing [actor](#actor)'s id (cleartext path) or [predicate](#predicate)'s point (confidential path), with a sub-flavor `tag`. May be encrypted. See [Issuance and retirement](#issuance-and-retirement).

### ClearToken
[Token](#token) with unencrypted [quantity](#quantity) and [flavor](#flavor). May be negative; portable only when non-negative.

### WideToken
[Token](#token) with encrypted [quantity](#quantity) that may be negative. Non-portable.

### Constraint
Logical predicate over [tokens](#token) and other secret values, accumulated during script execution and proven satisfied by [Bulletproofs](#bulletproofs) at the end of an [external transaction](#external-transaction).

### Predicate
[Script](#script) that authorizes access to a locked entity. Compressed under a public key per a Taproot variant.

### Output
Element of persistent blockchain state created and consumed by [external transactions](#external-transaction). Encodes a [predicate](#predicate) and a payload of [values](#value); stored in [Utreexo](#utreexo).

### Cell
In-[FlameVM](#flamevm) form of an [output](#output): a compressed, single-use container of [values](#value) locked by a [predicate](#predicate). Destroyed when accessed.

### Actor
Long-living, addressable entity holding persistent state and [scripts](#script). Stored uncompressed. Receives [message sends](#message-send) and [method calls](#method-call); supports concurrent multi-user interaction.

### Issuance
[Effect](#effect) that creates new [tokens](#token). Flavor binds to the issuing [actor](#actor) (cleartext path, `issuepub`) or [predicate](#predicate) (confidential path, `issuepriv`). See [Issuance and retirement](#issuance-and-retirement).

### Retirement
[Effect](#effect) that destroys [tokens](#token), removing them from circulation.

### Address
Tagged enum routing a payment or message: either a [predicate](#predicate) (unlock path for a cell) or a message target carrying [actor](#actor) ID, method key, arguments, and gas allotment.

### Anchor
Unique 256-bit value derived from the originating [transaction](#transaction); attached to each [message send](#message-send) to disambiguate otherwise-identical invocations and prevent replay.

### External transaction
[Transaction](#transaction) with user-determined results. Runs in external context with access to [Utreexo](#utreexo) and [Bulletproofs](#bulletproofs). Pays a fee atomically.

### Message send
[Effect](#effect) of an [external transaction](#external-transaction) (or another internal transaction) that schedules an [internal transaction](#internal-transaction) targeting an [actor](#actor) at a given [address](#address). Carries arguments, [gas](#gas), [vbytes](#vbyte), and a [refund predicate](#refund-predicate).

### Internal transaction
[Transaction](#transaction) triggered by a [message send](#message-send). Operates on uncompressed [actor](#actor) state. Cannot return values to the originator and cannot use [Bulletproofs](#bulletproofs).

### Method call
Synchronous invocation between [actors](#actor) within an [internal transaction](#internal-transaction). Returns values to the caller; failure aborts the enclosing internal transaction.

### Refund predicate
[Predicate](#predicate) specified by the sender of a [message send](#message-send); seals the send's arguments into a [cell](#cell) if the send fails.

### Gas
Unit of [script](#script) execution cost. Allotted via the [external transaction](#external-transaction) fee, both for the transaction's own [script](#script) and for each [message send](#message-send) it emits. Not refunded on failure.

### Vbyte
Virtual byte: protocol-level unit of persistent [actor](#actor) state allocation. Allotted by [message sends](#message-send), decremented per [block](#block), and reclaimed (after a 100-block maturity) when an [actor](#actor) is cleared. Not refunded on failure.

### Utreexo
Compressed accumulator of the unspent [output](#output) set, accessed by [external transactions](#external-transaction).

### Bulletproofs
Zero-knowledge R1CS proof system used by [external transactions](#external-transaction) for confidential operations on [tokens](#token) and custom constraints. Not available in [internal transactions](#internal-transaction).

### Schnorr ZKP
Discrete-log-based zero-knowledge proof protocol available in both [external](#external-transaction) and [internal](#internal-transaction) transactions via `Merlin` transcripts.
