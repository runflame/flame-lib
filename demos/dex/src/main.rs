//! `flamedex`: deploys and drives the DEX's coin and pool actors from a
//! `flamewallet` file. See README.md.

mod actors;
mod client;
mod list;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use flamed_rpc::{FlamedApiClient, TxValue};
use flamevm::{
    flavor_from_actor, ActorAddress, ActorID, CellValue, Scalar, ScriptBuilder, FLAME_FLAVOR,
};

use crate::client::{Client, Need, Result};
use crate::list::{List, Listing};

const SPARKS_PER_FLAME: u64 = 100_000_000;

#[derive(Parser)]
#[command(name = "flamedex", about = "A constant-product DEX on Flame actors")]
struct Cli {
    /// The `flamewallet` file that pays and receives.
    #[arg(long, env = "FLAMEWALLET", default_value = "wallet.json")]
    wallet: PathBuf,
    /// The node's JSON-RPC URL; the wallet file's by default.
    #[arg(long, env = "FLAMEWALLET_NODE")]
    node: Option<String>,
    /// Return once the node takes the transaction, without waiting for a block.
    #[arg(long)]
    no_wait: bool,
    /// A coin list to name coins by instead of the built-in one.
    #[arg(long, env = "FLAMEDEX_LIST")]
    list: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

/// Every COIN below is `TAG@ISSUER` (the issuer's key in hex), `TAG@me` for
/// this wallet's own, a `TAG` from the coin list, or a coin actor's id in
/// hex, which is checked against the state it holds. A pool is named by its
/// coin: there is one FLM/COIN pool per coin.
#[derive(Subcommand)]
enum Command {
    /// Deploys the coin TAG under this wallet's issuer key: up to MAX units, at most CAP per mint.
    DeployCoin {
        tag: String,
        #[arg(long, default_value_t = DEFAULT_MAX)]
        max: u64,
        #[arg(long, default_value_t = DEFAULT_CAP)]
        cap: u64,
        /// Flame sent along to buy storage; what is left stays as the actor's renewal fund.
        #[arg(long, default_value = "50")]
        fund: String,
        /// For tests: deploys under this issuer key, which the wallet cannot
        /// sign for, so the coin's constructor refuses and the deploy bounces.
        #[arg(long, hide = true)]
        forge_issuer: Option<String>,
    },
    /// Deploys the FLM/COIN pool.
    DeployPool {
        coin: String,
        #[arg(long, default_value = "50")]
        fund: String,
    },
    /// Mints AMOUNT of COIN to this wallet.
    Mint { coin: String, amount: u64 },
    /// Adds liquidity: FLAME flames and, by default, the matching amount of COIN.
    Add {
        coin: String,
        flame: String,
        /// COIN units; required for the first deposit, which sets the price.
        #[arg(long)]
        amount: Option<u64>,
    },
    /// Removes liquidity by returning LP units.
    Remove { coin: String, lp: u64 },
    /// Sells AMOUNT of FLM (in flames) or of COIN (in units) for the other side.
    Swap {
        coin: String,
        /// `flm` or `coin`: what is being sold.
        sell: String,
        amount: String,
        /// Percent the price may move before the pool pays the input back.
        #[arg(long, default_value_t = 1.0)]
        slippage: f64,
        /// The least the pool may pay, in the bought side's units, instead of `--slippage`.
        #[arg(long)]
        min_out: Option<String>,
    },
    /// Buys another storage lease for COIN, or with `--pool` for its pool,
    /// adding FUND flames to the actor's fund first.
    Renew {
        coin: String,
        #[arg(long)]
        pool: bool,
        #[arg(long, default_value = "0")]
        fund: String,
    },
    /// Supply, reserves, prices and storage of these coins and their pools.
    Status { coins: Vec<String> },
    /// This wallet's confirmed balance; flavors of the COINs named, and their LP, by name.
    Balance { coins: Vec<String> },
    /// The coin list's entries for these coins, as JSON, for coins that exist.
    Describe { coins: Vec<String> },
    /// This wallet's issuer key: the ISSUER of the coins it deploys.
    Issuer,
    /// Prints a coin's or pool's code size: what storage it needs.
    Sizes,
}

/// The limits `deploy-coin` signs by default.
const DEFAULT_MAX: u64 = 21_000_000;
const DEFAULT_CAP: u64 = 10_000;

/// A coin actor, from what its constructor is built of: its tag and its
/// issuer's key. Its id and flavor follow; nothing has to be looked up.
#[derive(Clone)]
pub struct Coin {
    tag: String,
    issuer: [u8; 32],
}

impl Coin {
    /// Tags are 1 to 16 letters, digits, `.`, `_` or `-`.
    pub fn new(tag: &str, issuer: [u8; 32]) -> Result<Coin> {
        let ok = (1..=16).contains(&tag.len())
            && tag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        if !ok {
            return Err(
                format!("{tag:?}: a tag is 1 to 16 letters, digits, '.', '_' or '-'").into(),
            );
        }
        Ok(Coin {
            tag: tag.to_string(),
            issuer,
        })
    }

