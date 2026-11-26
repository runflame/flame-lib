pub const VERSION_V1: u8 = 1;

pub const BURN_MAGIC: [u8; 4] = *b"FLMS";
pub const MINTING_MAGIC: [u8; 4] = *b"FLMB";

pub const BURN_PAYLOAD_LEN: usize = 69;
pub const BURN_WITH_DURATION_PAYLOAD_LEN: usize = 71;
pub const MINTING_PAYLOAD_LEN: usize = 41;
