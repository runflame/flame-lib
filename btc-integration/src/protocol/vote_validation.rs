use crate::btc::rpc::BtcTransactionWithPrevouts;
use crate::protocol::vote::{MintingVoteAuth, MintingVoteOutput, UncheckedMintingVote};
use corepc_client::bitcoin::{OutPoint, TxOut, Txid};
use flamechain::BlockHash;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedMintingVote {
    unchecked: UncheckedMintingVote,
}

impl AuthenticatedMintingVote {
    pub fn txid(&self) -> Txid {
        self.unchecked.txid()
    }

    pub fn output(&self) -> &MintingVoteOutput {
        self.unchecked.output()
    }

    pub fn auth(&self) -> &MintingVoteAuth {
        self.unchecked.auth()
    }

    pub fn block_height(&self) -> u32 {
        self.output().data.block_height()
    }

    pub fn block_hash(&self) -> BlockHash {
        self.output().data.block_hash()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MintingVoteValidator;

impl MintingVoteValidator {
    pub fn validate(
        unchecked: UncheckedMintingVote,
        prevout: &TxOut,
    ) -> Result<AuthenticatedMintingVote, MintingVoteValidationError> {
        if prevout.script_pubkey != unchecked.auth().minter().witness_script().to_p2wsh() {
            return Err(MintingVoteValidationError::InvalidPrevoutScript {
                input_index: unchecked.auth().input_index,
            });
        }

        Ok(AuthenticatedMintingVote { unchecked })
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MintingVoteValidationError {
    #[error("input {input_index} does not spend the expected native P2WSH output")]
    InvalidPrevoutScript { input_index: usize },
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MintingVoteProcessingError {
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

pub fn validate_transaction_votes(
    entry: &BtcTransactionWithPrevouts,
) -> Result<Vec<AuthenticatedMintingVote>, MintingVoteProcessingError> {
    let input_count = entry.transaction.input.len();
    if entry.prevouts.len() != input_count {
        return Err(MintingVoteProcessingError::InputCountMismatch {
            input_count,
            prevout_count: entry.prevouts.len(),
        });
    }

    let unchecked_votes = UncheckedMintingVote::from_tx(&entry.transaction);
    let mut votes = Vec::new();

    for unchecked in unchecked_votes {
        let input_index = unchecked.auth().input_index;
        let prevout = entry.prevouts[input_index].as_ref().ok_or(
            MintingVoteProcessingError::MissingPrevout {
                input_index,
                previous_output: unchecked.auth().previous_output,
            },
        )?;

        if let Ok(vote) = MintingVoteValidator::validate(unchecked, prevout) {
            votes.push(vote);
        }
    }

    Ok(votes)
}

#[cfg(test)]
mod tests {
    use super::{
        MintingVoteProcessingError, MintingVoteValidationError, MintingVoteValidator,
        validate_transaction_votes,
    };
    use crate::btc::rpc::BtcTransactionWithPrevouts;
    use crate::protocol::minter_p2wsh::MinterP2wsh;
    use crate::protocol::minter_witness_script;
    use crate::protocol::vote::{MintingVoteData, UncheckedMintingVote};
    use corepc_client::bitcoin::{
        Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness, absolute,
        transaction,
    };
    use flamechain::BlockHash;
    use flamevm::Predicate;

    #[test]
    fn validator_accepts_only_matching_native_p2wsh_prevout() {
        let witness_script = vote_witness_script();
        let transaction = transaction(vec![vote_input(&witness_script)], vec![vote_output()]);
        let unchecked = UncheckedMintingVote::from_tx(&transaction)
            .pop()
            .expect("unchecked vote");

        assert_eq!(
            MintingVoteValidator::validate(unchecked.clone(), &regular_prevout()),
            Err(MintingVoteValidationError::InvalidPrevoutScript { input_index: 0 })
        );

        let vote = MintingVoteValidator::validate(unchecked, &p2wsh_prevout(&witness_script))
            .expect("matching P2WSH prevout");
        assert_eq!(vote.auth().input_index, 0);
    }

    #[test]
    fn preserves_multiple_valid_inputs_from_the_same_minter() {
        let witness_script = vote_witness_script();
        let transaction = transaction(
            vec![vote_input(&witness_script), vote_input(&witness_script)],
            vec![vote_output()],
        );
        let entry = BtcTransactionWithPrevouts {
            transaction,
            prevouts: vec![
                Some(p2wsh_prevout(&witness_script)),
                Some(p2wsh_prevout(&witness_script)),
            ],
        };

        let votes = validate_transaction_votes(&entry).expect("available prevouts");

        assert_eq!(votes.len(), 2);
        assert_eq!(votes[0].auth().input_index, 0);
        assert_eq!(votes[1].auth().input_index, 1);
    }

    #[test]
    fn missing_candidate_prevout_fails_closed() {
        let witness_script = vote_witness_script();
        let transaction = transaction(vec![vote_input(&witness_script)], vec![vote_output()]);
        let previous_output = transaction.input[0].previous_output;
        let entry = BtcTransactionWithPrevouts {
            transaction,
            prevouts: vec![None],
        };

        assert_eq!(
            validate_transaction_votes(&entry),
            Err(MintingVoteProcessingError::MissingPrevout {
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

    fn vote_witness_script() -> Vec<u8> {
        let predicate = Predicate::opaque(Predicate::unspendable_key());
        minter_witness_script::build_with_authorization(
            &predicate,
            corepc_client::bitcoin::Script::from_bytes(&[0x51]),
        )
        .into_bytes()
    }

    fn vote_input(witness_script: &[u8]) -> TxIn {
        TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[witness_script]),
        }
    }

    fn vote_output() -> TxOut {
        TxOut {
            value: Amount::ZERO,
            script_pubkey: MintingVoteData::V1 {
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
