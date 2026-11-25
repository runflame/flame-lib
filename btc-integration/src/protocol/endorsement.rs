use crate::protocol::consts::{MINTING_MAGIC, MINTING_PAYLOAD_LEN, VERSION_V1};
use crate::protocol::minter_identity::MinterIdentity;
use bitcoin::opcodes::all::{OP_PUSHBYTES_41, OP_RETURN};
use bitcoin::{Amount, OutPoint, ScriptBuf, Transaction, TxIn, TxOut, Txid};
use flamechain::{BlockHash, CoreFlameHeight};
use readerwriter::{ReadError, Reader};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UncheckedMintingEndorsement {
    txid: Txid,
    output: MintingEndorsementOutput,
    auth: MintingEndorsementAuth,
}

impl UncheckedMintingEndorsement {
    pub fn from_tx(transaction: &Transaction) -> Vec<Self> {
        let mut outputs =
            transaction
                .output
                .iter()
                .enumerate()
                .filter_map(|(output_index, output)| {
                    MintingEndorsementOutput::from_output(output_index, output)
                });

        let Some(output) = outputs.next() else {
            return Vec::new();
        };
        if outputs.next().is_some() {
            return Vec::new();
        }

        let txid = transaction.compute_txid();
        transaction
            .input
            .iter()
            .enumerate()
            .filter_map(|(input_index, input)| {
                Some(Self {
                    txid,
                    output: output.clone(),
                    auth: MintingEndorsementAuth::from_input(input_index, input)?,
                })
            })
            .collect()
    }

    pub fn txid(&self) -> Txid {
        self.txid
    }

    pub fn output(&self) -> &MintingEndorsementOutput {
        &self.output
    }

    pub fn auth(&self) -> &MintingEndorsementAuth {
        &self.auth
    }
}

#[derive(Clone, Debug)]
pub struct MintingEndorsementAuth {
    pub input_index: usize,
    pub previous_output: OutPoint,
    minter: MinterIdentity,
}

impl MintingEndorsementAuth {
    pub fn from_input(input_index: usize, input: &TxIn) -> Option<Self> {
        let witness_script = input.witness.last()?;
        let minter = MinterIdentity::new(ScriptBuf::from_bytes(witness_script.to_vec())).ok()?;
        Some(Self {
            input_index,
            previous_output: input.previous_output,
            minter,
        })
    }

    pub const fn minter(&self) -> &MinterIdentity {
        &self.minter
    }
}

impl PartialEq for MintingEndorsementAuth {
    fn eq(&self, other: &Self) -> bool {
        self.input_index == other.input_index
            && self.previous_output == other.previous_output
            && self.minter == other.minter
    }
}

