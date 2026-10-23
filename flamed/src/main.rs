//! The `flamed` binary.
//!
//! This file gates devnet code on `feature = "devnet"` alone, unlike the
//! library, which gates on `any(test, feature = "devnet")`. It has to:
//! `cargo test` builds this target's own test harness with `cfg(test)` on,
//! but links the library built *without* it, so an `any(test, ..)` arm here
//! would name items that library does not have. For the same reason every
//! devnet `use` lives inside a gated body.

use std::path::PathBuf;
use std::sync::{Arc, LockResult, Mutex, MutexGuard};

use clap::{Parser, Subcommand};
use flamed::config::{GenesisFile, NodeConfig};
use flamed::node::{Node, SharedNode};
use tokio_util::sync::CancellationToken;

/// The Flame node.
#[derive(Parser)]
#[command(name = "flamed", about = "The Flame node", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Derives genesis.json from a network definition. Devnet builds only.
    #[cfg(feature = "devnet")]
    Genesis {
        /// The network definition to read.
        #[arg(long)]
        chainparams: PathBuf,
        /// Where to write the genesis.
        #[arg(long, default_value = "./genesis.json")]
        out: PathBuf,
    },
    /// Prints the first receiving address of the wallet whose BIP-39
    /// mnemonic is read from stdin, with no passphrase: the address a
    /// wallet restored from that mnemonic shows first. Devnet builds only.
    #[cfg(feature = "devnet")]
    Address {
        /// The network definition whose `network` the address is for.
        #[arg(long)]
        chainparams: PathBuf,
    },
    /// Opens a chain and serves it.
    Run {
        /// This node's policy file.
        #[arg(long, default_value = "./flamed.toml")]
        config: PathBuf,
    },
}

fn main() {
    // `Display`, not the `Debug` a `Result`-returning main would print: an
    // error here is for an operator to read, and several of them say what
    // to do about it.
    if let Err(error) = dispatch() {
        eprintln!("flamed: {error}");
        std::process::exit(1);
    }
}

fn dispatch() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        #[cfg(feature = "devnet")]
        Command::Genesis { chainparams, out } => {
            let chainparams = flamed::config::ChainParamsFile::load(&chainparams)?;
            let genesis = flamed::genesis::write(&chainparams, &out)?;
            println!(
                "genesis {} with {} allocation(s) -> {}",
                genesis.genesis_hash,
                genesis.contracts.len(),
                out.display()
            );
            Ok(())
        }
        #[cfg(feature = "devnet")]
        Command::Address { chainparams } => {
            println!("{}", first_address(&chainparams)?);
            Ok(())
        }
        // Built by hand rather than with `#[tokio::main]`, because
        // `flamed genesis` is synchronous and must not need a runtime.
        Command::Run { config } => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(run(config)),
    }
}

/// `m/35263'/network'/0'/0/0` of the mnemonic on stdin, in bech32f. The
/// phrase is a secret: it is never echoed, not even in an error, since a
/// parse error can name the word it stopped at.
#[cfg(feature = "devnet")]
fn first_address(chainparams: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    use std::io::Read;

    use flamekd::{util, Mnemonic, Network};
    use flamepayments::SpendAccount;
    use zeroize::Zeroizing;

    let network: Network = flamed::config::ChainParamsFile::load(chainparams)?
        .network
        .into();
    let mut phrase = Zeroizing::new(String::new());
    std::io::stdin().read_to_string(&mut phrase)?;
    let mnemonic =
        Mnemonic::parse(phrase.trim()).map_err(|_| "stdin is not a valid BIP-39 mnemonic")?;
    let seed = Zeroizing::new(mnemonic.to_seed(""));
    let address = SpendAccount::from_seed(&seed, network, 0)?
        .address_at(util::RECEIVING, 0)?
        .to_bech32(network);
    Ok(address)
}

