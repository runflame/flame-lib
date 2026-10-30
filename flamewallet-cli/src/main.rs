//! `flamewallet`: a development wallet for a `flamed` node.
//!
//! Every command prints text for people, or with `--json` one JSON value
//! for scripts. Errors go to stderr with exit status 1.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use flamed_rpc::{TxId, TxStatusResult};
use flamewallet_cli::{
    format_qty, parse_qty, DerivationPath, Error, Flavor, IssueRequest, NetworkName, Secret,
    SendRequest, Session, WalletFile, DEFAULT_FEE, DEFAULT_GAS, DEFAULT_NODE,
};
use serde::Serialize;
use serde_json::json;
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(
    name = "flamewallet",
    version,
    about = "A development wallet for a flamed node"
)]
struct Cli {
    /// The wallet file. It holds the secret in the clear.
    #[arg(
        long,
        global = true,
        env = "FLAMEWALLET",
        default_value = "./wallet.json"
    )]
    wallet: PathBuf,
    /// The node's JSON-RPC URL. `new` and `restore` store it in the file;
    /// other commands use it instead of the stored one.
    #[arg(long, global = true, env = "FLAMEWALLET_NODE")]
    node: Option<String>,
    /// Print one JSON value instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Creates a wallet from a fresh mnemonic and prints the mnemonic and
    /// the first receiving address.
    New {
        #[arg(long, default_value = "testnet")]
        network: NetworkName,
        /// 12, 15, 18, 21 or 24.
        #[arg(long, default_value_t = 12)]
        words: u8,
    },
    /// Creates a wallet from a mnemonic read from stdin, or with `--seed`
    /// from a 64-byte seed in hex: `a1` × 64 holds the devnet's genesis
    /// allocation.
    Restore {
        #[arg(long, default_value = "testnet")]
        network: NetworkName,
        /// Read a hex seed from stdin instead of a mnemonic.
        #[arg(long)]
        seed: bool,
        /// The BIP-39 passphrase; none by default, as `flamed address`.
        #[arg(long, env = "FLAMEWALLET_PASSPHRASE", default_value = "")]
        passphrase: String,
    },
    /// Prints the receiving addresses issued so far, or with `--path` the
    /// address at one path, which the wallet watches from then on.
    Address {
        /// Issue the next one first.
        #[arg(long, conflicts_with = "path")]
        new: bool,
        /// `branch/index`, as `2/0`: 0 is receiving, 1 change, any other
        /// branch yours to use.
        #[arg(long)]
        path: Option<DerivationPath>,
    },
    /// Balance per flavor.
    Balance,
    /// Every unspent coin.
    Coins,
    /// Pays `amount` of a flavor to an address. Flame amounts are in
    /// flames with up to eight decimals; other flavors in whole units.
    Send {
        to: String,
        amount: String,
        /// Spend only coins at this path, `branch/index`, and return the
        /// change there; by default any coin, change to `1/0`.
        #[arg(long)]
        from: Option<DerivationPath>,
        /// `FLM`, or a flavor's 64 hex digits.
        #[arg(long, default_value = "FLM")]
        flavor: Flavor,
        #[arg(long, default_value = "")]
        memo: String,
        /// Pay a cleartext token: the amount, flavor and memo are public;
        /// the change stays confidential.
        #[arg(long)]
        clear: bool,
        /// In sparks, always paid in Flame.
        #[arg(long, default_value_t = DEFAULT_FEE)]
        fee: u64,
        #[arg(long, default_value_t = DEFAULT_GAS)]
        gas: u64,
        /// Wait until it is in a block.
        #[arg(long)]
        wait: bool,
        /// How long `--wait` waits, in seconds.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Mints `amount` whole units of a new confidential flavor, named by
    /// `tag` and the issuer's address, and prints the flavor. Spends a
    /// Flame coin at the issuer's address for the fee and returns the rest
    /// there.
    Issue {
        amount: String,
        #[arg(long)]
        tag: String,
        /// The issuer, `branch/index`: one flavor per issuer and tag.
        #[arg(long, default_value = "0/0")]
        from: DerivationPath,
        /// Where the minted coin goes; the issuer's address by default.
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = "")]
        memo: String,
        /// In sparks, always paid in Flame.
        #[arg(long, default_value_t = DEFAULT_FEE)]
        fee: u64,
        #[arg(long, default_value_t = DEFAULT_GAS)]
        gas: u64,
        /// Wait until it is in a block.
        #[arg(long)]
        wait: bool,
        /// How long `--wait` waits, in seconds.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// The flavor `issue --tag TAG --from PATH` mints, without minting.
    Flavor {
        tag: String,
        #[arg(long, default_value = "0/0")]
        from: DerivationPath,
    },
    /// Transactions that touched the wallet, newest first.
    History {
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// The cursor a previous page printed, for the page before it.
        #[arg(long)]
        older: Option<String>,
    },
    /// Where a transaction is.
    Status { txid: String },
    /// Waits until a transaction is in a block.
    Wait {
        txid: String,
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// The node's tip.
    Tip,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("flamewallet: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), Error> {
    let out = Out { json: cli.json };
    match &cli.command {
        &Command::New { network, words } => {
            let phrase = Zeroizing::new(flamewallet_ffi::generate_mnemonic(words)?);
            let file = create(
                &cli,
                network,
                Secret::Mnemonic {
                    phrase: phrase.to_string(),
                    passphrase: String::new(),
                },
            )?;
            let address = first_address(&file)?;
            out.print(
                &json!({ "mnemonic": phrase.as_str(), "address": address }),
                || format!("mnemonic: {}\naddress:  {address}", phrase.as_str()),
            );
            if !out.json {
                eprintln!(
                    "the mnemonic is stored in the clear in {}",
                    cli.wallet.display()
                );
            }
        }
        Command::Restore {
            network,
            seed,
            passphrase,
        } => {
            let (network, seed) = (*network, *seed);
            let mut input = Zeroizing::new(String::new());
            std::io::stdin()
                .read_to_string(&mut input)
                .map_err(|source| Error::File {
                    path: "stdin".into(),
                    source,
                })?;
            let text = input.split_whitespace().collect::<Vec<_>>().join(" ");
            let secret = if seed {
                Secret::Seed { hex: text }
            } else {
                if !flamewallet_ffi::validate_mnemonic(text.clone()) {
                    return Err(Error::Invalid("stdin holds no valid mnemonic".into()));
                }
                Secret::Mnemonic {
                    phrase: text,
                    passphrase: passphrase.clone(),
                }
            };
            let file = create(&cli, network, secret)?;
            let address = first_address(&file)?;
            out.print(&json!({ "address": address }), || {
                format!("address: {address}")
            });
        }
        &Command::Address {
            path: Some(path), ..
        } => {
            let mut file = WalletFile::load(&cli.wallet)?;
            let address = file.open()?.address(path.key_path())?.receiving_address;
            // Issued receiving addresses and the change address are watched
            // without being listed.
            let receiving =
                path.branch == flamewallet_ffi::RECEIVING && path.index < file.next_index;
            let change = path.branch == flamewallet_ffi::CHANGE && path.index == 0;
            if !receiving && !change && !file.watch.contains(&path) {
                file.watch.push(path);
                file.save(&cli.wallet)?;
            }
            out.print(&json!({ "path": path, "address": address }), || {
                format!("{path}  {address}")
            });
        }
        &Command::Address { new, path: None } => {
            let mut file = WalletFile::load(&cli.wallet)?;
            let wallet = file.open()?;
            if new {
                wallet.next_address()?;
                file.next_index = wallet.next_index();
                file.save(&cli.wallet)?;
            }
            let addresses = (0..wallet.next_index().max(1))
                .map(|index| {
                    wallet
                        .address(flamewallet_ffi::KeyPath {
                            branch: flamewallet_ffi::RECEIVING,
                            index,
                        })
                        .map(|a| (index, a.receiving_address))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let json: Vec<_> = addresses
                .iter()
                .map(|(index, address)| json!({ "index": index, "address": address }))
                .collect();
            out.print(&json, || {
                addresses
                    .iter()
                    .map(|(index, address)| format!("{index}  {address}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        }
        Command::Balance => {
            let session = session(&cli)?;
            let coins = session.coins().await?;
            let balances = coins.balances();
            let uncounted = coins.uncounted();
            out.print(
                &json!({
                    "tip_height": coins.tip_height,
                    "balances": balances,
                    "uncounted": uncounted,
                }),
                || {
                    let mut lines: Vec<String> = balances
                        .iter()
                        .map(|b| {
                            let mut line = format!(
                                "{} {}",
                                format_qty(b.confirmed as i128, b.flavor),
                                b.flavor
                            );
                            if b.pending > 0 {
                                line += &format!(
                                    "  (+{} pending)",
                                    format_qty(b.pending as i128, b.flavor)
                                );
                            }
                            line
                        })
                        .collect();
                    if uncounted > 0 {
                        lines.push(format!("{uncounted} coin(s) not counted; see `coins`"));
                    }
                    lines.push(format!("at height {}", coins.tip_height));
                    lines.join("\n")
                },
            );
        }
        Command::Coins => {
            let session = session(&cli)?;
            let coins = session.coins().await?;
            out.print(&coins, || {
                let mut lines: Vec<String> = coins
                    .coins
                    .iter()
                    .map(|coin| {
                        let what = match &coin.value {
                            flamewallet_cli::CoinValue::Clear { qty, flavor } => {
                                format!("{} {flavor} clear", format_qty(*qty as i128, *flavor))
                            }
                            flamewallet_cli::CoinValue::Confidential { qty, flavor, memo } => {
                                let mut what =
                                    format!("{} {flavor}", format_qty(*qty as i128, *flavor));
                                if !memo.is_empty() {
                                    what += &format!("  memo {memo:?}");
                                }
                                what
                            }
                            flamewallet_cli::CoinValue::Unreadable { reason } => {
                                format!("unreadable: {reason}")
                            }
                            flamewallet_cli::CoinValue::NotToken => "not a token".into(),
                        };
                        let height = coin
                            .height
                            .map_or_else(|| "pending".to_string(), |h| format!("h{h}"));
                        format!(
                            "{}  {}/{}  {height}  {what}",
                            coin.id, coin.branch, coin.index
                        )
                    })
                    .collect();
                if lines.is_empty() {
                    lines.push("no coins".into());
                }
                lines.join("\n")
            });
        }
        Command::Send {
            to,
            amount,
            from,
            flavor,
            memo,
            clear,
            fee,
            gas,
            wait,
            timeout,
        } => {
            let (flavor, fee, gas, wait, timeout) = (*flavor, *fee, *gas, *wait, *timeout);
            let session = session(&cli)?;
            let sent = session
                .send(&SendRequest {
                    from: *from,
                    to: to.clone(),
                    qty: parse_qty(amount, flavor)?,
                    flavor,
                    memo: memo.as_bytes().to_vec(),
                    clear: *clear,
                    fee,
                    gas,
                })
                .await?;
            let status = if wait {
                Some(
                    session
                        .wait(sent.txid, Duration::from_secs(timeout))
                        .await?,
                )
            } else {
                None
            };
            out.print(&json!({ "sent": sent, "status": status }), || {
                let mut text = sent.txid.to_string();
                if let Some(status) = &status {
                    text += &format!("\n{}", describe(status));
                }
                text
            });
        }
        Command::Issue {
            amount,
            tag,
            from,
            to,
            memo,
            fee,
            gas,
            wait,
            timeout,
        } => {
            let (from, fee, gas, wait, timeout) = (*from, *fee, *gas, *wait, *timeout);
            let session = session(&cli)?;
            let flavor = session.issued_flavor(from, tag)?;
            let issued = session
                .issue(&IssueRequest {
                    from,
                    tag: tag.clone(),
                    qty: parse_qty(amount, flavor)?,
                    to: to.clone(),
                    memo: memo.as_bytes().to_vec(),
                    fee,
                    gas,
                })
                .await?;
            let status = if wait {
                Some(
                    session
                        .wait(issued.txid, Duration::from_secs(timeout))
                        .await?,
                )
            } else {
                None
            };
            out.print(&json!({ "issued": issued, "status": status }), || {
                let mut text = format!("{}\nflavor {}", issued.txid, issued.flavor);
                if let Some(status) = &status {
                    text += &format!("\n{}", describe(status));
                }
                text
            });
        }
        Command::Flavor { tag, from } => {
            let session = session(&cli)?;
            let flavor = session.issued_flavor(*from, tag)?;
            let issuer = session.address_at(*from)?.receiving_address;
            out.print(
                &json!({ "tag": tag, "from": from, "flavor": flavor, "issuer": issuer }),
                || flavor.to_hex(),
            );
        }
        Command::History { limit, older } => {
            let session = session(&cli)?;
            let page = session.history(older.clone(), *limit).await?;
            out.print(&page, || {
                let mut lines: Vec<String> = page
                    .items
                    .iter()
                    .map(|item| {
                        let when = match item.height {
                            Some(height) => format!("h{height}"),
                            None => "pending".into(),
                        };
                        let changes = item
                            .changes
                            .iter()
                            .map(|c| {
                                let sign = if c.delta > 0 { "+" } else { "" };
                                format!("{sign}{} {}", format_qty(c.delta, c.flavor), c.flavor)
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        let mut line = format!("{when}  {}  {changes}", item.txid);
                        match item.fee_sparks.parse::<i128>() {
                            Ok(0) => {}
                            Ok(fee) => {
                                line += &format!("  fee {} FLM", format_qty(fee, Flavor::flame()))
                            }
                            Err(_) => line += &format!("  fee {} sparks", item.fee_sparks),
                        }
                        if !item.from.is_empty() {
                            line += &format!("  from {}", item.from.join(", "));
                        }
                        if !item.to.is_empty() {
                            line += &format!("  to {}", item.to.join(", "));
                        }
                        if let Some(memo) = &item.memo {
                            line += &format!("  memo {memo:?}");
                        }
                        if item.unreadable > 0 {
                            line += &format!("  ({} unreadable)", item.unreadable);
                        }
                        line
                    })
                    .collect();
                if lines.is_empty() {
                    lines.push("no history".into());
                }
                if let Some(older) = &page.older {
                    lines.push(format!("older: --older {older}"));
                }
                lines.join("\n")
            });
        }
        Command::Status { txid } => {
            let status = session(&cli)?.status(parse_txid(txid)?).await?;
            out.print(&status, || describe(&status));
        }
        Command::Wait { txid, timeout } => {
            let status = session(&cli)?
                .wait(parse_txid(txid)?, Duration::from_secs(*timeout))
                .await?;
            out.print(&status, || describe(&status));
        }
        Command::Tip => {
            let tip = session(&cli)?.tip().await?;
            out.print(&tip, || format!("height {}  {}", tip.height, tip.hash));
        }
    }
    Ok(())
}

fn create(cli: &Cli, network: NetworkName, secret: Secret) -> Result<WalletFile, Error> {
    let file = WalletFile {
        version: 1,
        network,
        node: cli.node.clone().unwrap_or_else(|| DEFAULT_NODE.into()),
        secret,
        next_index: 1,
        watch: Vec::new(),
    };
    // Opened before it is written, so a bad seed leaves no file behind.
    file.open()?;
    file.create(&cli.wallet)?;
    Ok(file)
}

fn first_address(file: &WalletFile) -> Result<String, Error> {
    Ok(file
        .open()?
        .address(flamewallet_ffi::KeyPath {
            branch: flamewallet_ffi::RECEIVING,
            index: 0,
        })?
        .receiving_address)
}

fn session(cli: &Cli) -> Result<Session, Error> {
    let file = WalletFile::load(&cli.wallet)?;
    let node = cli.node.clone().unwrap_or_else(|| file.node.clone());
    Session::open(&file, &node)
}

fn parse_txid(text: &str) -> Result<TxId, Error> {
    hex::decode(text)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .map(TxId)
        .ok_or_else(|| Error::Invalid(format!("txid {text:?} is not 64 hex digits")))
}

fn describe(status: &TxStatusResult) -> String {
    match status {
        TxStatusResult::Unknown => "unknown".into(),
        TxStatusResult::Mempool => "in the mempool".into(),
        TxStatusResult::Confirmed { height, block } => {
            format!("confirmed at height {height} in {block}")
        }
    }
}

struct Out {
    json: bool,
}

impl Out {
    fn print<T: Serialize + ?Sized>(&self, value: &T, text: impl FnOnce() -> String) {
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(value).expect("output serializes")
            );
        } else {
            println!("{}", text());
        }
    }
}
