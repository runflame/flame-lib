use corepc_client::bitcoin::{ScriptBuf, TxOut};

use super::creation::mint_proof_to_script;
use super::parser::parse_mint_proof_output;

/// A commitment to a Flame block carried by a Bitcoin output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MintProof {
    /// Flame network identifier.
    pub network_id: u8,
    /// The 256-bit Flame block hash, in protocol wire order.
    pub flame_block_hash: [u8; 32],
    /// Whether the minter wants to participate in BFT pre-agreement.
    pub want_participate_in_consensus: bool,
}

impl MintProof {
    /// Encodes this proof as its canonical Bitcoin `scriptPubKey`.
    pub fn to_script(&self) -> ScriptBuf {
        mint_proof_to_script(self)
    }

    /// Parses this proof from a Bitcoin transaction output.
    ///
    /// Returns `None` when the output has a zero value or its script does not
    /// exactly match the mint-proof wire format.
    pub fn from_tx_out(output: &TxOut) -> Option<Self> {
        parse_mint_proof_output(output)
    }
}
