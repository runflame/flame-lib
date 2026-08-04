use std::hash::{Hash, Hasher};

use corepc_client::bitcoin::{ScriptBuf, TxOut};
use ed25519_dalek::VerifyingKey;
use flamevm::Predicate;

use crate::mint_proofs::core::creation::mint_proof_to_script;
use crate::mint_proofs::parse_mint_proof_output;

#[derive(Clone, Debug)]
pub struct MintingProofData {
    pub network_id: u8,
    pub flame_block_hash: [u8; 32],
    pub flame_reward_address: Predicate,
    pub validator_pubkey: Option<VerifyingKey>,
}

impl MintingProofData {
    pub fn to_script(&self) -> ScriptBuf {
        mint_proof_to_script(self)
    }

    pub fn from_tx_out(output: &TxOut) -> Option<Self> {
        parse_mint_proof_output(output)
    }
}

impl PartialEq for MintingProofData {
    fn eq(&self, other: &Self) -> bool {
        self.network_id == other.network_id
            && self.flame_block_hash == other.flame_block_hash
            && self.flame_reward_address.to_point() == other.flame_reward_address.to_point()
            && self.validator_pubkey == other.validator_pubkey
    }
}

impl Eq for MintingProofData {}

impl Hash for MintingProofData {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.network_id.hash(state);
        self.flame_block_hash.hash(state);
        self.flame_reward_address.to_point().as_bytes().hash(state);
        self.validator_pubkey.hash(state);
    }
}
