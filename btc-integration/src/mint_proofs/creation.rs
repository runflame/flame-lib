use corepc_client::bitcoin::ScriptBuf;

use super::constants::{MINT_PROOF_MAGIC, OP_RETURN, PUSH_DATA_LEN, SCRIPT_LEN};
use super::mint_proof::MintProof;

/// Converts a mint proof to the canonical
/// `OP_RETURN <37-byte direct push> <mint-proof data>` script.
pub fn mint_proof_to_script(proof: &MintProof) -> ScriptBuf {
    let mut script = Vec::with_capacity(SCRIPT_LEN);
    script.push(OP_RETURN);
    script.push(PUSH_DATA_LEN);
    script.extend_from_slice(&MINT_PROOF_MAGIC);
    script.push(proof.network_id);
    script.extend_from_slice(&proof.flame_block_hash);
    script.push(u8::from(proof.want_participate_in_consensus));
    ScriptBuf::from_bytes(script)
}
