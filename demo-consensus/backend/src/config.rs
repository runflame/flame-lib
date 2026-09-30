use std::net::SocketAddr;

use anyhow::{Context, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DemoConfig {
    pub listen_address: SocketAddr,
}

impl Default for DemoConfig {
    fn default() -> Self {
        Self {
            listen_address: ([127, 0, 0, 1], 3001).into(),
        }
    }
}

impl DemoConfig {
    pub fn from_env() -> Result<Self> {
        let mut config = Self::default();
        if let Some(value) = std::env::var_os("DEMO_LISTEN") {
            config.listen_address = value
                .into_string()
                .map_err(|_| anyhow::anyhow!("DEMO_LISTEN must be UTF-8"))?
                .parse()
                .context(
                    "DEMO_LISTEN must be an IP address and port, for example 127.0.0.1:3001",
                )?;
        }
        Ok(config)
    }
}
