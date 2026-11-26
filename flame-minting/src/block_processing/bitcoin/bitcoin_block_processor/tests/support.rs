use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use bitcoin::{
    Amount, BlockHash as BtcBlockHash, Transaction, TxOut, absolute, hashes::Hash, transaction,
};
use btc_integration::{Burn, BurnData, MinterP2wsh};
use flamechain::{
    BlockHash, BlockTip, Blockchain, ChainParams, CoreBlockHeader, CoreBlockTip, CoreFlameHeight,
};
use flamevm::Predicate;

use super::super::*;
use crate::block_processing::bitcoin::cursor_storage::InMemoryCursorStorage;
use crate::block_processing::journal::{
    FlameTransition, RewardsChanges,
    consensus::{
        BitcoinJournalEntry, ConsensusJournal, InMemoryBitcoinJournal, InMemoryConsensusJournal,
    },
};
use crate::consensus::{
    AcquisitionIndex, BitcoinRollbackInput, BitcoinRollbackPlan, ConsensusUndo, DoubleSign,
    EndorsementIndex, IncludedAcquisition, IncludedEndorsement, MinterEndorsementState,
    WeightedBlockHeader, WeightedEndorsement, engine::tests::endorsement_for_tip,
    storage::InMemoryConsensusStorage,
};
use crate::consensus::{ConsensusOutcome, MintingOutcome};
use crate::rewards::{
    engine::tests::{TestChainStorage, protocol_params},
    storage::InMemoryRewardsStorage,
};
use flame_chain_service::ChainPath;
use flamechain::Block;

use crate::rewards::{PendingReward, RewardIndex};

mod canonical_storage;
pub(super) use canonical_storage::{Storage, StoredState};
mod chain;
pub(super) use chain::Chain;
mod consensus_storage;
pub(super) use consensus_storage::{Consensus, ConsensusData};
mod cursor;
pub(super) use cursor::Cursor;
mod engine;
pub(super) use engine::Engine;
mod fixture;
pub(super) use fixture::Fixture;
mod journal;
pub(super) use journal::{Journal, Point};
mod rewards;
pub(super) use rewards::Rewards;

pub(super) type Processor = BitcoinBlockProcessor<
    Engine,
    Consensus,
    Chain,
    Journal,
    Storage,
    Rewards,
    Cursor,
    TestChainStorage,
>;

pub(super) fn btc_tip(height: u64) -> BlockId {
    BlockId {
        hash: BtcBlockHash::from_byte_array([height as u8; 32]),
        height,
    }
}

pub(super) fn included(minter: u8, hash: u8) -> IncludedEndorsement {
    IncludedEndorsement {
        btc_block: btc_tip(101),
        endorsement: endorsement_for_tip(
            minter,
            CoreBlockTip {
                hash: BlockHash::from([hash; 32]),
                height: 1.into(),
            },
        ),
    }
}

pub(super) fn acquisition(amount: u64) -> IncludedAcquisition {
    let data = BurnData::new(
        [1; 32].into(),
        Predicate::opaque(Predicate::unspendable_key()),
    );
    let transaction = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: Vec::new(),
        output: vec![TxOut {
            value: Amount::from_sat(amount),
            script_pubkey: data.to_script(),
        }],
    };
    IncludedAcquisition {
        btc_block: btc_tip(101),
        acquisition: Burn::from_tx(&transaction).pop().unwrap(),
    }
}

pub(super) fn tip(state: &Blockchain) -> BlockTip {
    BlockTip {
        hash: state.tip(),
        height: state.height().into(),
    }
}

pub(super) fn apply_stages() -> Vec<ApplyStage> {
    let mut status = JournalStatus::Applying(ApplyStage::RemoveEndorsements);
    let mut stages = Vec::new();
    while let JournalStatus::Applying(stage) = status {
        stages.push(stage);
        status = status.next().unwrap();
    }
    assert_eq!(stages.len(), 11);
    stages
}
