//! The three files a node reads: the network definition, the derived
//! genesis, and this node's own policy.
//!
//! `chainparams.toml` is the network: every node on it reads exactly these
//! bytes. `genesis.json` is derived from that file once and read on every
//! start. `flamed.toml` is one node's local policy and reaches no consensus
//! value at all.
//!
//! Byte-shaped fields are spelled with `flamed_rpc`'s newtypes, so a
//! predicate an operator copies between `genesis.json` and an RPC reply is
//! the same string in both places.

use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use flamechain::{BlockLimits, ChainParams, StorageParams};
use flamed_rpc::{BlockId, ContractEnvelope, ContractId, PredicatePoint};
use flamekd::Network;
use serde::{Deserialize, Serialize};

/// Emits the two shapes of one upstream parameter block from a single field
/// list: the optional form a network definition states changes in, and the
/// complete form `genesis.json` records.
///
/// One list, so a parameter added upstream cannot reach one file and miss the
/// other — and if upstream renames a field, this stops compiling, which is
/// the point.
macro_rules! params {
    ($config:ident, $section:ident, $target:ident, { $($field:ident : $ty:ty),* $(,)? }) => {
        /// Every field optional: a network states only what it changes.
        #[derive(Clone, Copy, Debug, Default, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $config {
            $(
                /// Overrides the upstream default.
                #[serde(default)]
                pub $field: Option<$ty>,
            )*
        }

        impl $config {
            /// Fills every unset field from the upstream `Default`.
            pub fn resolve(&self) -> $target {
                let defaults = $target::default();
                $target { $($field: self.$field.unwrap_or(defaults.$field),)* }
            }
        }

        /// Every field present: `genesis.json` records the parameters in
        /// full, so a node never inherits the defaults of whichever build
        /// happens to read it.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $section {
            $(
                /// As recorded at genesis.
                pub $field: $ty,
            )*
        }

        impl From<$target> for $section {
            fn from(params: $target) -> Self {
                Self { $($field: params.$field,)* }
            }
        }

        impl $section {
            /// The upstream form of these parameters.
            pub fn resolve(&self) -> $target {
                $target { $($field: self.$field,)* }
            }
        }
    };
}

params!(StorageConfig, StorageSection, StorageParams, {
    unit_bytes: u64,
    initial_pool_units: u64,
    lease_duration_blocks: u64,
    issued_units_per_block: u64,
    minimum_lease_units: u64,
    minimum_remaining_units: u64,
    lease_record_bytes: u64,
    initial_price_sparks_per_unit: u64,
});

params!(LimitsConfig, LimitsSection, BlockLimits, {
    max_transactions: usize,
    max_witness_bytes: usize,
    max_transaction_script_bytes: usize,
    max_script_bytes: usize,
    max_transaction_gas: u64,
    max_gas_credit: u64,
    max_external_gas: u64,
    max_internal_gas: u64,
    max_multiplications_per_transaction: usize,
    max_multiplications: usize,
    max_messages: usize,
    max_proofs_per_transaction: usize,
    max_proof_depth: usize,
});

/// Which network's address encoding an allocation is written in.
///
/// `flamekd::Network` carries no serde, and a config file needs a name for
/// it. There is no devnet HRP: a private network writes testnet addresses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkName {
    /// `tf1…` addresses.
    #[default]
    Testnet,
    /// `f1…` addresses.
    Mainnet,
}

impl From<NetworkName> for Network {
    fn from(name: NetworkName) -> Self {
        match name {
            NetworkName::Testnet => Network::Testnet,
            NetworkName::Mainnet => Network::Mainnet,
        }
    }
}

/// `chainparams.toml`: the network definition.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainParamsFile {
    /// The Flame protocol version. Only 1 exists.
    pub version: u32,
    /// The HRP an `address` allocation is read with.
    #[serde(default)]
    pub network: NetworkName,
    /// Storage parameters, where they differ from upstream.
    #[serde(default)]
    pub storage: StorageConfig,
    /// Block limits, where they differ from upstream.
    #[serde(default)]
    pub limits: LimitsConfig,
    /// Devnet only: who holds what at height zero.
    #[serde(default)]
    pub genesis: Vec<GenesisSpec>,
}

impl ChainParamsFile {
    /// Reads one from disk.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Ok(toml::from_str(&read(path)?)?)
    }

    /// The upstream parameters this file describes.
    pub fn params(&self) -> ChainParams {
        ChainParams {
            version: self.version,
            storage: self.storage.resolve(),
            limits: self.limits.resolve(),
        }
    }
}

