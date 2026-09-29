//! `flamewallet-ffi` for JavaScript, through wasm-bindgen.
//!
//! The surface is the FFI crate's, call for call, in JavaScript's idiom:
//! camelCase names, `Uint8Array` for bytes, `bigint` for every amount and
//! gas figure, string unions for enums, and a thrown `FlameError` whose
//! `kind` names the variant. Nothing here adds wallet logic; each function
//! converts, calls `flamewallet_ffi`, and converts back.
//!
//! Every call is synchronous and `buildTransfer` proves for a noticeable
//! time, so a UI runs this module in a Web Worker.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use wasm_bindgen::prelude::*;

mod types;

use types::{ContractInfo, IssuedAddress, KeyPath, Network, Opening, Transfer, TransferRequest};

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &'static str = include_str!("types.d.ts");

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(typescript_type = "Network")]
    pub type JsNetwork;
    #[wasm_bindgen(typescript_type = "KeyPath")]
    pub type JsKeyPath;
    #[wasm_bindgen(typescript_type = "KeyPath | undefined")]
    pub type JsOptionalKeyPath;
    #[wasm_bindgen(typescript_type = "IssuedAddress")]
    pub type JsIssuedAddress;
    #[wasm_bindgen(typescript_type = "ContractInfo")]
    pub type JsContractInfo;
    #[wasm_bindgen(typescript_type = "Opening")]
    pub type JsOpening;
    #[wasm_bindgen(typescript_type = "TransferRequest")]
    pub type JsTransferRequest;
    #[wasm_bindgen(typescript_type = "Transfer")]
    pub type JsTransfer;
}

/// A fresh English mnemonic of `wordCount` words: 12, 15, 18, 21 or 24.
#[wasm_bindgen(js_name = generateMnemonic)]
pub fn generate_mnemonic(
    #[wasm_bindgen(js_name = wordCount)] word_count: u8,
) -> Result<String, JsValue> {
    flamewallet_ffi::generate_mnemonic(word_count).map_err(error)
}

/// Whether `phrase` is a valid mnemonic in any BIP-39 language.
#[wasm_bindgen(js_name = validateMnemonic)]
pub fn validate_mnemonic(phrase: String) -> bool {
    flamewallet_ffi::validate_mnemonic(phrase)
}

/// The 64-byte BIP-39 seed of `phrase` under `passphrase` (`""` for none).
#[wasm_bindgen(js_name = mnemonicToSeed)]
pub fn mnemonic_to_seed(phrase: String, passphrase: String) -> Result<Vec<u8>, JsValue> {
    flamewallet_ffi::mnemonic_to_seed(phrase, passphrase).map_err(error)
}

/// The predicate a payment to `address` is locked with.
#[wasm_bindgen(js_name = addressToPredicate)]
pub fn address_to_predicate(address: String, network: JsNetwork) -> Result<Vec<u8>, JsValue> {
    let network: Network = from_js(network.into())?;
    flamewallet_ffi::address_to_predicate(address, network.into()).map_err(error)
}

/// Id, predicate, and a cleartext amount if the contract has one.
#[wasm_bindgen(js_name = decodeContract)]
pub fn decode_contract(bytes: Vec<u8>) -> Result<JsContractInfo, JsValue> {
    let info = flamewallet_ffi::decode_contract(bytes).map_err(error)?;
    Ok(to_js(&ContractInfo::from(info))?.into())
}

/// Whether `opening` opens the confidential contract in `contract`.
#[wasm_bindgen(js_name = openingMatches)]
pub fn opening_matches(contract: Vec<u8>, opening: JsOpening) -> Result<bool, JsValue> {
    let opening: Opening = from_js(opening.into())?;
    flamewallet_ffi::opening_matches(contract, opening.into()).map_err(error)
}

/// One account, `m/35263'/network'/0'`. Its keys live in this module's
/// memory until `free()` — or `using` — drops the handle, which zeroizes
/// them; nothing a caller receives is key material.
#[wasm_bindgen]
pub struct Wallet(Arc<flamewallet_ffi::Wallet>);

#[wasm_bindgen]
impl Wallet {
    /// The account below a 64-byte seed, with `nextIndex` receiving
    /// addresses already issued; a new wallet passes 0.
    #[wasm_bindgen(js_name = fromSeed)]
    pub fn from_seed(
        seed: Vec<u8>,
        network: JsNetwork,
        #[wasm_bindgen(js_name = nextIndex)] next_index: u32,
    ) -> Result<Wallet, JsValue> {
        let network: Network = from_js(network.into())?;
        flamewallet_ffi::Wallet::new(seed, network.into(), next_index)
            .map(Wallet)
            .map_err(error)
    }

