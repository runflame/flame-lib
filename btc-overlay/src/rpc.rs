//! The Bitcoin Core JSON-RPC subset `btc-integration` uses, plus `getoverlayinfo`.
//!
//! Responses are built from the `corepc-types` structs the client parses, so the wire
//! format matches by construction. The wallet calls are the watch-only subset a minter
//! uses (see [`wallet`]).
//!
//! The RPC username names the client; a client with a lag gets every chain answer from its
//! delayed view (see [`crate::lag`]). The password is not checked.

mod wallet;

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::State as Extract,
    http::{HeaderMap, Uri, header::AUTHORIZATION},
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use corepc_client::{
    bitcoin::{
        Address, Amount, Block, BlockHash, Network, Script, Transaction, consensus::encode,
        hex::DisplayHex,
    },
    types::{
        ScriptSig,
        v31::{
            CoinbaseTransaction, GetBlockHeaderVerbose, GetBlockVerboseThree,
            GetBlockVerboseThreePrevout, GetBlockVerboseThreeTransaction, GetBlockchainInfo,
            GetRawTransactionVerboseWithPrevout, RawTransactionInput,
            RawTransactionInputWithPrevout, RawTransactionOutput, ScriptPubKey, WaitForNewBlock,
        },
    },
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    lag::{Lag, View},
    overlay::{Overlay, Tip},
    state::{self, Entry},
};

const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMETER: i32 = -8;
const INVALID_ADDRESS_OR_KEY: i32 = -5;
const DESERIALIZATION_ERROR: i32 = -22;
const IN_WARMUP: i32 = -28;

pub fn router(overlay: Arc<Overlay>) -> Router {
    Router::new().fallback(handle).with_state(overlay)
}

struct RpcError(i32, String);

type RpcResult = Result<Value, RpcError>;

async fn handle(
    Extract(overlay): Extract<Arc<Overlay>>,
    uri: Uri,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Json<Value> {
    let client = client(&headers);
    let client = client.as_deref();
    // Wallet calls go to `/wallet/<name>`, as in Bitcoin Core.
    let wallet = uri
        .path()
        .strip_prefix("/wallet/")
        .map(|name| name.trim_end_matches('/'));
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = match request.get("params") {
        Some(Value::Array(params)) => params.clone(),
        _ => Vec::new(),
    };
    let result = match method {
        "getblockchaininfo" => blockchain_info(&overlay, &overlay.view(client)),
        "getoverlayinfo" => overlay_info(&overlay, client),
        "setclientlag" => set_client_lag(&overlay, &params),
        "getblockcount" => tip(&overlay.view(client)).map(|tip| json!(tip.height)),
        "getbestblockhash" => tip(&overlay.view(client)).map(|tip| json!(tip.hash)),
        "getblockhash" => block_hash(&overlay, &overlay.view(client), &params),
        "getblockheader" => block_header(&overlay, &overlay.view(client), &params),
        "getblock" => block(&overlay, &overlay.view(client), &params),
        "waitfornewblock" => wait_for_new_block(&overlay, client, &params).await,
        "sendrawtransaction" => send_raw_transaction(&overlay, &params),
        "createwallet" => wallet::create(&overlay, &params),
        "loadwallet" => wallet::load(&overlay, &params),
        "listwallets" => wallet::list(&overlay),
        "listwalletdir" => wallet::list_dir(&overlay),
        "getdescriptorinfo" => wallet::descriptor_info(&params),
        "getwalletinfo" => wallet::info(&overlay, wallet),
        "importdescriptors" => wallet::import_descriptors(&overlay, wallet, &params),
        "listunspent" => wallet::list_unspent(&overlay, wallet),
        "fundrawtransaction" => wallet::fund(&overlay, wallet, &params),
        _ => Err(RpcError(METHOD_NOT_FOUND, "Method not found".into())),
    };
    Json(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "result": result, "id": id }),
        Err(RpcError(code, message)) => json!({
            "jsonrpc": "2.0",
            "error": { "code": code, "message": message },
            "id": id,
        }),
    })
}

/// The username of a Basic `Authorization` header.
fn client(headers: &HeaderMap) -> Option<String> {
    let encoded = headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Basic ")?;
    let decoded = String::from_utf8(BASE64.decode(encoded.trim()).ok()?).ok()?;
    let (user, _password) = decoded.split_once(':')?;
    Some(user.to_owned())
}

fn to_value<T: serde::Serialize>(value: T) -> RpcResult {
    Ok(serde_json::to_value(value).expect("RPC results serialize"))
}