async fn run(config: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = NodeConfig::load(&config)?;
    let genesis = GenesisFile::load(&cfg.genesis_path())?;
    // A playground is the core chain when there is one; otherwise this
    // node mines a mock one, with nothing burned on it.
    // Only a devnet build mines the mock chain itself.
    #[cfg_attr(not(feature = "devnet"), allow(unused_variables))]
    let (core, mock): (Box<dyn flamed::CoreIndexer>, _) = match &cfg.playground_url {
        Some(url) => {
            let network = genesis.chain.network.into();
            (Box::new(flamed::PlaygroundIndexer::new(url, network)?), None)
        }
        None => {
            let indexer = flamed::MockIndexer::open(&cfg.indexer_dir())?;
            (Box::new(indexer.clone()), Some(indexer))
        }
    };
    let node: SharedNode = Arc::new(Mutex::new(Node::open_with(&genesis, &cfg, core)?));
    {
        let opened = node.lock().expect("a freshly built node is not poisoned");
        println!(
            "flamed: height {} with {} unspent contract(s), {} block(s) archived",
            opened.tip().height,
            opened.utxo_count(),
            opened.block_count()
        );
    }

    // Installed only now, so a signal during `Node::open` still ends the
    // process on the spot: replay only reads, and an operator who stops a
    // long one should not have to wait it out.
    let mut signals = Termination::install()?;

    let (addr, handle) = flamed::rpc::serve(Arc::clone(&node), cfg.rpc_bind).await?;
    println!("flamed: serving JSON-RPC on http://{addr}");

    // Cancelled by a signal, or by the minter when it cannot go on. Every
    // task that touches the node winds down on it.
    let shutdown = CancellationToken::new();

    #[cfg(feature = "devnet")]
    let minter = {
        match (mock, &cfg.playground_url) {
            (Some(indexer), _) => {
                let core = flamed::minter::CoreMiner::new(indexer, cfg.core_block_interval());
                println!(
                    "flamed: mining a mock core block every {}s",
                    cfg.core_block_interval_secs
                );
                tokio::spawn(core.run(shutdown.clone()));
            }
            (None, url) => println!(
                "flamed: following the mint playground at {}",
                url.as_deref().unwrap_or_default()
            ),
        }
        let minter = flamed::minter::Minter::new(Arc::clone(&node), cfg.block_interval());
        println!("flamed: minting every {}s", cfg.block_interval_secs);
        tokio::spawn(minter.run(shutdown.clone()))
    };

    tokio::select! {
        signal = signals.next() => println!("flamed: {signal}, shutting down (again to force)"),
        _ = shutdown.cancelled() => {}
        // Nothing stops the server but this function, so this arm is a
        // guard, not a path: should it ever stop on its own, the node goes
        // down with it rather than minting with no one able to ask.
        _ = handle.clone().stopped() => eprintln!("flamed: the RPC server stopped unexpectedly"),
    }
    shutdown.cancel();

    let wind_down = async {
        // A graceful stop: no new connections, and the calls already in
        // flight are answered first. `Err` only means it had already
        // stopped.
        let _ = handle.stop();
        handle.stopped().await;
        #[cfg(feature = "devnet")]
        return minter.await;
        #[cfg(not(feature = "devnet"))]
        Ok::<_, tokio::task::JoinError>(Ok::<(), std::convert::Infallible>(()))
    };
    let halted = tokio::select! {
        halted = wind_down => halted?,
        signal = signals.next() => {
            eprintln!("flamed: {signal} again, dropping the RPC calls in flight");
            force_exit(node).await;
        }
    };

    // The last word on the node. Each RPC call runs its work on the
    // blocking pool, and a client that hangs up drops the future but not
    // the work, so the server stopping does not mean the node is idle.
    // Taking the lock does: whatever held it has finished. A poisoned lock
    // is fine here, since nothing is going to change the node again.
    let height = tokio::task::spawn_blocking(move || tip_height(&node.lock())).await?;
    println!("flamed: stopped at height {height}");

    halted?;
    Ok(())
}

/// Ends the process without waiting for anything but the disk.
///
/// The block archive is written only under the node lock, so taking it
/// waits out an append already under way, and holding it while the
/// process exits means no other one can start. Everything else — a reply
/// half sent, a transaction half admitted to the mempool — lives in memory
/// and is rebuilt or resubmitted after a restart.
async fn force_exit(node: SharedNode) -> ! {
    let exited = tokio::task::spawn_blocking(move || {
        let guard = node.lock();
        println!("flamed: stopped at height {}, forced", tip_height(&guard));
        // `exit` runs no destructors, so the guard is never released.
        std::process::exit(1)
    })
    .await;
    // Only a panic in that closure gets here, and it was the lock that
    // panicked: nothing is writing, so there is nothing left to wait for.
    eprintln!("flamed: forced stop failed: {exited:?}");
    std::process::exit(1)
}

/// The tip height behind a lock that may be poisoned.
fn tip_height(guard: &LockResult<MutexGuard<'_, Node>>) -> u64 {
    match guard {
        Ok(node) => node.tip().height,
        Err(poisoned) => poisoned.get_ref().tip().height,
    }
}

/// SIGINT and SIGTERM, as a stream.
///
/// Installing the handlers replaces the default disposition, which is to
/// die on the spot, for the rest of the process. The handlers are made once
/// and kept, so a second signal that arrives while the first is being acted
/// on is queued rather than lost.
struct Termination {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

impl Termination {
    #[cfg(unix)]
    fn install() -> std::io::Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};

        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    /// Resolves on the next signal, naming it.
    #[cfg(unix)]
    async fn next(&mut self) -> &'static str {
        tokio::select! {
            _ = self.interrupt.recv() => "SIGINT",
            _ = self.terminate.recv() => "SIGTERM",
        }
    }

    #[cfg(not(unix))]
    fn install() -> std::io::Result<Self> {
        Ok(Self {})
    }

    #[cfg(not(unix))]
    async fn next(&mut self) -> &'static str {
        // A failure to listen is a Ctrl-C this node will never hear, and
        // pending forever is exactly what that looks like.
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
        "Ctrl-C"
    }
}