    fn constructor(&self) -> ScriptBuilder {
        actors::coin_constructor(self.tag.as_bytes(), self.issuer)
    }

    fn id(&self) -> Result<ActorID> {
        init_id(self.constructor())
    }

    fn flavor(&self) -> Result<Scalar> {
        Ok(flavor_from_actor(&self.id()?, self.tag.as_bytes()))
    }

    /// `TAG@ISSUER`: what names this coin anywhere.
    fn spec(&self) -> String {
        format!("{}@{}", self.tag, hex::encode(self.issuer))
    }

    /// `TAG@` and the issuer key's first four bytes, for messages.
    fn short(&self) -> String {
        format!("{}@{}", self.tag, &hex::encode(self.issuer)[..8])
    }

    /// Its coin-list entry, under `name`.
    pub fn listing(&self, name: &str) -> Result<Listing> {
        let pool = self.pool()?;
        Ok(Listing {
            symbol: self.tag.clone(),
            name: name.to_string(),
            description: String::new(),
            issuer: hex::encode(self.issuer),
            actor: hex::encode(self.id()?.to_hash()),
            flavor: hex::encode(self.flavor()?.to_bytes()),
            pool: hex::encode(pool.id()?.to_hash()),
            lp_flavor: hex::encode(pool.lp()?.to_bytes()),
        })
    }

    fn pool(&self) -> Result<Pool> {
        Ok(Pool { b: self.flavor()? })
    }
}

/// The FLM/coin pool: its constructor holds only the two flavors.
struct Pool {
    b: Scalar,
}

impl Pool {
    fn id(&self) -> Result<ActorID> {
        init_id(actors::pool_constructor(FLAME_FLAVOR, self.b))
    }

    fn lp(&self) -> Result<Scalar> {
        Ok(flavor_from_actor(&self.id()?, actors::LP_TAG))
    }
}

/// The id an `Init` address with this constructor deploys to.
fn init_id(ctor: ScriptBuilder) -> Result<ActorID> {
    Ok(ActorAddress::Init {
        ctor: ctor.to_cell()?.into(),
    }
    .to_actor_id())
}

/// A 32-byte key or id in hex.
pub fn key(text: &str) -> Result<[u8; 32]> {
    hex::decode(text)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("{text}: expected 32 bytes in hex").into())
}

