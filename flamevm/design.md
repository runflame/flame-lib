# FlameVM design

FlameVM implements most of transaction verifications rules in the Flame blockchain in a form of a Forth-like stack machine.

FlameVM is used in two different contexts: external transactions and internal transactions. *External transactions* have user-defined results, zero-knowledge proof for confidential transfers and compute, and have access to Utreexo storage: scalable compressed set of *unspent transaction outputs*. *Internal transactions* operate on uncompressed storage and invoke multiplayer smart contracts that react to user-defined messages without pre-determined results.

Values in the FlameVM program can be of various data types, including linear types (tokens, zero-knowledge constraints etc.) Values can be _portable_ and _non-portable_, _copyable_ and _non-copyable_. Portable values can exist in the long-term blockhain state outside of VM execution. Copyable types can be duplicated during VM execution.

Successful VM execution equals to successful transaction verification. Therefore, VM execution encodes both built-in network rules, as well as enables authors to create custom rules within their contracts.

## External transaction lifecycle

Any interaction with Flame begins with authoring, signing and broadcasting an external transaction. A typical external transaction is similar to Bitcoin transaction: 
1. transaction claims one or more _unspent transaction outputs_,
2. unlocks the assets within the output with a transaction signature,
3. merges and splits values into new quantities, 
4. finally, creates new outputs — for the destination payment and for the remaining balance (also known as “change output”).

When a transaction appears in a new block, each network node verifies the transaction validity per FlameVM rules, locates and destroys outputs that are consumed and creates new outputs in its permanent storage.

External transaction in Flame differs from Bitcoin transaction format in the following ways:
1. There is no rigid layout for inputs and outputs. In Flame, the transaction is mostly a string of instructions (a “program”) that contains instructions `input`, `output`, `merge`, `split` and so on in any order. 
2. Asset quantites and types can be encrypted and operations on them (merges and splits) can be done in zero-knowledge. External transaction contains a separate string of ZK proof that proves the correctness of the operations and protects the network against counterfeiting.
3. Each output, when instantiated inside the VM, is called an object that protects its payload. The object payload can be any collection of values: tokens, numbers, strings, dictionaries. The payload is protected by a predicate program or a public key similar to a Taproot scheme in Bitcoin.
4. In addition to creating idle outputs, external transaction can also “send messages” thus triggering creation of internal transactions to interact with _contracts_.

## Internal transactions lifecycle





