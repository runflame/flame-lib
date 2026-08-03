use std::collections::BTreeMap;

use corepc_client::bitcoin::{Address, Amount};

use crate::{BlockTip, MintingProofData};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintingProof {
    pub minting_proof_data: MintingProofData,
    pub burned_amount: Amount,
    pub btc_sender_address: Address,
    pub bitcoin_block_tip: BlockTip,
}

pub trait MintingProofStorage {
    fn insert(&mut self, flame_block_hash: [u8; 32], minting_proof: MintingProof);

    fn get(&self, flame_block_hash: [u8; 32]) -> Vec<MintingProof>;
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
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use corepc_client::bitcoin::{Address, Amount, BlockHash, Network};

    use super::*;

    fn proof(block_hash: [u8; 32], burned_sats: u64, bitcoin_height: u64) -> MintingProof {
        let btc_sender_address = Address::from_str("bcrt1qcmnpjjjw78yhyjrxtql6lk7pzpujs3h244p7ae")
            .expect("valid address")
            .require_network(Network::Regtest)
            .expect("regtest address");

        MintingProof {
            minting_proof_data: MintingProofData {
                network_id: 7,
                flame_block_hash: block_hash,
                want_participate_in_consensus: true,
            },
            burned_amount: Amount::from_sat(burned_sats),
            btc_sender_address,
            bitcoin_block_tip: BlockTip {
                hash: BlockHash::from_str(
                    "0000000000000000000000000000000000000000000000000000000000000001",
                )
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
}
