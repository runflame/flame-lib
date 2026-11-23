//! The watch-only wallet of a minter in Bitcoin Core, over RPC.

use bitcoin::{Amount, OutPoint, Script, ScriptBuf, Transaction, Txid};
use corepc_client::types::v31::{
    CreateWallet, GetDescriptorInfo, ImportDescriptors, ListWalletDir, ListWallets, LoadWallet,
};
use serde_json::json;

use super::rpc_client::{BtcFundOptions, BtcUnspentOutput, RpcClient, unsigned_input};
use crate::BitcoinConfig;
use crate::wallet::{BtcFundedTransaction, BtcFundingInput, WalletError, WatchOnlyWallet};

/// [`WatchOnlyWallet`]: a wallet of Bitcoin Core that watches the P2WSH
/// output of one witness script and holds no keys.
pub struct RpcWatchOnlyWallet {
    client: RpcClient,
}

impl RpcWatchOnlyWallet {
    /// The wallet of the node that watches the P2WSH output of
    /// `witness_script`.
    ///
    /// The first call creates the wallet in Bitcoin Core, the next ones load
    /// it. An existing wallet is taken to have its descriptor and its history
    /// already.
    pub async fn open(
        config: &BitcoinConfig,
        witness_script: &Script,
    ) -> Result<Self, WalletError> {
        let name = format!("flame-minter-{}", witness_script.wscript_hash());
        let node = RpcClient::new(&config.node_rpc_url, config.auth.clone())?;
        let created = load_or_create(&node, &name).await?;

        let wallet = RpcWatchOnlyWallet {
            client: RpcClient::new(&config.wallet_rpc_url(&name), config.auth.clone())?,
        };
        if created {
            wallet.import_descriptor(witness_script).await?;
        }
        Ok(wallet)
    }

    /// Makes this wallet watch the P2WSH output of `witness_script`, from now
    /// on: nothing older is scanned.
    async fn import_descriptor(&self, witness_script: &Script) -> Result<(), WalletError> {
        let raw = format!("raw({})", witness_script.to_p2wsh().to_hex_string());
        let info: GetDescriptorInfo = self.client.call("getdescriptorinfo", &[json!(raw)]).await?;
        let request = json!([{ "desc": info.descriptor, "timestamp": "now" }]);
        let result: ImportDescriptors = self.client.call("importdescriptors", &[request]).await?;
        match result.0.as_slice() {
            [import] if import.success => Ok(()),
            _ => Err(WalletError::Refused(format!(
                "the import of the minter descriptor failed: {result:?}"
            ))),
        }
    }
}

/// Loads the wallet `name` of the node, or creates it. Returns whether
/// this call created it.
async fn load_or_create(node: &RpcClient, name: &str) -> Result<bool, WalletError> {
    if is_loaded(node, name).await? {
        return Ok(false);
    }
    let directory: ListWalletDir = node.call("listwalletdir", &[]).await?;
    let exists = directory.wallets.iter().any(|wallet| wallet.name == name);
    // In both calls `load_on_startup` is set: Bitcoin Core loads the
    // wallet again after a restart.
    let result = if exists {
        node.call::<LoadWallet>("loadwallet", &[json!(name), json!(true)])
            .await
            .map(|_| ())
    } else {
        let args = [
            json!(name),
            json!(true),  // disable_private_keys
            json!(true),  // blank
            json!(""),    // passphrase
            json!(false), // avoid_reuse
            json!(true),  // descriptors
            json!(true),  // load_on_startup
        ];
        node.call::<CreateWallet>("createwallet", &args)
            .await
            .map(|_| ())
    };
    match result {
        Ok(()) => Ok(!exists),
        // Another connection may have loaded or created the same wallet.
        Err(_) if is_loaded(node, name).await? => Ok(false),
        Err(error) => Err(error),
    }
}

async fn is_loaded(node: &RpcClient, name: &str) -> Result<bool, WalletError> {
    let loaded: ListWallets = node.call("listwallets", &[]).await?;
    Ok(loaded.0.iter().any(|loaded| loaded == name))
}

