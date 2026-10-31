//! One wallet talking to one node, for the transactions the DEX needs.
//!
//! A wallet transfer moves confidential tokens; an actor can only count
//! cleartext ones. So every DEX transaction opens its inputs: a confidential
//! coin is `decrypt`ed with the opening its note gives, a clear coin is
//! taken as is, the coins of each flavor are merged, the amount the message
//! carries is split off, and the change goes back to the wallet's change
//! address as a clear coin. The message is then one `send` to the actor.

use std::collections::BTreeMap;
use std::path::Path;

use curve25519_dalek::scalar::Scalar as DalekScalar;
use flamechain::codec::{contract_from_bytes, proof_from_bytes};
use flamechain::utreexo::Proof;
use flamed_rpc::{
    codes, ActorResult, BlockTxEnvelope, ClientError, FlamedApiClient, HttpClient,
    HttpClientBuilder, TxId,
};
use flamekd::Network;
use flamepayments::{block_tx, open_note, sign, Opening, SpendAccount};
use flamevm::{
    ActorAddress, ActorID, CellEncode, CellValue, Contract, Limits, Scalar, ScriptBuilder,
    TxHeader, Value, FLAME_FLAVOR,
};
use serde::Deserialize;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// The execution budget of one DEX transaction, its message grant included.
const TX_GAS: u64 = 10_000_000;
/// The branch of the wallet's tree the issuer key is at, `m/…/2/0`: next
/// to the receiving (0) and change (1) branches, and never handed out as an
/// address, so it holds nothing for a signed program to be run against.
const ISSUER_BRANCH: u32 = 2;

/// The grant each message carries. A delivery that runs out fails and
/// bounces, so it is generous: unused message gas is not charged.
pub const MESSAGE_GAS: u64 = 2_000_000;

pub struct Client {
    wallet: Option<Wallet>,
    pub rpc: HttpClient,
}

struct Wallet {
    account: SpendAccount,
    network: Network,
    receiving: u32,
}

/// The part of a `flamewallet` file this tool reads: the secret, the
/// network, the node and the receiving counter.
#[derive(Deserialize)]
struct WalletFile {
    network: String,
    node: String,
    secret: Secret,
    next_index: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Secret {
    Mnemonic {
        phrase: String,
        #[serde(default)]
        passphrase: String,
    },
    Seed {
        hex: String,
    },
}

/// One coin the wallet can spend now: confirmed, with a proof.
struct Coin {
    contract: Contract,
    proof: Proof,
    branch: u32,
    index: u32,
    qty: u64,
    flavor: Scalar,
    opening: Option<Opening>,
}

/// What a message carries: `qty` of `flavor` split off the wallet's coins.
#[derive(Clone, Copy)]
pub struct Need {
    pub flavor: Scalar,
    pub qty: u64,
}

impl Client {
    pub fn open(wallet: &Path, node: Option<&str>) -> Result<Client> {
        let text =
            std::fs::read_to_string(wallet).map_err(|e| format!("{}: {e}", wallet.display()))?;
        let file: WalletFile = serde_json::from_str(&text)?;
        let seed: Vec<u8> = match &file.secret {
            Secret::Mnemonic { phrase, passphrase } => {
                flamewallet_ffi::mnemonic_to_seed(phrase.clone(), passphrase.clone())?
            }
            Secret::Seed { hex } => hex::decode(hex.trim()).map_err(|_| "the seed is not hex")?,
        };
        let seed: [u8; 64] = seed.try_into().map_err(|_| "the seed is not 64 bytes")?;
        let network = match file.network.as_str() {
            "mainnet" => Network::Mainnet,
            _ => Network::Testnet,
        };
        let account = SpendAccount::from_seed(&seed, network, file.next_index)?;
        let node = node.map(str::to_owned).unwrap_or_else(|| file.node.clone());
        Ok(Client {
            wallet: Some(Wallet {
                account,
                network,
                receiving: file.next_index.max(1),
            }),
            rpc: HttpClientBuilder::default().build(node)?,
        })
    }

    /// A client that only reads the chain.
    pub fn reader(node: &str) -> Result<Client> {
        Ok(Client {
            wallet: None,
            rpc: HttpClientBuilder::default().build(node)?,
        })
    }

    fn wallet(&self) -> Result<&Wallet> {
        self.wallet
            .as_ref()
            .ok_or_else(|| "this needs a wallet file".into())
    }

