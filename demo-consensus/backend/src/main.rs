use anyhow::{Context, Result};
use demo_consensus_backend::{config::DemoConfig, server};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let config = DemoConfig::from_env()?;
    let listener = TcpListener::bind(config.listen_address)
        .await
        .context("bind demo HTTP address")?;
    let shutdown = shutdown_signal()?;
    server::serve(listener, shutdown).await
}

fn shutdown_signal() -> Result<impl std::future::Future<Output = ()>> {
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    #[cfg(unix)]
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    Ok(async move {
        #[cfg(unix)]
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
        #[cfg(not(unix))]
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "Ctrl+C handler failed");
        }
        tracing::info!("stopping consensus demo");
    })
}
