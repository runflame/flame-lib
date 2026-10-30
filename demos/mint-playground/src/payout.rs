//! Pays the ledger's rewards on chain, in FLM, from one funded wallet.
//! Each reward is a cleartext token, so anyone can audit what was paid
//! to whom; the payout wallet's change stays confidential.
//!
//! Enabled by `MINT_PAYOUT_MNEMONIC_FILE` (a file holding the payout
//! wallet's BIP-39 phrase, such as a podman secret) together with
//! `MINT_PAYOUT_NODE` (the flamed RPC URL). Without either, the playground
//! stays a pure simulation.
//!
//! One payout is in flight at a time. Its change is spendable only once it
//! is mined, so a second transaction before then would have nothing to
//! spend. Each payout is written to the ledger before it is submitted, and
//! forgotten only when it can never be mined: the node does not know it and
//! is two blocks past the tip its proofs were read at.

use std::time::Duration;

use flamed_rpc::TxStatusResult;
use flamewallet_cli::{Flavor, Payment, Session, DEFAULT_FEE, DEFAULT_GAS};
use flamewallet_ffi::{Network, Wallet};

use crate::{db, Command, Store};

/// Recipients per transaction. With the change that makes 13 outputs, the
/// most a single-input transfer proves (`flamepayments::MAX_OUTPUTS`).
const RECIPIENTS: i64 = 12;

/// How often the ledger and the node are checked.
const INTERVAL: Duration = Duration::from_secs(5);

/// One FLM: smaller balances wait until they grow.
const DEFAULT_MIN_SPARKS: i64 = 100_000_000;

const MEMO: &[u8] = b"Mint playground reward";

/// Writes a fresh payout wallet phrase, and nothing else, to `out`: what
/// `--new-payout-key` pipes into `podman secret create`, so the phrase is
/// born on the host that keeps it and is never shown or stored anywhere
/// else.
pub fn new_key(out: &mut impl std::io::Write) -> Result<(), Box<dyn std::error::Error>> {
    let phrase = zeroize::Zeroizing::new(flamewallet_ffi::generate_mnemonic(24)?);
    out.write_all(phrase.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

pub struct Payout {
    session: Session,
    min_sparks: i64,
}

impl Payout {
    pub fn from_env() -> Result<Option<Self>, String> {
        let file = std::env::var_os("MINT_PAYOUT_MNEMONIC_FILE");
        let node = std::env::var("MINT_PAYOUT_NODE").ok();
        let (file, node) = match (file, node) {
            (None, None) => return Ok(None),
            (Some(file), Some(node)) => (file, node),
            _ => {
                return Err(
                    "Set both MINT_PAYOUT_MNEMONIC_FILE and MINT_PAYOUT_NODE, or neither.".into(),
                )
            }
        };
        let min_sparks = match std::env::var("MINT_PAYOUT_MIN_SPARKS") {
            Ok(value) => value
                .parse::<i64>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or("MINT_PAYOUT_MIN_SPARKS must be a positive integer.")?,
            Err(_) => DEFAULT_MIN_SPARKS,
        };
        let phrase = std::fs::read_to_string(&file)
            .map_err(|error| format!("{}: {error}", file.to_string_lossy()))?;
        let wallet =
            Wallet::from_mnemonic(phrase.trim().into(), String::new(), Network::Testnet, 0)
                .map_err(|error| format!("payout wallet: {error}"))?;
        let client = flamed_rpc::HttpClientBuilder::default()
            .build(&node)
            .map_err(|error| format!("payout node {node}: {error}"))?;
        let session = Session {
            wallet,
            client,
            watch: Vec::new(),
        };
        let address = session
            .watched()
            .map_err(|error| error.to_string())?
            .remove(0)
            .receiving_address;
        println!("Payouts: from {address} via {node}, at least {min_sparks} sparks each");
        Ok(Some(Self {
            session,
            min_sparks,
        }))
    }

    /// Runs until the ledger goes away. Node and wallet errors are printed
    /// when they change and retried, never fatal: the simulation goes on.
    pub async fn run(self, store: Store) -> Result<(), String> {
        let mut last_error = String::new();
        loop {
            tokio::time::sleep(INTERVAL).await;
            match self.round(&store).await {
                Ok(()) => last_error.clear(),
                Err(Failure::Ledger(error)) => return Err(error),
                Err(Failure::Chain(error)) => {
                    if error != last_error {
                        eprintln!("Payout: {error}");
                        last_error = error;
                    }
                }
            }
        }
    }

    async fn round(&self, store: &Store) -> Result<(), Failure> {
        let state = ledger(store, |reply| {
            Command::PayoutState(self.min_sparks, RECIPIENTS, reply)
        })
        .await?;
        if let Some(pending) = state.pending {
            return self.settle(store, pending).await;
        }
        if state.due.is_empty() {
            return Ok(());
        }
        let payments: Vec<Payment> = state
            .due
            .iter()
            .map(|item| Payment {
                to: item.address.clone(),
                qty: item.qty_sparks as u64,
                flavor: Flavor::flame(),
                memo: MEMO.to_vec(),
                clear: true,
            })
            .collect();
        let built = self
            .session
            .build_payments(&payments, DEFAULT_FEE, DEFAULT_GAS)
            .await
            .map_err(chain)?;
        let txid = built.txid.to_string();
        let total: i64 = state.due.iter().map(|item| item.qty_sparks).sum();
        let count = state.due.len();
        let pending = db::PendingPayout {
            txid: txid.clone(),
            tip_height: built.tip_height as i64,
        };
        ledger(store, |reply| {
            Command::RecordPayout(pending, state.due, reply)
        })
        .await?;
        // A failed submission stays pending: whether the node took it is
        // settled by asking, once the answer can no longer change.
        self.session.submit(&built).await.map_err(chain)?;
        println!("Payout {txid}: {total} sparks to {count} addresses");
        Ok(())
    }

    async fn settle(&self, store: &Store, pending: db::PendingPayout) -> Result<(), Failure> {
        let txid = serde_json::from_value(pending.txid.clone().into())
            .map_err(|_| Failure::Ledger(format!("payout {} has an invalid txid", pending.txid)))?;
        let height = match self.session.status(txid).await.map_err(chain)? {
            TxStatusResult::Mempool => return Ok(()),
            TxStatusResult::Confirmed { height, .. } => Some(height as i64),
            TxStatusResult::Unknown => {
                let tip = self.session.tip().await.map_err(chain)?.height as i64;
                if tip <= pending.tip_height + 1 {
                    return Ok(());
                }
                eprintln!("Payout {}: never mined; paying again", pending.txid);
                None
            }
        };
        let txid = pending.txid;
        ledger(store, |reply| Command::SettlePayout(txid, height, reply)).await
    }
}

enum Failure {
    /// The ledger is gone or refused a write: stop.
    Ledger(String),
    /// The node or the wallet: try again next round.
    Chain(String),
}

fn chain(error: flamewallet_cli::Error) -> Failure {
    Failure::Chain(error.to_string())
}

/// Like the clock, waits for queue capacity instead of shedding load.
async fn ledger<T>(
    store: &Store,
    command: impl FnOnce(crate::Reply<T>) -> Command,
) -> Result<T, Failure> {
    let (reply, result) = tokio::sync::oneshot::channel();
    store
        .0
        .send(command(reply))
        .await
        .map_err(|_| Failure::Ledger("ledger stopped".into()))?;
    result
        .await
        .map_err(|_| Failure::Ledger("ledger stopped".into()))?
        .map_err(|error| Failure::Ledger(error.to_string()))
}