/// Reads a COIN argument. An actor id is trusted only once the constructor
/// rebuilt from its state hashes to that id: then its code is a coin's.
async fn coin(client: &Client, list: &List, spec: &str) -> Result<Coin> {
    if spec.len() == 64 && spec.bytes().all(|b| b.is_ascii_hexdigit()) {
        let id = actor_id(spec)?;
        let not_a_coin = || format!("{spec} is not a flamedex coin");
        let actor = client.actor(&id).await?;
        let state = actor
            .decoded_state
            .ok_or("the actor's state is unavailable: frozen?")?;
        let entries = dict_entries(&state)?;
        let tag = entry(entries, actors::COIN_TAG)
            .ok()
            .and_then(cell_bytes)
            .ok_or_else(not_a_coin)?;
        let issuer = entry(entries, actors::COIN_ISSUER)
            .ok()
            .and_then(cell_bytes)
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .ok_or_else(not_a_coin)?;
        let found = Coin::new(&String::from_utf8_lossy(&tag), issuer).map_err(|_| not_a_coin())?;
        if found.id()? != id {
            return Err(not_a_coin().into());
        }
        return Ok(found);
    }
    match spec.split_once('@') {
        Some((tag, "me")) => Coin::new(tag, client.issuer()?),
        Some((tag, issuer)) => Coin::new(tag, key(issuer)?),
        None => match list.find(spec) {
            Some(l) => Coin::new(&l.symbol, key(&l.issuer)?),
            None => Err(format!(
                "{spec} is not in the coin list: name it TAG@ISSUER, or TAG@me for this wallet's own"
            )
            .into()),
        },
    }
}

/// The bytes of a one-Cell value in decoded state.
fn cell_bytes(value: &TxValue) -> Option<Vec<u8>> {
    match value {
        TxValue::Cells { root, cells } => cells
            .iter()
            .find(|c| c.id == *root)
            .map(|c| c.data.to_vec()),
        _ => None,
    }
}

/// Whether an actor exists. Sending to one that does not, or deploying one
/// that does, bounces the message, and a bounce seals its tokens in a
/// contract no wallet spends: so every command asks first.
async fn exists(client: &Client, id: &ActorID) -> Result<bool> {
    match client.rpc.actor(flamed_rpc::ActorId(id.to_hash())).await {
        Ok(_) => Ok(true),
        Err(flamed_rpc::ClientError::Call(e)) if e.code() == flamed_rpc::codes::NOT_FOUND => {
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

async fn require(client: &Client, id: &ActorID, what: &str) -> Result<ActorAddress> {
    if !exists(client, id).await? {
        return Err(format!(
            "{what} is not deployed (actor {})",
            hex::encode(id.to_hash())
        )
        .into());
    }
    Ok(ActorAddress::from(*id))
}

fn actor_id(hex_id: &str) -> Result<ActorID> {
    let bytes: [u8; 32] = hex::decode(hex_id)?
        .try_into()
        .map_err(|_| "an actor id is 32 bytes")?;
    Ok(ActorID::from(bytes))
}

/// Flames with up to eight decimals, in sparks.
fn sparks(text: &str) -> Result<u64> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    if frac.len() > 8 {
        return Err(format!("{text}: at most eight decimals").into());
    }
    let whole: u64 = whole.parse()?;
    let frac: u64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<8}").parse()?
    };
    whole
        .checked_mul(SPARKS_PER_FLAME)
        .and_then(|w| w.checked_add(frac))
        .ok_or_else(|| format!("{text}: too large").into())
}

fn flames(sparks: u128) -> String {
    let whole = sparks / u128::from(SPARKS_PER_FLAME);
    let frac = sparks % u128::from(SPARKS_PER_FLAME);
    if frac == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{frac:08}")
            .trim_end_matches('0')
            .to_string()
    }
}

/// `d` → `d'`: puts what `value` pushes at `k`.
fn field(
    s: ScriptBuilder,
    k: u64,
    value: impl FnOnce(ScriptBuilder) -> ScriptBuilder,
) -> ScriptBuilder {
    value(s).roll_k(1).push_int(k).roll_k(2).put()
}

/// `v` → `{k: v}`
fn wrap(s: ScriptBuilder, k: u64) -> ScriptBuilder {
    s.push_int(0u64).dict().push_int(k).roll_k(2).put()
}