    /// The point a payment to the wallet's first receiving address locks to:
    /// where actors pay this wallet.
    pub fn recipient(&self) -> Result<[u8; 32]> {
        Ok(self
            .wallet()?
            .account
            .predicate_at(0, 0)?
            .to_point()
            .to_bytes())
    }

    /// The key this wallet issues coins under.
    pub fn issuer(&self) -> Result<[u8; 32]> {
        Ok(self
            .wallet()?
            .account
            .predicate_at(ISSUER_BRANCH, 0)?
            .to_point()
            .to_bytes())
    }

    /// Where a coin deploy's message goes if it bounces: a key next to the
    /// issuer key that no wallet scans. A bounced message is sealed whole in
    /// a contract nothing spends, and one that carries a program, as a coin
    /// deploy does, makes `flamewallet` fail to read the address it lands at.
    pub fn deploy_refund(&self) -> Result<[u8; 32]> {
        Ok(self
            .wallet()?
            .account
            .predicate_at(ISSUER_BRANCH, 1)?
            .to_point()
            .to_bytes())
    }

    /// The issuer key's signature on `program`, for `signcall`.
    pub fn sign_as_issuer(&self, program: &ScriptBuilder) -> Result<[u8; 64]> {
        let program = program.to_cell()?;
        let key = self.wallet()?.account.spending_key_at(ISSUER_BRANCH, 0)?;
        Ok(musig::Signature::sign(&mut flamevm::signcall_transcript(&program), key).to_bytes())
    }

    fn change(&self) -> Result<[u8; 32]> {
        Ok(self
            .wallet()?
            .account
            .predicate_at(1, 0)?
            .to_point()
            .to_bytes())
    }

    /// Every confirmed coin at the receiving addresses and the change one.
    async fn coins(&self) -> Result<Vec<Coin>> {
        let w = self.wallet()?;
        let mut paths: Vec<(u32, u32)> = (0..w.receiving).map(|i| (0, i)).collect();
        paths.push((1, 0));
        let mut coins = Vec::new();
        for (branch, index) in paths {
            let address = w.account.address_at(branch, index)?;
            let unspent = self
                .rpc
                .unspent(address.to_bech32(w.network), Some(true))
                .await?;
            for entry in unspent.outputs {
                // A pending coin has no proof yet, and one a pending
                // transaction spends is already gone from the list.
                let (Some(_), Some(proof)) = (entry.height, entry.proof) else {
                    continue;
                };
                // A bounced message comes back as a contract holding the
                // whole message; one carrying a program does not decode,
                // and no wallet spends it either way.
                let Ok(contract) = contract_from_bytes(&entry.bytes.0) else {
                    continue;
                };
                let proof = proof_from_bytes(&Vec::from(proof))
                    .map_err(|e| format!("a proof the node sent: {e:?}"))?;
                let (qty, flavor, opening) = match contract.payload() {
                    Value::ClearToken(t) => match t.qty().to_u64() {
                        Some(qty) => (qty, t.flv(), None),
                        None => continue,
                    },
                    Value::Token(_) => {
                        let note = entry.note.map(Vec::from);
                        let view = w.account.viewing_key_at(branch, index)?;
                        match open_note(&contract, note.as_deref(), &address, &view) {
                            Ok(received) => (
                                received.opening.qty,
                                received.opening.flv,
                                Some(received.opening),
                            ),
                            Err(_) => continue,
                        }
                    }
                    _ => continue,
                };
                coins.push(Coin {
                    contract,
                    proof,
                    branch,
                    index,
                    qty,
                    flavor,
                    opening,
                });
            }
        }
        Ok(coins)
    }

    /// Confirmed balance per flavor, as the wallet can spend it.
    pub async fn balances(&self) -> Result<BTreeMap<[u8; 32], u128>> {
        let mut out = BTreeMap::new();
        for coin in self.coins().await? {
            *out.entry(coin.flavor.to_bytes()).or_default() += u128::from(coin.qty);
        }
        Ok(out)
    }

    /// Builds, signs and submits one message to `to`: `needs` are split off
    /// the wallet's coins and left on the stack in order for `payload` to
    /// turn into the message's one Value. A block between reading coins and
    /// submitting makes the proofs stale; that refusal is retried once.
    pub async fn send(
        &self,
        to: &ActorAddress,
        needs: &[Need],
        payload: impl Fn(ScriptBuilder) -> ScriptBuilder,
    ) -> Result<TxId> {
        self.send_refunding(to, needs, payload, self.recipient()?)
            .await
    }

