//! Flame mint proofs embedded in Bitcoin transaction outputs.

mod core;
pub mod indexer;
pub mod mint_proof_sender;
pub mod minting_proof_storage;

pub use indexer as mint_proof_indexer;

pub use core::constants::{
    MINT_PROOF_DATA_LEN, MINT_PROOF_MAGIC, PARTICIPATING_MINT_PROOF_DATA_LEN,
};
pub use core::creation::mint_proof_to_script;
pub use core::minting_proof_data::MintingProofData;
pub use core::parser::{parse_mint_proof_output, parse_mint_proofs};

#[cfg(test)]
mod tests {
    use super::*;
    use corepc_client::bitcoin::{Amount, ScriptBuf, TxOut};
    use ed25519_dalek::SigningKey;
    use flamevm::Predicate;

    fn proof(flag: bool) -> MintingProofData {
        MintingProofData {
            network_id: 7,
            flame_block_hash: [0xab; 32],
            flame_reward_address: Predicate::opaque(Predicate::unspendable_key()),
            validator_pubkey: flag.then(|| SigningKey::from_bytes(&[0x42; 32]).verifying_key()),
        }
    }

    fn output(value: u64, script_pubkey: ScriptBuf) -> TxOut {
        TxOut {
            value: Amount::from_sat(value),
            script_pubkey,
        }
    }

    #[test]
    fn encodes_the_canonical_non_participating_script() {
        let proof = proof(false);
        let script = proof.to_script();
        let bytes = script.as_bytes();

        assert_eq!(bytes.len(), 70);
        assert_eq!(&bytes[..6], &[0x6a, 0x44, b'F', b'L', b'M', 7]);
        assert_eq!(&bytes[6..38], &[0xab; 32]);
        assert_eq!(
            &bytes[38..70],
            proof.flame_reward_address.to_point().as_bytes()
        );
    }

    #[test]
    fn encodes_the_canonical_participating_script_with_validator_key() {
        let proof = proof(true);
        let script = proof.to_script();
        let bytes = script.as_bytes();

        assert_eq!(bytes.len(), 103);
        assert_eq!(&bytes[..7], &[0x6a, 0x4c, 0x64, b'F', b'L', b'M', 7]);
        assert_eq!(&bytes[7..39], &[0xab; 32]);
        assert_eq!(
            &bytes[39..71],
            proof.flame_reward_address.to_point().as_bytes()
        );
        assert_eq!(
            &bytes[71..103],
            proof.validator_pubkey.as_ref().unwrap().as_bytes()
        );
    }

    #[test]
    fn parses_valid_output() {
        for expected in [proof(false), proof(true)] {
            let tx_out = output(1, expected.to_script());

            assert_eq!(MintingProofData::from_tx_out(&tx_out), Some(expected));
        }
    }

    #[test]
    fn rejects_zero_value_output() {
        let tx_out = output(0, proof(true).to_script());

        assert_eq!(parse_mint_proof_output(&tx_out), None);
    }

    #[test]
    fn rejects_malformed_scripts() {
        let valid = proof(true).to_script().into_bytes();
        let mut cases = Vec::new();

        let mut wrong_opcode = valid.clone();
        wrong_opcode[0] = 0x00;
        cases.push(wrong_opcode);

        let mut wrong_push_length = valid.clone();
        wrong_push_length[2] = 0x63;
        cases.push(wrong_push_length);

        let mut wrong_magic = valid.clone();
        wrong_magic[3] = b'X';
        cases.push(wrong_magic);

        let mut invalid_validator_pubkey = valid.clone();
        invalid_validator_pubkey[71..].fill(2);
        cases.push(invalid_validator_pubkey);

        let mut trailing_data = valid;
        trailing_data.push(0);
        cases.push(trailing_data);

        for bytes in cases {
            let tx_out = output(1, ScriptBuf::from_bytes(bytes));
            assert_eq!(parse_mint_proof_output(&tx_out), None);
        }
    }

    #[test]
    fn rejects_every_truncated_script() {
        let valid = proof(true).to_script().into_bytes();

        for length in 0..valid.len() {
            let tx_out = output(1, ScriptBuf::from_bytes(valid[..length].to_vec()));
            assert_eq!(
                parse_mint_proof_output(&tx_out),
                None,
                "accepted script truncated to {length} bytes"
            );
        }
    }
}
