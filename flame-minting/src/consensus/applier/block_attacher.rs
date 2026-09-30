use flame_chain_service::ChainAccess;
use flame_storage::CanonicalStorage;
use flamechain::{BlockHash, BlockHeader, BlockTip, ChainError};

pub struct BlockAttacher<'a, S: CanonicalStorage, H: ChainAccess> {
    pub canonical_storage: &'a S,
    pub chain: &'a H,
}

impl<S: CanonicalStorage, H: ChainAccess> BlockAttacher<'_, S, H> {
    pub async fn attach_block(
        &self,
        block_tip: BlockTip,
    ) -> Result<BlockHeader, BlockAttacherError<S::Error, H::Error>> {
        let (_stored_state_tip, mut state) = self
            .canonical_storage
            .get_state()
            .await
            .map_err(BlockAttacherError::CanonicalStorage)?
            .ok_or(BlockAttacherError::MissingState)?;

        let state_height = state.height();
        let block_height = block_tip.height.as_u64();
        if state_height.checked_add(1) != Some(block_height) {
            return Err(BlockAttacherError::InvalidBlockHeight {
                state_height,
                block_height,
            });
        }

        let state_tip = state.tip();

        let block = self
            .chain
            .get_block(block_tip)
            .await
            .map_err(BlockAttacherError::ChainAccess)?
            .ok_or(BlockAttacherError::MissingBlock(block_tip))?;

        if block.header.parent != state_tip {
            return Err(BlockAttacherError::ParentMismatch {
                expected: state_tip,
                actual: block.header.parent,
            });
        }

        self.chain
            .set_as_child(
                BlockTip {
                    hash: state_tip,
                    height: state_height.into(),
                },
                block_tip,
            )
            .await
            .map_err(BlockAttacherError::ChainAccess)?;

        state
            .connect(&block)
            .map_err(BlockAttacherError::Blockchain)?;

        self.canonical_storage
            .commit_state(&state)
            .await
            .map_err(BlockAttacherError::CanonicalStorage)?;

        Ok(block.header.clone())
    }
}

#[derive(Debug)]
pub enum BlockAttacherError<S, H> {
    CanonicalStorage(S),
    ChainAccess(H),
    Blockchain(ChainError),
    MissingState,
    MissingBlock(BlockTip),
    InvalidBlockHeight {
        state_height: u64,
        block_height: u64,
    },
    ParentMismatch {
        expected: BlockHash,
        actual: BlockHash,
    },
}
