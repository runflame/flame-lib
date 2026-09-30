use flame_storage::ChainStorage;

pub struct FlameIndexer<C: ChainStorage> {
    pub chain_storage: C,
}
