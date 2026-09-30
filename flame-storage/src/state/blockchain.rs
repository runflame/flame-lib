pub struct BlockchainStateStorage<P, C> {
    pub provisional: P,
    pub canonical: C,
}
