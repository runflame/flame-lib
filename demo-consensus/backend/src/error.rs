#[derive(Debug, thiserror::Error)]
pub enum DemoError {
    #[error("{0}")]
    InvalidRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Timeout(String),
}
