use crate::protocol::consts::{
    ACQUISITION_MAGIC, ACQUISITION_PAYLOAD_LEN, ACQUISITION_WITH_DURATION_PAYLOAD_LEN,
    OP_PUSHDATA1, OP_RETURN, VERSION_V1,
};
use crate::protocol::minter_p2wsh::{MinterP2wsh, parse_predicate};
use corepc_client::bitcoin::{Amount, ScriptBuf, Transaction, TxOut, Txid};
use ed25519_dalek::VerifyingKey;
use flamevm::Predicate;
use readerwriter::{ReadError, Reader};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Acquisition {
    amount: Amount,
    txid: Txid,
    output_index: usize,
    data: AcquisitionData,
}

impl Acquisition {
    pub fn from_tx(transaction: &Transaction) -> Vec<Self> {
        let txid = transaction.compute_txid();
        transaction
            .output
            .iter()
            .enumerate()
            .filter_map(|(output_index, output)| {
                Some(Self {
                    amount: output.value,
                    txid,
                    output_index,
                    data: AcquisitionData::from_output(output)?,
                })
            })
            .collect()
    }

    pub const fn amount(&self) -> Amount {
        self.amount
    }

    pub const fn txid(&self) -> Txid {
        self.txid
    }

    pub const fn output_index(&self) -> usize {
        self.output_index
    }

    pub const fn data(&self) -> &AcquisitionData {
        &self.data
    }
}

#[derive(Clone, Debug)]
pub struct AcquisitionData {
    pub version: u8,
    pub minter_p2wsh: MinterP2wsh,
    pub access_predicate: Predicate,
    pub validator_pubkey: VerifyingKey,
    pub duration: Option<u16>,
}

impl AcquisitionData {
    pub fn new(
        minter_p2wsh: MinterP2wsh,
        access_predicate: Predicate,
        validator_pubkey: VerifyingKey,
    ) -> Self {
        Self {
            version: VERSION_V1,
            minter_p2wsh,
            access_predicate,
            validator_pubkey,
            duration: None,
        }
    }

    pub fn with_duration(mut self, duration: u16) -> Self {
        self.duration = Some(duration);
        self
    }

    pub fn to_script(&self) -> ScriptBuf {
        let payload_len = if self.duration.is_some() {
            ACQUISITION_WITH_DURATION_PAYLOAD_LEN
        } else {
            ACQUISITION_PAYLOAD_LEN
        };
        let mut script = Vec::with_capacity(3 + payload_len);
        script.extend_from_slice(&[OP_RETURN, OP_PUSHDATA1, payload_len as u8]);
        script.extend_from_slice(&ACQUISITION_MAGIC);
        script.push(self.version);
        script.extend_from_slice(self.minter_p2wsh.as_bytes());
        script.extend_from_slice(self.access_predicate.to_point().as_bytes());
        script.extend_from_slice(self.validator_pubkey.as_bytes());
        if let Some(duration) = self.duration {
            script.extend_from_slice(&duration.to_le_bytes());
        }
        ScriptBuf::from_bytes(script)
    }

    pub fn from_output(output: &TxOut) -> Option<Self> {
        if output.value == Amount::ZERO {
            return None;
        }

        let mut reader = output.script_pubkey.as_bytes();
        reader.read_all(Self::read).ok()
    }

    fn read(reader: &mut impl Reader) -> Result<Self, ReadError> {
        if reader.read_u8()? != OP_RETURN || reader.read_u8()? != OP_PUSHDATA1 {
            return Err(ReadError::InvalidFormat);
        }

        let payload_len = usize::from(reader.read_u8()?);
        if !matches!(
            payload_len,
            ACQUISITION_PAYLOAD_LEN | ACQUISITION_WITH_DURATION_PAYLOAD_LEN
        ) {
            return Err(ReadError::InvalidFormat);
        }

        let valid = reader.read_bytes(ACQUISITION_MAGIC.len())? == ACQUISITION_MAGIC
            && reader.read_u8()? == VERSION_V1;
        if !valid {
            return Err(ReadError::InvalidFormat);
        }

        let minter_p2wsh = MinterP2wsh::from(reader.read_u8x32()?);
        let access_predicate =
            parse_predicate(&reader.read_u8x32()?).ok_or(ReadError::InvalidFormat)?;
        let validator_pubkey = VerifyingKey::from_bytes(&reader.read_u8x32()?)
            .map_err(|_| ReadError::InvalidFormat)?;
        let duration = match payload_len {
            ACQUISITION_PAYLOAD_LEN => None,
            ACQUISITION_WITH_DURATION_PAYLOAD_LEN => {
                // TODO: readerwriter does not have read_u16 method
                let mut bytes = [0; size_of::<u16>()];
                reader.read(&mut bytes)?;
                Some(u16::from_le_bytes(bytes))
            }
            _ => return Err(ReadError::InvalidFormat),
        };

        Ok(Self {
            version: VERSION_V1,
            minter_p2wsh,
            access_predicate,
            validator_pubkey,
            duration,
        })
    }
}

