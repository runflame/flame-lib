//! The watch-only wallet calls a minter makes: set up a wallet watching its P2WSH, list its
//! coins, fund an endorsement from them. Signing stays with the minter. Wallets never run dry
//! (see [`crate::state::Coin`] and `COIN_FLOOR`).

use std::collections::HashMap;

use corepc_client::{
    bitcoin::{Address, Network, OutPoint, Transaction, Txid, consensus::encode},
    types::v31::{
        CreateWallet, FundRawTransaction, GetDescriptorInfo, GetWalletInfo, GetWalletInfoScanning,
        ImportDescriptorsResult, ListUnspentItem, ListWalletDir, ListWalletDirWallet,
    },
};
use serde_json::{Value, json};

use super::{INVALID_ADDRESS_OR_KEY, INVALID_PARAMETER, RpcError, RpcResult, param, to_value};
use crate::{
    descriptor,
    overlay::Overlay,
    state::{FundError, StateError},
};

const WALLET_ERROR: i32 = -4;
const WALLET_NOT_FOUND: i32 = -18;
const WALLET_NOT_SPECIFIED: i32 = -19;
const WALLET_ALREADY_LOADED: i32 = -35;

fn name_param(params: &[Value]) -> Result<String, RpcError> {
    param(params, 0)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| RpcError(INVALID_PARAMETER, "invalid wallet name".into()))
}

/// The wallet named in the request path; it must exist.
fn wallet<'a>(overlay: &Overlay, wallet: Option<&'a str>) -> Result<&'a str, RpcError> {
    let name = wallet.ok_or_else(|| {
        RpcError(
            WALLET_NOT_SPECIFIED,
            "Wallet file not specified (must request wallet RPC through /wallet/<filename> uri-path)."
                .into(),
        )
    })?;
    if !overlay.state().has_wallet(name) {
        return Err(RpcError(
            WALLET_NOT_FOUND,
            "Requested wallet does not exist or is not loaded".into(),
        ));
    }
    Ok(name)
}

fn state_error(error: StateError) -> RpcError {
    RpcError(WALLET_ERROR, error.to_string())
}

pub(super) fn create(overlay: &Overlay, params: &[Value]) -> RpcResult {
    let name = name_param(params)?;
    if param(params, 1).and_then(Value::as_bool) != Some(true) {
        return Err(RpcError(
            WALLET_ERROR,
            "the overlay has watch-only wallets only (disable_private_keys)".into(),
        ));
    }
    overlay.state().create_wallet(&name).map_err(state_error)?;
    to_value(CreateWallet {
        name,
        warnings: None,
    })
}

/// Every wallet is always loaded.
pub(super) fn load(overlay: &Overlay, params: &[Value]) -> RpcResult {
    let name = name_param(params)?;
    if overlay.state().has_wallet(&name) {
        return Err(RpcError(
            WALLET_ALREADY_LOADED,
            format!("Wallet \"{name}\" is already loaded."),
        ));
    }
    Err(RpcError(
        WALLET_NOT_FOUND,
        format!("Wallet {name} not found."),
    ))
}

pub(super) fn list(overlay: &Overlay) -> RpcResult {
    to_value(overlay.state().wallet_names())
}

pub(super) fn list_dir(overlay: &Overlay) -> RpcResult {
    let wallets = overlay
        .state()
        .wallet_names()
        .into_iter()
        .map(|name| ListWalletDirWallet {
            name,
            warnings: None,
        })
        .collect();
    to_value(ListWalletDir { wallets })
}

pub(super) fn info(overlay: &Overlay, name: Option<&str>) -> RpcResult {
    let name = wallet(overlay, name)?;
    to_value(GetWalletInfo {
        wallet_name: name.into(),
        wallet_version: 169_900,
        format: "sqlite".into(),
        tx_count: 0,
        keypool_size: 0,
        keypool_size_hd_internal: None,
        unlocked_until: None,
        private_keys_enabled: false,
        avoid_reuse: false,
        scanning: GetWalletInfoScanning::NotScanning(false),
        descriptors: true,
        external_signer: false,
        blank: true,
        birthtime: None,
        flags: vec!["descriptor_wallet".into(), "disable_private_keys".into()],
        last_processed_block: None,
    })
}

fn raw_descriptor_error() -> RpcError {
    RpcError(
        INVALID_ADDRESS_OR_KEY,
        "the overlay understands raw(<script hex>) descriptors only".into(),
    )
}

pub(super) fn descriptor_info(params: &[Value]) -> RpcResult {
    let given = param(params, 0)
        .and_then(Value::as_str)
        .ok_or_else(raw_descriptor_error)?;
    let script = descriptor::parse_raw(given).ok_or_else(raw_descriptor_error)?;
    let body = format!("raw({})", script.to_hex_string());
    let checksum = descriptor::checksum(&body).expect("hex is in the descriptor charset");
    to_value(GetDescriptorInfo {
        descriptor: format!("{body}#{checksum}"),
        multipath_expansion: None,
        checksum,
        is_range: false,
        is_solvable: false,
        has_private_keys: false,
    })
}