/// A pool's reserves and LP supply, from its decoded state.
struct Reserves {
    a: u128,
    b: u128,
    supply: u128,
}

fn dict_entries(state: &TxValue) -> Result<&[flamed_rpc::DictEntry]> {
    match state {
        TxValue::Dict { entries } => Ok(entries),
        _ => Err("the actor's state is not a Dict".into()),
    }
}

fn entry(entries: &[flamed_rpc::DictEntry], key: u64) -> Result<&TxValue> {
    entries
        .iter()
        .find(|e| e.key == key.to_string())
        .map(|e| &e.value)
        .ok_or_else(|| format!("no key {key} in the actor's state").into())
}

fn number(value: &TxValue) -> Result<u128> {
    let text = match value {
        TxValue::Scalar { value } => value,
        TxValue::ClearToken { quantity, .. } => quantity,
        _ => return Err("not a number".into()),
    };
    Ok(text.parse()?)
}

async fn reserves(client: &Client, id: &ActorID) -> Result<Reserves> {
    let actor = client.actor(id).await?;
    let state = actor
        .decoded_state
        .ok_or("the pool's state is unavailable: frozen?")?;
    let entries = dict_entries(&state)?;
    Ok(Reserves {
        a: number(entry(entries, actors::POOL_A)?)?,
        b: number(entry(entries, actors::POOL_B)?)?,
        supply: number(entry(entries, actors::POOL_SUPPLY)?)?,
    })
}

/// What the pool pays for `dx` into reserve `x` out of reserve `y`.
fn quote(dx: u128, x: u128, y: u128) -> u128 {
    let dx = dx * u128::from(actors::FEE_KEEP);
    let den = x * 1_000 + dx;
    if den == 0 {
        0
    } else {
        dx * y / den
    }
}

async fn finish(client: &Client, no_wait: bool, txid: flamed_rpc::TxId) -> Result<()> {
    println!("txid {}", hex::encode(txid.0));
    if !no_wait {
        let height = client.wait(txid).await?;
        println!("confirmed at height {height}");
    }
    Ok(())
}

/// Sends `fund` Flame to the `Init` address of `ctor`, in `{1: Flame}` and
/// whatever `extra` adds to that Dict.
async fn deploy(
    client: &Client,
    ctor: ScriptBuilder,
    fund: u64,
    what: &str,
    extra: impl Fn(ScriptBuilder) -> ScriptBuilder,
    refund: [u8; 32],
) -> Result<(ActorID, flamed_rpc::TxId)> {
    let address = ActorAddress::Init {
        ctor: ctor.to_cell()?.into(),
    };
    let id = address.to_actor_id();
    if exists(client, &id).await? {
        return Err(format!(
            "{what} is already deployed (actor {})",
            hex::encode(id.to_hash())
        )
        .into());
    }
    let txid = client
        .send_refunding(
            &address,
            &[Need {
                flavor: FLAME_FLAVOR,
                qty: fund,
            }],
            |s| extra(wrap(s, actors::KEY_TOKEN)),
            refund,
        )
        .await?;
    Ok((id, txid))
}

