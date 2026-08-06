use std::collections::BTreeMap;

use corepc_client::bitcoin::{Amount, BlockHash};

use crate::mint_proofs::MintingProofData;
use crate::rpc::BtcBlockTip;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintingProof {
    pub minting_proof_data: MintingProofData,
    pub burned_amount: Amount,
    pub bitcoin_block_tip: BtcBlockTip,
}

pub type MintingProofsByBitcoinBlock = BTreeMap<BlockHash, Vec<MintingProof>>;

pub trait MintingProofStorage {
    fn insert(&mut self, flame_block_hash: [u8; 32], minting_proof: MintingProof);

    fn get(&self, flame_block_hash: [u8; 32]) -> Vec<MintingProof>;

    fn remove_by_bitcoin_blocks(
        &mut self,
        block_hashes: &[BlockHash],
    ) -> MintingProofsByBitcoinBlock;
}

#[derive(Debug, Default)]
pub struct InMemoryMintingProofStorage {
    minting_proofs: BTreeMap<[u8; 32], Vec<MintingProof>>,
}

impl InMemoryMintingProofStorage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.minting_proofs.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.minting_proofs.is_empty()
    }
}

impl MintingProofStorage for InMemoryMintingProofStorage {
    fn insert(&mut self, flame_block_hash: [u8; 32], minting_proof: MintingProof) {
        self.minting_proofs
            .entry(flame_block_hash)
            .or_default()
            .push(minting_proof);
    }

    fn get(&self, flame_block_hash: [u8; 32]) -> Vec<MintingProof> {
        self.minting_proofs
            .get(&flame_block_hash)
            .cloned()
            .unwrap_or_default()
    }

    fn remove_by_bitcoin_blocks(
        &mut self,
        block_hashes: &[BlockHash],
    ) -> MintingProofsByBitcoinBlock {
        let mut removed = MintingProofsByBitcoinBlock::new();

        self.minting_proofs.retain(|_, proofs| {
            proofs.retain(|proof| {
                let bitcoin_block_hash = proof.bitcoin_block_tip.hash;
                if block_hashes.contains(&bitcoin_block_hash) {
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

        removed
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use corepc_client::bitcoin::{Amount, BlockHash};
    use ed25519_dalek::SigningKey;
    use flamevm::Predicate;

    use super::*;

    fn proof(block_hash: [u8; 32], burned_sats: u64, bitcoin_height: u64) -> MintingProof {
        MintingProof {
            minting_proof_data: MintingProofData {
                network_id: 7,
                flame_block_hash: block_hash,
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

    #[test]
    fn in_memory_storage_groups_proofs_by_flame_block_hash() {
        let first = proof([0x11; 32], 1_000, 100);
        let second = proof([0x11; 32], 2_000, 101);
        let other = proof([0x22; 32], 3_000, 102);
        let mut storage = InMemoryMintingProofStorage::new();

        storage.insert([0x11; 32], first.clone());
        storage.insert([0x11; 32], second.clone());
        storage.insert([0x22; 32], other.clone());

        assert_eq!(storage.len(), 3);
        assert_eq!(storage.get([0x11; 32]), vec![first, second]);
        assert_eq!(storage.get([0x22; 32]), vec![other]);
        assert_eq!(storage.get([0x33; 32]), Vec::new());
    }

    #[test]
    fn removes_and_returns_proofs_from_discarded_bitcoin_blocks() {
        let discarded_first = proof([0x11; 32], 1_000, 100);
        let discarded_second = proof([0x22; 32], 2_000, 100);
        let retained = proof([0x11; 32], 3_000, 101);
        let discarded_hash = discarded_first.bitcoin_block_tip.hash;
        let mut storage = InMemoryMintingProofStorage::new();

        storage.insert([0x11; 32], discarded_first.clone());
        storage.insert([0x22; 32], discarded_second.clone());
        storage.insert([0x11; 32], retained.clone());

        assert_eq!(
            storage.remove_by_bitcoin_blocks(&[discarded_hash]),
            BTreeMap::from([(discarded_hash, vec![discarded_first, discarded_second])])
        );
        assert_eq!(storage.get([0x11; 32]), vec![retained]);
        assert!(storage.get([0x22; 32]).is_empty());
    }
}
