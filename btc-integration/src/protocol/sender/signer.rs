use std::error::Error;

use async_trait::async_trait;
use corepc_client::bitcoin::{ScriptBuf, Transaction};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequiredInput {
    P2wsh { witness_script: ScriptBuf },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignTransactionRequest {
    pub transaction: Transaction,
    pub required_inputs: Vec<RequiredInput>,
}

impl SignTransactionRequest {
    pub fn new(transaction: Transaction) -> Self {
        Self {
            transaction,
            required_inputs: Vec::new(),
        }
    }

    pub fn requiring(mut self, input: RequiredInput) -> Self {
        self.required_inputs.push(input);
        self
    }
}

#[async_trait]
pub trait TransactionSigner: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    async fn fund_and_sign(
        &self,
        request: SignTransactionRequest,
    ) -> Result<Transaction, Self::Error>;
}