pub(super) fn import_descriptors(
    overlay: &Overlay,
    name: Option<&str>,
    params: &[Value],
) -> RpcResult {
    let name = wallet(overlay, name)?;
    let requests = param(params, 0)
        .and_then(Value::as_array)
        .ok_or_else(|| RpcError(INVALID_PARAMETER, "expected an array of requests".into()))?;
    let mut state = overlay.state();
    let results: Vec<_> = requests
        .iter()
        .map(|request| {
            let script = request
                .get("desc")
                .and_then(Value::as_str)
                .and_then(descriptor::parse_raw);
            let outcome = match script {
                Some(script) => state.watch(name, &script).map_err(|e| e.to_string()),
                None => Err(raw_descriptor_error().1),
            };
            ImportDescriptorsResult {
                success: outcome.is_ok(),
                warnings: None,
                error: outcome
                    .err()
                    .map(|message| json!({ "code": INVALID_ADDRESS_OR_KEY, "message": message })),
            }
        })
        .collect();
    to_value(results)
}

pub(super) fn list_unspent(overlay: &Overlay, name: Option<&str>) -> RpcResult {
    let name = wallet(overlay, name)?;
    let coins = overlay.state().unspent(name).map_err(state_error)?;
    let items: Vec<_> = coins
        .into_iter()
        .map(|coin| {
            let script = &coin.output.script_pubkey;
            let descriptor = format!("raw({})", script.to_hex_string());
            let checksum = descriptor::checksum(&descriptor).expect("hex is in the charset");
            ListUnspentItem {
                txid: coin.outpoint.txid.to_string(),
                vout: i64::from(coin.outpoint.vout),
                address: Address::from_script(script, Network::Bitcoin)
                    .map(|address| address.to_string())
                    .unwrap_or_default(),
                label: Some(String::new()),
                script_pubkey: script.to_hex_string(),
                amount: coin.output.value.to_btc(),
                confirmations: coin.confirmations as i64,
                redeem_script: None,
                spendable: false,
                solvable: false,
                descriptor: None,
                safe: true,
                parent_descriptors: Some(vec![format!("{descriptor}#{checksum}")]),
            }
        })
        .collect();
    to_value(items)
}

pub(super) fn fund(overlay: &Overlay, name: Option<&str>, params: &[Value]) -> RpcResult {
    wallet(overlay, name)?;
    let transaction: Transaction = param(params, 0)
        .and_then(Value::as_str)
        .and_then(|hex| encode::deserialize_hex(hex).ok())
        .ok_or_else(|| RpcError(super::DESERIALIZATION_ERROR, "TX decode failed".into()))?;
    let options = param(params, 1).cloned().unwrap_or_else(|| json!({}));
    if options.get("add_inputs").and_then(Value::as_bool) == Some(true)
        || transaction.input.is_empty()
    {
        return Err(RpcError(
            WALLET_ERROR,
            "the overlay funds preselected inputs only (add_inputs: false)".into(),
        ));
    }
    let change = options
        .get("changeAddress")
        .and_then(Value::as_str)
        .and_then(|address| address.parse::<Address<_>>().ok())
        .and_then(|address| address.require_network(Network::Bitcoin).ok())
        .ok_or_else(|| {
            RpcError(
                INVALID_ADDRESS_OR_KEY,
                "changeAddress must be a mainnet address".into(),
            )
        })?
        .script_pubkey();
    let mut weights = HashMap::new();
    for entry in options
        .get("input_weights")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let outpoint = entry
            .get("txid")
            .and_then(Value::as_str)
            .and_then(|txid| txid.parse::<Txid>().ok())
            .zip(entry.get("vout").and_then(Value::as_u64))
            .map(|(txid, vout)| OutPoint::new(txid, vout as u32));
        let weight = entry.get("weight").and_then(Value::as_u64);
        match outpoint.zip(weight) {
            Some((outpoint, weight)) => weights.insert(outpoint, weight),
            None => return Err(RpcError(INVALID_PARAMETER, "invalid input_weights".into())),
        };
    }
    let funded = overlay
        .state()
        .fund(transaction, change, &weights)
        .map_err(|error| match error {
            FundError::Insufficient => RpcError(WALLET_ERROR, error.to_string()),
            _ => RpcError(INVALID_PARAMETER, error.to_string()),
        })?;
    to_value(FundRawTransaction {
        hex: encode::serialize_hex(&funded.transaction),
        fee: funded.fee.to_btc(),
        change_position: funded
            .change_position
            .map_or(-1, |position| position as i64),
    })
}
