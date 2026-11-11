# Bitcoin integration

## Networks

`BitcoinConfig::network` names the Bitcoin a node follows, and every endpoint is checked against it on connect:

- `regtest`: a regtest bitcoind. The only network where test acquisitions are sent.
- `overlay`: the overlay service, which serves real mainnet headers with simulated burns. It reports the mainnet chain
  and also answers `getoverlayinfo`; a node configured as `mainnet` refuses it.
- `mainnet`: a real mainnet bitcoind.

The RPC is split into two traits: `ChainSource`, the read-only calls the indexer uses, and `SenderRpc`, publishing and
wallet calls. `connect_indexer(&config)` connects an indexer alone, with no wallet and no sender.

## Regtest

```no_run
use btc_integration::{BitcoinConfig, BitcoinConnection, MinterIdentity,
    SecretStorage, TestAcquisitionConfig};
use corepc_client::bitcoin::Amount;

# async fn example<S: SecretStorage>(config: BitcoinConfig, identity: MinterIdentity,
# secret_storage: S, acquisition_wallet_rpc_url: String, access_predicate: flamevm::Predicate,
# height: u32,
# hash: flamechain::BlockHash) -> Result<(), Box<dyn std::error::Error>> {
let connection = BitcoinConnection::regtest(
    config,
    identity,
    secret_storage,
    TestAcquisitionConfig {
        wallet_rpc_url: acquisition_wallet_rpc_url,
        access_predicate,
    },
).await?;
let sender = connection.get_sender(); // Arc<TestSender<Core31RpcApi, S>>
let indexer = connection.create_indexer();
let mut updates = indexer.subscribe();
indexer.startup().await?;

sender.send_acquisition(Amount::from_sat(50_000)).await?;
sender.send_endorsement(height, hash).await?;
// The application consumes get_history(cursor, limit) and persists its cursor.
indexer.shutdown().await?;
# Ok(())
# }
```
