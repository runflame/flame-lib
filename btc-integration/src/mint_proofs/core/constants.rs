pub const MINT_PROOF_MAGIC: [u8; 3] = *b"FLM";

pub const MINT_PROOF_DATA_LEN: usize = 37;

pub(in crate::mint_proofs) const SCRIPT_LEN: usize = 2 + MINT_PROOF_DATA_LEN;
pub(in crate::mint_proofs) const OP_RETURN: u8 = 0x6a;
pub(in crate::mint_proofs) const PUSH_DATA_LEN: u8 = MINT_PROOF_DATA_LEN as u8;