fn param(params: &[Value], index: usize) -> Option<&Value> {
    params.get(index).filter(|value| !value.is_null())
}

fn hash_param(params: &[Value], index: usize) -> Result<BlockHash, RpcError> {
    param(params, index)
        .and_then(Value::as_str)
        .and_then(|hash| hash.parse().ok())
        .ok_or_else(|| RpcError(INVALID_PARAMETER, "invalid block hash".into()))
}

fn tip(view: &View) -> Result<Tip, RpcError> {
    view.tip
        .ok_or_else(|| RpcError(IN_WARMUP, "Loading the first headers".into()))
}

fn blockchain_info(overlay: &Overlay, view: &View) -> RpcResult {
    let state = overlay.state();
    let tip = state
        .block(&tip(view)?.hash)
        .expect("a viewed tip is connected");
    let header = tip.header.header;
    to_value(GetBlockchainInfo {
        chain: "main".into(),
        blocks: tip.header.height as i64,
        headers: tip.header.height as i64,
        best_block_hash: tip.hash().to_string(),
        bits: bits_hex(&header),
        target: header.target().to_be_bytes().to_lower_hex_string(),
        difficulty: header.difficulty_float(),
        time: i64::from(header.time),
        median_time: i64::from(header.time),
        verification_progress: 1.0,
        initial_block_download: false,
        chain_work: header.work().to_be_bytes().to_lower_hex_string(),
        size_on_disk: 0,
        pruned: false,
        prune_height: None,
        automatic_pruning: None,
        prune_target_size: None,
        signet_challenge: None,
        warnings: vec!["Flame overlay: real headers, simulated transactions".into()],
    })
}

/// The live tip, and the caller's view and lag.
fn overlay_info(overlay: &Overlay, client: Option<&str>) -> RpcResult {
    let lag = client.and_then(|client| overlay.lag(client));
    let view = overlay.view(client);
    let state = overlay.state();
    Ok(json!({
        "start_height": state.start_height(),
        "tip": state.tip().map(|block| block.hash()),
        "queued": state.queued(),
        "client": client,
        "lag": lag.map(|lag| lag.to_string()),
        "client_tip": view.tip.map(|tip| tip.hash),
    }))
}

/// `setclientlag <client> <lag>`: seconds like 30, a range like "10..120", or null for none.
/// It lasts until the overlay restarts, which goes back to the lag config.
fn set_client_lag(overlay: &Overlay, params: &[Value]) -> RpcResult {
    let client = param(params, 0)
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError(INVALID_PARAMETER, "invalid client name".into()))?;
    let lag = param(params, 1)
        .map(|lag| Lag::deserialize(lag).map_err(|e| RpcError(INVALID_PARAMETER, e.to_string())))
        .transpose()?;
    overlay.set_lag(client, lag);
    Ok(Value::Null)
}

fn block_hash(overlay: &Overlay, view: &View, params: &[Value]) -> RpcResult {
    let height = param(params, 0)
        .and_then(Value::as_u64)
        .ok_or_else(|| RpcError(INVALID_PARAMETER, "invalid height".into()))?;
    overlay
        .state()
        .hash_on_branch(&tip(view)?.hash, height)
        .map(|hash| json!(hash))
        .ok_or_else(|| RpcError(INVALID_PARAMETER, "Block height out of range".into()))
}

/// The block, its confirmations (-1 off the viewed chain) and the next block's hash.
fn lookup(
    overlay: &Overlay,
    view: &View,
    hash: BlockHash,
) -> Result<(state::Block, i64, Option<BlockHash>), RpcError> {
    let not_found = || RpcError(INVALID_ADDRESS_OR_KEY, "Block not found".into());
    if !view.shows(&hash) {
        return Err(not_found());
    }
    let tip = tip(view)?;
    let state = overlay.state();
    let block = state.block(&hash).cloned().ok_or_else(not_found)?;
    let height = block.header.height;
    let (confirmations, next) = if state.hash_on_branch(&tip.hash, height) == Some(hash) {
        let confirmations = (tip.height - height + 1) as i64;
        (confirmations, state.hash_on_branch(&tip.hash, height + 1))
    } else {
        (-1, None)
    };
    Ok((block, confirmations, next))
}

