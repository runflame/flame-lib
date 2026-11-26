#![doc = include_str!("../README.md")]

mod bitcoin_config;
pub mod identity;
pub mod protocol;
pub mod wallet;
pub mod watcher;

pub use prelude::*;

pub mod prelude {
    pub use crate::bitcoin_config::BitcoinConfig;
    pub use crate::identity::{IdentityConfig, IdentityKey, IdentityKeyError};
    pub use crate::protocol::{
        AuthenticatedMintingEndorsement, BtcBlockRecords, BtcRecordsError, BtcRecordsEvent,
        BtcRecordsWatchError, BtcRecordsWatcher, Burn, BurnData, MinterIdentity,
        MinterIdentityError, MinterP2wsh, MintingEndorsementAuth, MintingEndorsementData,
        MintingEndorsementOutput, MintingEndorsementProcessingError,
        MintingEndorsementValidationError, MintingEndorsementValidator,
        UncheckedMintingEndorsement, validate_transaction_endorsements,
    };
    pub use crate::wallet::{
        BtcFundedTransaction, BtcFundingInput, BtcWallet, MinterWallet, WalletError,
        WatchOnlyWallet,
    };
    pub use corepc_client::Auth as BitcoinRpcAuth;
}
