use corepc_client::Auth;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

const WALLET_NAME: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

#[derive(Clone)]
pub struct BitcoinConfig {
    pub node_rpc_url: String,
    pub auth: Auth,
}

impl BitcoinConfig {
    pub(crate) fn wallet_rpc_url(&self, name: &str) -> String {
        // Bitcoin Core decodes the name it finds in the path.
        let name = utf8_percent_encode(name, WALLET_NAME);
        format!("{}/wallet/{name}", self.node_rpc_url.trim_end_matches('/'))
    }
}
