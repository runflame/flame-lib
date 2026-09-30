use std::future::Future;

use anyhow::{Context, Result};
use tokio::net::TcpListener;

use crate::api::{ApiState, router};

pub async fn serve(
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let address = listener.local_addr()?;
    let state = ApiState::start().await?;
    tracing::info!(%address, "consensus demo is ready");
    let result = axum::serve(listener, router(state.clone()))
        .with_graceful_shutdown(shutdown)
        .await
        .context("serve demo API");
    let cleanup = state.shutdown().await;
    result?;
    cleanup
}
