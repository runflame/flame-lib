use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use btc_integration::{Acquisition, AuthenticatedMintingVote, MinterP2wsh, UncheckedMintingVote};
use corepc_client::bitcoin::{Transaction, Txid};
use flame_minting::{
    consensus::{ConsensusStorage, MintingProtocolParams},
    orchestrator::cursor_storage::CursorStorage,
};

use crate::types::*;

use super::{DemoSession, bitcoin_tip, flame_tip, runtime::RecordedBlock};

type VoteKey = (String, u32, u32);
type DoubleSignKey = (u32, u32);

struct ProtocolSnapshotBuilder<'a> {
    parameters: &'a MintingProtocolParams,
    minters: HashMap<MinterP2wsh, MinterId>,
    acquisitions: Vec<AcquisitionSnapshot>,
    votes: BTreeMap<VoteKey, VoteSnapshot>,
    double_signs: BTreeMap<DoubleSignKey, Vec<VoteKey>>,
}

impl DemoSession {
    pub async fn snapshot(&mut self) -> Result<DemoSnapshot> {
        self.wait_for_processing(self.bitcoin.tip().await?).await?;
        let (bitcoin, pending) = self.bitcoin.snapshot().await?;
        let cursor = self.runtime.cursor.get_cursor().await?;
        let history = self.runtime.history.lock().await.clone();
        let mut builder = ProtocolSnapshotBuilder {
            parameters: &self.runtime.parameters,
            minters: self
                .minters
                .iter()
                .map(|minter| (minter.identity.p2wsh(), minter.id))
                .collect(),
            acquisitions: Vec::new(),
            votes: BTreeMap::new(),
            double_signs: BTreeMap::new(),
        };
        for record in history
            .iter()
            .filter(|record| record.block.btc_block_tip.height <= cursor.height)
        {
            builder.add_confirmed(record);
        }
        for transaction in &pending {
            builder.add_pending(transaction);
        }
        let double_signs = builder
            .double_signs
            .iter()
            .map(|(&(minter, height), votes)| DoubleSignSnapshot {
                minter_id: MinterId(minter),
                core_height: height,
                votes: votes
                    .iter()
                    .filter_map(|key| builder.votes.get(key).cloned())
                    .collect(),
            })
            .collect();
        let parameters = &self.runtime.parameters;
        Ok(DemoSnapshot {
            bitcoin,
            flame: FlameChainSnapshot {
                canonical_tip: Some(self.canonical_tip().await?),
                blocks: self.flame.snapshots().await?,
            },
            minters: self.minters().await?,
            acquisitions: builder.acquisitions,
            votes: builder.votes.into_values().collect(),
            consensus: ConsensusSnapshot {
                btc_cursor: bitcoin_tip(cursor),
                heaviest_tip: self
                    .runtime
                    .consensus
                    .get_block_tip_with_most_weight()
                    .await?
                    .map(flame_tip),
                parameters: ProtocolParameters {
                    acquisition_maturity: parameters.acquisition_maturity,
                    default_acquisition_duration: parameters.default_acquisition_duration,
                    min_acquisition_duration: parameters.min_acquisition_duration,
                    max_vote_delay: parameters.max_vote_delay,
                },
                double_signs,
            },
        })
    }
}

