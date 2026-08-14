use std::collections::BTreeMap;

use async_trait::async_trait;
use corepc_client::bitcoin::{Amount, BlockHash};
use tokio::sync::RwLock;
use flamechain::BlockHash as FlameBlockHash;

use crate::mint_proofs::MintingProofData;
use crate::rpc::BtcBlockTip;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintingProof {
    pub minting_proof_data: MintingProofData,
    pub burned_amount: Amount,
    pub bitcoin_block_tip: BtcBlockTip,
}

pub type MintingProofsByBitcoinBlock = BTreeMap<BlockHash, Vec<MintingProof>>;

#[async_trait]
pub trait MintingProofStorage: Send + Sync {
    async fn get(&self, flame_block_hash: FlameBlockHash) -> Vec<MintingProof>;

    async fn apply_chain_update(
        &self,
        discarded_block_hashes: &[BlockHash],
        new_proofs: &[MintingProof],
    ) -> MintingProofsByBitcoinBlock;
}

#[derive(Debug, Default)]
pub struct InMemoryMintingProofStorage {
    minting_proofs: RwLock<BTreeMap<FlameBlockHash, Vec<MintingProof>>>,
}

impl InMemoryMintingProofStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn len(&self) -> usize {
        self.minting_proofs
            .read()
            .await
            .values()
            .map(Vec::len)
            .sum()
    }

    pub async fn is_empty(&self) -> bool {
        self.minting_proofs.read().await.is_empty()
    }
}

#[async_trait]
impl MintingProofStorage for InMemoryMintingProofStorage {
    async fn get(&self, flame_block_hash: FlameBlockHash) -> Vec<MintingProof> {
        self.minting_proofs
            .read()
            .await
            .get(&flame_block_hash)
            .cloned()
            .unwrap_or_default()
    }

    async fn apply_chain_update(
        &self,
        discarded_block_hashes: &[BlockHash],
        new_proofs: &[MintingProof],
    ) -> MintingProofsByBitcoinBlock {
        let mut removed = MintingProofsByBitcoinBlock::new();
        let mut stored_proofs = self.minting_proofs.write().await;

        stored_proofs.retain(|_, proofs| {
            proofs.retain(|proof| {
                let bitcoin_block_hash = proof.bitcoin_block_tip.hash;
                if discarded_block_hashes.contains(&bitcoin_block_hash) {
                    removed
                        .entry(bitcoin_block_hash)
                        .or_default()
                        .push(proof.clone());
                    false
                } else {
                    true
                }
            });

            !proofs.is_empty()
        });

        for proof in new_proofs {
            stored_proofs
                .entry(proof.minting_proof_data.flame_block_hash)
                .or_default()
                .push(proof.clone());
        }

        removed
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use corepc_client::bitcoin::{Amount, BlockHash};
    use ed25519_dalek::SigningKey;
    use flamevm::Predicate;
    use flamechain::FlameNetwork;

    use super::*;

    fn proof(block_hash: [u8; 32], burned_sats: u64, bitcoin_height: u64) -> MintingProof {
        MintingProof {
            minting_proof_data: MintingProofData {
                network: FlameNetwork::Regtest,
                flame_block_hash: FlameBlockHash::from(block_hash),
                flame_reward_address: Predicate::opaque(Predicate::unspendable_key()),
                validator_pubkey: Some(SigningKey::from_bytes(&[7; 32]).verifying_key()),
            },
            burned_amount: Amount::from_sat(burned_sats),
            bitcoin_block_tip: BtcBlockTip {
                hash: BlockHash::from_str(&format!("{bitcoin_height:064x}"))
                    .expect("valid block hash"),
                height: bitcoin_height,
            },
        }
    }

    #[tokio::test]
    async fn in_memory_storage_groups_proofs_by_flame_block_hash() {
        let first = proof([0x11; 32], 1_000, 100);
        let second = proof([0x11; 32], 2_000, 101);
        let other = proof([0x22; 32], 3_000, 102);
        let storage = InMemoryMintingProofStorage::new();

        storage
            .apply_chain_update(&[], &[first.clone(), second.clone(), other.clone()])
            .await;

        assert_eq!(storage.len().await, 3);
        assert_eq!(
            storage.get(FlameBlockHash::from([0x11; 32])).await,
            vec![first, second]
        );
        assert_eq!(
            storage.get(FlameBlockHash::from([0x22; 32])).await,
            vec![other]
        );
        assert_eq!(
            storage.get(FlameBlockHash::from([0x33; 32])).await,
            Vec::new()
        );
    }

    #[tokio::test]
    async fn removes_and_returns_proofs_from_discarded_bitcoin_blocks() {
        let discarded_first = proof([0x11; 32], 1_000, 100);
        let discarded_second = proof([0x22; 32], 2_000, 100);
        let retained = proof([0x11; 32], 3_000, 101);
        let discarded_hash = discarded_first.bitcoin_block_tip.hash;
        let storage = InMemoryMintingProofStorage::new();

        storage
            .apply_chain_update(
                &[],
                &[
                    discarded_first.clone(),
                    discarded_second.clone(),
                    retained.clone(),
                ],
            )
            .await;

        assert_eq!(
            storage.apply_chain_update(&[discarded_hash], &[]).await,
            BTreeMap::from([(discarded_hash, vec![discarded_first, discarded_second])])
        );
        assert_eq!(
            storage.get(FlameBlockHash::from([0x11; 32])).await,
            vec![retained]
        );
        assert!(
            storage
                .get(FlameBlockHash::from([0x22; 32]))
                .await
                .is_empty()
        );
    }
}
