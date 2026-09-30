use std::sync::Arc;

use crate::{Block, Blockchain, ChainError};

pub trait FlameBlockValidator {
    fn validate_flame_block(
        &self,
        parent: &Blockchain,
        block: Arc<Block>,
    ) -> Result<(), ChainError>;
}