    /// `send`, with a bounce sealed under `refund` instead of the wallet's
    /// receiving address.
    pub async fn send_refunding(
        &self,
        to: &ActorAddress,
        needs: &[Need],
        payload: impl Fn(ScriptBuilder) -> ScriptBuilder,
        refund: [u8; 32],
    ) -> Result<TxId> {
        let mut retried = false;
        loop {
            let coins = self.coins().await?;
            let bytes = self.build(&coins, to, needs, &payload, refund)?;
            match self.rpc.submit_tx(BlockTxEnvelope(bytes)).await {
                Ok(txid) => return Ok(txid),
                Err(ClientError::Call(e)) if e.code() == codes::MEMPOOL_REJECTED && !retried => {
                    retried = true;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn build(
        &self,
        coins: &[Coin],
        to: &ActorAddress,
        needs: &[Need],
        payload: &impl Fn(ScriptBuilder) -> ScriptBuilder,
        refund: [u8; 32],
    ) -> Result<Vec<u8>> {
        // Every transaction spends at least one coin: `send` needs the
        // anchor an input seeds. With nothing to pay, a Flame coin goes in
        // and comes back whole.
        let mut groups: Vec<(Need, Vec<&Coin>)> = Vec::new();
        for need in needs {
            groups.push((*need, select(coins, need.flavor, need.qty)?));
        }
        if groups.is_empty() {
            let flame = Need {
                flavor: FLAME_FLAVOR,
                qty: 0,
            };
            groups.push((flame, select(coins, flame.flavor, 1)?));
        }

        let change = self.change()?;
        let mut script = ScriptBuilder::new();
        let mut keys: Vec<DalekScalar> = Vec::new();
        let mut proofs = Vec::new();
        for (need, chosen) in &groups {
            let mut total = 0u64;
            for (i, coin) in chosen.iter().enumerate() {
                script = script
                    .push_literal(CellValue::contract(coin.contract.clone())?)
                    .input()
                    .signtx();
                if let Some(o) = &coin.opening {
                    script = script
                        .push_int(o.flv)
                        .push_int(Scalar::from(o.flv_blinding))
                        .push_int(o.qty)
                        .push_int(Scalar::from(o.qty_blinding))
                        .decrypt();
                }
                if i > 0 {
                    script = script.merge().verify();
                }
                total += coin.qty;
                keys.push(
                    self.wallet()?
                        .account
                        .spending_key_at(coin.branch, coin.index)?,
                );
                proofs.push(coin.proof.clone());
            }
            // total
            if need.qty == 0 {
                script = script.push_point(change).output();
            } else if total > need.qty {
                script = script
                    .push_int(need.qty)
                    .split()
                    .roll_k(1)
                    .push_point(change)
                    .output();
            }
            // need
        }
        let script = payload(script)
            .push_literal(CellValue::from_bytes(refund.to_vec())?)
            .push_int(MESSAGE_GAS)
            .push_cell(to.to_cell()?)
            .send();
        let header = TxHeader {
            version: 1,
            locktime: 0,
        };
        let limits = Limits { gas: TX_GAS };
        let unsigned = script
            .build_tx(header, limits)
            .map_err(|e| format!("the transaction does not build: {e:?}"))?;
        let signed = sign(unsigned, &keys).map_err(|e| format!("signing: {e:?}"))?;
        Ok(block_tx(signed, limits, proofs).to_bytes()?)
    }

    pub async fn actor(&self, id: &ActorID) -> Result<ActorResult> {
        Ok(self.rpc.actor(flamed_rpc::ActorId(id.to_hash())).await?)
    }

    /// Waits until `txid` is in a block.
    pub async fn wait(&self, txid: TxId) -> Result<u64> {
        loop {
            if let flamed_rpc::TxStatusResult::Confirmed { height, .. } =
                self.rpc.tx_status(txid).await?
            {
                return Ok(height);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}

/// The largest coins of `flavor` until they cover `qty`.
fn select(coins: &[Coin], flavor: Scalar, qty: u64) -> Result<Vec<&Coin>> {
    let mut of: Vec<&Coin> = coins.iter().filter(|c| c.flavor == flavor).collect();
    of.sort_by(|a, b| b.qty.cmp(&a.qty));
    let mut chosen = Vec::new();
    let mut total = 0u128;
    for coin in of {
        if total >= u128::from(qty) {
            break;
        }
        total += u128::from(coin.qty);
        chosen.push(coin);
    }
    if total < u128::from(qty) || chosen.is_empty() {
        return Err(format!(
            "the wallet holds {total} of flavor {} confirmed, {qty} needed",
            hex::encode(flavor.to_bytes())
        )
        .into());
    }
    Ok(chosen)
}
