use std::{future::Future, pin::Pin, sync::Arc};

use anyhow::Result;
use tokio::sync::Mutex;

use crate::{
    DemoSession,
    error::DemoError,
    types::{DemoSnapshot, ResetRequest},
};

type SessionFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

struct SessionState {
    session: Option<DemoSession>,
}

#[derive(Clone)]
pub struct ApiState {
    inner: Arc<Mutex<SessionState>>,
}

impl ApiState {
    pub async fn start() -> Result<Self> {
        let session = DemoSession::start().await?;
        Ok(Self {
            inner: Arc::new(Mutex::new(SessionState {
                session: Some(session),
            })),
        })
    }

    pub(super) async fn execute<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a mut DemoSession) -> SessionFuture<'a, T> + Send + 'static,
    {
        let state = self.clone();
        tokio::spawn(async move {
            let mut state = state.inner.lock().await;
            let session = state
                .session
                .as_mut()
                .ok_or_else(|| DemoError::Unavailable("demo is stopped".into()))?;
            operation(session).await
        })
        .await?
    }

    pub(super) async fn reset(&self, _request: ResetRequest) -> Result<DemoSnapshot> {
        let state = self.clone();
        tokio::spawn(async move {
            let mut state = state.inner.lock().await;
            if state.session.is_none() {
                return Err(DemoError::Unavailable("demo is stopped".into()).into());
            }
            let mut replacement = DemoSession::start().await?;
            let snapshot = match replacement.snapshot().await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    if let Err(cleanup) = replacement.shutdown().await {
                        tracing::error!(%cleanup, "failed reset cleanup");
                    }
                    return Err(error);
                }
            };
            let previous = state.session.replace(replacement);
            if let Some(previous) = previous
                && let Err(error) = previous.shutdown().await
            {
                tracing::error!(%error, "old session cleanup failed after reset");
            }
            Ok(snapshot)
        })
        .await?
    }

    pub async fn shutdown(&self) -> Result<()> {
        let state = self.clone();
        tokio::spawn(async move {
            let mut state = state.inner.lock().await;
            if let Some(session) = state.session.take() {
                session.shutdown().await?;
            }
            Ok(())
        })
        .await?
    }
}