fn block_header(overlay: &Overlay, view: &View, params: &[Value]) -> RpcResult {
    let (block, confirmations, next) = lookup(overlay, view, hash_param(params, 0)?)?;
    let header = block.header.header;
    if param(params, 1).and_then(Value::as_bool) == Some(false) {
        return Ok(json!(encode::serialize_hex(&header)));
    }
    to_value(GetBlockHeaderVerbose {
        hash: block.hash().to_string(),
        confirmations,
        height: block.header.height as i64,
        version: header.version.to_consensus(),
        version_hex: format!("{:08x}", header.version.to_consensus()),
        merkle_root: header.merkle_root.to_string(),
        time: i64::from(header.time),
        median_time: i64::from(header.time),
        nonce: i64::from(header.nonce),
        bits: bits_hex(&header),
        target: header.target().to_be_bytes().to_lower_hex_string(),
        difficulty: header.difficulty_float(),
        chain_work: header.work().to_be_bytes().to_lower_hex_string(),
        n_tx: (block.injected.len() + 1) as u32,
        previous_block_hash: Some(header.prev_blockhash.to_string()),
        next_block_hash: next.map(|hash| hash.to_string()),
    })
}

fn block(overlay: &Overlay, view: &View, params: &[Value]) -> RpcResult {
    let (block, confirmations, next) = lookup(overlay, view, hash_param(params, 0)?)?;
    let entries = block.entries();
    let full = Block {
        header: block.header.header,
        txdata: entries
            .iter()
            .map(|entry| entry.transaction.clone())
            .collect(),
    };
    match param(params, 1).and_then(Value::as_u64).unwrap_or(1) {
        0 => Ok(json!(encode::serialize_hex(&full))),
        3 => verbose_block(&block, &full, &entries, confirmations, next),
        _ => Err(RpcError(
            INVALID_PARAMETER,
            "the overlay serves getblock verbosity 0 and 3 only".into(),
        )),
    }
}

fn verbose_block(
    block: &state::Block,
    full: &Block,
    entries: &[Entry],
    confirmations: i64,
    next: Option<BlockHash>,
) -> RpcResult {
    let header = full.header;
    let coinbase = &entries[0].transaction;
    to_value(GetBlockVerboseThree {
        hash: block.hash().to_string(),
        confirmations,
        size: full.total_size() as i64,
        // Weight is three times the stripped size plus the total size.
        stripped_size: Some(((full.weight().to_wu() - full.total_size() as u64) / 3) as i64),
        weight: full.weight().to_wu(),
        coinbase_tx: CoinbaseTransaction {
            version: coinbase.version.0,
            locktime: coinbase.lock_time.to_consensus_u32(),
            sequence: coinbase.input[0].sequence.0,
            coinbase: coinbase.input[0].script_sig.to_hex_string(),
            witness: None,
        },
        height: block.header.height as i64,
        version: header.version.to_consensus(),
        version_hex: format!("{:08x}", header.version.to_consensus()),
        merkle_root: header.merkle_root.to_string(),
        tx: entries
            .iter()
            .map(|entry| verbose_transaction(entry, block, confirmations))
            .collect(),
        time: i64::from(header.time),
        median_time: Some(i64::from(header.time)),
        nonce: i64::from(header.nonce),
        bits: bits_hex(&header),
        target: header.target().to_be_bytes().to_lower_hex_string(),
        difficulty: header.difficulty_float(),
        chain_work: header.work().to_be_bytes().to_lower_hex_string(),
        n_tx: entries.len() as i64,
        previous_block_hash: Some(header.prev_blockhash.to_string()),
        next_block_hash: next.map(|hash| hash.to_string()),
    })
}

