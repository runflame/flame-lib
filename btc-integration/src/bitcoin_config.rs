use corepc_client::Auth;

/// How to reach a Bitcoin Core node.
#[derive(Clone)]
pub struct BitcoinConfig {
    /// The RPC of the node, like `http://127.0.0.1:18443`.
    pub node_rpc_url: String,
    pub auth: Auth,
}

impl BitcoinConfig {
    /// The RPC of the wallet `name` of the node.
    pub(crate) fn wallet_rpc_url(&self, name: &str) -> String {
        format!("{}/wallet/{name}", self.node_rpc_url.trim_end_matches('/'))
    }
}
