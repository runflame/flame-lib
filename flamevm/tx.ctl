// External transaction wire layout. Application code validates the signature,
// R1CS proof, program graph, and canonical level-one pruned log reference.
struct Tx {
    body: ^TxBody,
    signature: [U8; 64],
    r1cs_length: U16,
    r1cs_proof: [U8; r1cs_length],
}

struct TxBody {
    header: TxHeader,
    program: ^Cell,
    log: ^TxLog,
}

struct TxHeader {
    version: U32,
    locktime: U32,
}

// External logs always have a Header entry, so the entries Trie is nonempty.
// The transmitted reference is pruned and is never loaded as this type.
struct TxLog {
    entry_count: U64,
    entries: ^Cell,
}
