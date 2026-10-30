//! `flamedex`: deploys and drives the DEX's coin and pool actors from a
//! `flamewallet` file. See README.md.

mod actors;
mod client;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use flamed_rpc::TxValue;
use flamevm::{flavor_from_actor, ActorAddress, ActorID, Scalar, ScriptBuilder, FLAME_FLAVOR};
use serde::{Deserialize, Serialize};

use crate::client::{Client, Need, Result};

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
    /// The registry of deployed coins and pools, which the web page reads.
    #[arg(long, env = "FLAMEDEX_REGISTRY", default_value = "dex.json")]
    registry: PathBuf,
    /// Return once the node takes the transaction, without waiting for a block.
    #[arg(long)]
    no_wait: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Deploys a coin actor that issues TAG, up to MAX units, at most CAP per mint.
    DeployCoin {
        tag: String,
        #[arg(long, default_value_t = 21_000_000)]
        max: u64,
        #[arg(long, default_value_t = 10_000)]
        cap: u64,
        /// Flame sent along to buy storage; what is left stays as the actor's renewal fund.
        #[arg(long, default_value = "50")]
        fund: String,
        /// A display name for the web page.
        #[arg(long)]
        name: Option<String>,
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
    /// Buys another storage lease for a coin or pool, adding FUND flames to its fund first.
    Renew {
        /// A coin's tag, or `pool:TAG`.
        actor: String,
        #[arg(long, default_value = "0")]
        fund: String,
    },
    /// Reserves, prices and storage of every pool and coin in the registry.
    Status,
    /// This wallet's confirmed balance, named by the registry.
    Balance,
    /// Prints a coin's or pool's code size: what storage it needs.
    Sizes,
}

#[derive(Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    coins: Vec<CoinEntry>,
    #[serde(default)]
    pools: Vec<PoolEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CoinEntry {
    tag: String,
    name: String,
    actor: String,
    flavor: String,
    max: u64,
    cap: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct PoolEntry {
    coin: String,
    actor: String,
    flavor_a: String,
    flavor_b: String,
    lp_flavor: String,
}

impl Registry {
    fn load(path: &Path) -> Result<Registry> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
            Err(e) => Err(format!("{}: {e}", path.display()).into()),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)? + "\n")?;
        Ok(())
    }

    fn coin(&self, tag: &str) -> Result<&CoinEntry> {
        self.coins
            .iter()
            .find(|c| c.tag.eq_ignore_ascii_case(tag))
            .ok_or_else(|| format!("no coin {tag} in the registry").into())
    }

    fn pool(&self, tag: &str) -> Result<&PoolEntry> {
        self.pools
            .iter()
            .find(|p| p.coin.eq_ignore_ascii_case(tag))
            .ok_or_else(|| format!("no FLM/{tag} pool in the registry").into())
    }

    fn name_of(&self, flavor: &[u8; 32]) -> String {
        let hex = hex::encode(flavor);
        if *flavor == FLAME_FLAVOR.to_bytes() {
            return "FLM".into();
        }
        if let Some(c) = self.coins.iter().find(|c| c.flavor == hex) {
            return c.tag.clone();
        }
        if let Some(p) = self.pools.iter().find(|p| p.lp_flavor == hex) {
            return format!("LP-{}", p.coin);
        }
        hex
    }
}

fn actor_id(hex_id: &str) -> Result<ActorID> {
    let bytes: [u8; 32] = hex::decode(hex_id)?
        .try_into()
        .map_err(|_| "an actor id is 32 bytes")?;
    Ok(ActorID::from(bytes))
}

