use crate::mint_proofs::minting_proof_storage::InMemoryMintingProofStorage;
use crate::rpc::Core31RpcApi;
use crate::{BitcoinRpcAuth, MintProofIndexer, MintProofSenderV31};
use flamechain::FlameNetwork;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtcIntegrationConfig {
    pub btc_rpc_api: String,
    pub btc_rpc_auth: BitcoinRpcAuth,
    pub flame_network: FlameNetwork,
}

pub fn create_mint_proof_components(
    config: BtcIntegrationConfig,
) -> Result<
    (
        MintProofSenderV31,
        MintProofIndexer<Core31RpcApi, InMemoryMintingProofStorage>,
    ),
    corepc_client::client_sync::Error,
> {
    let rpc_api = Arc::new(Core31RpcApi::new(&config.btc_rpc_api, config.btc_rpc_auth)?);
    let sender = MintProofSenderV31::new(Arc::clone(&rpc_api), config.flame_network);
    let indexer = MintProofIndexer::new(
        rpc_api,
        Arc::new(InMemoryMintingProofStorage::new()),
        config.flame_network,
    );

    Ok((sender, indexer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_mint_proof_components_from_bitcoin_rpc_config() {
        let config = BtcIntegrationConfig {
            btc_rpc_api: "http://127.0.0.1:8332".to_owned(),
            btc_rpc_auth: BitcoinRpcAuth::UserPass("user".to_owned(), "password".to_owned()),
            flame_network: FlameNetwork::Regtest,
        };

        let (_sender, _indexer) =
            create_mint_proof_components(config).expect("create mint-proof components");
    }
}
