use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use btc_overlay::{Overlay, blockbook::Blockbook, rpc, state::State};
use clap::Parser;

/// A fake Bitcoin Core for Flame nodes: real mainnet headers, simulated transactions.
#[derive(Parser)]
struct Args {
    /// Blockbook root URL, e.g. http://127.0.0.1:8080/indexer/mainnet
    #[arg(long)]
    blockbook: String,
    /// Where the overlay log lives; it holds every block's contents.
    #[arg(long, default_value = "overlay.log")]
    log: PathBuf,
    /// JSON-RPC listen address.
    #[arg(long, default_value = "127.0.0.1:18443")]
    listen: SocketAddr,
    /// First block served on a fresh log (default: 20 below the tip).
    #[arg(long)]
    start_height: Option<u64>,
    /// Seconds between header polls.
    #[arg(long, default_value_t = 5)]
    poll_secs: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let overlay = Arc::new(Overlay::new(
        Arc::new(Blockbook::new(&args.blockbook)),
        State::open(&args.log)?,
        args.start_height,
    ));
    // Serve only once there is a chain.
    overlay.sync().await?;

    let follower = Arc::clone(&overlay);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(args.poll_secs)).await;
            match follower.sync().await {
                Ok(true) => {
                    if let Some(tip) = *follower.subscribe().borrow() {
                        eprintln!("tip {} {}", tip.height, tip.hash);
                    }
                }
                Ok(false) => {}
                Err(error) => eprintln!("sync failed: {error}"),
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    eprintln!("btc-overlay listening on {}", args.listen);
    axum::serve(listener, rpc::router(overlay))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
