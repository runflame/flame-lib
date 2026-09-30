//! Turning a network definition into a genesis, and checking one on the way
//! back in.
//!
//! Deriving is devnet-only: contracts enter a public chain by minting, and
//! nothing else. Verifying is not, because any node that opens a
//! `genesis.json` has to know the file says the truth.

use flamevm::{CellError, ClearToken, Contract, ContractID, Scalar, VMError, Value, FLAME_FLAVOR};
use sha2::{Digest, Sha256};

// Only the deriving half resolves a holder; a node that merely opens a
// genesis file reads the predicate it recorded.
#[cfg(any(test, feature = "devnet"))]
use flamed_rpc::PredicatePoint;
#[cfg(any(test, feature = "devnet"))]
use flamekd::{Network, ReceivingAddress};
#[cfg(any(test, feature = "devnet"))]
use flamevm::{Point, Predicate};

use crate::cells::contract_from_bytes;
use crate::config::GenesisFile;

/// The anchor of the allocation at `index`.
///
/// Nothing precedes genesis, so an anchor cannot be ratcheted from a spent
/// contract the way every later one is; position is the only thing there is
/// to bind it to.
pub fn genesis_anchor(index: u64) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"flame.genesis");
    digest.update(index.to_le_bytes());
    digest.finalize().into()
}

/// The contracts a genesis file records, checked against their own bytes.
///
/// The genesis hash commits the ids through the accumulator root and nothing
/// else in this file: not the bytes, and not the `predicate`, `anchor` and
/// `qty_sparks` an operator reads to learn who holds the supply. Checking the
/// id against the bytes is what stands between a node and an unspendable
/// allocation; checking the other three is what stands between an operator
/// and a file that lies about money. The token must also be native Flame: a
/// file written under another `FLAME_FLAVOR` agrees with itself everywhere
/// else, so that check is the one that names the real problem.
pub fn contracts(genesis: &GenesisFile) -> Result<Vec<(ContractID, Contract)>, GenesisError> {
    let mut resolved = Vec::with_capacity(genesis.contracts.len());
    for (position, record) in genesis.contracts.iter().enumerate() {
        let index = record.index;
        // An allocation's index is what its anchor is derived from, so the
        // numbering is not decoration: a file that renumbers or reorders
        // its allocations describes a different genesis.
        if index != position as u64 {
            return Err(GenesisError::ContractIndexMismatch { position, index });
        }
        let contract = contract_from_bytes(&record.bytes.0)
            .map_err(|source| GenesisError::ContractBytes { index, source })?;

        let id = contract.id();
        if id != record.id.0 {
            return Err(GenesisError::ContractIdMismatch { index });
        }
        if contract.predicate.to_point().to_bytes() != record.predicate.0 {
            return Err(GenesisError::ContractFieldMismatch {
                index,
                field: "predicate",
            });
        }
        // Both directions: the bytes must carry the recorded anchor, and
        // that anchor must be the one this allocation's position gives.
        if contract.anchor.0 != record.anchor || record.anchor != genesis_anchor(index) {
            return Err(GenesisError::ContractFieldMismatch {
                index,
                field: "anchor",
            });
        }
        let Some(token) = sparks_held(&contract, record.qty_sparks) else {
            return Err(GenesisError::ContractFieldMismatch {
                index,
                field: "qty_sparks",
            });
        };
        if token.flv() != FLAME_FLAVOR {
            return Err(GenesisError::ForeignFlavor { index });
        }
        resolved.push((id, contract));
    }
    Ok(resolved)
}

/// The cleartext token an allocation holds, if it holds exactly this much.
fn sparks_held(contract: &Contract, qty_sparks: u64) -> Option<&ClearToken> {
    match contract.payload() {
        Value::ClearToken(token) if token.qty() == Scalar::from(qty_sparks) => Some(token),
        _ => None,
    }
}

/// An allocation's holder, from whichever of the two forms named it.
#[cfg(any(test, feature = "devnet"))]
fn holder(
    address: Option<&str>,
    predicate: Option<&PredicatePoint>,
    network: Network,
    index: u64,
) -> Result<Predicate, GenesisError> {
    let point = match (address, predicate) {
        (Some(address), None) => *ReceivingAddress::from_bech32(address, network)
            .map_err(|source| GenesisError::Address { index, source })?
            .spending_key(),
        (None, Some(predicate)) => Point::from_bytes(predicate.0)
            .to_compressed()
            .decompress()
            .ok_or(GenesisError::InvalidPredicate { index })?,
        _ => return Err(GenesisError::AllocationHolder { index }),
    };
    Ok(Predicate::opaque(point.compress()))
}

#[cfg(any(test, feature = "devnet"))]
pub use devnet::{derive, write};

#[cfg(any(test, feature = "devnet"))]
mod devnet {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    use flamechain::Blockchain;
    use flamed_rpc::{BlockId, ContractId, PredicatePoint};
    use flamevm::{Anchor, ClearToken, Contract, Scalar, Value, FLAME_FLAVOR};

    use super::{genesis_anchor, holder, GenesisError};
    use crate::cells::contract_bytes;
    use crate::config::{ChainParamsFile, GenesisContract, GenesisFile};

