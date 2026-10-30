//! One wallet talking to one node.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use flamed_rpc::{
    codes, BlockTxEnvelope, ClientError, ContractId, Counterparty, FlamedApiClient,
    HistoryContract, HistoryEntry, HttpClient, HttpClientBuilder, TipResult, TxId, TxStatusResult,
};
use flamewallet_ffi::{
    ContractValue, FlameError, IssueRequest as FfiIssueRequest, IssuedAddress, KeyPath, Opening,
    Transfer, TransferInput, TransferOutput, TransferRequest, Wallet, CHANGE, RECEIVING,
};
use serde::{Serialize, Serializer};

use crate::amount::{format_qty, Flavor};
use crate::error::Error;
use crate::path::DerivationPath;
use crate::store::WalletFile;

/// What a send pays when the caller names no fee. The devnet's floor is
/// zero; this is what the node's own tests and Safely pay.
pub const DEFAULT_FEE: u64 = 1_000;

/// A limit, not a price: gas is not charged yet, and the largest transfer
/// burns under 40k. Safely sends the same.
pub const DEFAULT_GAS: u64 = 1_000_000;

/// How often [`Session::wait`] asks.
const POLL: Duration = Duration::from_secs(1);

pub struct Session {
    pub wallet: Arc<Wallet>,
    pub client: HttpClient,
    /// Paths watched beyond the receiving and change ones.
    pub watch: Vec<DerivationPath>,
}

/// One unspent contract locked to one of the wallet's keys.
#[derive(Clone, Debug, Serialize)]
pub struct Coin {
    pub id: ContractId,
    pub txid: TxId,
    pub branch: u32,
    pub index: u32,
    /// Absent while the transaction that creates it is pending.
    pub height: Option<u64>,
    #[serde(flatten)]
    pub value: CoinValue,
    #[serde(skip)]
    contract: Vec<u8>,
    #[serde(skip)]
    proof: Option<Vec<u8>>,
    #[serde(skip)]
    opening: Option<Opening>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoinValue {
    /// A cleartext token; a genesis allocation and `issuepub` output are.
    Clear {
        #[serde(serialize_with = "as_string")]
        qty: u64,
        flavor: Flavor,
    },
    /// A confidential token whose note opened.
    Confidential {
        #[serde(serialize_with = "as_string")]
        qty: u64,
        flavor: Flavor,
        memo: String,
    },
    /// A confidential token whose note did not open: ours, never counted.
    Unreadable { reason: String },
    /// Not a bare token; no transfer spends it.
    NotToken,
}

impl CoinValue {
    pub fn token(&self) -> Option<(u64, Flavor)> {
        match self {
            CoinValue::Clear { qty, flavor } | CoinValue::Confidential { qty, flavor, .. } => {
                Some((*qty, *flavor))
            }
            CoinValue::Unreadable { .. } | CoinValue::NotToken => None,
        }
    }
}

impl Coin {
    /// Confirmed, readable, and with a proof against the tip it was read at.
    pub fn is_spendable(&self) -> bool {
        self.proof.is_some() && self.value.token().is_some()
    }
}

/// What the wallet holds, and the tip it was read at.
#[derive(Clone, Debug, Serialize)]
pub struct Coins {
    pub tip_height: u64,
    pub coins: Vec<Coin>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Balance {
    pub flavor: Flavor,
    /// Confirmed coins, every one spendable now.
    #[serde(serialize_with = "as_string")]
    pub confirmed: u128,
    /// Coins a pending transaction creates: change on its way back, or a
    /// payment not yet mined.
    #[serde(serialize_with = "as_string")]
    pub pending: u128,
    pub coins: usize,
}

impl Coins {
    /// Per flavor, Flame first. Unreadable and non-token coins count in
    /// no flavor.
    pub fn balances(&self) -> Vec<Balance> {
        let mut by_flavor: BTreeMap<(bool, Flavor), Balance> = BTreeMap::new();
        for coin in &self.coins {
            let Some((qty, flavor)) = coin.value.token() else {
                continue;
            };
            let balance = by_flavor
                .entry((!flavor.is_flame(), flavor))
                .or_insert(Balance {
                    flavor,
                    confirmed: 0,
                    pending: 0,
                    coins: 0,
                });
            if coin.height.is_some() {
                balance.confirmed += u128::from(qty);
            } else {
                balance.pending += u128::from(qty);
            }
            balance.coins += 1;
        }
        if by_flavor.is_empty() {
            by_flavor.insert(
                (false, Flavor::flame()),
                Balance {
                    flavor: Flavor::flame(),
                    confirmed: 0,
                    pending: 0,
                    coins: 0,
                },
            );
        }
        by_flavor.into_values().collect()
    }

