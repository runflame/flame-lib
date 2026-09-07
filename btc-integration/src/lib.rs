#![doc = include_str!("../README.md")]

mod bitcoin_connection;
pub mod btc;
pub mod protocol;

pub use prelude::*;

pub mod prelude {
    pub use crate::bitcoin_connection::{
        BitcoinConfig, BitcoinConnection, BitcoinConnectionError, TestAcquisitionConfig,
    };
    pub use crate::btc::bitcoin_facade::BitcoinFacade;
    pub use crate::btc::rpc::{
        BtcBlockTip, BtcFundedTransaction, BtcFundingInput, BtcTransactionWithPrevouts,
    };
    pub use crate::btc::transaction_builder::BitcoinTransactionBuilder;
    pub use crate::protocol::{
        Acquisition, AcquisitionData, AuthenticatedMintingVote, HistoryChange, HistoryError,
        HistoryUpdate, IndexedBlock, MinterIdentity, MinterIdentityError, MinterP2wsh,
        MintingSendError, MintingVoteAuth, MintingVoteData, MintingVoteOutput,
        MintingVoteProcessingError, MintingVoteValidationError, MintingVoteValidator,
        ProtocolIndexer, RequiredInput, SecretStorage, Sender, ShutdownError,
        SignTransactionRequest, SignerContractViolation, StartupError, TestSender, TestSigner,
        TestSignerError, TransactionSigner, UncheckedMintingVote, VoteSender, VoteSigner,
        VoteSignerError, validate_transaction_votes,
    };
    pub use corepc_client::client_sync::Auth as BitcoinRpcAuth;
    pub use flamechain::BlockHash;

    use crate::btc::rpc;

    pub type ProtocolIndexerV31 = ProtocolIndexer<rpc::Core31RpcApi>;
}
