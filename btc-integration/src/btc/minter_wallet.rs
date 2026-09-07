//! Managed watch-only wallet initialization. No private keys or funding are used.

use corepc_client::{
    bitcoin::{Script, ScriptBuf},
    client_sync::{
        Auth, Error, Result,
        v31::{Client, ImportDescriptorsRequest},
    },
    types::v31::{CreateWallet, GetWalletInfoScanning},
};
use serde_json::json;

use super::rpc::Core31RpcApi;
use crate::protocol::MinterIdentity;

pub(crate) async fn prepare(
    node_url: &str,
    auth: Auth,
    identity: &MinterIdentity,
) -> Result<Core31RpcApi> {
    let node_url = node_url.to_owned();
    let script = identity.witness_script().clone();
    tokio::task::spawn_blocking(move || prepare_sync(&node_url, auth, script))
        .await
        .map_err(|error| Error::Returned(format!("Minter wallet setup task failed: {error}")))?
}

fn prepare_sync(node_url: &str, auth: Auth, script: ScriptBuf) -> Result<Core31RpcApi> {
    let name = format!("flame-minter-{}", script.wscript_hash());
    let wallet_url = format!("{}/wallet/{name}", node_url.trim_end_matches('/'));
    let node = Client::new_with_auth(node_url, auth.clone())?;
    let created = load_or_create_wallet(&node, &name)?;

    let wallet = Client::new_with_auth(&wallet_url, auth.clone())?;
    validate_wallet(&wallet)?;
    if created {
        import_descriptor(&wallet, &script)?;
    }

    Core31RpcApi::new(&wallet_url, auth)
}

fn validate_wallet(wallet: &Client) -> Result<()> {
    let info = wallet.get_wallet_info()?;
    if info.private_keys_enabled || !info.descriptors {
        return Err(Error::Returned(
            "managed Minter wallet must be a watch-only descriptor wallet".into(),
        ));
    }
    if !matches!(info.scanning, GetWalletInfoScanning::NotScanning(false)) {
        return Err(Error::Returned(
            "Minter wallet is already scanning; retry after it finishes".into(),
        ));
    }
    Ok(())
}

fn import_descriptor(wallet: &Client, script: &Script) -> Result<()> {
    let descriptor = wallet
        .get_descriptor_info(&format!("raw({})", script.to_p2wsh().to_hex_string()))?
        .descriptor;
    let result = wallet.import_descriptors(&[ImportDescriptorsRequest::new(descriptor, "now")])?;
    match result.0.as_slice() {
        [import] if import.success => Ok(()),
        _ => Err(Error::Returned(format!(
            "Minter descriptor import failed: {result:?}"
        ))),
    }
}

/// Returns whether this call created the wallet. Existing wallets are assumed
/// to have their descriptor and transaction history already indexed.
fn load_or_create_wallet(node: &Client, name: &str) -> Result<bool> {
    if node.list_wallets()?.0.iter().any(|loaded| loaded == name) {
        return Ok(false);
    }
    let exists = node
        .list_wallet_dir()?
        .wallets
        .iter()
        .any(|wallet| wallet.name == name);
    let result = if exists {
        node.load_wallet(name).map(|_| ())
    } else {
        create_watch_only_wallet(node, name)
    };
    match result {
        Ok(_) => Ok(!exists),
        // Another connection may have loaded or created the same named wallet.
        Err(_) if node.list_wallets()?.0.iter().any(|loaded| loaded == name) => Ok(false),
        Err(error) => Err(error),
    }
}

fn create_watch_only_wallet(node: &Client, name: &str) -> Result<()> {
    // The typed create_wallet method does not expose these options.
    node.call::<CreateWallet>(
        "createwallet",
        &[
            json!(name),
            json!(true),  // disable_private_keys
            json!(true),  // blank
            json!(""),    // passphrase
            json!(false), // avoid_reuse
            json!(true),  // descriptors
        ],
    )?;
    Ok(())
}
