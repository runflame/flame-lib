# FlameVM design

Flame Virtual Machine implements transaction verifications rules in the Flame network. FlameVM is a stack machine instantiated for each transaction. It receives a transaction script and blockchain state as an input and emits updates to the blockchain state as a result of the execution.

Successful VM execution implies successful transaction verification. Therefore, VM execution encodes both built-in network rules, as well as enables authors to create custom rules within their applications.

## External and internal transactions

FlameVM is used in two different contexts: external transactions and internal transactions. 

*External transactions* have user-defined results, zero-knowledge proof for confidential transfers and compute, and have access to Utreexo storage: scalable compressed set of *unspent transaction outputs*. 

*Internal transactions* operate on uncompressed storage and invoke multiplayer smart contracts that react to user-defined messages without pre-determined results.

External transactions contain pre-determined results and can be verified concurrently. Internal transactions are verified serially and results of one internal transaction may affect the execution of the next internal transaction.

## Value types

Values in the FlameVM script can be of various data types, including linear types (tokens, zero-knowledge constraints etc.) Values can be _portable_ and _non-portable_, _copyable_ and _non-copyable_. Portable values can exist in the long-term blockhain state outside of VM execution. Copyable types can be duplicated during VM execution.

## Tokens

The most important value type in Flame is a linear type _token_: an instance of an asset with _quantity_ (number of atomic units) and _flavor_ (unique identifier of the asset kind, or its issuer). The built-in currency (also called _flame_) has the same type as any custom user-defined token. Custom tokens can be user-issued or programmatically issued. 

Both quantity and flavor of any token can be encrypted and transferred confidentially.

All tokens are bearer instruments, they cannot be duplicated, created out of thin air or accidentally destroyed. Tokens are suitable for representing both financial instruments and capabilities in complex systems (such as voting rights, access rights etc.)

FlameVM has three kinds of token types: `Token` (regular encrypted token), `ClearToken` (unencrypted quantity and flavor), and `WideToken` (encrypted token with possibly negative quantity). `Token` and `ClearToken` are portable types — they can be stored 

## External transaction lifecycle

Any interaction with Flame begins with authoring, signing and broadcasting an external transaction. A typical external transaction is similar to Bitcoin transaction: 
1. transaction claims one or more _unspent transaction outputs_,
2. unlocks the assets within the output with a transaction signature,
3. merges and splits values into new quantities, 
4. finally, creates new outputs — for the destination payment and for the remaining balance (also known as “change output”).

When a transaction appears in a new block, each network node verifies the transaction validity per FlameVM rules, locates and destroys outputs that are consumed and creates new outputs in its permanent storage.

External transaction in Flame differs from Bitcoin transaction format in the following ways:
1. There is no rigid layout for inputs and outputs. In Flame, the transaction is mostly a string of instructions (a “script”) that contains instructions `input`, `output`, `merge`, `split` and so on in any order. 
2. Asset quantites and types can be encrypted and operations on them (merges and splits) can be done in zero-knowledge, therefore enabling confidential transfers and keeping account balances encrypted. External transaction contains a separate string of ZK proof that proves the correctness of the operations and protects the network against counterfeiting.
3. Each output, when instantiated inside the VM, is called a cell that protects its payload. The cell payload can be any collection of values: tokens, numbers, strings, dictionaries. The payload is protected by a predicate script (compressed under a public key) according to a variant of Taproot scheme.
4. In addition to creating idle outputs, external transaction can also “send messages” thus triggering creation of internal transactions to interact with _actors_.

External transaction fee is paid atomically: if the transaction fails, no fee is deducted. Therefore the external transaction results are fully deterministic. Transaction either succeeds in its entirety, or does not exist on the blockchain.

## Cells and actors

In Flame the notion of “smart contracts” is implemented with two entities: _cells_ and _actors_. Both can store arbitrary data protected by user-defined script, therefore implementing smart contract mechanics. 

Cells are stored in a compressed form, designed for serial access via _predicates_, and destroyed after each use. Cells are designed for private covenants such as plain accounts, multisignature vaults, private escrow agreements etc.

Actors are long-living entities stored in uncompressed form that enable concurrent multi-user interaction. Actors are designed for autonomous applications such as AMM DEXes, where users can send requests concurrently.

Actors allow more complex scripts with unpredictable results due to concurrency, that in turn enable actors to serve unlimited number of users without interactive coordination.

## Internal transactions lifecycle

Access to _actors_ is done via _internal transactions_, that are triggered by _message sends_ from an external transaction. This separation exists in order to securely commit limited resources towards script execution cost and actor storage. An external transaction allocates _gas_ and _storage units_ for a message send, gets included in the block, and then all message sends in a block are executed as separate internal transactions Users do not have direct control over the order of message sends: the consensus of Flame minters decides the order of external and therefore internal transactions.

