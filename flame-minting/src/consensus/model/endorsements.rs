use bitcoin::Txid;
use btc_integration::{AuthenticatedMintingEndorsement, MinterP2wsh, watcher::BlockId};
use flamechain::{BlockHash, CoreFlameHeight};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncludedEndorsement {
    pub btc_block: BlockId,
    pub endorsement: AuthenticatedMintingEndorsement,
}

impl IncludedEndorsement {
    pub fn block_height(&self) -> CoreFlameHeight {
        self.endorsement.output().data.block_height()
    }

    pub fn block_hash(&self) -> BlockHash {
        self.endorsement.output().data.block_hash()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeightedEndorsement {
    pub original: IncludedEndorsement,
    pub effective_minting_power: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoubleSign {
    pub minter: MinterP2wsh,
    pub target_flame_height: CoreFlameHeight,
    pub endorsements: Vec<IncludedEndorsement>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MinterEndorsementState {
    pub endorsements: Vec<WeightedEndorsement>,
    pub pending_endorsements: Vec<IncludedEndorsement>,
    pub double_signs: Vec<DoubleSign>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EndorsementIndex {
    pub txid: Txid,
    pub input_index: usize,
}

impl From<&AuthenticatedMintingEndorsement> for EndorsementIndex {
    fn from(endorsement: &AuthenticatedMintingEndorsement) -> Self {
        Self {
            txid: endorsement.txid(),
            input_index: endorsement.auth().input_index,
        }
    }
}

pub type PendingEndorsements = Vec<IncludedEndorsement>;
