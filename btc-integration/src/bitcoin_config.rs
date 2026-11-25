use corepc_client::Auth;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// What a wallet name keeps as it is in an URL: letters, digits and `-._~`.
const WALLET_NAME: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

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
        // Bitcoin Core decodes the name it finds in the path.
        let name = utf8_percent_encode(name, WALLET_NAME);
        format!("{}/wallet/{name}", self.node_rpc_url.trim_end_matches('/'))
    }
}
