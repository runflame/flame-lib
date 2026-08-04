use corepc_client::bitcoin::ScriptBuf;

use crate::mint_proofs::MintingProofData;
use crate::mint_proofs::core::constants::{
    MINT_PROOF_DATA_LEN, MINT_PROOF_MAGIC, OP_PUSHDATA1, OP_RETURN,
    PARTICIPATING_MINT_PROOF_DATA_LEN,
};

pub fn mint_proof_to_script(proof: &MintingProofData) -> ScriptBuf {
    let payload_len = if proof.validator_pubkey.is_some() {
        PARTICIPATING_MINT_PROOF_DATA_LEN
    } else {
        MINT_PROOF_DATA_LEN
    };
    let push_prefix_len = if payload_len <= 75 { 1 } else { 2 };
    let mut script = Vec::with_capacity(1 + push_prefix_len + payload_len);
    script.push(OP_RETURN);
    if payload_len <= 75 {
        script.push(payload_len as u8);
    } else {
        script.push(OP_PUSHDATA1);
        script.push(payload_len as u8);
    }
    script.extend_from_slice(&MINT_PROOF_MAGIC);
    script.push(proof.network_id);
    script.extend_from_slice(&proof.flame_block_hash);
    script.extend_from_slice(proof.flame_reward_address.to_point().as_bytes());
    if let Some(validator_pubkey) = &proof.validator_pubkey {
        script.extend_from_slice(validator_pubkey.as_bytes());
    }
    ScriptBuf::from_bytes(script)
}
