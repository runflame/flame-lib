pub const MINT_PROOF_MAGIC: [u8; 3] = *b"FLM";

/// Payload length for a mint proof that does not register a validator.
pub const MINT_PROOF_DATA_LEN: usize = 68;

/// Payload length for a mint proof that registers a validator.
pub const PARTICIPATING_MINT_PROOF_DATA_LEN: usize = MINT_PROOF_DATA_LEN + 32;

pub(in crate::mint_proofs) const OP_RETURN: u8 = 0x6a;
pub(in crate::mint_proofs) const OP_PUSHDATA1: u8 = 0x4c;