async fn status(client: &Client, list: &List, specs: &[String]) -> Result<()> {
    for spec in specs {
        let coin = coin(client, list, spec).await?;
        let id = coin.id()?;
        if !exists(client, &id).await? {
            println!("{} not deployed", coin.short());
            continue;
        }
        let a = client.actor(&id).await?;
        let state = a.decoded_state.as_ref().and_then(|s| dict_entries(s).ok());
        let read = |key| {
            state
                .and_then(|e| entry(e, key).ok())
                .and_then(|v| number(v).ok())
                .map(|n| n.to_string())
                .unwrap_or("?".into())
        };
        println!(
            "{} supply {:>12} / {}, at most {} per mint  storage {}/{} B{}  {}",
            coin.short(),
            read(actors::COIN_SUPPLY),
            read(actors::COIN_MAX),
            read(actors::COIN_CAP),
            a.storage_used,
            a.storage_capacity,
            if a.frozen { " FROZEN" } else { "" },
            hex::encode(id.to_hash())
        );
        let pool = coin.pool()?.id()?;
        if !exists(client, &pool).await? {
            println!("FLM/{:<6} not deployed", coin.tag);
            continue;
        }
        let a = client.actor(&pool).await?;
        match reserves(client, &pool).await {
            Ok(r) => {
                let price = if r.b > 0 {
                    format!(
                        "{:.8} FLM per {}",
                        r.a as f64 / SPARKS_PER_FLAME as f64 / r.b as f64,
                        coin.tag
                    )
                } else {
                    "no price".into()
                };
                println!(
                    "FLM/{:<6} {} FLM + {} {}  LP {}  {}  storage {}/{} B  {}",
                    coin.tag,
                    flames(r.a),
                    r.b,
                    coin.tag,
                    r.supply,
                    price,
                    a.storage_used,
                    a.storage_capacity,
                    hex::encode(pool.to_hash())
                );
            }
            Err(e) => println!("FLM/{}: {e}", coin.tag),
        }
    }
    Ok(())
}