fn flavor(hex_flavor: &str) -> Result<Scalar> {
    let bytes: [u8; 32] = hex::decode(hex_flavor)?
        .try_into()
        .map_err(|_| "a flavor is 32 bytes")?;
    Scalar::from_bytes(bytes).ok_or_else(|| "not a canonical flavor".into())
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

async fn reserves(client: &Client, pool: &PoolEntry) -> Result<Reserves> {
    let actor = client.actor(&actor_id(&pool.actor)?).await?;
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

async fn deploy(
    client: &Client,
    ctor: ScriptBuilder,
    fund: u64,
) -> Result<(ActorID, flamed_rpc::TxId)> {
    let address = ActorAddress::Init {
        ctor: ctor.to_cell()?.into(),
    };
    let id = ActorID::from(address.to_hash());
    let txid = client
        .send(
            &address,
            &[Need {
                flavor: FLAME_FLAVOR,
                qty: fund,
            }],
            |s| wrap(s, actors::KEY_TOKEN),
        )
        .await?;
    Ok((id, txid))
}

async fn status(registry: &Registry, client: &Client) -> Result<()> {
    for coin in &registry.coins {
        let a = client.actor(&actor_id(&coin.actor)?).await?;
        let supply = a
            .decoded_state
            .as_ref()
            .and_then(|s| dict_entries(s).ok())
            .and_then(|e| entry(e, actors::COIN_SUPPLY).ok())
            .and_then(|v| number(v).ok());
        println!(
            "{:<8} supply {:>12} / {}  storage {}/{} B{}  {}",
            coin.tag,
            supply.map(|s| s.to_string()).unwrap_or("?".into()),
            coin.max,
            a.storage_used,
            a.storage_capacity,
            if a.frozen { " FROZEN" } else { "" },
            coin.actor
        );
    }
    for pool in &registry.pools {
        let a = client.actor(&actor_id(&pool.actor)?).await?;
        match reserves(client, pool).await {
            Ok(r) => {
                let price = if r.b > 0 {
                    format!(
                        "{:.8} FLM per {}",
                        r.a as f64 / SPARKS_PER_FLAME as f64 / r.b as f64,
                        pool.coin
                    )
                } else {
                    "no price".into()
                };
                println!(
                    "FLM/{:<6} {} FLM + {} {}  LP {}  {}  storage {}/{} B",
                    pool.coin,
                    flames(r.a),
                    r.b,
                    pool.coin,
                    r.supply,
                    price,
                    a.storage_used,
                    a.storage_capacity
                );
            }
            Err(e) => println!("FLM/{}: {e}", pool.coin),
        }
    }
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
    let mut registry = Registry::load(&cli.registry)?;
    if let Command::Sizes = cli.command {
        let a = FLAME_FLAVOR;
        let b = Scalar::from(7u64);
        for (what, script) in [
            ("coin ctor", actors::coin_constructor(b"EMBER", 1, 1)),
            ("coin runtime", actors::coin_runtime(b"EMBER")),
            ("pool ctor", actors::pool_constructor(a, b)),
            ("pool runtime", actors::pool_runtime(a)),
        ] {
            println!("{what}: {} bytes", script.to_bytecode().len());
        }
        return Ok(());
    }
    if let (Command::Status, Some(node), false) = (&cli.command, &cli.node, cli.wallet.exists()) {
        return status(&registry, &Client::reader(node)?).await;
    }
    let client = Client::open(&cli.wallet, cli.node.as_deref())?;
    let recipient = client.recipient()?;

    match cli.command {
        Command::DeployCoin {
            tag,
            max,
            cap,
            fund,
            name,
        } => {
            if registry.coin(&tag).is_ok() {
                return Err(format!("{tag} is already in the registry").into());
            }
            let (id, txid) = deploy(
                &client,
                actors::coin_constructor(tag.as_bytes(), max, cap),
                sparks(&fund)?,
            )
            .await?;
            let flavor = flavor_from_actor(&id, tag.as_bytes());
            println!("coin {tag}: actor {}", hex::encode(id.to_hash()));
            println!("flavor {}", hex::encode(flavor.to_bytes()));
            registry.coins.push(CoinEntry {
                name: name.unwrap_or_else(|| tag.clone()),
                tag,
                actor: hex::encode(id.to_hash()),
                flavor: hex::encode(flavor.to_bytes()),
                max,
                cap,
            });
            registry.save(&cli.registry)?;
            finish(&client, cli.no_wait, txid).await
        }
        Command::DeployPool { coin, fund } => {
            let coin = registry.coin(&coin)?.clone();
            if registry.pool(&coin.tag).is_ok() {
                return Err(format!("FLM/{} is already in the registry", coin.tag).into());
            }
            let b = flavor(&coin.flavor)?;
            let (id, txid) = deploy(
                &client,
                actors::pool_constructor(FLAME_FLAVOR, b),
                sparks(&fund)?,
            )
            .await?;
            println!("pool FLM/{}: actor {}", coin.tag, hex::encode(id.to_hash()));
            registry.pools.push(PoolEntry {
                coin: coin.tag.clone(),
                actor: hex::encode(id.to_hash()),
                flavor_a: hex::encode(FLAME_FLAVOR.to_bytes()),
                flavor_b: coin.flavor.clone(),
                lp_flavor: hex::encode(flavor_from_actor(&id, actors::LP_TAG).to_bytes()),
            });
            registry.save(&cli.registry)?;
            finish(&client, cli.no_wait, txid).await
        }
        Command::Mint { coin, amount } => {
            let coin = registry.coin(&coin)?;
            let to = ActorAddress::from(actor_id(&coin.actor)?);
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
            coin,
            flame,
            amount,
        } => {
            let pool = registry.pool(&coin)?.clone();
            let qa = sparks(&flame)?;
            let r = reserves(&client, &pool).await?;
            let qb = match amount {
                Some(qb) => qb,
                None if r.supply > 0 && r.a > 0 => {
                    // At the pool's price, rounded up so the A side sets the share.
                    let qb = (u128::from(qa) * r.b).div_ceil(r.a);
                    u64::try_from(qb).map_err(|_| "the matching amount does not fit")?
                }
                None => return Err("the first deposit sets the price: pass --amount".into()),
            };
            println!(
                "depositing {} FLM and {qb} {}",
                flames(qa.into()),
                pool.coin
            );
            let to = ActorAddress::from(actor_id(&pool.actor)?);
            let needs = [
                Need {
                    flavor: FLAME_FLAVOR,
                    qty: qa,
                },
                Need {
                    flavor: flavor(&pool.flavor_b)?,
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
        Command::Remove { coin, lp } => {
            let pool = registry.pool(&coin)?.clone();
            let to = ActorAddress::from(actor_id(&pool.actor)?);
            let needs = [Need {
                flavor: flavor(&pool.lp_flavor)?,
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
            coin,
            sell,
            amount,
            slippage,
            min_out,
        } => {
            let pool = registry.pool(&coin)?.clone();
            let r = reserves(&client, &pool).await?;
            let (sold, qty, x, y) = match sell.to_ascii_lowercase().as_str() {
                "flm" => (FLAME_FLAVOR, sparks(&amount)?, r.a, r.b),
                "coin" => (flavor(&pool.flavor_b)?, amount.parse()?, r.b, r.a),
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
                (format!("{out} {}", pool.coin), format!("{min}"))
            } else {
                (format!("{} FLM", flames(out)), flames(min.into()))
            };
            println!("expecting {got}, at least {least}");
            let to = ActorAddress::from(actor_id(&pool.actor)?);
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
        Command::Renew { actor, fund } => {
            let id = match actor.strip_prefix("pool:") {
                Some(tag) => registry.pool(tag)?.actor.clone(),
                None => registry.coin(&actor)?.actor.clone(),
            };
            let to = ActorAddress::from(actor_id(&id)?);
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
        Command::Status => status(&registry, &client).await,
        Command::Balance => {
            for (flavor, qty) in client.balances().await? {
                let name = registry.name_of(&flavor);
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