fn verbose_transaction(
    entry: &Entry,
    block: &state::Block,
    confirmations: i64,
) -> GetBlockVerboseThreeTransaction {
    let transaction = &entry.transaction;
    let time = u64::from(block.header.header.time);
    let inputs = transaction
        .input
        .iter()
        .zip(&entry.prevouts)
        .map(|(input, prevout)| RawTransactionInputWithPrevout {
            input: RawTransactionInput {
                coinbase: transaction
                    .is_coinbase()
                    .then(|| input.script_sig.to_hex_string()),
                txid: (!transaction.is_coinbase()).then(|| input.previous_output.txid.to_string()),
                vout: (!transaction.is_coinbase()).then_some(input.previous_output.vout),
                script_sig: (!transaction.is_coinbase()).then(|| ScriptSig {
                    asm: input.script_sig.to_asm_string(),
                    hex: input.script_sig.to_hex_string(),
                }),
                txin_witness: (!input.witness.is_empty()).then(|| {
                    input
                        .witness
                        .iter()
                        .map(|item| item.to_lower_hex_string())
                        .collect()
                }),
                sequence: input.sequence.0,
            },
            prevout: prevout.as_ref().map(|prevout| GetBlockVerboseThreePrevout {
                generated: false,
                height: block.header.height as i64,
                value: prevout.value.to_btc(),
                script_pubkey: script_pubkey(&prevout.script_pubkey),
            }),
        })
        .collect();
    GetBlockVerboseThreeTransaction {
        transaction: GetRawTransactionVerboseWithPrevout {
            in_active_chain: None,
            hex: encode::serialize_hex(transaction),
            txid: transaction.compute_txid().to_string(),
            hash: transaction.compute_wtxid().to_string(),
            size: transaction.total_size() as u64,
            vsize: transaction.vsize() as u64,
            weight: transaction.weight().to_wu(),
            version: transaction.version.0,
            lock_time: transaction.lock_time.to_consensus_u32(),
            inputs,
            outputs: transaction
                .output
                .iter()
                .enumerate()
                .map(|(index, output)| RawTransactionOutput {
                    value: output.value.to_btc(),
                    index: index as u64,
                    script_pubkey: script_pubkey(&output.script_pubkey),
                })
                .collect(),
            block_hash: Some(block.hash().to_string()),
            confirmations: u64::try_from(confirmations).ok(),
            transaction_time: Some(time),
            block_time: Some(time),
        },
        fee: None,
    }
}

fn script_pubkey(script: &Script) -> ScriptPubKey {
    let type_ = if script.is_op_return() {
        "nulldata"
    } else if script.is_p2wsh() {
        "witness_v0_scripthash"
    } else if script.is_p2wpkh() {
        "witness_v0_keyhash"
    } else if script.is_p2tr() {
        "witness_v1_taproot"
    } else if script.is_p2pkh() {
        "pubkeyhash"
    } else if script.is_p2sh() {
        "scripthash"
    } else {
        "nonstandard"
    };
    ScriptPubKey {
        asm: script.to_asm_string(),
        descriptor: None,
        hex: script.to_hex_string(),
        required_signatures: None,
        type_: type_.into(),
        address: Address::from_script(script, Network::Bitcoin)
            .ok()
            .map(|address| address.to_string()),
        addresses: None,
    }
}

fn bits_hex(header: &corepc_client::bitcoin::block::Header) -> String {
    format!("{:08x}", header.bits.to_consensus())
}

async fn wait_for_new_block(
    overlay: &Overlay,
    client: Option<&str>,
    params: &[Value],
) -> RpcResult {
    let timeout_ms = param(params, 0).and_then(Value::as_u64).unwrap_or(0);
    let mut tips = overlay.subscribe();
    let known = match param(params, 1) {
        Some(_) => Some(hash_param(params, 1)?),
        None => overlay.view(client).tip.map(|tip| tip.hash),
    };
    // Wakes on a tip change, or when a delayed one becomes visible to the client.
    let changed = async {
        loop {
            tips.borrow_and_update();
            let view = overlay.view(client);
            if view.tip.is_some_and(|tip| Some(tip.hash) != known) {
                return;
            }
            let visible = async {
                match view.next {
                    Some(at) => tokio::time::sleep_until(at.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                changed = tips.changed() => if changed.is_err() {
                    std::future::pending::<()>().await;
                },
                () = visible => {}
            }
        }
    };
    if timeout_ms == 0 {
        changed.await;
    } else {
        let _ = tokio::time::timeout(Duration::from_millis(timeout_ms), changed).await;
    }
    let tip = tip(&overlay.view(client))?;
    to_value(WaitForNewBlock {
        hash: tip.hash.to_string(),
        height: tip.height as i64,
    })
}

fn send_raw_transaction(overlay: &Overlay, params: &[Value]) -> RpcResult {
    let transaction: Transaction = param(params, 0)
        .and_then(Value::as_str)
        .and_then(|hex| encode::deserialize_hex(hex).ok())
        .ok_or_else(|| RpcError(DESERIALIZATION_ERROR, "TX decode failed".into()))?;
    let max_burn = match param(params, 2) {
        Some(value) => value
            .as_f64()
            .and_then(|btc| Amount::from_btc(btc).ok())
            .ok_or_else(|| RpcError(INVALID_PARAMETER, "invalid maxburnamount".into()))?,
        None => Amount::ZERO,
    };
    overlay
        .state()
        .submit(transaction, max_burn)
        .map(|txid| json!(txid))
        .map_err(|error| RpcError(error.code(), error.to_string()))
}