impl Eq for MintingEndorsementAuth {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintingEndorsementOutput {
    pub output_index: u32,
    pub data: MintingEndorsementData,
}

impl MintingEndorsementOutput {
    pub fn from_output(output_index: usize, output: &TxOut) -> Option<Self> {
        Some(Self {
            output_index: u32::try_from(output_index).ok()?,
            data: MintingEndorsementData::from_output(output)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MintingEndorsementData {
    V1 {
        flame_block_height: u32,
        flame_block_hash: BlockHash,
    },
}

impl MintingEndorsementData {
    pub fn block_height(&self) -> CoreFlameHeight {
        match self {
            Self::V1 {
                flame_block_height, ..
            } => CoreFlameHeight::new(*flame_block_height),
        }
    }

    pub fn block_hash(&self) -> BlockHash {
        match self {
            Self::V1 {
                flame_block_hash, ..
            } => *flame_block_hash,
        }
    }

    pub fn to_script(&self) -> ScriptBuf {
        let Self::V1 {
            flame_block_height,
            flame_block_hash,
        } = self;
        let mut script = Vec::with_capacity(2 + MINTING_PAYLOAD_LEN);
        script.extend_from_slice(&[OP_RETURN.to_u8(), OP_PUSHBYTES_41.to_u8()]);
        script.extend_from_slice(&MINTING_MAGIC);
        script.push(VERSION_V1);
        script.extend_from_slice(&flame_block_height.to_le_bytes());
        script.extend_from_slice(flame_block_hash.as_bytes());
        ScriptBuf::from_bytes(script)
    }

    /// The output that carries the endorsement. It holds no value.
    pub fn to_output(&self) -> TxOut {
        TxOut {
            value: Amount::ZERO,
            script_pubkey: self.to_script(),
        }
    }

    pub(crate) fn from_output(output: &TxOut) -> Option<Self> {
        let mut reader = output.script_pubkey.as_bytes();
        reader.read_all(Self::read).ok()
    }

    fn read(reader: &mut impl Reader) -> Result<Self, ReadError> {
        let valid = reader.read_u8()? == OP_RETURN.to_u8()
            && reader.read_u8()? == OP_PUSHBYTES_41.to_u8()
            && reader.read_bytes(MINTING_MAGIC.len())? == MINTING_MAGIC
            && reader.read_u8()? == VERSION_V1;

        if !valid {
            return Err(ReadError::InvalidFormat);
        }

        Ok(Self::V1 {
            flame_block_height: reader.read_u32()?,
            flame_block_hash: BlockHash::from(reader.read_u8x32()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{MintingEndorsementData, UncheckedMintingEndorsement};
    use crate::protocol::{consts::VERSION_V1, minter_witness_script};
    use bitcoin::{
        Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness, absolute,
        transaction,
    };
    use flamechain::BlockHash;
    use flamevm::Predicate;

    #[test]
    fn one_endorsement_input_and_one_endorsement_output() {
        let transaction = transaction(vec![endorsement_input()], vec![endorsement_output()]);

        let endorsements = UncheckedMintingEndorsement::from_tx(&transaction);

        assert_eq!(endorsements.len(), 1);
        assert_eq!(endorsements[0].txid, transaction.compute_txid());
        assert_eq!(endorsements[0].auth.input_index, 0);
        assert_eq!(endorsements[0].output.output_index, 0);
    }

    #[test]
    fn finds_one_endorsement_among_multiple_inputs_and_outputs() {
        let transaction = transaction(
            vec![regular_input(), endorsement_input(), regular_input()],
            vec![regular_output(), endorsement_output(), regular_output()],
        );

        let endorsements = UncheckedMintingEndorsement::from_tx(&transaction);

        assert_eq!(endorsements.len(), 1);
        assert_eq!(endorsements[0].auth.input_index, 1);
        assert_eq!(endorsements[0].output.output_index, 1);
    }

    #[test]
    fn multiple_endorsement_inputs_and_one_endorsement_output() {
        let transaction = transaction(
            vec![
                endorsement_input(),
                endorsement_input(),
                endorsement_input(),
            ],
            vec![endorsement_output()],
        );

        let endorsements = UncheckedMintingEndorsement::from_tx(&transaction);

        assert_eq!(endorsements.len(), 3);
        assert_eq!(
            endorsements
                .iter()
                .map(|endorsement| endorsement.auth.input_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(
            endorsements
                .iter()
                .all(|endorsement| endorsement.output.output_index == 0)
        );
    }

    #[test]
    fn one_endorsement_input_and_multiple_endorsement_outputs_produces_no_endorsements() {
        let transaction = transaction(
            vec![endorsement_input()],
            vec![endorsement_output(), endorsement_output(), regular_output()],
        );

        assert!(UncheckedMintingEndorsement::from_tx(&transaction).is_empty());
    }

    fn transaction(inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: inputs,
            output: outputs,
        }
    }

    fn endorsement_input() -> TxIn {
        let flame_predicate = Predicate::opaque(Predicate::unspendable_key());
        let witness_script = minter_witness_script::build_with_authorization(
            &flame_predicate,
            &ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
            bitcoin::Script::from_bytes(&[0x51]),
        )
        .into_bytes();

        TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[witness_script]),
        }
    }

    fn regular_input() -> TxIn {
        TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }
    }

    fn endorsement_output() -> TxOut {
        TxOut {
            value: Amount::ZERO,
            script_pubkey: MintingEndorsementData::V1 {
                flame_block_height: 0x1234_5678,
                flame_block_hash: BlockHash::from([VERSION_V1; 32]),
            }
            .to_script(),
        }
    }

    fn regular_output() -> TxOut {
        TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new(),
        }
    }
}