impl ProtocolSnapshotBuilder<'_> {
    fn add_confirmed(&mut self, record: &RecordedBlock) {
        let block = bitcoin_tip(record.block.btc_block_tip);
        let transaction_status = TransactionStatus::Confirmed {
            block: block.clone(),
        };
        for acquisition in &record.block.acquisitions {
            let accepted = record
                .outcome
                .accepted_acquisitions
                .iter()
                .find(|accepted| &accepted.acquisition == acquisition);
            let processing = match accepted {
                Some(accepted) => {
                    let start = block.height + u64::from(self.parameters.acquisition_maturity);
                    AcquisitionProcessingStatus::Accepted {
                        activates_at_btc_height: start,
                        expires_at_btc_height_exclusive: start
                            + u64::from(accepted.duration(self.parameters)),
                    }
                }
                None => AcquisitionProcessingStatus::Rejected,
            };
            self.add_acquisition(acquisition, transaction_status.clone(), processing);
        }
        for vote in &record.block.votes {
            self.add_vote(
                vote.txid(),
                vote.output(),
                vote.auth().minter().p2wsh(),
                transaction_status.clone(),
                VoteProcessingStatus::Rejected,
            );
        }
        for vote in record.outcome.pending_votes.values().flatten() {
            self.set_vote_status(&vote.vote, VoteProcessingStatus::PendingBlock);
        }
        for vote in &record.outcome.accepted_votes {
            self.set_vote_status(
                &vote.original.vote,
                VoteProcessingStatus::Accepted {
                    effective_minting_power: vote.effective_minting_power,
                },
            );
        }
        for vote in &record.outcome.removed_votes {
            self.set_vote_status(
                &vote.original.vote,
                VoteProcessingStatus::InvalidatedByDoubleSign,
            );
        }
        for sign in &record.outcome.double_signs {
            let Some(&minter) = self.minters.get(&sign.minter) else {
                continue;
            };
            let mut keys = Vec::new();
            for vote in &sign.votes {
                self.set_vote_status(&vote.vote, VoteProcessingStatus::InvalidatedByDoubleSign);
                keys.push((
                    vote.vote.txid().to_string(),
                    vote.vote.output().output_index,
                    minter.0,
                ));
            }
            self.double_signs
                .insert((minter.0, sign.target_flame_height.as_u32()), keys);
        }
    }

    fn add_pending(&mut self, transaction: &Transaction) {
        for acquisition in Acquisition::from_tx(transaction) {
            self.add_acquisition(
                &acquisition,
                TransactionStatus::Mempool,
                AcquisitionProcessingStatus::Unprocessed,
            );
        }
        for vote in UncheckedMintingVote::from_tx(transaction) {
            self.add_vote(
                vote.txid(),
                vote.output(),
                vote.auth().minter().p2wsh(),
                TransactionStatus::Mempool,
                VoteProcessingStatus::Unprocessed,
            );
        }
    }

    fn add_acquisition(
        &mut self,
        acquisition: &Acquisition,
        transaction_status: TransactionStatus,
        processing_status: AcquisitionProcessingStatus,
    ) {
        let Some(&minter_id) = self.minters.get(&acquisition.data().minter_p2wsh) else {
            return;
        };
        self.acquisitions.push(AcquisitionSnapshot {
            txid: BitcoinTxid(acquisition.txid().to_string()),
            output_index: acquisition.output_index() as u32,
            minter_id,
            amount_sats: acquisition.amount().to_sat(),
            duration_blocks: acquisition
                .data()
                .duration
                .unwrap_or(self.parameters.default_acquisition_duration.get()),
            transaction_status,
            processing_status,
        });
    }

    fn add_vote(
        &mut self,
        txid: Txid,
        output: &btc_integration::MintingVoteOutput,
        minter: MinterP2wsh,
        transaction_status: TransactionStatus,
        processing_status: VoteProcessingStatus,
    ) {
        let Some(&minter_id) = self.minters.get(&minter) else {
            return;
        };
        let txid = txid.to_string();
        self.votes.insert(
            (txid.clone(), output.output_index, minter_id.0),
            VoteSnapshot {
                txid: BitcoinTxid(txid),
                output_index: output.output_index,
                minter_id,
                core_height: output.data.block_height().as_u32(),
                block_hash: FlameBlockHash(hex::encode(output.data.block_hash().as_bytes())),
                transaction_status,
                processing_status,
            },
        );
    }

    fn set_vote_status(&mut self, vote: &AuthenticatedMintingVote, status: VoteProcessingStatus) {
        let Some(&minter) = self.minters.get(&vote.auth().minter().p2wsh()) else {
            return;
        };
        let key = (
            vote.txid().to_string(),
            vote.output().output_index,
            minter.0,
        );
        if let Some(snapshot) = self.votes.get_mut(&key) {
            snapshot.processing_status = status;
        }
    }
}