impl PartialEq for AcquisitionData {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
            && self.minter_p2wsh == other.minter_p2wsh
            && self.access_predicate.to_point() == other.access_predicate.to_point()
            && self.validator_pubkey == other.validator_pubkey
            && self.duration == other.duration
    }
}

impl Eq for AcquisitionData {}

#[cfg(test)]
mod tests {
    use super::{Acquisition, AcquisitionData};
    use crate::protocol::consts::{
        ACQUISITION_MAGIC, ACQUISITION_PAYLOAD_LEN, ACQUISITION_WITH_DURATION_PAYLOAD_LEN,
        OP_PUSHDATA1, OP_RETURN, VERSION_V1,
    };
    use corepc_client::bitcoin::{Amount, ScriptBuf, Transaction, TxOut, absolute, transaction};
    use ed25519_dalek::SigningKey;
    use flamevm::Predicate;

    #[test]
    fn parses_acquisition_output_with_little_endian_duration() {
        let output = acquisition_output(42, Some(0x1234));

        let data = AcquisitionData::from_output(&output).expect("valid acquisition output");

        assert_eq!(data.version, VERSION_V1);
        assert_eq!(data.minter_p2wsh.as_bytes(), &[0x11; 32]);
        assert_eq!(data.duration, Some(0x1234));
        assert_eq!(
            data.validator_pubkey,
            SigningKey::from_bytes(&[0x22; 32]).verifying_key()
        );
    }

    #[test]
    fn encodes_data_in_the_same_canonical_format_that_it_parses() {
        let access_predicate = Predicate::opaque(Predicate::unspendable_key());
        let data = AcquisitionData::new(
            [0x11; 32].into(),
            access_predicate,
            SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        )
        .with_duration(0x1234);
        let output = TxOut {
            value: Amount::from_sat(42),
            script_pubkey: data.to_script(),
        };

        assert_eq!(AcquisitionData::from_output(&output), Some(data));
    }

    #[test]
    fn collects_all_valid_acquisition_outputs() {
        let transaction = bitcoin_transaction(vec![
            regular_output(),
            acquisition_output(10, None),
            acquisition_output(0, None),
            acquisition_output(20, Some(100)),
        ]);

        let acquisitions = Acquisition::from_tx(&transaction);

        assert_eq!(acquisitions.len(), 2);
        assert_eq!(acquisitions[0].txid, transaction.compute_txid());
        assert_eq!(acquisitions[0].output_index, 1);
        assert_eq!(acquisitions[0].amount, Amount::from_sat(10));
        assert_eq!(acquisitions[0].data.duration, None);
        assert_eq!(acquisitions[1].output_index, 3);
        assert_eq!(acquisitions[1].amount, Amount::from_sat(20));
        assert_eq!(acquisitions[1].data.duration, Some(100));
    }

    fn acquisition_output(value: u64, duration: Option<u16>) -> TxOut {
        let access_predicate = Predicate::opaque(Predicate::unspendable_key());
        let validator_pubkey = SigningKey::from_bytes(&[0x22; 32]).verifying_key();
        let payload_len = if duration.is_some() {
            ACQUISITION_WITH_DURATION_PAYLOAD_LEN
        } else {
            ACQUISITION_PAYLOAD_LEN
        };
        let mut script = Vec::with_capacity(payload_len + 3);
        script.extend_from_slice(&[OP_RETURN, OP_PUSHDATA1, payload_len as u8]);
        script.extend_from_slice(&ACQUISITION_MAGIC);
        script.push(VERSION_V1);
        script.extend_from_slice(&[0x11; 32]);
        script.extend_from_slice(access_predicate.to_point().as_bytes());
        script.extend_from_slice(validator_pubkey.as_bytes());
        if let Some(duration) = duration {
            script.extend_from_slice(&duration.to_le_bytes());
        }

        TxOut {
            value: Amount::from_sat(value),
            script_pubkey: ScriptBuf::from_bytes(script),
        }
    }

    fn regular_output() -> TxOut {
        TxOut {
            value: Amount::from_sat(1),
            script_pubkey: ScriptBuf::new(),
        }
    }

    fn bitcoin_transaction(output: Vec<TxOut>) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: Vec::new(),
            output,
        }
    }
}
