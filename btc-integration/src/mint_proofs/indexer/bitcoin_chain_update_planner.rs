use std::sync::Arc;

use corepc_client::client_sync::Result as RpcResult;

use crate::rpc::{BtcBlockTip, RpcApi};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainUpdatePlan {
    Extension {
        new_blocks: Vec<BtcBlockTip>,
    },
    Reorg {
        discarded_blocks: Vec<BtcBlockTip>,
        new_blocks: Vec<BtcBlockTip>,
    },
}

pub struct BitcoinChainUpdatePlanner<R> {
    applied_tip: BtcBlockTip,
    rpc_api: Arc<R>,
}

impl<R: RpcApi> BitcoinChainUpdatePlanner<R> {
    pub fn new(applied_tip: BtcBlockTip, rpc_api: Arc<R>) -> Self {
        Self {
            applied_tip,
            rpc_api,
        }
    }

    pub fn mark_applied(&mut self, tip: BtcBlockTip) {
        self.applied_tip = tip;
    }

    pub async fn plan_update(&self, candidate_tip: BtcBlockTip) -> RpcResult<ChainUpdatePlan> {
        let mut old = self
            .rpc_api
            .block_header_info(self.applied_tip.hash)
            .await?;
        let mut new = self.rpc_api.block_header_info(candidate_tip.hash).await?;
        let mut discarded_blocks = Vec::new();
        let mut new_blocks_reverse = Vec::new();

        while old.tip.height > new.tip.height {
            discarded_blocks.push(old.tip);
            old = self.rpc_api.previous_header(old).await?;
        }

        while new.tip.height > old.tip.height {
            new_blocks_reverse.push(new.tip);
            new = self.rpc_api.previous_header(new).await?;
        }

        while old.tip.hash != new.tip.hash {
            discarded_blocks.push(old.tip);
            new_blocks_reverse.push(new.tip);
            old = self.rpc_api.previous_header(old).await?;
            new = self.rpc_api.previous_header(new).await?;
        }

        new_blocks_reverse.reverse();

        if discarded_blocks.is_empty() {
            Ok(ChainUpdatePlan::Extension {
                new_blocks: new_blocks_reverse,
            })
        } else {
            Ok(ChainUpdatePlan::Reorg {
                discarded_blocks,
                new_blocks: new_blocks_reverse,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, str::FromStr};

    use async_trait::async_trait;
    use corepc_client::{
        bitcoin::{Amount, BlockHash, Transaction, Txid},
        client_sync::{Error, Result},
    };

    use super::*;
    use crate::rpc::BtcBlockHeaderInfo;

    struct TestRpc {
        headers: BTreeMap<BlockHash, BtcBlockHeaderInfo>,
    }

    #[async_trait]
    impl RpcApi for TestRpc {
        async fn best_block_tip(&self) -> Result<BtcBlockTip> {
            unreachable!("not needed by the chain update planner")
        }

        async fn block_header_info(&self, block_hash: BlockHash) -> Result<BtcBlockHeaderInfo> {
            self.headers.get(&block_hash).copied().ok_or_else(|| {
                Error::Returned(format!("test block header {block_hash} was not found"))
            })
        }

        async fn transactions_in_block(&self, _block_hash: BlockHash) -> Result<Vec<Transaction>> {
            unreachable!("not needed by the chain update planner")
        }

        async fn transactions_at_height(
            &self,
            _height: u64,
        ) -> Result<(BlockHash, Vec<Transaction>)> {
            unreachable!("not needed by the chain update planner")
        }

        async fn block_hash_at_height(&self, _height: u64) -> Result<BlockHash> {
            unreachable!("not needed by the chain update planner")
        }

        async fn wait_for_next_block(&self, _prev_block: BtcBlockTip) -> Result<BtcBlockTip> {
            unreachable!("not needed by the chain update planner")
        }

        async fn publish_transaction(&self, _signed_transaction: &Transaction) -> Result<Txid> {
            unreachable!("not needed by the chain update planner")
        }

        async fn fund_and_sign_transaction(
            &self,
            _transaction: &Transaction,
        ) -> Result<Transaction> {
            unreachable!("not needed by the chain update planner")
        }

        async fn publish_mint_transaction(
            &self,
            _transaction: &Transaction,
            _max_burn_amount: Amount,
        ) -> Result<Txid> {
            unreachable!("not needed by the chain update planner")
        }
    }

    fn hash(value: u64) -> BlockHash {
        BlockHash::from_str(&format!("{value:064x}")).expect("valid block hash")
    }

    fn tip(value: u64, height: u64) -> BtcBlockTip {
        BtcBlockTip {
            hash: hash(value),
            height,
        }
    }

    fn rpc_with_headers(headers: &[(BtcBlockTip, Option<BlockHash>)]) -> Arc<TestRpc> {
        Arc::new(TestRpc {
            headers: headers
                .iter()
                .map(|(tip, previous_block_hash)| {
                    (
                        tip.hash,
                        BtcBlockHeaderInfo {
                            tip: *tip,
                            previous_block_hash: *previous_block_hash,
                        },
                    )
                })
                .collect(),
        })
    }

    #[tokio::test]
    async fn returns_new_blocks_in_chain_order() {
        let block_1 = tip(1, 1);
        let block_2 = tip(2, 2);
        let block_3 = tip(3, 3);
        let rpc = rpc_with_headers(&[
            (block_1, Some(hash(0))),
            (block_2, Some(block_1.hash)),
            (block_3, Some(block_2.hash)),
        ]);
        let planner = BitcoinChainUpdatePlanner::new(block_1, rpc);

        assert_eq!(
            planner.plan_update(block_3).await.unwrap(),
            ChainUpdatePlan::Extension {
                new_blocks: vec![block_2, block_3],
            }
        );
    }

    #[tokio::test]
    async fn returns_discarded_and_new_blocks_for_a_reorg() {
        let ancestor = tip(10, 10);
        let old_11 = tip(11, 11);
        let old_12 = tip(12, 12);
        let new_11 = tip(21, 11);
        let new_12 = tip(22, 12);
        let new_13 = tip(23, 13);
        let rpc = rpc_with_headers(&[
            (ancestor, Some(hash(9))),
            (old_11, Some(ancestor.hash)),
            (old_12, Some(old_11.hash)),
            (new_11, Some(ancestor.hash)),
            (new_12, Some(new_11.hash)),
            (new_13, Some(new_12.hash)),
        ]);
        let planner = BitcoinChainUpdatePlanner::new(old_12, rpc);

        assert_eq!(
            planner.plan_update(new_13).await.unwrap(),
            ChainUpdatePlan::Reorg {
                discarded_blocks: vec![old_12, old_11],
                new_blocks: vec![new_11, new_12, new_13],
            }
        );
    }

    #[tokio::test]
    async fn handles_a_new_tip_below_the_previous_tip() {
        let ancestor = tip(10, 10);
        let old_11 = tip(11, 11);
        let old_12 = tip(12, 12);
        let rpc = rpc_with_headers(&[
            (ancestor, Some(hash(9))),
            (old_11, Some(ancestor.hash)),
            (old_12, Some(old_11.hash)),
        ]);
        let planner = BitcoinChainUpdatePlanner::new(old_12, rpc);

        assert_eq!(
            planner.plan_update(ancestor).await.unwrap(),
            ChainUpdatePlan::Reorg {
                discarded_blocks: vec![old_12, old_11],
                new_blocks: Vec::new(),
            }
        );
    }
}