/// Inputs are not reserved: the caller sends the transaction before it funds
/// the next one.
impl WatchOnlyWallet for RpcWatchOnlyWallet {
    /// Select confirmed outputs belonging to the script and return change to it.
    async fn fund_from_script(
        &self,
        transaction: &Transaction,
        script_pubkey: &ScriptBuf,
        input_weight: u64,
    ) -> Result<BtcFundedTransaction, WalletError> {
        if !transaction.input.is_empty() {
            return Err(WalletError::Refused(
                "funding requires a transaction without inputs".to_owned(),
            ));
        }
        let utxos = confirmed_for_script(self.client.list_unspent().await?, script_pubkey);

        // The largest output first, then the two largest, and so on, until
        // they pay for the transaction. Without an output there is nothing
        // to try.
        let mut refusal = WalletError::NoConfirmedOutputs;
        for count in 1..=utxos.len() {
            let selected = &utxos[..count];
            let outpoints = selected
                .iter()
                .map(|utxo| utxo.outpoint)
                .collect::<Vec<_>>();
            let candidate = add_inputs(transaction, &outpoints);
            // The change returns to the script: every output has its address.
            let change_address = &selected[0].address;
            let options = script_funding_options(&outpoints, change_address, input_weight);

            match self.client.fund_raw_transaction(&candidate, &options).await {
                Ok(funded) => return validate_and_attach_prevouts(funded, selected),
                // These outputs do not pay for it: one more may.
                Err(WalletError::Refused(reason)) => refusal = WalletError::Refused(reason),
                Err(other) => return Err(other),
            }
        }
        Err(refusal)
    }

    /// `sendrawtransaction`: the node sends it, whichever wallet the request
    /// names.
    async fn publish(&self, transaction: &Transaction) -> Result<Txid, WalletError> {
        self.client
            .send_raw_transaction(transaction, Amount::ZERO)
            .await
    }
}

/// The confirmed safe outputs of the script, the largest first.
fn confirmed_for_script(
    utxos: Vec<BtcUnspentOutput>,
    script_pubkey: &ScriptBuf,
) -> Vec<BtcUnspentOutput> {
    let mut utxos = utxos
        .into_iter()
        .filter(|utxo| {
            utxo.confirmations > 0 && utxo.safe && utxo.prevout.script_pubkey == *script_pubkey
        })
        .collect::<Vec<_>>();
    utxos.sort_by(|left, right| right.prevout.value.cmp(&left.prevout.value));
    utxos
}

fn add_inputs(transaction: &Transaction, outpoints: &[OutPoint]) -> Transaction {
    let mut candidate = transaction.clone();
    candidate
        .input
        .extend(outpoints.iter().copied().map(unsigned_input));
    candidate
}

fn script_funding_options(
    outpoints: &[OutPoint],
    change_address: &str,
    input_weight: u64,
) -> BtcFundOptions {
    BtcFundOptions {
        add_inputs: Some(false),
        change_address: Some(change_address.to_owned()),
        include_watching: Some(true),
        input_weights: outpoints
            .iter()
            .map(|outpoint| (*outpoint, input_weight))
            .collect(),
    }
}

fn validate_and_attach_prevouts(
    transaction: Transaction,
    selected: &[BtcUnspentOutput],
) -> Result<BtcFundedTransaction, WalletError> {
    let inputs = transaction
        .input
        .iter()
        .map(|input| {
            selected
                .iter()
                .find(|utxo| utxo.outpoint == input.previous_output)
                .map(|utxo| BtcFundingInput {
                    outpoint: utxo.outpoint,
                    prevout: utxo.prevout.clone(),
                })
                .ok_or(WalletError::UnexpectedTransaction)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BtcFundedTransaction {
        transaction,
        inputs,
    })
}

#[cfg(test)]
mod tests {
    use bitcoin::hashes::Hash;
    use bitcoin::{Amount, OutPoint, ScriptBuf, TxOut, Txid};

    use super::{BtcUnspentOutput, confirmed_for_script};

    fn script(byte: u8) -> ScriptBuf {
        ScriptBuf::from_bytes(vec![byte])
    }

    /// An output of the wallet; `vout` tells the outputs apart.
    fn utxo(vout: u32, script_pubkey: ScriptBuf, sat: u64) -> BtcUnspentOutput {
        BtcUnspentOutput {
            outpoint: OutPoint::new(Txid::all_zeros(), vout),
            prevout: TxOut {
                value: Amount::from_sat(sat),
                script_pubkey,
            },
            address: String::new(),
            confirmations: 1,
            safe: true,
        }
    }

    #[test]
    fn only_confirmed_safe_outputs_of_the_script_are_selected_the_largest_first() {
        let ours = script(0x51);
        let unconfirmed = BtcUnspentOutput {
            confirmations: 0,
            ..utxo(3, ours.clone(), 9_000)
        };
        let unsafe_to_spend = BtcUnspentOutput {
            safe: false,
            ..utxo(4, ours.clone(), 9_000)
        };
        let wallet = vec![
            utxo(0, ours.clone(), 1_000),
            utxo(1, script(0x52), 5_000),
            utxo(2, ours.clone(), 3_000),
            unconfirmed,
            unsafe_to_spend,
        ];

        let selected = confirmed_for_script(wallet, &ours);

        let vouts: Vec<u32> = selected.iter().map(|utxo| utxo.outpoint.vout).collect();
        assert_eq!(vouts, [2, 0]);
    }
}
