use std::convert::Infallible;

use async_trait::async_trait;
use btc_integration::{
    BitcoinConfig, BitcoinConnection, TestAcquisitionConfig,
    btc::rpc::Core31RpcApi,
    protocol::{MinterIdentity, SecretStorage, TestSigner},
};
use corepc_client::{
    bitcoin::{
        Address, Amount,
        secp256k1::{Secp256k1, SecretKey},
    },
    client_sync::Auth,
};
use ed25519_dalek::SigningKey;
use flamevm::Predicate;

use super::TestContext;

pub struct RegtestSecretStorage {
    secret_key_bytes: [u8; 32],
}

#[async_trait]
impl SecretStorage for RegtestSecretStorage {
    type Error = Infallible;

    async fn get_secret_key(&self) -> Result<SecretKey, Self::Error> {
        Ok(SecretKey::from_slice(&self.secret_key_bytes).expect("valid test secret key"))
    }
}

pub async fn create_connection(
    ctx: &TestContext,
) -> bitcoind::anyhow::Result<BitcoinConnection<TestSigner<Core31RpcApi, RegtestSecretStorage>>> {
    let secp = Secp256k1::new();
    let secret_key_bytes = [0x41; 32];
    let secret_key = SecretKey::from_slice(&secret_key_bytes)?;
    let public_key = secret_key.public_key(&secp);
    let access_predicate = Predicate::opaque(Predicate::unspendable_key());
    let identity = MinterIdentity::single_key(&access_predicate, &public_key);
    let witness_script = identity.witness_script();

    let p2wsh_script_pubkey = witness_script.to_p2wsh();
    let p2wsh_address = Address::from_script(
        &p2wsh_script_pubkey,
        corepc_client::bitcoin::Network::Regtest,
    )?;
    let connection = BitcoinConnection::regtest(
        BitcoinConfig {
            node_rpc_url: ctx.node.rpc_url(),
            auth: Auth::CookieFile(ctx.cookie_file.clone()),
        },
        identity,
        RegtestSecretStorage { secret_key_bytes },
        TestAcquisitionConfig {
            wallet_rpc_url: ctx.rpc_url.clone(),
            access_predicate,
            validator_pubkey: SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        },
    )
    .await?;

    let funding = ctx.send_simple_transaction(p2wsh_address, Amount::from_sat(10_000))?;
    ctx.rpc.publish_transaction(&funding.tx).await?;
    ctx.generate_next_block()?;

    Ok(connection)
}
