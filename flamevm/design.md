# FlameVM design

FlameVM implements most of transaction verifications rules in the Flame blockchain in a form of a Forth-like stack machine.

FlameVM is used in two different contexts: external transactions and internal transactions. *External transactions* have user-defined results, zero-knowledge proof for confidential transfers and compute, and have access to Utreexo storage: scalable compressed set of *unspent transaction outputs*. *Internal transactions* operate on uncompressed storage and invoke multiplayer smart contracts that react to user-defined messages without pre-determined results.

Values in the FlameVM program can be of various data types, including linear types (tokens, zero-knowledge constraints etc.) Values can be _portable_ and _non-portable_, _copyable_ and _non-copyable_. Portable values can exist in the long-term blockhain state outside of VM execution. Copyable types can be duplicated during VM execution.

Successful VM execution equals to successful transaction verification. Therefore, VM execution encodes both built-in network rules, as well as enables authors to create custom rules within their contracts.

## Tokens

The most important value type in Flame is a linear type _token_: an instance of an asset with _quantity_ (number of atomic units) and _flavor_ (unique identifier of the asset kind, or its issuer). The built-in currency (also called _flame_) has the same type as any custom user-defined token. Custom tokens can be user-issued or programmatically issued. 

Both quantity and flavor of any token can be encrypted and transferred confidentially.

All tokens are bearer instruments, they cannot be duplicated, created out of thin air or accidentally destroyed. Tokens are suitable for representing both financial instruments and capabilities in complex systems (such as voting rights, access rights etc.)

FlameVM has three kinds of token types: `Token` (regular encrypted token), `ClearToken` (unencrypted quantity and flavor), and `WideToken` (encrypted token with possibly negative quantity).

## External transaction lifecycle

Any interaction with Flame begins with authoring, signing and broadcasting an external transaction. A typical external transaction is similar to Bitcoin transaction: 
1. transaction claims one or more _unspent transaction outputs_,
2. unlocks the assets within the output with a transaction signature,
3. merges and splits values into new quantities, 
4. finally, creates new outputs — for the destination payment and for the remaining balance (also known as “change output”).

When a transaction appears in a new block, each network node verifies the transaction validity per FlameVM rules, locates and destroys outputs that are consumed and creates new outputs in its permanent storage.

External transaction in Flame differs from Bitcoin transaction format in the following ways:
1. There is no rigid layout for inputs and outputs. In Flame, the transaction is mostly a string of instructions (a “program”) that contains instructions `input`, `output`, `merge`, `split` and so on in any order. 
2. Asset quantites and types can be encrypted and operations on them (merges and splits) can be done in zero-knowledge, therefore enabling confidential transfers and keeping account balances encrypted. External transaction contains a separate string of ZK proof that proves the correctness of the operations and protects the network against counterfeiting.
3. Each output, when instantiated inside the VM, is called an object that protects its payload. The object payload can be any collection of values: tokens, numbers, strings, dictionaries. The payload is protected by a predicate program (compressed under a public key) according to a variant of Taproot scheme.
4. In addition to creating idle outputs, external transaction can also “send messages” thus triggering creation of internal transactions to interact with _contracts_.

External transaction fee is paid atomically: if the transaction fails, no fee is deducted. Therefore the external transaction results are fully deterministic. Transaction either succeeds in its entirety, or does not exist on the blockchain.

## Objects and contracts

In Flame the notion of “smart contracts” is implemented with two entities: _objects_ and _contracts_. Both can store arbitrary data protected by user-defined program, therefore implementing smart contract mechanics. 

Objects are stored in a compressed form, designed for serial access via _predicates_, and destroyed after each use. Objects are designed for private covenants such as plain accounts, multisignature vaults, private escrow contracts etc.

Contracts are long-living programs stored in uncompressed form that enable concurrent multi-user interaction. Contracts are designed for autonomous applications such as AMM DEXes, where users can send requests concurrently.

Contracts allow more complex programs with unpredictable results due to concurrency, that in turn enable contracts to serve unlimited number of users without interactive coordination.

## Internal transactions lifecycle

Access to _contracts_ is done via _internal transactions_ that are triggered by _message sends_ from an external transaction. This separation exists in order to securely commit limited resources towards program execution cost and contract storage. An external transaction allocates _gas_ and _storage units_ for a message send, gets included in the block, and then all message sends in a block are executed as separate internal transactions Users do not have direct control over the order of message sends: the consensus of Flame minters decides the order of external and therefore internal transactions.

_Message send_ is a distinct effect of the external transaction, similar to an output. But unlike an output, the _send_ contains an address of a contract, method name, arguments (including asset values), gas and storage allotments. The external transaction therefore cannot receive results of the execution and the method call should not return any values.

If message send fails, all its arguments are compressed into an _object_ under the “refund predicate” specified by the user. Gas and storage allotments are not refunded.

When a node processes a _send_, it instantiates an internal transaction uniquely identified by the send arguments and executes it via an instance of FlameVM launched in “internal context”. In such context, FlameVM has access to the global set of deployed contracts and allows synchronous _calls_ between them. Unlike _sends_, _calls_ can return results to the callers and permit easy model for composing distinct contracts in a single system. All calls are executed within a single transaction.

Internal context permits most of the operations from external context, apart from the Bulletproofs zero-knowlegde proof because it is tied to the results of the transaction that are not known to the prover ahead of time. For the same reason, external context does not permit access to the global contracts’ state via _calls_ since their results cannot be determined during transaction composition.

## Cryptography

FlameVM cryptographic operations can be divided in three categories:

Primitives: common hash functions such as SHA2, token encryption and decryption, operations for building custom schnorr ZKP statements.

Authorization: opening objects with transaction signature and with a signature over a custom message.

Confidentiality: Bulletproofs R1CS proof system for external transactions enables confidential merge and split operation for tokens, and also custom constraints over custom parameters inside _objects_. Bulletproofs are available in external transactions only due to being bound to exact results of the transaction. Confidential operations can also be implemented with user-defined schnorr ZKP protocols that are available in both internal and external transactions.

## Gas

TBD.

## Storage

TBD.





