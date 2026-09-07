//! Bitcoin workflows

use crate::btc::rpc::{
    BtcBlockHeaderInfo, BtcBlockTip, BtcFundedTransaction, BtcTransactionWithPrevouts, RpcApi,
    rpc_error_code,
};
use crate::btc::transaction_builder::BitcoinTransactionBuilder;
use corepc_client::{
    bitcoin::{Amount, BlockHash, ScriptBuf, Transaction, Txid},
    client_sync::{Error, Result},
};
use std::sync::Arc;

pub struct BitcoinFacade<R: ?Sized> {
    rpc: Arc<R>,
    transaction_builder: BitcoinTransactionBuilder<R>,
}

impl<R: ?Sized> BitcoinFacade<R> {
    pub fn new(rpc: Arc<R>) -> Self {
        Self {
            transaction_builder: BitcoinTransactionBuilder::new(Arc::clone(&rpc)),
            rpc,
        }
    }

    pub fn rpc(&self) -> &R {
        &self.rpc
    }
}

impl<R: RpcApi + ?Sized> BitcoinFacade<R> {
    pub async fn best_block_tip(&self) -> Result<BtcBlockTip> {
        self.rpc.best_block_tip().await
    }

    pub async fn block_header_info(&self, hash: BlockHash) -> Result<BtcBlockHeaderInfo> {
        self.rpc.block_header_info(hash).await
    }

    pub async fn previous_header(&self, header: BtcBlockHeaderInfo) -> Result<BtcBlockHeaderInfo> {
        let hash = header.previous_block_hash.ok_or_else(|| {
            Error::Returned(format!(
                "block {:?} does not have a previous block",
                header.tip
            ))
        })?;
        let previous = self.rpc.block_header_info(hash).await?;
        if previous.tip.hash != hash
            || previous.tip.height.checked_add(1) != Some(header.tip.height)
        {
            return Err(Error::UnexpectedStructure);
        }
        Ok(previous)
    }

    /// Return the tip and its ancestors, newest first, stopping at genesis or `limit`.
    pub async fn recent_block_tips(
        &self,
        tip: BtcBlockTip,
        limit: usize,
    ) -> Result<Vec<BtcBlockTip>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut header = self.block_header_info(tip.hash).await?;
        if header.tip != tip {
            return Err(Error::UnexpectedStructure);
        }
        let mut tips = vec![tip];
        while tips.len() < limit && header.tip.height > 0 {
            header = self.previous_header(header).await?;
            tips.push(header.tip);
        }
        Ok(tips)
    }

    /// A rollback below this height means the block is no longer canonical.
    pub async fn is_canonical(&self, tip: BtcBlockTip) -> Result<bool> {
        match self.rpc.block_hash_at_height(tip.height).await {
            Ok(hash) => Ok(hash == tip.hash),
            Err(error) if rpc_error_code(&error) == Some(-8) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub async fn block_hash_at_height(&self, height: u64) -> Result<BlockHash> {
        self.rpc.block_hash_at_height(height).await
    }

    pub async fn transactions_in_block(&self, hash: BlockHash) -> Result<Vec<Transaction>> {
        self.rpc.transactions_in_block(hash).await
    }

    pub async fn transactions_with_prevouts_in_block(
        &self,
        hash: BlockHash,
    ) -> Result<Vec<BtcTransactionWithPrevouts>> {
        self.rpc.transactions_with_prevouts_in_block(hash).await
    }

    pub async fn transactions_at_height(
        &self,
        height: u64,
    ) -> Result<(BlockHash, Vec<Transaction>)> {
        let hash = self.rpc.block_hash_at_height(height).await?;
        Ok((hash, self.rpc.transactions_in_block(hash).await?))
    }

    pub async fn wait_for_next_block(&self, previous: BtcBlockTip) -> Result<BtcBlockTip> {
        loop {
            // Keep each server-side wait below the client's 60-second HTTP timeout.
            // Pass the known tip on every call to avoid missing changes between requests.
            let tip = self.rpc.wait_for_new_block(previous.hash, 45_000).await?;
            if tip.hash != previous.hash {
                return Ok(tip);
            }
        }
    }

    pub async fn publish_transaction(&self, transaction: &Transaction) -> Result<Txid> {
        self.rpc.send_raw_transaction(transaction, None).await
    }

    /// Publish with an explicit burn allowance
    pub async fn publish_transaction_with_burn(
        &self,
        transaction: &Transaction,
        max_burn_amount: Amount,
    ) -> Result<Txid> {
        self.rpc
            .send_raw_transaction(transaction, Some(max_burn_amount))
            .await
    }

    pub async fn fund_and_sign_with_wallet(
        &self,
        transaction: &Transaction,
    ) -> Result<Transaction> {
        self.transaction_builder
            .fund_and_sign_with_wallet(transaction)
            .await
    }

    /// Fund without reserving inputs. The caller supplies signatures and must publish
    /// before funding the next transaction.
    pub async fn fund_transaction_from_script(
        &self,
        transaction: &Transaction,
        script_pubkey: &ScriptBuf,
        input_weight: u64,
    ) -> Result<BtcFundedTransaction> {
        self.transaction_builder
            .fund_transaction_from_script(transaction, script_pubkey, input_weight)
            .await
    }
}

#[cfg(test)]
#[path = "bitcoin_facade_tests.rs"]
mod tests;