/// One allocation: exactly one of `address` and `predicate` names the holder.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenesisSpec {
    /// A bech32f receiving address, in the file's network.
    #[serde(default)]
    pub address: Option<String>,
    /// A predicate point, for a holder with no address to print.
    #[serde(default)]
    pub predicate: Option<PredicatePoint>,
    /// How much this holder starts with.
    pub qty_sparks: u64,
}

/// The `chain` section of `genesis.json`: the parameters, in full.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainSection {
    /// The Flame protocol version.
    pub version: u32,
    /// Storage parameters, every field written out.
    pub storage: StorageSection,
    /// Block limits, every field written out.
    pub limits: LimitsSection,
}

impl From<ChainParams> for ChainSection {
    fn from(params: ChainParams) -> Self {
        Self {
            version: params.version,
            storage: params.storage.into(),
            limits: params.limits.into(),
        }
    }
}

impl ChainSection {
    /// The upstream parameters this section records.
    pub fn params(&self) -> ChainParams {
        ChainParams {
            version: self.version,
            storage: self.storage.resolve(),
            limits: self.limits.resolve(),
        }
    }
}

/// `genesis.json`: what `flamed genesis` derived, and what `flamed run`
/// rebuilds the chain from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenesisFile {
    /// The genesis block's hash. A node that derives a different one refuses
    /// to start.
    pub genesis_hash: BlockId,
    /// The parameters the chain was built with.
    pub chain: ChainSection,
    /// The allocations, in the order the network definition listed them.
    pub contracts: Vec<GenesisContract>,
}

impl GenesisFile {
    /// Reads one from disk.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Ok(serde_json::from_str(&read(path)?)?)
    }
}

/// One allocation, as recorded.
///
/// The bytes are here because a genesis contract appears in no effect log and
/// the chain keeps only roots, so this file is the only place it exists. The
/// other fields are what an operator reads to learn who holds the supply, and
/// `Node::open` checks every one of them against the bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenesisContract {
    /// Its position in the network definition.
    pub index: u64,
    /// The contract id, which is the hash of the bytes below.
    pub id: ContractId,
    /// The anchor, derived from the index.
    #[serde(with = "flamed_rpc::codec::hex32")]
    pub anchor: [u8; 32],
    /// The predicate that locks it, resolved from whichever form named it.
    pub predicate: PredicatePoint,
    /// How much it holds.
    pub qty_sparks: u64,
    /// The contract itself.
    pub bytes: ContractEnvelope,
}

/// `flamed.toml`: one node's local policy.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    /// Where `blocks.bin` lives.
    pub data_dir: PathBuf,
    /// Where `genesis.json` lives; `<data_dir>/genesis.json` by default.
    #[serde(default)]
    pub genesis: Option<PathBuf>,
    /// Where the JSON-RPC server listens.
    pub rpc_bind: SocketAddr,
    /// Devnet only: how often the minter mints. Zero is refused rather
    /// than read as "never" or as "as fast as this node can".
    #[serde(default = "default_block_interval_secs")]
    pub block_interval_secs: u64,
    /// The fee this node's mempool will not go below.
    #[serde(default)]
    pub minimum_fee: u64,
}

fn default_block_interval_secs() -> u64 {
    15
}

impl NodeConfig {
    /// Reads one from disk.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let config: NodeConfig = toml::from_str(&read(path)?)?;
        if config.block_interval_secs == 0 {
            return Err(ConfigError::BlockInterval);
        }
        Ok(config)
    }

    /// How often this node mints.
    pub fn block_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.block_interval_secs)
    }

    /// Where this node reads its genesis from.
    pub fn genesis_path(&self) -> PathBuf {
        self.genesis
            .clone()
            .unwrap_or_else(|| self.data_dir.join("genesis.json"))
    }

    /// Where this node archives blocks.
    pub fn blocks_path(&self) -> PathBuf {
        self.data_dir.join("blocks.bin")
    }
}

fn read(path: &Path) -> Result<String, ConfigError> {
    fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })
}

/// A configuration file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file itself.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The file that could not be read.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// Its TOML.
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    /// Its JSON.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// A block interval of zero, which is not a way to say anything.
    #[error("block_interval_secs must be at least 1; omit it for the default")]
    BlockInterval,
}