/// Coin-list entries for the coins named, named by their tag: what a pull
/// request to coins.json adds, after a better name and a description.
async fn describe(client: &Client, list: &List, specs: &[String]) -> Result<()> {
    let mut coins = Vec::new();
    for spec in specs {
        let c = coin(client, list, spec).await?;
        if !exists(client, &c.id()?).await? {
            return Err(format!("{} is not deployed", c.spec()).into());
        }
        coins.push(c.listing(&c.tag)?);
    }
    println!("{}", serde_json::to_string_pretty(&coins)?);
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = run(Cli::parse()).await {
        eprintln!("flamedex: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    if let Command::Sizes = cli.command {
        let a = FLAME_FLAVOR;
        let b = Scalar::from(7u64);
        for (what, script) in [
            ("coin ctor", actors::coin_constructor(b"EMBER", [7; 32])),
            ("coin runtime", actors::coin_runtime()),
            ("coin terms", actors::coin_terms([7; 32], 1, 1)),
            ("pool ctor", actors::pool_constructor(a, b)),
            ("pool runtime", actors::pool_runtime(a)),
        ] {
            println!("{what}: {} bytes", script.to_bytecode().len());
        }
        return Ok(());
    }
    let list = List::load(cli.list.as_deref())?;
    // Reading needs only a node; a wallet file supplies one if none is named.
    if let (Command::Status { coins } | Command::Describe { coins }, Some(node), false) =
        (&cli.command, &cli.node, cli.wallet.exists())
    {
        let reader = Client::reader(node)?;
        return match cli.command {
            Command::Status { .. } => status(&reader, &list, coins).await,
            _ => describe(&reader, &list, coins).await,
        };
    }
    let client = Client::open(&cli.wallet, cli.node.as_deref())?;
    let recipient = client.recipient()?;
    let coin = |spec: String| {
        let (client, list) = (&client, &list);
        async move { coin(client, list, &spec).await }
    };

    match cli.command {
        Command::DeployCoin {
            tag,
            max,
            cap,
            fund,
            forge_issuer,
        } => {
            let issuer = match forge_issuer {
                Some(k) => key(&k)?,
                None => client.issuer()?,
            };
            let c = Coin::new(&tag, issuer)?;
            let terms = actors::coin_terms(c.id()?.to_hash(), max, cap);
            let sig = client.sign_as_issuer(&terms)?;
            let terms = CellValue::from(terms.to_cell()?);
            let sig = CellValue::from_bytes(sig.to_vec())?;
            let extra = |s: ScriptBuilder| {
                let s = field(s, actors::KEY_TERMS, |s| s.push_literal(terms.clone()));
                field(s, actors::KEY_SIG, |s| s.push_literal(sig.clone()))
            };
            let (id, txid) = deploy(
                &client,
                c.constructor(),
                sparks(&fund)?,
                &c.spec(),
                extra,
                client.deploy_refund()?,
            )
            .await?;
            println!("coin {}", c.spec());
            println!("actor {}", hex::encode(id.to_hash()));
            println!("flavor {}", hex::encode(c.flavor()?.to_bytes()));
            finish(&client, cli.no_wait, txid).await
        }
        Command::DeployPool { coin: spec, fund } => {
            let c = coin(spec).await?;
            require(&client, &c.id()?, &c.short()).await?;
            let pool = c.pool()?;
            let (id, txid) = deploy(
                &client,
                actors::pool_constructor(FLAME_FLAVOR, pool.b),
                sparks(&fund)?,
                &format!("FLM/{}", c.short()),
                |s| s,
                recipient,
            )
            .await?;
            println!(
                "pool FLM/{}: actor {}",
                c.short(),
                hex::encode(id.to_hash())
            );
            finish(&client, cli.no_wait, txid).await
        }
        Command::Mint { coin: spec, amount } => {
            let c = coin(spec).await?;
            let to = require(&client, &c.id()?, &c.short()).await?;
            let txid = client
                .send(&to, &[], |s| {
                    let s = s.push_int(0u64).dict();
                    let s = field(s, actors::KEY_TOKEN, |s| s.push_int(amount));
                    let s = field(s, actors::KEY_RECIPIENT, |s| s.push_point(recipient));
                    field(s, actors::KEY_OP, |s| s.push_int(actors::OP_MINT))
                })
                .await?;
            finish(&client, cli.no_wait, txid).await
        }
        Command::Add {
            coin: spec,
            flame,
            amount,
        } => {
            let c = coin(spec).await?;
            let pool = c.pool()?;
            let id = pool.id()?;
            let to = require(&client, &id, &format!("FLM/{}", c.short())).await?;
            let qa = sparks(&flame)?;
            let r = reserves(&client, &id).await?;
            let qb = match amount {
                Some(qb) => qb,
                None if r.supply > 0 && r.a > 0 => {
                    // At the pool's price, rounded up so the A side sets the share.
                    let qb = (u128::from(qa) * r.b).div_ceil(r.a);
                    u64::try_from(qb).map_err(|_| "the matching amount does not fit")?
                }
                None => return Err("the first deposit sets the price: pass --amount".into()),
            };
            println!("depositing {} FLM and {qb} {}", flames(qa.into()), c.tag);
            let needs = [
                Need {
                    flavor: FLAME_FLAVOR,
                    qty: qa,
                },
                Need {
                    flavor: pool.b,
                    qty: qb,
                },
            ];
            let txid = client
                .send(&to, &needs, |s| {
                    // ta tb
                    let s = s.push_int(0u64).dict();
                    let s = s.push_int(actors::KEY_TOKEN_B).roll_k(2).put();
                    let s = s.push_int(actors::KEY_TOKEN).roll_k(2).put();
                    let s = field(s, actors::KEY_RECIPIENT, |s| s.push_point(recipient));
                    field(s, actors::KEY_OP, |s| s.push_int(actors::OP_ADD))
                })
                .await?;
            finish(&client, cli.no_wait, txid).await
        }
        Command::Remove { coin: spec, lp } => {
            let c = coin(spec).await?;
            let pool = c.pool()?;
            let to = require(&client, &pool.id()?, &format!("FLM/{}", c.short())).await?;
            let needs = [Need {
                flavor: pool.lp()?,
                qty: lp,
            }];
            let txid = client
                .send(&to, &needs, |s| {
                    let s = wrap(s, actors::KEY_TOKEN);
                    let s = field(s, actors::KEY_RECIPIENT, |s| s.push_point(recipient));
                    field(s, actors::KEY_OP, |s| s.push_int(actors::OP_REMOVE))
                })
                .await?;
            finish(&client, cli.no_wait, txid).await
        }
        Command::Swap {
            coin: spec,
            sell,
            amount,
            slippage,
            min_out,
        } => {
            let c = coin(spec).await?;
            let pool = c.pool()?;
            let id = pool.id()?;
            let to = require(&client, &id, &format!("FLM/{}", c.short())).await?;
            let r = reserves(&client, &id).await?;
            let (sold, qty, x, y) = match sell.to_ascii_lowercase().as_str() {
                "flm" => (FLAME_FLAVOR, sparks(&amount)?, r.a, r.b),
                "coin" => (pool.b, amount.parse()?, r.b, r.a),
                other => return Err(format!("sell `flm` or `coin`, not {other}").into()),
            };
            let out = quote(qty.into(), x, y);
            if out == 0 {
                return Err("the pool would pay nothing for that".into());
            }
            let min = match min_out {
                Some(m) if sold == FLAME_FLAVOR => m.parse()?,
                Some(m) => sparks(&m)?,
                None => ((out as f64) * (1.0 - slippage / 100.0)).floor().max(1.0) as u64,
            };
            let (got, least) = if sold == FLAME_FLAVOR {
                (format!("{out} {}", c.tag), format!("{min}"))
            } else {
                (format!("{} FLM", flames(out)), flames(min.into()))
            };
            println!("expecting {got}, at least {least}");
            let txid = client
                .send(&to, &[Need { flavor: sold, qty }], |s| {
                    let s = wrap(s, actors::KEY_TOKEN);
                    let s = field(s, actors::KEY_RECIPIENT, |s| s.push_point(recipient));
                    let s = field(s, actors::KEY_MIN, |s| s.push_int(min));
                    field(s, actors::KEY_OP, |s| s.push_int(actors::OP_SWAP))
                })
                .await?;
            finish(&client, cli.no_wait, txid).await
        }
        Command::Renew {
            coin: spec,
            pool,
            fund,
        } => {
            let c = coin(spec).await?;
            let to = if pool {
                require(&client, &c.pool()?.id()?, &format!("FLM/{}", c.short())).await?
            } else {
                require(&client, &c.id()?, &c.short()).await?
            };
            let fund = sparks(&fund)?;
            let needs: Vec<Need> = if fund > 0 {
                vec![Need {
                    flavor: FLAME_FLAVOR,
                    qty: fund,
                }]
            } else {
                vec![]
            };
            let txid = client
                .send(&to, &needs, |s| {
                    let s = if fund > 0 {
                        wrap(s, actors::KEY_TOKEN)
                    } else {
                        s.push_int(0u64).dict()
                    };
                    field(s, actors::KEY_OP, |s| s.push_int(actors::OP_RENEW))
                })
                .await?;
            finish(&client, cli.no_wait, txid).await
        }
        Command::Status { coins } => status(&client, &list, &coins).await,
        Command::Describe { coins } => describe(&client, &list, &coins).await,
        Command::Issuer => {
            println!("{}", hex::encode(client.issuer()?));
            Ok(())
        }
        Command::Balance { coins } => {
            // Names for the flavors of the coins asked about and their LP.
            let mut names = std::collections::BTreeMap::new();
            names.insert(FLAME_FLAVOR.to_bytes(), "FLM".to_string());
            for spec in coins {
                let c = coin(spec).await?;
                let pool = c.pool()?;
                names.insert(c.flavor()?.to_bytes(), c.tag.clone());
                names.insert(pool.lp()?.to_bytes(), format!("LP-{}", c.tag));
            }
            for (flavor, qty) in client.balances().await? {
                let name = names
                    .get(&flavor)
                    .cloned()
                    .unwrap_or_else(|| hex::encode(flavor));
                if flavor == FLAME_FLAVOR.to_bytes() {
                    println!("{} FLM", flames(qty));
                } else {
                    println!("{qty} {name}");
                }
            }
            Ok(())
        }
        Command::Sizes => unreachable!(),
    }
}
