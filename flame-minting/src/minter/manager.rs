use flame_chain_service::CoreBlockSource;

use crate::consensus::ConsensusStorage;

use super::MinterJournal;

pub struct MinterManager<I: CoreBlockSource, C: ConsensusStorage, J: MinterJournal> {
    pub flame_indexer: I,
    pub consensus_storage: C,
    pub journal: J,
}
