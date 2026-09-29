use flame_storage::CanonicalStorage;
use flamechain::{BlockTip, ChainError};

pub struct BlockDetacher<'a, S: CanonicalStorage> {
    pub canonical_storage: &'a S,
}

impl<S: CanonicalStorage> BlockDetacher<'_, S> {
    pub async fn detach_block(
        &self,
        block_tip: BlockTip,
    ) -> Result<(), BlockDetacherError<S::Error>> {
        let (_, mut state) = self
            .canonical_storage
            .get_state()
            .await
            .map_err(BlockDetacherError::CanonicalStorage)?
            .ok_or(BlockDetacherError::MissingState)?;
        let state_tip = BlockTip {
            hash: state.tip(),
            height: state.height().into(),
        };
        if state_tip != block_tip {
            return Err(BlockDetacherError::TipMismatch {
                expected: block_tip,
                actual: state_tip,
            });
        }

        state
            .disconnect_tip(block_tip.hash)
            .map_err(BlockDetacherError::Blockchain)?;
        self.canonical_storage
            .commit_state(&state)
            .await
            .map_err(BlockDetacherError::CanonicalStorage)
    }
}

#[derive(Debug)]
pub enum BlockDetacherError<S> {
    CanonicalStorage(S),
    Blockchain(ChainError),
    MissingState,
    TipMismatch {
        expected: BlockTip,
        actual: BlockTip,
    },
}
