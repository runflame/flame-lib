//! The FFI records as JavaScript sees them.
//!
//! Each mirrors one `flamewallet_ffi` record field for field and converts
//! losslessly both ways; they exist only to carry serde attributes the FFI
//! crate should not know about. `src/types.d.ts` is their TypeScript, and
//! the two change together.

use serde::{Deserialize, Serialize};

use flamewallet_ffi as ffi;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Mainnet,
    Testnet,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct KeyPath {
    pub branch: u32,
    pub index: u32,
}

#[derive(Serialize)]
pub struct IssuedAddress {
    pub path: KeyPath,
    pub address: String,
    #[serde(with = "serde_bytes")]
    pub predicate: Vec<u8>,
}

#[derive(Serialize)]
pub struct ContractInfo {
    #[serde(with = "serde_bytes")]
    pub id: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub predicate: Vec<u8>,
    pub value: ContractValue,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ContractValue {
    Clear {
        qty: u64,
        #[serde(with = "serde_bytes")]
        flavor: Vec<u8>,
    },
    Confidential,
    Other,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Opening {
    pub qty: u64,
    #[serde(with = "serde_bytes")]
    pub flavor: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub qty_blinding: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub flavor_blinding: Vec<u8>,
}

#[derive(Deserialize)]
pub struct TransferInput {
    #[serde(with = "serde_bytes")]
    pub contract: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub proof: Vec<u8>,
    pub path: KeyPath,
    #[serde(default)]
    pub opening: Option<Opening>,
}

#[derive(Deserialize)]
pub struct TransferOutput {
    #[serde(with = "serde_bytes")]
    pub predicate: Vec<u8>,
    pub qty: u64,
    #[serde(default, with = "serde_bytes")]
    pub flavor: Option<Vec<u8>>,
}

#[derive(Deserialize)]
pub struct TransferRequest {
    pub inputs: Vec<TransferInput>,
    pub outputs: Vec<TransferOutput>,
    pub fee: u64,
    pub gas: u64,
    #[serde(default)]
    pub locktime: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedOutput {
    #[serde(with = "serde_bytes")]
    pub contract_id: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub contract: Vec<u8>,
    pub opening: Opening,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transfer {
    #[serde(with = "serde_bytes")]
    pub txid: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub block_tx: Vec<u8>,
    pub outputs: Vec<CreatedOutput>,
}

impl From<Network> for ffi::Network {
    fn from(network: Network) -> ffi::Network {
        match network {
            Network::Mainnet => ffi::Network::Mainnet,
            Network::Testnet => ffi::Network::Testnet,
        }
    }
}

impl From<ffi::Network> for Network {
    fn from(network: ffi::Network) -> Network {
        match network {
            ffi::Network::Mainnet => Network::Mainnet,
            ffi::Network::Testnet => Network::Testnet,
        }
    }
}

impl From<KeyPath> for ffi::KeyPath {
    fn from(path: KeyPath) -> ffi::KeyPath {
        ffi::KeyPath {
            branch: path.branch,
            index: path.index,
        }
    }
}

impl From<ffi::KeyPath> for KeyPath {
    fn from(path: ffi::KeyPath) -> KeyPath {
        KeyPath {
            branch: path.branch,
            index: path.index,
        }
    }
}

impl From<ffi::IssuedAddress> for IssuedAddress {
    fn from(issued: ffi::IssuedAddress) -> IssuedAddress {
        IssuedAddress {
            path: issued.path.into(),
            address: issued.address,
            predicate: issued.predicate,
        }
    }
}

impl From<ffi::ContractInfo> for ContractInfo {
    fn from(info: ffi::ContractInfo) -> ContractInfo {
        ContractInfo {
            id: info.id,
            predicate: info.predicate,
            value: match info.value {
                ffi::ContractValue::Clear { qty, flavor } => ContractValue::Clear { qty, flavor },
                ffi::ContractValue::Confidential => ContractValue::Confidential,
                ffi::ContractValue::Other => ContractValue::Other,
            },
        }
    }
}

impl From<Opening> for ffi::Opening {
    fn from(opening: Opening) -> ffi::Opening {
        ffi::Opening {
            qty: opening.qty,
            flavor: opening.flavor,
            qty_blinding: opening.qty_blinding,
            flavor_blinding: opening.flavor_blinding,
        }
    }
}

impl From<ffi::Opening> for Opening {
    fn from(opening: ffi::Opening) -> Opening {
        Opening {
            qty: opening.qty,
            flavor: opening.flavor,
            qty_blinding: opening.qty_blinding,
            flavor_blinding: opening.flavor_blinding,
        }
    }
}

impl From<TransferRequest> for ffi::TransferRequest {
    fn from(request: TransferRequest) -> ffi::TransferRequest {
        ffi::TransferRequest {
            inputs: request
                .inputs
                .into_iter()
                .map(|input| ffi::TransferInput {
                    contract: input.contract,
                    proof: input.proof,
                    path: input.path.into(),
                    opening: input.opening.map(Into::into),
                })
                .collect(),
            outputs: request
                .outputs
                .into_iter()
                .map(|output| ffi::TransferOutput {
                    predicate: output.predicate,
                    qty: output.qty,
                    flavor: output.flavor,
                })
                .collect(),
            fee: request.fee,
            gas: request.gas,
            locktime: request.locktime,
        }
    }
}

impl From<ffi::Transfer> for Transfer {
    fn from(transfer: ffi::Transfer) -> Transfer {
        Transfer {
            txid: transfer.txid,
            block_tx: transfer.block_tx,
            outputs: transfer
                .outputs
                .into_iter()
                .map(|output| CreatedOutput {
                    contract_id: output.contract_id,
                    contract: output.contract,
                    opening: output.opening.into(),
                })
                .collect(),
        }
    }
}