    /// Coins counted in no balance.
    pub fn uncounted(&self) -> usize {
        self.coins
            .iter()
            .filter(|c| c.value.token().is_none())
            .count()
    }
}

pub struct SendRequest {
    /// Spend only coins at this path, and return change there; otherwise
    /// any coin, with change to the change address.
    pub from: Option<DerivationPath>,
    pub to: String,
    pub qty: u64,
    pub flavor: Flavor,
    pub memo: Vec<u8>,
    /// Pay a cleartext token: the amount public, the memo in the clear.
    pub clear: bool,
    /// In sparks.
    pub fee: u64,
    pub gas: u64,
}

/// One recipient of [`Session::build_payments`].
#[derive(Clone, Debug)]
pub struct Payment {
    pub to: String,
    pub qty: u64,
    pub flavor: Flavor,
    pub memo: Vec<u8>,
    /// Pay a cleartext token: the amount public, the memo in the clear.
    pub clear: bool,
}

/// A signed transfer the node has not seen yet.
pub struct Built {
    pub txid: TxId,
    pub block_tx: Vec<u8>,
    pub inputs: Vec<ContractId>,
    /// The tip its proofs were read at. They survive one block past it, so
    /// once the node is two blocks further on, this transfer can never be
    /// accepted.
    pub tip_height: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Sent {
    pub txid: TxId,
    pub inputs: Vec<ContractId>,
    #[serde(serialize_with = "as_string")]
    pub fee: u64,
}

pub struct IssueRequest {
    /// The issuer: the coin spent is one at this path, and the Flame left
    /// over returns to it.
    pub from: DerivationPath,
    /// Names the flavor, with the issuer's key.
    pub tag: String,
    pub qty: u64,
    /// Where the minted coin goes; the issuer's address when `None`.
    pub to: Option<String>,
    pub memo: Vec<u8>,
    /// In sparks.
    pub fee: u64,
    pub gas: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Issued {
    pub txid: TxId,
    pub flavor: Flavor,
    pub from: DerivationPath,
    /// The coin the issuance spent, at the issuer's address.
    pub input: ContractId,
    #[serde(serialize_with = "as_string")]
    pub fee: u64,
}

/// One transaction that touched the wallet's addresses, as it moved the
/// wallet's balances.
#[derive(Clone, Debug, Serialize)]
pub struct HistoryItem {
    /// All zeros for the genesis allocation.
    pub txid: TxId,
    pub height: Option<u64>,
    pub time: Option<u64>,
    pub pending: bool,
    pub fee_sparks: String,
    /// Received minus spent, per flavor. A send counts its fee here, since
    /// the change came back without it.
    pub changes: Vec<FlavorDelta>,
    /// The memo of a payment received, if the sender wrote one.
    pub memo: Option<String>,
    /// Who paid the wallet: the other addresses whose coins this
    /// transaction spent.
    pub from: Vec<String>,
    /// Whom the wallet paid: when only the wallet's coins were spent, every
    /// other address it created coins for. Empty otherwise, since what a
    /// payer creates beside the wallet's coins is the payer's change.
    pub to: Vec<String>,
    /// Contracts of the wallet's this transaction touched whose amount could
    /// not be read, and so are in no delta.
    pub unreadable: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct FlavorDelta {
    pub flavor: Flavor,
    #[serde(serialize_with = "as_string")]
    pub delta: i128,
}

#[derive(Clone, Debug, Serialize)]
pub struct HistoryPage {
    pub items: Vec<HistoryItem>,
    /// Pass back as `older` for the page before this one.
    pub older: Option<String>,
}

impl Session {
    pub fn open(file: &WalletFile, node: &str) -> Result<Session, Error> {
        Ok(Session {
            wallet: file.open()?,
            client: HttpClientBuilder::default().build(node)?,
            watch: file.watch.clone(),
        })
    }

    /// The address at `path`.
    pub fn address_at(&self, path: DerivationPath) -> Result<IssuedAddress, Error> {
        Ok(self.wallet.address(path.key_path())?)
    }

