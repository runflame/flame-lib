use crate::{Block, Blockchain, ChainError};

pub trait FlameBlockValidator {
    fn validate_flame_block(&self, parent: &Blockchain, block: &Block) -> Result<(), ChainError>;
}