    /// The account below `phrase` and `passphrase`, without the caller ever
    /// seeing the seed.
    #[wasm_bindgen(js_name = fromMnemonic)]
    pub fn from_mnemonic(
        phrase: String,
        passphrase: String,
        network: JsNetwork,
        #[wasm_bindgen(js_name = nextIndex)] next_index: u32,
    ) -> Result<Wallet, JsValue> {
        let network: Network = from_js(network.into())?;
        flamewallet_ffi::Wallet::from_mnemonic(phrase, passphrase, network.into(), next_index)
            .map(Wallet)
            .map_err(error)
    }

    pub fn network(&self) -> Result<JsNetwork, JsValue> {
        Ok(to_js(&Network::from(self.0.network()))?.into())
    }

    /// The address and predicate at `path`, without issuing anything.
    pub fn address(&self, path: JsKeyPath) -> Result<JsIssuedAddress, JsValue> {
        let path: KeyPath = from_js(path.into())?;
        let issued = self.0.address(path.into()).map_err(error)?;
        Ok(to_js(&IssuedAddress::from(issued))?.into())
    }

    /// Issues the next receiving address and advances the counter.
    #[wasm_bindgen(js_name = nextAddress)]
    pub fn next_address(&self) -> Result<JsIssuedAddress, JsValue> {
        let issued = self.0.next_address().map_err(error)?;
        Ok(to_js(&IssuedAddress::from(issued))?.into())
    }

    /// The next receiving index not yet issued: what an app stores.
    #[wasm_bindgen(js_name = nextIndex)]
    pub fn next_index(&self) -> u32 {
        self.0.next_index()
    }

    /// Which path below `nextIndex + gap` owns `predicate`, if any.
    pub fn owns(&self, predicate: Vec<u8>, gap: u32) -> Result<JsOptionalKeyPath, JsValue> {
        let owner = self.0.owns(predicate, gap).map_err(error)?;
        Ok(to_js(&owner.map(KeyPath::from))?.into())
    }

    /// The bech32f receiving key an indexer is given.
    #[wasm_bindgen(js_name = receivingKey)]
    pub fn receiving_key(&self) -> String {
        self.0.receiving_key()
    }

    /// Builds, proves and signs a transfer; blocks for the proof.
    #[wasm_bindgen(js_name = buildTransfer)]
    pub fn build_transfer(&self, request: JsTransferRequest) -> Result<JsTransfer, JsValue> {
        let request: TransferRequest = from_js(request.into())?;
        let transfer = self.0.build_transfer(request.into()).map_err(error)?;
        Ok(to_js(&Transfer::from(transfer))?.into())
    }
}

/// The error every call throws: an `Error` named `FlameError`, with `kind`
/// set to the variant and the variant's fields alongside.
fn error(error: flamewallet_ffi::FlameError) -> JsValue {
    use flamewallet_ffi::FlameError as E;
    let (kind, fields): (&str, Vec<(&str, JsValue)>) = match &error {
        E::InvalidMnemonic { reason } => ("invalidMnemonic", vec![("reason", reason.into())]),
        E::InvalidSeed { reason } => ("invalidSeed", vec![("reason", reason.into())]),
        E::InvalidAddress { reason } => ("invalidAddress", vec![("reason", reason.into())]),
        E::InvalidKeyPath { reason } => ("invalidKeyPath", vec![("reason", reason.into())]),
        E::InvalidBytes { what, reason } => (
            "invalidBytes",
            vec![("what", what.into()), ("reason", reason.into())],
        ),
        E::KeyMismatch { input } => ("keyMismatch", vec![("input", (*input).into())]),
        E::Transfer { reason } => ("transfer", vec![("reason", reason.into())]),
    };
    let thrown = js_sys::Error::new(&error.to_string());
    thrown.set_name("FlameError");
    // `Reflect::set` on a fresh, unfrozen Error cannot fail.
    let _ = js_sys::Reflect::set(&thrown, &"kind".into(), &kind.into());
    for (name, value) in fields {
        let _ = js_sys::Reflect::set(&thrown, &name.into(), &value);
    }
    thrown.into()
}

/// A value the caller shaped wrongly — a missing field, a number where a
/// `Uint8Array` goes — is refused as a `TypeError`, not as a `FlameError`:
/// it is a programming mistake, never bad data from a chain.
fn from_js<T: DeserializeOwned>(value: JsValue) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(value)
        .map_err(|error| js_sys::TypeError::new(&error.to_string()).into())
}

/// Bytes become `Uint8Array` through `serde_bytes`, every `u64` a `bigint`,
/// and `None` is `undefined`.
fn to_js<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    let serializer = serde_wasm_bindgen::Serializer::new()
        .serialize_large_number_types_as_bigints(true)
        .serialize_missing_as_null(false);
    value.serialize(&serializer).map_err(Into::into)
}