    /// Every receiving address issued, index 0 always, the one change
    /// address sends return to, and every other path handed out.
    pub fn watched(&self) -> Result<Vec<IssuedAddress>, Error> {
        let issued = self.wallet.next_index().max(1);
        let mut addresses = (0..issued)
            .map(|index| {
                self.wallet.address(KeyPath {
                    branch: RECEIVING,
                    index,
                })
            })
            .collect::<Result<Vec<_>, FlameError>>()?;
        addresses.push(self.change_address()?);
        for &path in &self.watch {
            if !addresses.iter().any(|a| a.path == path.key_path()) {
                addresses.push(self.address_at(path)?);
            }
        }
        Ok(addresses)
    }

    pub fn change_address(&self) -> Result<IssuedAddress, Error> {
        Ok(self.wallet.address(KeyPath {
            branch: CHANGE,
            index: 0,
        })?)
    }

    pub async fn tip(&self) -> Result<TipResult, Error> {
        Ok(self.client.tip().await?)
    }

    /// Every unspent coin, the mempool applied: coins a pending transaction
    /// spends are gone, coins it creates are listed without a height.
    ///
    /// One `unspent` call per address, each answered at its own tip; a
    /// block between two calls is harmless, since a send rebuilds from
    /// scratch if the node refuses its proofs.
    pub async fn coins(&self) -> Result<Coins, Error> {
        self.coins_at(self.watched()?).await
    }

    /// The coins at `from` alone, watched or not; every watched coin when
    /// `None`.
    async fn coins_from(&self, from: Option<DerivationPath>) -> Result<Coins, Error> {
        match from {
            Some(path) => self.coins_at(vec![self.address_at(path)?]).await,
            None => self.coins().await,
        }
    }

    async fn coins_at(&self, addresses: Vec<IssuedAddress>) -> Result<Coins, Error> {
        let mut coins = Vec::new();
        let mut tip_height = 0;
        for address in addresses {
            let unspent = self
                .client
                .unspent(address.receiving_address, Some(true))
                .await?;
            tip_height = tip_height.max(unspent.tip.height);
            for entry in unspent.outputs {
                let note = entry.note.map(Vec::from);
                let (value, opening) = self.read(&entry.bytes.0, note, address.path)?;
                coins.push(Coin {
                    id: entry.id,
                    txid: entry.txid,
                    branch: address.path.branch,
                    index: address.path.index,
                    height: entry.height,
                    value,
                    contract: entry.bytes.0,
                    proof: entry.proof.map(Vec::from),
                    opening,
                });
            }
        }
        Ok(Coins { tip_height, coins })
    }

