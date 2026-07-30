use corepc_client::bitcoin::{Amount, Transaction, TxOut};

use super::constants::{MINT_PROOF_MAGIC, OP_RETURN, PUSH_DATA_LEN};
use super::mint_proof::MintProof;

struct MintProofParser<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> MintProofParser<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn parse(mut self) -> Option<MintProof> {
        if self.read_byte()? != OP_RETURN
            || self.read_byte()? != PUSH_DATA_LEN
            || self.read_array::<3>()? != &MINT_PROOF_MAGIC
        {
            return None;
        }

        let network_id = self.read_byte()?;
        let flame_block_hash = *self.read_array::<32>()?;
        let want_participate_in_consensus = match self.read_byte()? {
            0 => false,
            1 => true,
            _ => return None,
        };

        if self.cursor != self.bytes.len() {
            return None;
        }

        Some(MintProof {
            network_id,
            flame_block_hash,
            want_participate_in_consensus,
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

/// Parses a mint proof from a Bitcoin transaction output.
///
/// The output value must be non-zero. The script must use the exact canonical
/// encoding from the protocol, including the one-byte `0x25` data push. Boolean
/// flag values other than `0` and `1` are rejected.
pub fn parse_mint_proof_output(output: &TxOut) -> Option<MintProof> {
    if output.value == Amount::ZERO {
        return None;
    }

    MintProofParser::new(output.script_pubkey.as_bytes()).parse()
}

/// Returns all valid mint proofs from a Bitcoin transaction, in output order.
pub fn parse_mint_proofs(transaction: &Transaction) -> Vec<MintProof> {
    transaction
        .output
        .iter()
        .filter_map(parse_mint_proof_output)
        .collect()
}
