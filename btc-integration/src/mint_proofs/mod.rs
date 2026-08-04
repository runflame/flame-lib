//! Flame mint proofs embedded in Bitcoin transaction outputs.

mod core;
pub mod indexer;
pub mod mint_proof_sender;
pub mod minting_proof_storage;

pub use indexer as mint_proof_indexer;

pub use core::constants::{MINT_PROOF_DATA_LEN, MINT_PROOF_MAGIC};
pub use core::creation::mint_proof_to_script;
pub use core::minting_proof_data::MintingProofData;
pub use core::parser::{parse_mint_proof_output, parse_mint_proofs};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mint_proofs::core::constants;
    use corepc_client::bitcoin::{Amount, ScriptBuf, TxOut};

    fn proof(flag: bool) -> MintingProofData {
        MintingProofData {
            network_id: 7,
            flame_block_hash: [0xab; 32],
            want_participate_in_consensus: flag,
        }
    }

    fn output(value: u64, script_pubkey: ScriptBuf) -> TxOut {
        TxOut {
            value: Amount::from_sat(value),
            script_pubkey,
        }
    }

    #[test]
    fn encodes_the_canonical_script() {
        let script = proof(true).to_script();
        let bytes = script.as_bytes();

        assert_eq!(bytes.len(), constants::SCRIPT_LEN);
        assert_eq!(&bytes[..6], &[0x6a, 0x25, b'F', b'L', b'M', 7]);
        assert_eq!(&bytes[6..38], &[0xab; 32]);
        assert_eq!(bytes[38], 1);
    }

    #[test]
    fn parses_valid_output() {
        let expected = proof(false);
        let tx_out = output(1, expected.to_script());

        assert_eq!(MintingProofData::from_tx_out(&tx_out), Some(expected));
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
        wrong_push_length[1] = 0x24;
        cases.push(wrong_push_length);

        let mut wrong_magic = valid.clone();
        wrong_magic[2] = b'X';
        cases.push(wrong_magic);

        let mut invalid_flag = valid.clone();
        invalid_flag[constants::SCRIPT_LEN - 1] = 2;
        cases.push(invalid_flag);

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
