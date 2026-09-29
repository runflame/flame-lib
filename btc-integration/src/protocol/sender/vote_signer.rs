use crate::btc::bitcoin_facade::BitcoinFacade;
use std::{error::Error, sync::Arc};

use async_trait::async_trait;
use corepc_client::{
    bitcoin::{
        Amount, ScriptBuf, Transaction, ecdsa,
        secp256k1::{Message, PublicKey, Secp256k1, SecretKey},
        sighash::{EcdsaSighashType, SighashCache},
    },
    client_sync::Error as RpcError,
};
use thiserror::Error;

use super::signer::{RequiredInput, SignTransactionRequest, TransactionSigner};
use crate::{
    btc::rpc::{BtcFundedTransaction, RpcApi},
    protocol::{minter_witness_script, vote::MintingVoteOutput},
};

#[async_trait]
pub trait SecretStorage: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    async fn get_secret_key(&self) -> Result<SecretKey, Self::Error>;
}

#[async_trait]
impl<S: SecretStorage + ?Sized> SecretStorage for Arc<S> {
    type Error = S::Error;

    async fn get_secret_key(&self) -> Result<SecretKey, Self::Error> {
        self.as_ref().get_secret_key().await
    }
}

struct SecretKeyAccess<S> {
    storage: S,
}

impl<S> SecretKeyAccess<S>
where
    S: SecretStorage,
{
    fn new(storage: S) -> Self {
        Self { storage }
    }

    async fn with_secret_key<T>(
        &self,
        operation: impl FnOnce(&SecretKey) -> T,
    ) -> Result<T, S::Error> {
        let secret_key = ErasingSecretKey(self.storage.get_secret_key().await?);
        Ok(operation(&secret_key.0))
    }
}

struct ErasingSecretKey(SecretKey);

impl Drop for ErasingSecretKey {
    fn drop(&mut self) {
        self.0.non_secure_erase();
    }
}

pub struct VoteSigner<R: ?Sized, S> {
    btc_facade: Arc<BitcoinFacade<R>>,
    secret_key_access: SecretKeyAccess<S>,
}

impl<R: ?Sized, S: SecretStorage> VoteSigner<R, S> {
    pub fn new(secret_storage: S, btc_facade: Arc<BitcoinFacade<R>>) -> Self {
        Self {
            btc_facade,
            secret_key_access: SecretKeyAccess::new(secret_storage),
        }
    }
}

#[async_trait]
impl<R, S> TransactionSigner for VoteSigner<R, S>
where
    R: RpcApi + ?Sized,
    S: SecretStorage,
{
    type Error = VoteSignerError<S::Error>;

    async fn fund_and_sign(
        &self,
        request: SignTransactionRequest,
    ) -> Result<Transaction, Self::Error> {
        let (witness_script, public_key) = validate_request(&request)?;
        let script_pubkey = witness_script.to_p2wsh();
        let funded = self
            .btc_facade
            .fund_transaction_from_script(
                &request.transaction,
                &script_pubkey,
                minter_witness_script::INPUT_WEIGHT,
            )
            .await?;

        validate_funded_transaction(&request.transaction, &funded, &script_pubkey)?;

        self.secret_key_access
            .with_secret_key(|secret_key| {
                validate_secret_key(secret_key, &public_key)?;
                sign_transaction(funded, witness_script, secret_key)
            })
            .await
            .map_err(VoteSignerError::SecretStorage)
            .and_then(|result| result)
    }
}

fn validate_request<E>(
    request: &SignTransactionRequest,
) -> Result<(&ScriptBuf, PublicKey), VoteSignerError<E>>
where
    E: Error + Send + Sync + 'static,
{
    if !request.transaction.input.is_empty() {
        return Err(VoteSignerError::InvalidRequest(
            "a vote transaction must not contain inputs before funding",
        ));
    }
    if request.transaction.output.len() != 1
        || request.transaction.output[0].value != Amount::ZERO
        || MintingVoteOutput::from_output(0, &request.transaction.output[0]).is_none()
    {
        return Err(VoteSignerError::InvalidRequest(
            "VoteSigner only accepts a single zero-value Minting vote output",
        ));
    }

    let [RequiredInput::P2wsh { witness_script }] = request.required_inputs.as_slice() else {
        return Err(VoteSignerError::InvalidRequest(
            "VoteSigner requires exactly one Minter P2WSH input",
        ));
    };
    let parsed = minter_witness_script::parse(witness_script).map_err(|_| {
        VoteSignerError::InvalidRequest("the Minter witness script has an invalid FLMV prefix")
    })?;
    let public_key = minter_witness_script::single_key_public_key(parsed.authorization).ok_or(
        VoteSignerError::InvalidRequest(
            "VoteSigner only supports <compressed-pubkey> OP_CHECKSIG authorization",
        ),
    )?;
    Ok((witness_script, public_key))
}

