//! Integration helpers for Bitcoin transactions.

pub mod mint_proofs;
pub mod rpc;

pub use mint_proofs::{
    MintProof, mint_proof_to_script, parse_mint_proof_output, parse_mint_proofs,
};
pub use rpc::{BlockTip, Core31RpcApi, RpcApi};
