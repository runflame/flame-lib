use corepc_client::bitcoin::{Amount, Transaction, TxOut};
use curve25519_dalek::ristretto::CompressedRistretto;
use ed25519_dalek::VerifyingKey;
use flamevm::Predicate;

use crate::mint_proofs::core::constants::{
    MINT_PROOF_DATA_LEN, MINT_PROOF_MAGIC, OP_PUSHDATA1, OP_RETURN,
    PARTICIPATING_MINT_PROOF_DATA_LEN,
};
use crate::mint_proofs::core::minting_proof_data::MintingProofData;

struct MintProofParser<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> MintProofParser<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn parse(mut self) -> Option<MintingProofData> {
        if self.read_byte()? != OP_RETURN {
            return None;
        }

        let payload_len = match self.read_byte()? {
            length if usize::from(length) == MINT_PROOF_DATA_LEN => MINT_PROOF_DATA_LEN,
            OP_PUSHDATA1 if usize::from(self.read_byte()?) == PARTICIPATING_MINT_PROOF_DATA_LEN => {
                PARTICIPATING_MINT_PROOF_DATA_LEN
            }
            _ => return None,
        };
        let payload_end = self.cursor.checked_add(payload_len)?;

        if self.read_array::<3>()? != &MINT_PROOF_MAGIC {
            return None;
        }

        let network_id = self.read_byte()?;
        let flame_block_hash = *self.read_array::<32>()?;
        let flame_address = Predicate::opaque(CompressedRistretto(*self.read_array::<32>()?));
        let validator_pubkey = match payload_len {
            PARTICIPATING_MINT_PROOF_DATA_LEN => {
                Some(VerifyingKey::from_bytes(self.read_array::<32>()?).ok()?)
            }
            MINT_PROOF_DATA_LEN => None,
            _ => return None,
        };

        if self.cursor != payload_end || payload_end != self.bytes.len() {
            return None;
        }

        Some(MintingProofData {
            network_id,
            flame_block_hash,
            flame_reward_address: flame_address,
            validator_pubkey,
        })
    }

    fn read_byte(&mut self) -> Option<u8> {
        Some(self.read_array::<1>()?[0])
    }

    fn read_array<const N: usize>(&mut self) -> Option<&'a [u8; N]> {
        let end = self.cursor + N;
        let result = self.bytes.get(self.cursor..end)?.try_into().ok()?;
        self.cursor = end;
        Some(result)
    }
}

pub fn parse_mint_proof_output(output: &TxOut) -> Option<MintingProofData> {
    if output.value == Amount::ZERO {
        return None;
    }

    MintProofParser::new(output.script_pubkey.as_bytes()).parse()
}

pub fn parse_mint_proofs(transaction: &Transaction) -> Vec<MintingProofData> {
    transaction
        .output
        .iter()
        .filter_map(parse_mint_proof_output)
        .collect()
}
