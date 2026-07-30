/// Magic prefix identifying a Flame mint proof.
pub const MINT_PROOF_MAGIC: [u8; 3] = *b"FLM";

/// Number of bytes in the payload pushed after `OP_RETURN`.
pub const MINT_PROOF_DATA_LEN: usize = 37;

pub(super) const SCRIPT_LEN: usize = 2 + MINT_PROOF_DATA_LEN;
pub(super) const OP_RETURN: u8 = 0x6a;
pub(super) const PUSH_DATA_LEN: u8 = MINT_PROOF_DATA_LEN as u8;
