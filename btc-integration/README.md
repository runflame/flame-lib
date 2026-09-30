# Bitcoin integration

## Regtest

Only regtest is supported.

```no_run
use btc_integration::{BitcoinConfig, BitcoinConnection, MinterIdentity,
    SecretStorage, TestAcquisitionConfig};
use corepc_client::bitcoin::Amount;

# async fn example<S: SecretStorage>(config: BitcoinConfig, identity: MinterIdentity,
# secret_storage: S, acquisition_wallet_rpc_url: String, access_predicate: flamevm::Predicate,
# validator_pubkey: ed25519_dalek::VerifyingKey, height: u32,
# hash: flamechain::BlockHash) -> Result<(), Box<dyn std::error::Error>> {
let connection = BitcoinConnection::regtest(
    config,
    identity,
    secret_storage,
    TestAcquisitionConfig {
        wallet_rpc_url: acquisition_wallet_rpc_url,
        access_predicate,
        validator_pubkey,
    },
).await?;
let sender = connection.get_sender(); // Arc<TestSender<Core31RpcApi, S>>
let indexer = connection.create_indexer();
let mut updates = indexer.subscribe();
indexer.startup().await?;

sender.send_acquisition(Amount::from_sat(50_000)).await?;
sender.send_vote(height, hash).await?;
// The application consumes get_history(cursor, limit) and persists its cursor.
indexer.shutdown().await?;
# Ok(())
# }
```
