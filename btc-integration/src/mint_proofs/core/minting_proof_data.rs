use corepc_client::bitcoin::{ScriptBuf, TxOut};

use crate::mint_proofs::core::creation::mint_proof_to_script;
use crate::parse_mint_proof_output;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MintingProofData {
    pub network_id: u8,
    pub flame_block_hash: [u8; 32],
    pub want_participate_in_consensus: bool,
}

impl MintingProofData {
    pub fn to_script(&self) -> ScriptBuf {
        mint_proof_to_script(self)
    }

    pub fn from_tx_out(output: &TxOut) -> Option<Self> {
        parse_mint_proof_output(output)
    }
}
