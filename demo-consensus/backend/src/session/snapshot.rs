use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use bitcoin::{Transaction, Txid};
use btc_integration::{
    AuthenticatedMintingEndorsement, Burn, MinterP2wsh, UncheckedMintingEndorsement,
};
use flame_minting::{
    consensus::{ConsensusStorage, MintingProtocolParams},
    orchestrator::cursor_storage::CursorStorage,
};

use crate::types::*;

use super::{DemoSession, bitcoin_tip, flame_tip, runtime::RecordedBlock};

type EndorsementKey = (String, u32, u32);
type DoubleSignKey = (u32, u32);

struct ProtocolSnapshotBuilder<'a> {
    parameters: &'a MintingProtocolParams,
    minters: HashMap<MinterP2wsh, MinterId>,
    acquisitions: Vec<AcquisitionSnapshot>,
    endorsements: BTreeMap<EndorsementKey, EndorsementSnapshot>,
    double_signs: BTreeMap<DoubleSignKey, Vec<EndorsementKey>>,
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
            endorsements: BTreeMap::new(),
            double_signs: BTreeMap::new(),
        };
        for record in history
            .iter()
            .filter(|record| record.block.id.height <= cursor.height)
        {
            builder.add_confirmed(record);
        }
        for transaction in &pending {
            builder.add_pending(transaction);
        }
        let double_signs = builder
            .double_signs
            .iter()
            .map(|(&(minter, height), endorsements)| DoubleSignSnapshot {
                minter_id: MinterId(minter),
                core_height: height,
                endorsements: endorsements
                    .iter()
                    .filter_map(|key| builder.endorsements.get(key).cloned())
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
            endorsements: builder.endorsements.into_values().collect(),
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
                    max_endorsement_delay: parameters.max_endorsement_delay,
                },
                double_signs,
            },
        })
    }
}

impl ProtocolSnapshotBuilder<'_> {
    fn add_confirmed(&mut self, record: &RecordedBlock) {
        let block = bitcoin_tip(record.block.id);
        let transaction_status = TransactionStatus::Confirmed {
            block: block.clone(),
        };
        for acquisition in &record.block.burns {
            let accepted = record
                .outcome
                .consensus
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
        for endorsement in &record.block.endorsements {
            self.add_endorsement(
                endorsement.txid(),
                endorsement.output(),
                endorsement.auth().minter().p2wsh(),
                transaction_status.clone(),
                EndorsementProcessingStatus::Rejected,
            );
        }
        for endorsement in record
            .outcome
            .consensus
            .pending_endorsements
            .values()
            .flatten()
        {
            self.set_endorsement_status(
                &endorsement.endorsement,
                EndorsementProcessingStatus::PendingBlock,
            );
        }
        for endorsement in &record.outcome.consensus.accepted_endorsements {
            self.set_endorsement_status(
                &endorsement.original.endorsement,
                EndorsementProcessingStatus::Accepted {
                    effective_minting_power: endorsement.effective_minting_power,
                },
            );
        }
        for endorsement in &record.outcome.consensus.removed_endorsements {
            self.set_endorsement_status(
                &endorsement.original.endorsement,
                EndorsementProcessingStatus::InvalidatedByDoubleSign,
            );
        }
        for sign in &record.outcome.consensus.double_signs {
            let Some(&minter) = self.minters.get(&sign.minter) else {
                continue;
            };
            let mut keys = Vec::new();
            for endorsement in &sign.endorsements {
                self.set_endorsement_status(
                    &endorsement.endorsement,
                    EndorsementProcessingStatus::InvalidatedByDoubleSign,
                );
                keys.push((
                    endorsement.endorsement.txid().to_string(),
                    endorsement.endorsement.output().output_index,
                    minter.0,
                ));
            }
            self.double_signs
                .insert((minter.0, sign.target_flame_height.as_u32()), keys);
        }
    }

    fn add_pending(&mut self, transaction: &Transaction) {
        for acquisition in Burn::from_tx(transaction) {
            self.add_acquisition(
                &acquisition,
                TransactionStatus::Mempool,
                AcquisitionProcessingStatus::Unprocessed,
            );
        }
        for endorsement in UncheckedMintingEndorsement::from_tx(transaction) {
            self.add_endorsement(
                endorsement.txid(),
                endorsement.output(),
                endorsement.auth().minter().p2wsh(),
                TransactionStatus::Mempool,
                EndorsementProcessingStatus::Unprocessed,
            );
        }
    }

    fn add_acquisition(
        &mut self,
        acquisition: &Burn,
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

    fn add_endorsement(
        &mut self,
        txid: Txid,
        output: &btc_integration::MintingEndorsementOutput,
        minter: MinterP2wsh,
        transaction_status: TransactionStatus,
        processing_status: EndorsementProcessingStatus,
    ) {
        let Some(&minter_id) = self.minters.get(&minter) else {
            return;
        };
        let txid = txid.to_string();
        self.endorsements.insert(
            (txid.clone(), output.output_index, minter_id.0),
            EndorsementSnapshot {
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

    fn set_endorsement_status(
        &mut self,
        endorsement: &AuthenticatedMintingEndorsement,
        status: EndorsementProcessingStatus,
    ) {
        let Some(&minter) = self.minters.get(&endorsement.auth().minter().p2wsh()) else {
            return;
        };
        let key = (
            endorsement.txid().to_string(),
            endorsement.output().output_index,
            minter.0,
        );
        if let Some(snapshot) = self.endorsements.get_mut(&key) {
            snapshot.processing_status = status;
        }
    }
}