    /// Builds, proves, signs and submits a payment, with change for every
    /// flavor it spends going back to the change address, or with `from`
    /// to that address.
    ///
    /// A block that lands between reading the coins and submitting makes
    /// the proofs stale and the node refuse them, so a refusal is retried
    /// once from fresh coins; a second one is the node's real answer.
    pub async fn send(&self, request: &SendRequest) -> Result<Sent, Error> {
        let mut retried = false;
        let payment = Payment {
            to: request.to.clone(),
            qty: request.qty,
            flavor: request.flavor,
            memo: request.memo.clone(),
            clear: request.clear,
        };
        loop {
            let coins = self.coins_from(request.from).await?;
            let (transfer, inputs) = self.build(
                request.from,
                std::slice::from_ref(&payment),
                request.fee,
                request.gas,
                &coins,
            )?;
            match self
                .client
                .submit_tx(BlockTxEnvelope(transfer.block_tx))
                .await
            {
                Ok(txid) => {
                    return Ok(Sent {
                        txid,
                        inputs,
                        fee: request.fee,
                    })
                }
                Err(ClientError::Call(error))
                    if error.code() == codes::MEMPOOL_REJECTED && !retried =>
                {
                    retried = true;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Builds and signs one transfer paying every payment from the
    /// wallet's confirmed coins, with change to the change address, and
    /// does not submit it: for a caller that must record the txid before
    /// the node can see the transfer. [`Session::submit`] sends it.
    ///
    /// Change is spendable only once mined, so a second build before then
    /// finds only the coins this one did not spend.
    pub async fn build_payments(
        &self,
        payments: &[Payment],
        fee: u64,
        gas: u64,
    ) -> Result<Built, Error> {
        let coins = self.coins().await?;
        let (transfer, inputs) = self.build(None, payments, fee, gas, &coins)?;
        Ok(Built {
            txid: TxId(
                <[u8; 32]>::try_from(transfer.txid.as_slice()).map_err(|_| {
                    Error::Invalid(format!("a txid of {} bytes", transfer.txid.len()))
                })?,
            ),
            block_tx: transfer.block_tx,
            inputs,
            tip_height: coins.tip_height,
        })
    }

    /// Hands a [`Built`] transfer to the node, once: a refusal is returned,
    /// not retried, since the caller decides what a refused txid means.
    pub async fn submit(&self, built: &Built) -> Result<TxId, Error> {
        Ok(self
            .client
            .submit_tx(BlockTxEnvelope(built.block_tx.clone()))
            .await?)
    }

    fn build(
        &self,
        from: Option<DerivationPath>,
        payments: &[Payment],
        fee: u64,
        gas: u64,
        coins: &Coins,
    ) -> Result<(Transfer, Vec<ContractId>), Error> {
        let mut need: BTreeMap<Flavor, u128> = BTreeMap::new();
        for payment in payments {
            *need.entry(payment.flavor).or_default() += u128::from(payment.qty);
        }
        *need.entry(Flavor::flame()).or_default() += u128::from(fee);

        let spendable: Vec<&Coin> = coins.coins.iter().filter(|c| c.is_spendable()).collect();
        let change = match from {
            Some(path) => self.address_at(path)?.receiving_address,
            None => self.change_address()?.receiving_address,
        };
        let mut inputs = Vec::new();
        let mut outputs: Vec<TransferOutput> = payments
            .iter()
            .map(|payment| TransferOutput {
                address: payment.to.clone(),
                qty: payment.qty,
                flavor: native_or(payment.flavor),
                memo: payment.memo.clone(),
                clear: payment.clear,
            })
            .collect();
        for (&flavor, &amount) in &need {
            if amount == 0 {
                continue;
            }
            let (chosen, total) = select(&spendable, flavor, amount)?;
            inputs.extend(chosen);
            if total > amount {
                outputs.push(TransferOutput {
                    address: change.clone(),
                    qty: u64::try_from(total - amount).map_err(|_| {
                        Error::Invalid(format!("the {flavor} change does not fit one coin"))
                    })?,
                    flavor: native_or(flavor),
                    memo: Vec::new(),
                    clear: false,
                });
            }
        }

        let ids = inputs.iter().map(|coin| coin.id).collect();
        let transfer = self.wallet.build_transfer(TransferRequest {
            inputs: inputs
                .into_iter()
                .map(|coin| TransferInput {
                    contract: coin.contract.clone(),
                    proof: coin.proof.clone().expect("only spendable coins are chosen"),
                    path: KeyPath {
                        branch: coin.branch,
                        index: coin.index,
                    },
                    opening: coin.opening.clone(),
                })
                .collect(),
            outputs,
            fee,
            gas,
            locktime: 0,
        })?;
        Ok((transfer, ids))
    }

    /// The flavor [`Session::issue`] mints from `from` under `tag`.
    pub fn issued_flavor(&self, from: DerivationPath, tag: &str) -> Result<Flavor, Error> {
        let issuer = self.address_at(from)?;
        Flavor::from_bytes(&flamewallet_ffi::issued_flavor(
            issuer.predicate,
            tag.as_bytes().to_vec(),
        )?)
    }

    /// Mints `qty` of the tag's flavor by spending the largest Flame coin
    /// at the issuer's path, and returns that coin, less the fee, to the
    /// same address: the next issuance has a coin to spend once this one
    /// is mined.
    ///
    /// Retried once from fresh coins on a refusal, as [`Session::send`].
    pub async fn issue(&self, request: &IssueRequest) -> Result<Issued, Error> {
        let flavor = self.issued_flavor(request.from, &request.tag)?;
        let mut retried = false;
        loop {
            let coins = self.coins_from(Some(request.from)).await?;
            let (transfer, input) = self.build_issue(request, flavor, &coins)?;
            match self.client.submit_tx(BlockTxEnvelope(transfer)).await {
                Ok(txid) => {
                    return Ok(Issued {
                        txid,
                        flavor,
                        from: request.from,
                        input,
                        fee: request.fee,
                    })
                }
                Err(ClientError::Call(error))
                    if error.code() == codes::MEMPOOL_REJECTED && !retried =>
                {
                    retried = true;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn build_issue(
        &self,
        request: &IssueRequest,
        flavor: Flavor,
        coins: &Coins,
    ) -> Result<(Vec<u8>, ContractId), Error> {
        let issuer = self.address_at(request.from)?;
        let coin = coins
            .coins
            .iter()
            .filter(|coin| coin.is_spendable())
            .filter_map(|coin| match coin.value.token() {
                Some((qty, f)) if f.is_flame() && qty >= request.fee => Some((qty, coin)),
                _ => None,
            })
            .max_by_key(|(qty, _)| *qty)
            .map(|(_, coin)| coin)
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "no confirmed Flame coin of at least the fee at {} ({}); \
                     send one there, or wait for a pending one to be mined",
                    request.from, issuer.receiving_address
                ))
            })?;
        let (held, _) = coin.value.token().expect("a token was chosen");

        let mut outputs = vec![TransferOutput {
            address: request
                .to
                .clone()
                .unwrap_or_else(|| issuer.receiving_address.clone()),
            qty: request.qty,
            flavor: native_or(flavor),
            memo: request.memo.clone(),
            clear: false,
        }];
        if held > request.fee {
            outputs.push(TransferOutput {
                address: issuer.receiving_address.clone(),
                qty: held - request.fee,
                flavor: None,
                memo: Vec::new(),
                clear: false,
            });
        }
        let transfer = self.wallet.build_issue(FfiIssueRequest {
            issuer: TransferInput {
                contract: coin.contract.clone(),
                proof: coin.proof.clone().expect("only spendable coins are chosen"),
                path: issuer.path,
                opening: coin.opening.clone(),
            },
            tag: request.tag.as_bytes().to_vec(),
            qty: request.qty,
            outputs,
            fee: request.fee,
            gas: request.gas,
            locktime: 0,
        })?;
        Ok((transfer.block_tx, coin.id))
    }

    pub async fn status(&self, txid: TxId) -> Result<TxStatusResult, Error> {
        Ok(self.client.tx_status(txid).await?)
    }

    /// Asks until the transaction is in a block, or `timeout` has passed.
    pub async fn wait(&self, txid: TxId, timeout: Duration) -> Result<TxStatusResult, Error> {
        let start = Instant::now();
        loop {
            let status = self.status(txid).await?;
            if matches!(status, TxStatusResult::Confirmed { .. }) {
                return Ok(status);
            }
            if start.elapsed() >= timeout {
                return Err(Error::Timeout {
                    txid: txid.to_string(),
                    secs: timeout.as_secs(),
                });
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// A page of history, newest first, with the mempool's transactions on
    /// top of the first page.
    pub async fn history(&self, older: Option<String>, limit: u32) -> Result<HistoryPage, Error> {
        let watched = self.watched()?;
        let paths: HashMap<[u8; 32], KeyPath> = watched
            .iter()
            .map(|a| {
                let predicate =
                    <[u8; 32]>::try_from(a.predicate.as_slice()).expect("a predicate is 32 bytes");
                (predicate, a.path)
            })
            .collect();
        let first = older.is_none();
        let page = self
            .client
            .history(
                watched.into_iter().map(|a| a.receiving_address).collect(),
                older,
                limit,
                Some(first),
            )
            .await?;

        let mut items = Vec::new();
        for entry in page.pending.unwrap_or_default() {
            items.push(self.history_item(entry, &paths, true)?);
        }
        for entry in page.entries {
            items.push(self.history_item(entry, &paths, false)?);
        }
        Ok(HistoryPage {
            items,
            older: page.older,
        })
    }

    fn history_item(
        &self,
        entry: HistoryEntry,
        paths: &HashMap<[u8; 32], KeyPath>,
        pending: bool,
    ) -> Result<HistoryItem, Error> {
        let mut deltas: BTreeMap<(bool, Flavor), i128> = BTreeMap::new();
        let mut unreadable = 0;
        let mut memo = None;
        for (contracts, sign) in [(&entry.created, 1), (&entry.spent, -1)] {
            for contract in contracts {
                match self.read_history(contract, paths)? {
                    CoinValue::Clear { qty, flavor } => {
                        *deltas.entry((!flavor.is_flame(), flavor)).or_default() +=
                            sign * i128::from(qty);
                    }
                    CoinValue::Confidential {
                        qty,
                        flavor,
                        memo: text,
                    } => {
                        *deltas.entry((!flavor.is_flame(), flavor)).or_default() +=
                            sign * i128::from(qty);
                        if sign > 0 && !text.is_empty() {
                            memo = Some(text);
                        }
                    }
                    CoinValue::Unreadable { .. } | CoinValue::NotToken => unreadable += 1,
                }
            }
        }
        let to = if entry.others_spent.is_empty() {
            addresses(&entry.others_created)
        } else {
            Vec::new()
        };
        Ok(HistoryItem {
            from: addresses(&entry.others_spent),
            to,
            txid: entry.txid,
            height: entry.height,
            time: entry.time,
            pending,
            fee_sparks: entry.fee_sparks,
            changes: deltas
                .into_iter()
                .map(|((_, flavor), delta)| FlavorDelta { flavor, delta })
                .collect(),
            memo,
            unreadable,
        })
    }

    fn read_history(
        &self,
        contract: &HistoryContract,
        paths: &HashMap<[u8; 32], KeyPath>,
    ) -> Result<CoinValue, Error> {
        let Some(&path) = paths.get(&contract.predicate.0) else {
            return Ok(CoinValue::Unreadable {
                reason: "locked to an address this wallet does not watch".into(),
            });
        };
        let note = contract.note.clone().map(Vec::from);
        Ok(self.read(&contract.bytes.0, note, path)?.0)
    }

    /// What a contract locked to `path` holds, and the opening that spends
    /// it if it is confidential.
    fn read(
        &self,
        contract: &[u8],
        note: Option<Vec<u8>>,
        path: KeyPath,
    ) -> Result<(CoinValue, Option<Opening>), Error> {
        let info = flamewallet_ffi::decode_contract(contract.to_vec())?;
        Ok(match info.value {
            ContractValue::Clear { qty, flavor, .. } => (
                CoinValue::Clear {
                    qty,
                    flavor: Flavor::from_bytes(&flavor)?,
                },
                None,
            ),
            ContractValue::Confidential => {
                match self.wallet.open_note(contract.to_vec(), note, path) {
                    Ok(received) => (
                        CoinValue::Confidential {
                            qty: received.opening.qty,
                            flavor: Flavor::from_bytes(&received.opening.flavor)?,
                            memo: String::from_utf8_lossy(&received.memo).into_owned(),
                        },
                        Some(received.opening),
                    ),
                    Err(FlameError::Note { reason, .. }) => {
                        (CoinValue::Unreadable { reason }, None)
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            ContractValue::Other => (CoinValue::NotToken, None),
        })
    }
}

/// The largest coins of `flavor` first, until they cover `amount`: the
/// fewest inputs, and so the smallest proof.
pub(crate) fn select<'a>(
    spendable: &[&'a Coin],
    flavor: Flavor,
    amount: u128,
) -> Result<(Vec<&'a Coin>, u128), Error> {
    let mut candidates: Vec<(u64, &Coin)> = spendable
        .iter()
        .filter_map(|coin| match coin.value.token() {
            Some((qty, f)) if f == flavor => Some((qty, *coin)),
            _ => None,
        })
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let mut chosen = Vec::new();
    let mut total = 0u128;
    for (qty, coin) in &candidates {
        if total >= amount {
            break;
        }
        chosen.push(*coin);
        total += u128::from(*qty);
    }
    if total < amount {
        let have: u128 = candidates.iter().map(|(qty, _)| u128::from(*qty)).sum();
        return Err(Error::Insufficient {
            flavor,
            have: format_qty(have as i128, flavor),
            need: format_qty(amount as i128, flavor),
        });
    }
    Ok((chosen, total))
}

/// `None` is how the facade spells Flame.
fn native_or(flavor: Flavor) -> Option<Vec<u8>> {
    (!flavor.is_flame()).then(|| flavor.0.to_vec())
}

/// Quantities ride JSON as exact decimal strings, as the node writes them:
/// a JavaScript number loses precision past 2^53.
fn as_string<T: ToString, S: Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}

#[cfg(test)]
pub(crate) fn coin_for_test(id: u8, qty: u64, flavor: Flavor, confirmed: bool) -> Coin {
    Coin {
        id: ContractId([id; 32]),
        txid: TxId([id; 32]),
        branch: RECEIVING,
        index: 0,
        height: confirmed.then_some(1),
        value: CoinValue::Clear { qty, flavor },
        contract: Vec::new(),
        proof: confirmed.then(Vec::new),
        opening: None,
    }
}

/// Counterparties as the wallet shows them: by tracking address, or by
/// predicate where the node could name none.
fn addresses(others: &[Counterparty]) -> Vec<String> {
    others
        .iter()
        .map(|other| {
            other
                .address
                .clone()
                .unwrap_or_else(|| hex::encode(other.predicate.0))
        })
        .collect()
}
