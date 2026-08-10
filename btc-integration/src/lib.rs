mod factory;
pub mod mint_proofs;
pub mod rpc;
// TODO: implement more elegant solution for integration testing
pub mod test;

pub use prelude::*;

pub mod prelude {
    pub use crate::factory::{BtcIntegrationConfig, create_mint_proof_components};
    pub use crate::mint_proofs::MintingProofData;
    pub use crate::mint_proofs::indexer::indexer::{
        MintProofIndexer, MintingProofUpdate, NewMintingProofs, ShutdownError, StartupError,
    };
    pub use crate::mint_proofs::mint_proof_sender::{MintProofSendError, MintProofSender};
    pub use crate::mint_proofs::minting_proof_storage::{
        MintingProof, MintingProofsByBitcoinBlock,
    };
    pub use crate::rpc::BtcBlockTip;
    use crate::{mint_proofs, rpc};
    pub use corepc_client::client_sync::Auth as BitcoinRpcAuth;

    pub type MintProofSenderV31 = MintProofSender<rpc::Core31RpcApi>;
    pub type MintProofIndexerV31 = MintProofIndexer<
        rpc::Core31RpcApi,
        mint_proofs::minting_proof_storage::InMemoryMintingProofStorage,
    >;
}