    /// Derives a genesis from a network definition.
    ///
    /// Deterministic by construction: allocations keep their written order,
    /// anchors come from that order, and the accumulator sorts the ids
    /// itself. The same input file gives the same bytes on any machine.
    pub fn derive(chainparams: &ChainParamsFile) -> Result<GenesisFile, GenesisError> {
        let network = chainparams.network.into();
        let mut seen = BTreeSet::new();
        let mut contracts = Vec::with_capacity(chainparams.genesis.len());
        let mut ids = Vec::with_capacity(chainparams.genesis.len());

        for (position, spec) in chainparams.genesis.iter().enumerate() {
            let index = position as u64;
            let predicate = holder(
                spec.address.as_deref(),
                spec.predicate.as_ref(),
                network,
                index,
            )?;
            let point = predicate.to_point().to_bytes();
            // Two allocations to one predicate get different anchors and so
            // different ids, and the chain would take both. Refuse here
            // instead: an operator who wrote a holder twice meant one.
            if !seen.insert(point) {
                return Err(GenesisError::DuplicatePredicate { index });
            }

            let anchor = genesis_anchor(index);
            let payload =
                Value::ClearToken(ClearToken::new(Scalar::from(spec.qty_sparks), FLAME_FLAVOR));
            let contract = Contract::new(predicate, Anchor(anchor), payload)
                .map_err(|source| GenesisError::Contract { index, source })?;
            let bytes = contract_bytes(&contract)
                .map_err(|source| GenesisError::ContractBytes { index, source })?;

            ids.push(contract.id());
            contracts.push(GenesisContract {
                index,
                id: ContractId(contract.id()),
                anchor,
                predicate: PredicatePoint(point),
                qty_sparks: spec.qty_sparks,
                bytes: bytes.into(),
            });
        }

        let params = chainparams.params();
        let (chain, _catchup) = Blockchain::devnet_genesis(params, &ids)?;
        Ok(GenesisFile {
            genesis_hash: BlockId(chain.tip().into_bytes()),
            chain: params.into(),
            contracts,
        })
    }

    /// Derives a genesis and writes it. The CLI and the tests share this
    /// path, so what the tests exercise is what the CLI runs.
    pub fn write(chainparams: &ChainParamsFile, path: &Path) -> Result<GenesisFile, GenesisError> {
        let genesis = derive(chainparams)?;
        let mut json = serde_json::to_string_pretty(&genesis)?;
        json.push('\n');
        fs::write(path, json).map_err(|source| GenesisError::Write {
            path: path.display().to_string(),
            source,
        })?;
        Ok(genesis)
    }
}

/// A genesis could not be derived, or could not be believed.
#[derive(Debug, thiserror::Error)]
pub enum GenesisError {
    /// Neither or both of the two holder forms.
    #[error("allocation {index} names its holder by neither or both of address and predicate")]
    AllocationHolder {
        /// Which allocation.
        index: u64,
    },
    /// An address that would not parse for this network.
    #[error("allocation {index} has an address this network cannot read: {source}")]
    Address {
        /// Which allocation.
        index: u64,
        /// Why.
        source: flamekd::Error,
    },
    /// A predicate point that is not a group element.
    #[error("allocation {index} has a predicate that is not a point")]
    InvalidPredicate {
        /// Which allocation.
        index: u64,
    },
    /// The same holder twice.
    #[error("allocation {index} repeats a predicate an earlier allocation already used")]
    DuplicatePredicate {
        /// Which allocation.
        index: u64,
    },
    /// The VM refused the contract.
    #[error("allocation {index} is not a portable contract: {source}")]
    Contract {
        /// Which allocation.
        index: u64,
        /// Why.
        source: VMError,
    },
    /// Its bytes would not encode or decode.
    #[error("allocation {index} has bytes that do not round trip: {source}")]
    ContractBytes {
        /// Which allocation.
        index: u64,
        /// Why.
        source: CellError,
    },
    /// The recorded id is not the hash of the recorded bytes.
    #[error("allocation {index} records an id its own bytes do not hash to")]
    ContractIdMismatch {
        /// Which allocation.
        index: u64,
    },
    /// An allocation is numbered other than by its position.
    #[error("the allocation at position {position} is numbered {index}")]
    ContractIndexMismatch {
        /// Where it is in the file.
        position: usize,
        /// What it calls itself.
        index: u64,
    },
    /// A recorded field disagrees with the bytes.
    #[error("allocation {index} records a {field} its own bytes disagree with")]
    ContractFieldMismatch {
        /// Which allocation.
        index: u64,
        /// Which field.
        field: &'static str,
    },
    /// The allocation's token is not native Flame: the file was written
    /// under another `FLAME_FLAVOR`.
    #[error(
        "allocation {index} holds a token whose flavor is not FLAME_FLAVOR; \
         regenerate genesis.json with `flamed genesis`"
    )]
    ForeignFlavor {
        /// Which allocation.
        index: u64,
    },
    /// The chain refused the seeded ids.
    #[error(transparent)]
    Chain(#[from] flamechain::ChainError),
    /// The file would not serialize.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// The file would not be written.
    #[error("cannot write {path}: {source}")]
    Write {
        /// Where.
        path: String,
        /// Why.
        source: std::io::Error,
    },
}
