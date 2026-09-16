use flamechain::BlockHash;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConsensusOutcome {
    pub selected_tip: Option<BlockHash>,
    pub missing_blocks: Vec<BlockHash>,
}
