use std::sync::Arc;

use crate::mint_proofs::MintingProofData;
use crate::rpc::RpcApi;
use corepc_client::{
    bitcoin::{
        Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness, absolute,
        transaction,
    },
    client_sync::Error as RpcError,
};
use ed25519_dalek::VerifyingKey;
use flamevm::Predicate;
use thiserror::Error;
use types::{BlockHash, FlameNetwork};

#[derive(Debug, Error)]
pub enum MintProofSendError {
    #[error("mint-proof amount must be greater than zero")]
    ZeroAmount,
    #[error("Bitcoin Core RPC error: {0}")]
    Rpc(#[from] RpcError),
}

pub struct MintProofSender<R: RpcApi> {
    rpc: Arc<R>,
    network: FlameNetwork,
}

impl<R: RpcApi> MintProofSender<R> {
    pub fn new(rpc: Arc<R>, network: FlameNetwork) -> Self {
        Self { rpc, network }
    }

    pub async fn send_mint_proof(
        &self,
        amount: Amount,
        inputs: &[OutPoint],
        flame_block_hash: BlockHash,
        flame_address: Predicate,
        validator_pubkey: Option<VerifyingKey>,
    ) -> Result<Txid, MintProofSendError> {
        if amount == Amount::ZERO {
            return Err(MintProofSendError::ZeroAmount);
        }

        let proof = MintingProofData {
            network: self.network,
            flame_block_hash,
            flame_reward_address: flame_address,
            validator_pubkey,
        };
        let transaction = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: inputs
                .iter()
                .copied()
                .map(|previous_output| TxIn {
                    previous_output,
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::MAX,
                    witness: Witness::default(),
                })
                .collect(),
            output: vec![TxOut {
                value: amount,
                script_pubkey: proof.to_script(),
            }],
        };

        let signed_transaction = self.rpc.fund_and_sign_transaction(&transaction).await?;
        Ok(self
            .rpc
            .publish_mint_transaction(&signed_transaction, amount)
            .await?)
    }
}