_Message send_ is a distinct effect of the external transaction, similar to an output. But unlike an output, the _send_ contains an address of an actor, method name, arguments (including asset values), gas and storage allotments. The external transaction therefore cannot receive results of the execution and the method call should not return any values.

If message send fails, all its arguments are compressed into a _cell_ under the “refund predicate” specified by the user. Gas and storage allotments are not refunded.

When a node processes a _send_, it instantiates an internal transaction uniquely identified by the send arguments and executes it via an instance of FlameVM launched in “internal context”. In such context, FlameVM has access to the global set of deployed actors and allows synchronous _calls_ between them. Unlike _sends_, _calls_ can return results to the callers and permit easy model for composing distinct actors in a single system. All calls are executed within a single transaction.

Internal context permits most of the operations from external context, apart from the Bulletproofs zero-knowlegde proof because it is tied to the results of the transaction that are not known to the prover ahead of time. For the same reason, external context does not permit access to the global actors’ state via _calls_ since their results cannot be determined during transaction composition.

## Cryptography

FlameVM cryptographic operations can be divided in three categories:

Primitives: common hash functions such as SHA2, token encryption and decryption, operations for building custom schnorr ZKP statements.

Authorization: opening cells with transaction signature and with a signature over a custom message.

Confidentiality: Bulletproofs R1CS proof system for external transactions enables confidential merge and split operation for tokens, and also custom constraints over custom parameters inside _cells_. Bulletproofs are available in external transactions only due to being bound to exact results of the transaction. Confidential operations can also be implemented with user-defined schnorr ZKP protocols that are available in both internal and external transactions.

## Gas

TBD.

## Storage

TBD.

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

### Token
Linear [value](#value) representing an asset instance. Bearer: cannot be duplicated, created, or destroyed except by explicit operations. Three concrete types: `Token`, [`ClearToken`](#cleartoken), [`WideToken`](#widetoken).

### Quantity
Number of atomic units in a [token](#token). May be encrypted.

### Flavor
Asset-kind identifier of a [token](#token). May be encrypted.

### ClearToken
[Token](#token) with unencrypted [quantity](#quantity) and [flavor](#flavor).

### WideToken
[Token](#token) with encrypted [quantity](#quantity) that may be negative.

### Predicate
[Script](#script) that authorizes access to a locked entity. Compressed under a public key per a Taproot variant.

### Output
Element of persistent blockchain state created and consumed by [external transactions](#external-transaction). Encodes a [predicate](#predicate) and a payload of [values](#value).

### Cell
In-[FlameVM](#flamevm) form of an [output](#output): a compressed, single-use container of [values](#value) locked by a [predicate](#predicate). Destroyed when accessed.

### Actor
Long-living, addressable entity holding persistent state and [script](#script). Stored uncompressed. Receives [message sends](#message-send) and [method calls](#method-call); supports concurrent multi-user interaction.

### Address
Identifier of an [actor](#actor); the target of [message sends](#message-send) and [method calls](#method-call).

### External transaction
[Transaction](#transaction) with user-determined results. Runs in external context with access to [Utreexo](#utreexo) and [Bulletproofs](#bulletproofs). Pays a fee atomically.

### Message send
Effect of an [external transaction](#external-transaction) that schedules an [internal transaction](#internal-transaction) targeting an [actor](#actor) at a given [address](#address). Carries arguments, [gas](#gas), [storage units](#storage-unit), and a [refund predicate](#refund-predicate).

### Internal transaction
[Transaction](#transaction) triggered by a [message send](#message-send). Operates on uncompressed [actor](#actor) state. Cannot return values to the originator and cannot use [Bulletproofs](#bulletproofs).

### Method call
Synchronous invocation between [actors](#actor) within an [internal transaction](#internal-transaction). Returns values to the caller.

### Refund predicate
[Predicate](#predicate) specified by the sender of a [message send](#message-send); seals the send's arguments into a [cell](#cell) if the send fails.

### Gas
Unit of [script](#script) execution cost, allotted to a [message send](#message-send) by the originating [external transaction](#external-transaction). Not refunded on failure.

### Storage unit
Unit of persistent state allocation cost, allotted alongside [gas](#gas) for [actor](#actor) state changes. Not refunded on failure.

### Utreexo
Compressed accumulator of the unspent [output](#output) set, accessed by [external transactions](#external-transaction).

### Bulletproofs
Zero-knowledge R1CS proof system used by [external transactions](#external-transaction) for confidential operations on [tokens](#token) and custom constraints. Not available in [internal transactions](#internal-transaction).

### Schnorr ZKP
Discrete-log-based zero-knowledge proof protocol available in both [external](#external-transaction) and [internal](#internal-transaction) transactions.
