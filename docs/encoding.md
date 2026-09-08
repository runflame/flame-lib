# Data encoding

## Transactions & blocks

Transaction (Tx) is encoded as part of Bag of Cells (BoC). That BoC is available in the context of all VM instantiations for the external and internal transactions caused by messages be external tx. Signature, R1CS proof, contract data, predicate taproot branches and actor decompression are accessed through that BoC.

* First cell contains TxHeader fields in payload. Child refs are as follows:
  * First child ref: script encoded in snake encoding
  * Second child cell: signature.
  * Third child cell: r1cs proof in payload.
  * Fourth child cell: TxLog (but the log itself is pruned from BoC, so only its cell id remains which is also TxID).
* TxID is computed as txlog, as always, so signature and r1cs proof are not bound to the cell id.
* Other cells contain other data referenced from the script (see below).
* The whole Bag of Cells is committed to the block.

UnsignedTx can be encoded in the same manner, but with child references missing (signature, r1cs).

TxLog is a Cell Trie of TxEntries, where each entry is encoded as a separate cell.

Question: 8K limit makes merkleization not so handy, and also if we do not commit TxID into a block, it's not possible to merkleize the effects of the transaction. Maybe we don't need to merkleize the raw effects, but actual effect on the state? Such as utxos and actor states? Retirement and data logging might be useful to commit though.

Answer: we can commit the txid as a third cell reference, but not transfer the cell itself - and read it from the results.
Then, the txlog can be serialized as a Trie: where each leaf has 1-byte txentry kind prefix and then flat serialized data.

## Data/value types

Primitive types are encoded directly without tags. Tag will be used when encoding flamevm::Value sum-type.
* Scalars are encoded as 32-byte sequences.
* Points are encoded as 32-byte sequences.
* Strings are deprecated: instead we introduce direct work with Cells. Snake strings are not 
* Dict: a Trie-based encoding of leaf Values. Root of the Trie is encoded as a cell.
* Token: encoded as 64-byte pair of two pedersen commitments.
* WideToken: not encoded (non-portable)
* ClearToken: 64-byte pair of scalars.
* Contract: not portable/encodable as a VM type, but in "output" form it is encoded as a Cell - see below a separate section.
* Merlin, Variable, Expression, Constraint, MultiscalarMul: non-portable, not encoded

## Contract encoding

UTXOs are encoded as a separate Cell, where Cell ID is the UTXO ID.

Contract is a cell with:
* first 32 bytes encode predicate (ristretto point)
* second 32 bytes encode anchor
* the rest is the value encoding. E.g. token is encoded immediately, but Dict is encoded in the first child cell (as a root of a trie).

## Actor encoding

* Actor storage is a single arbitrary Value (typically a Dict, but can be any portable Value)
* TBD: snake-encoded code, single value storage, list of purchased storage leases.
* When the actor is pruned, only its cell ID is stored in the registry and the entire Actor state can be automatically resurrected from BoC when it is being accessed.