fn validate_secret_key<E>(
    secret_key: &SecretKey,
    expected: &PublicKey,
) -> Result<(), VoteSignerError<E>>
where
    E: Error + Send + Sync + 'static,
{
    let actual = secret_key.public_key(&Secp256k1::signing_only());
    if actual != *expected {
        return Err(VoteSignerError::SecretKeyMismatch);
    }
    Ok(())
}

fn validate_funded_transaction<E>(
    original: &Transaction,
    funded: &BtcFundedTransaction,
    script_pubkey: &ScriptBuf,
) -> Result<(), VoteSignerError<E>>
where
    E: Error + Send + Sync + 'static,
{
    if funded.transaction.version != original.version
        || funded.transaction.lock_time != original.lock_time
        || funded.transaction.input.is_empty()
        || funded.transaction.input.len() != funded.inputs.len()
    {
        return Err(VoteSignerError::RpcContractViolation);
    }
    if funded
        .transaction
        .input
        .iter()
        .zip(&funded.inputs)
        .any(|(input, funding)| {
            input.previous_output != funding.outpoint
                || funding.prevout.script_pubkey != *script_pubkey
        })
    {
        return Err(VoteSignerError::RpcContractViolation);
    }

    let original_output_count = funded
        .transaction
        .output
        .iter()
        .filter(|output| *output == &original.output[0])
        .count();
    let valid_outputs =
        funded.transaction.output.len() <= 2
            && original_output_count == 1
            && funded.transaction.output.iter().all(|output| {
                output == &original.output[0] || output.script_pubkey == *script_pubkey
            });
    if !valid_outputs {
        return Err(VoteSignerError::RpcContractViolation);
    }
    Ok(())
}

fn sign_transaction<E>(
    mut funded: BtcFundedTransaction,
    witness_script: &ScriptBuf,
    secret_key: &SecretKey,
) -> Result<Transaction, VoteSignerError<E>>
where
    E: Error + Send + Sync + 'static,
{
    for (input_index, funding) in funded.inputs.iter().enumerate() {
        let sighash = SighashCache::new(&funded.transaction)
            .p2wsh_signature_hash(
                input_index,
                witness_script.as_script(),
                funding.prevout.value,
                EcdsaSighashType::All,
            )
            .map_err(|error| VoteSignerError::Sighash {
                input_index,
                message: error.to_string(),
            })?;
        let signature = ecdsa::Signature::sighash_all(
            Secp256k1::signing_only().sign_ecdsa(&Message::from(sighash), secret_key),
        );
        funded.transaction.input[input_index].witness =
            corepc_client::bitcoin::Witness::from_slice(&[
                signature.to_vec(),
                witness_script.as_bytes().to_vec(),
            ]);
    }
    Ok(funded.transaction)
}

#[derive(Debug, Error)]
pub enum VoteSignerError<E>
where
    E: Error + Send + Sync + 'static,
{
    #[error("secret storage error: {0}")]
    SecretStorage(#[source] E),
    #[error("Bitcoin Core RPC error: {0}")]
    Rpc(#[from] RpcError),
    #[error("invalid VoteSigner request: {0}")]
    InvalidRequest(&'static str),
    #[error("the secret key does not match the public key in the Minter witness script")]
    SecretKeyMismatch,
    #[error("Bitcoin Core returned a funded transaction that violates the VoteSigner contract")]
    RpcContractViolation,
    #[error("failed to calculate the P2WSH signature hash for input {input_index}: {message}")]
    Sighash { input_index: usize, message: String },
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use corepc_client::bitcoin::secp256k1::SecretKey;

    use super::{SecretKeyAccess, SecretStorage};

    struct CountingStorage {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SecretStorage for CountingStorage {
        type Error = Infallible;

        async fn get_secret_key(&self) -> Result<SecretKey, Self::Error> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(SecretKey::from_slice(&[0x41; 32]).expect("valid test secret key"))
        }
    }

    #[tokio::test]
    async fn secret_key_access_reads_storage_for_every_operation() {
        let calls = Arc::new(AtomicUsize::new(0));
        let access = SecretKeyAccess::new(CountingStorage {
            calls: Arc::clone(&calls),
        });

        let first = access
            .with_secret_key(|key| key.secret_bytes())
            .await
            .expect("first storage access");
        let second = access
            .with_secret_key(|key| key.secret_bytes())
            .await
            .expect("second storage access");

        assert_eq!(first, [0x41; 32]);
        assert_eq!(second, [0x41; 32]);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}
