use std::{error, fmt, sync::Arc};

use corepc_client::{
    bitcoin::{
        Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness, absolute,
        transaction,
    },
    client_sync::Error as RpcError,
};

use crate::{MintProof, RpcApi};

#[derive(Debug)]
pub enum MintProofSendError {
    ZeroAmount,
    Rpc(RpcError),
}

impl fmt::Display for MintProofSendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroAmount => formatter.write_str("mint-proof amount must be greater than zero"),
            Self::Rpc(error) => write!(formatter, "Bitcoin Core RPC error: {error}"),
        }
    }
}

impl error::Error for MintProofSendError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::ZeroAmount => None,
            Self::Rpc(error) => Some(error),
        }
    }
}

impl From<RpcError> for MintProofSendError {
    fn from(error: RpcError) -> Self {
        Self::Rpc(error)
    }
}

pub struct MintProofSender<R: RpcApi> {
    rpc: Arc<R>,
    network_id: u8,
}

impl<R: RpcApi> MintProofSender<R> {
    pub fn new(rpc: Arc<R>, network_id: u8) -> Self {
        Self { rpc, network_id }
    }

    pub fn send_mint_proof(
        &self,
        amount: Amount,
        inputs: &[OutPoint],
        flame_block_hash: [u8; 32],
        wants_to_participate: bool,
    ) -> Result<Txid, MintProofSendError> {
        if amount == Amount::ZERO {
            return Err(MintProofSendError::ZeroAmount);
        }

        let proof = MintProof {
            network_id: self.network_id,
            flame_block_hash,
            want_participate_in_consensus: wants_to_participate,
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

        let signed_transaction = self.rpc.fund_and_sign_transaction(&transaction)?;
        Ok(self
            .rpc
            .publish_mint_transaction(&signed_transaction, amount)?)
    }
}
