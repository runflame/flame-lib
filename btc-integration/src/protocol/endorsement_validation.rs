use crate::protocol::endorsement::{
    MintingEndorsementAuth, MintingEndorsementOutput, UncheckedMintingEndorsement,
};
use bitcoin::{OutPoint, Transaction, TxOut, Txid};
use flamechain::{BlockHash, CoreBlockTip, CoreFlameHeight};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedMintingEndorsement {
    unchecked: UncheckedMintingEndorsement,
}

impl AuthenticatedMintingEndorsement {
    pub fn txid(&self) -> Txid {
        self.unchecked.txid()
    }

    pub fn output(&self) -> &MintingEndorsementOutput {
        self.unchecked.output()
    }

    pub fn auth(&self) -> &MintingEndorsementAuth {
        self.unchecked.auth()
    }

    pub fn block_height(&self) -> CoreFlameHeight {
        self.output().data.block_height()
    }

    pub fn block_hash(&self) -> BlockHash {
        self.output().data.block_hash()
    }

    pub fn block_tip(&self) -> CoreBlockTip {
        CoreBlockTip {
            height: self.block_height(),
            hash: self.block_hash(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MintingEndorsementValidator;

impl MintingEndorsementValidator {
    pub fn validate(
        unchecked: UncheckedMintingEndorsement,
        prevout: &TxOut,
    ) -> Result<AuthenticatedMintingEndorsement, MintingEndorsementValidationError> {
        if prevout.script_pubkey != unchecked.auth().minter().witness_script().to_p2wsh() {
            return Err(MintingEndorsementValidationError::InvalidPrevoutScript {
                input_index: unchecked.auth().input_index,
            });
        }

        Ok(AuthenticatedMintingEndorsement { unchecked })
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MintingEndorsementValidationError {
    #[error("input {input_index} does not spend the expected native P2WSH output")]
    InvalidPrevoutScript { input_index: usize },
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MintingEndorsementProcessingError {
    #[error(
        "prevout entry count {prevout_count} does not match transaction count {transaction_count}"
    )]
    TransactionCountMismatch {
        transaction_count: usize,
        prevout_count: usize,
    },
    #[error("prevout count {prevout_count} does not match input count {input_count}")]
    InputCountMismatch {
        input_count: usize,
        prevout_count: usize,
    },
    #[error("prevout for input {input_index} ({previous_output}) is unavailable")]
    MissingPrevout {
        input_index: usize,
        previous_output: OutPoint,
    },
}

pub fn validate_transaction_endorsements(
    transaction: &Transaction,
    prevouts: &[Option<TxOut>],
) -> Result<Vec<AuthenticatedMintingEndorsement>, MintingEndorsementProcessingError> {
    let input_count = transaction.input.len();
    if prevouts.len() != input_count {
        return Err(MintingEndorsementProcessingError::InputCountMismatch {
            input_count,
            prevout_count: prevouts.len(),
        });
    }

    let unchecked_endorsements = UncheckedMintingEndorsement::from_tx(transaction);
    let mut endorsements = Vec::new();

    for unchecked in unchecked_endorsements {
        let input_index = unchecked.auth().input_index;
        let prevout = prevouts[input_index].as_ref().ok_or(
            MintingEndorsementProcessingError::MissingPrevout {
                input_index,
                previous_output: unchecked.auth().previous_output,
            },
        )?;

        if let Ok(endorsement) = MintingEndorsementValidator::validate(unchecked, prevout) {
            endorsements.push(endorsement);
        }
    }

    Ok(endorsements)
}

#[cfg(test)]
mod tests {
    use super::{
        MintingEndorsementProcessingError, MintingEndorsementValidationError,
        MintingEndorsementValidator, validate_transaction_endorsements,
    };
    use crate::protocol::endorsement::{MintingEndorsementData, UncheckedMintingEndorsement};
    use crate::protocol::minter_p2wsh::MinterP2wsh;
    use crate::protocol::minter_witness_script;
    use bitcoin::{
        Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness, absolute,
        transaction,
    };
    use flamechain::BlockHash;
    use flamevm::Predicate;

    #[test]
    fn validator_accepts_only_matching_native_p2wsh_prevout() {
        let witness_script = endorsement_witness_script();
        let transaction = transaction(
            vec![endorsement_input(&witness_script)],
            vec![endorsement_output()],
        );
        let unchecked = UncheckedMintingEndorsement::from_tx(&transaction)
            .pop()
            .expect("unchecked endorsement");

        assert_eq!(
            MintingEndorsementValidator::validate(unchecked.clone(), &regular_prevout()),
            Err(MintingEndorsementValidationError::InvalidPrevoutScript { input_index: 0 })
        );

        let endorsement =
            MintingEndorsementValidator::validate(unchecked, &p2wsh_prevout(&witness_script))
                .expect("matching P2WSH prevout");
        assert_eq!(endorsement.auth().input_index, 0);
    }

    #[test]
    fn missing_candidate_prevout_fails_closed() {
        let witness_script = endorsement_witness_script();
        let transaction = transaction(
            vec![endorsement_input(&witness_script)],
            vec![endorsement_output()],
        );
        let previous_output = transaction.input[0].previous_output;

        assert_eq!(
            validate_transaction_endorsements(&transaction, &[None]),
            Err(MintingEndorsementProcessingError::MissingPrevout {
                input_index: 0,
                previous_output,
            })
        );
    }

    fn transaction(inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: inputs,
            output: outputs,
        }
    }

    fn endorsement_witness_script() -> Vec<u8> {
        let predicate = Predicate::opaque(Predicate::unspendable_key());
        minter_witness_script::build_with_authorization(
            &predicate,
            &ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
            bitcoin::Script::from_bytes(&[0x51]),
        )
        .into_bytes()
    }

    fn endorsement_input(witness_script: &[u8]) -> TxIn {
        TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[witness_script]),
        }
    }

    fn endorsement_output() -> TxOut {
        TxOut {
            value: Amount::ZERO,
            script_pubkey: MintingEndorsementData::V1 {
                flame_block_height: 42,
                flame_block_hash: BlockHash::from([0x42; 32]),
            }
            .to_script(),
        }
    }

    fn p2wsh_prevout(witness_script: &[u8]) -> TxOut {
        let minter_p2wsh = MinterP2wsh::from_witness_script(witness_script);
        let mut script_pubkey = Vec::with_capacity(34);
        script_pubkey.extend_from_slice(&[0x00, 0x20]);
        script_pubkey.extend_from_slice(minter_p2wsh.as_bytes());
        TxOut {
            value: Amount::from_sat(1),
            script_pubkey: ScriptBuf::from_bytes(script_pubkey),
        }
    }

    fn regular_prevout() -> TxOut {
        TxOut {
            value: Amount::from_sat(1),
            script_pubkey: ScriptBuf::new(),
        }
    }
}
