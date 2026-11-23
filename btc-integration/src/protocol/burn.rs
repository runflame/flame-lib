use crate::protocol::consts::{
    BURN_MAGIC, BURN_PAYLOAD_LEN, BURN_WITH_DURATION_PAYLOAD_LEN, VERSION_V1,
};
use crate::protocol::minter_p2wsh::MinterP2wsh;
use crate::protocol::{push_bytes, read_predicate};
use bitcoin::opcodes::all::OP_RETURN;
use bitcoin::{Amount, ScriptBuf, Transaction, TxOut, Txid};
use flamevm::Predicate;
use readerwriter::{ReadError, Reader};
use std::cell::LazyCell;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Burn {
    amount: Amount,
    txid: Txid,
    output_index: usize,
    data: BurnData,
}

impl Burn {
    pub fn from_tx(transaction: &Transaction) -> Vec<Self> {
        // The txid is a hash of the whole transaction: it is computed only
        // for a transaction that has a burn.
        let txid = LazyCell::new(|| transaction.compute_txid());
        transaction
            .output
            .iter()
            .enumerate()
            .filter_map(|(output_index, output)| {
                let data = BurnData::from_output(output)?;
                Some(Self {
                    amount: output.value,
                    txid: *txid,
                    output_index,
                    data,
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

    pub const fn data(&self) -> &BurnData {
        &self.data
    }
}

#[derive(Clone, Debug)]
pub struct BurnData {
    pub version: u8,
    pub minter_p2wsh: MinterP2wsh,
    pub access_predicate: Predicate,
    pub duration: Option<u16>,
}

impl BurnData {
    pub fn new(minter_p2wsh: MinterP2wsh, access_predicate: Predicate) -> Self {
        Self {
            version: VERSION_V1,
            minter_p2wsh,
            access_predicate,
            duration: None,
        }
    }

    pub fn with_duration(mut self, duration: u16) -> Self {
        self.duration = Some(duration);
        self
    }

    pub fn to_script(&self) -> ScriptBuf {
        let mut payload = Vec::with_capacity(BURN_WITH_DURATION_PAYLOAD_LEN);
        payload.extend_from_slice(&BURN_MAGIC);
        payload.push(self.version);
        payload.extend_from_slice(self.minter_p2wsh.as_bytes());
        payload.extend_from_slice(self.access_predicate.to_point().as_bytes());
        if let Some(duration) = self.duration {
            payload.extend_from_slice(&duration.to_le_bytes());
        }
        ScriptBuf::new_op_return(push_bytes(payload))
    }

    /// The output that burns `amount`.
    pub fn to_output(&self, amount: Amount) -> TxOut {
        TxOut {
            value: amount,
            script_pubkey: self.to_script(),
        }
    }

    pub fn from_output(output: &TxOut) -> Option<Self> {
        if output.value == Amount::ZERO {
            return None;
        }

        let mut reader = output.script_pubkey.as_bytes();
        reader.read_all(Self::read).ok()
    }

    fn read(reader: &mut impl Reader) -> Result<Self, ReadError> {
        if reader.read_u8()? != OP_RETURN.to_u8() {
            return Err(ReadError::InvalidFormat);
        }

        let payload_len = usize::from(reader.read_u8()?);
        if !matches!(
            payload_len,
            BURN_PAYLOAD_LEN | BURN_WITH_DURATION_PAYLOAD_LEN
        ) {
            return Err(ReadError::InvalidFormat);
        }

        let valid =
            reader.read_bytes(BURN_MAGIC.len())? == BURN_MAGIC && reader.read_u8()? == VERSION_V1;
        if !valid {
            return Err(ReadError::InvalidFormat);
        }

        let minter_p2wsh = MinterP2wsh::from(reader.read_u8x32()?);
        let access_predicate = read_predicate(reader)?;
        let duration = match payload_len {
            BURN_PAYLOAD_LEN => None,
            BURN_WITH_DURATION_PAYLOAD_LEN => {
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
            duration,
        })
    }
}

impl PartialEq for BurnData {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
            && self.minter_p2wsh == other.minter_p2wsh
            && self.access_predicate.to_point() == other.access_predicate.to_point()
            && self.duration == other.duration
    }
}

impl Eq for BurnData {}

#[cfg(test)]
mod tests {
    use super::{Burn, BurnData};
    use crate::protocol::consts::{
        BURN_MAGIC, BURN_PAYLOAD_LEN, BURN_WITH_DURATION_PAYLOAD_LEN, VERSION_V1,
    };
    use bitcoin::opcodes::all::OP_RETURN;
    use bitcoin::{Amount, ScriptBuf, Transaction, TxOut, absolute, transaction};
    use flamevm::Predicate;

    #[test]
    fn parses_burn_output_with_little_endian_duration() {
        let output = burn_output(42, Some(0x1234));

        let data = BurnData::from_output(&output).expect("valid burn output");

        assert_eq!(data.version, VERSION_V1);
        assert_eq!(data.minter_p2wsh.as_bytes(), &[0x11; 32]);
        assert_eq!(data.duration, Some(0x1234));
    }

    #[test]
    fn encodes_data_in_the_same_canonical_format_that_it_parses() {
        let access_predicate = Predicate::opaque(Predicate::unspendable_key());
        for (duration, push_opcode, script_len) in [(None, 0x45, 71), (Some(0x1234), 0x47, 73)] {
            let mut data = BurnData::new([0x11; 32].into(), access_predicate.clone());
            data.duration = duration;
            let output = TxOut {
                value: Amount::from_sat(42),
                script_pubkey: data.to_script(),
            };

            assert_eq!(&output.script_pubkey.as_bytes()[..2], &[0x6a, push_opcode]);
            assert_eq!(output.script_pubkey.len(), script_len);
            assert_eq!(BurnData::from_output(&output), Some(data));
        }
    }

    #[test]
    fn rejects_noncanonical_and_malformed_outputs() {
        for duration in [None, Some(0x1234)] {
            let valid = burn_output(42, duration);
            let script = valid.script_pubkey.as_bytes();
            let truncated = script[..script.len() - 1].to_vec();
            let mut trailing = script.to_vec();
            trailing.push(0);
            let mut noncanonical = vec![0x6a, 0x4c];
            noncanonical.extend_from_slice(&script[1..]);
            let mut legacy = vec![0x6a, 0x4c, if duration.is_some() { 103 } else { 101 }];
            legacy.extend_from_slice(&script[2..71]);
            legacy.extend_from_slice(
                ed25519_dalek::SigningKey::from_bytes(&[0x22; 32])
                    .verifying_key()
                    .as_bytes(),
            );
            legacy.extend_from_slice(&script[71..]);

            for bytes in [truncated, trailing, noncanonical, legacy] {
                let output = TxOut {
                    value: valid.value,
                    script_pubkey: ScriptBuf::from_bytes(bytes),
                };
                assert!(BurnData::from_output(&output).is_none());
            }
        }
    }

    #[test]
    fn collects_all_valid_burn_outputs() {
        let transaction = bitcoin_transaction(vec![
            regular_output(),
            burn_output(10, None),
            burn_output(0, None),
            burn_output(20, Some(100)),
        ]);

        let burns = Burn::from_tx(&transaction);

        assert_eq!(burns.len(), 2);
        assert_eq!(burns[0].txid, transaction.compute_txid());
        assert_eq!(burns[0].output_index, 1);
        assert_eq!(burns[0].amount, Amount::from_sat(10));
        assert_eq!(burns[0].data.duration, None);
        assert_eq!(burns[1].output_index, 3);
        assert_eq!(burns[1].amount, Amount::from_sat(20));
        assert_eq!(burns[1].data.duration, Some(100));
    }

    fn burn_output(value: u64, duration: Option<u16>) -> TxOut {
        let access_predicate = Predicate::opaque(Predicate::unspendable_key());
        let payload_len = if duration.is_some() {
            BURN_WITH_DURATION_PAYLOAD_LEN
        } else {
            BURN_PAYLOAD_LEN
        };
        let mut script = Vec::with_capacity(payload_len + 2);
        script.extend_from_slice(&[OP_RETURN.to_u8(), payload_len as u8]);
        script.extend_from_slice(&BURN_MAGIC);
        script.push(VERSION_V1);
        script.extend_from_slice(&[0x11; 32]);
        script.extend_from_slice(access_predicate.to_point().as_bytes());
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
