//! Integration helpers for Bitcoin transactions.

mod mint_proof_sender;
pub mod mint_proofs;
pub mod rpc;
pub mod test;

pub use mint_proof_sender::{MintProofSendError, MintProofSender};
pub use mint_proofs::{
    MintProof, mint_proof_to_script, parse_mint_proof_output, parse_mint_proofs,
};
pub use rpc::{BlockTip, Core31RpcApi, RpcApi};
