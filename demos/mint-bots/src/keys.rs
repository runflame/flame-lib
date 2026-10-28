//! The bots' receiving addresses: `m/35263'/1'/0'/0/n` of one BIP-39
//! phrase, the receiving addresses `flamewallet restore` gives on testnet,
//! so whoever holds the phrase can spend what the playground pays the bots.

use std::io::Read;

use flamekd::{
    util::{PURPOSE, RECEIVING, TESTNET},
    Mnemonic, Network, SpendKey, HARDENED,
};
use zeroize::Zeroizing;

/// The clock bot's address, then the minimax bot's.
pub fn addresses(phrase: &str) -> Result<[String; 2], String> {
    let mnemonic = Mnemonic::parse(phrase.trim())
        // The parse error can name a word of the phrase: never print it.
        .map_err(|_| "the bots' phrase is not a valid BIP-39 mnemonic".to_string())?;
    let derive = || -> Result<[String; 2], flamekd::Error> {
        let receiving = SpendKey::from_mnemonic(&mnemonic, "")?
            .derive_child(HARDENED | PURPOSE)?
            .derive_child(HARDENED | TESTNET)?
            .derive_child(HARDENED)?
            .to_recv()
            .derive_child(RECEIVING)?;
        let address = |n| -> Result<String, flamekd::Error> {
            Ok(receiving
                .derive_child(n)?
                .to_address()
                .to_bech32(Network::Testnet))
        };
        Ok([address(0)?, address(1)?])
    };
    derive().map_err(|error| format!("the bots' addresses: {error}"))
}

/// Writes a fresh 24-word phrase, and nothing else, to `out`: what
/// `--new-key` pipes into `podman secret create`.
pub fn new_key(out: &mut impl std::io::Write) -> Result<(), Box<dyn std::error::Error>> {
    let mut entropy = Zeroizing::new([0u8; 32]);
    std::fs::File::open("/dev/urandom")?.read_exact(&mut *entropy)?;
    let phrase = Zeroizing::new(Mnemonic::from_entropy(&*entropy)?.to_string());
    out.write_all(phrase.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_the_wallets_first_two_receiving_addresses() {
        let mut out = Vec::new();
        new_key(&mut out).unwrap();
        let phrase = String::from_utf8(out).unwrap();
        assert_eq!(phrase.split_whitespace().count(), 24);
        assert!(phrase.ends_with('\n'));

        let seed = Mnemonic::parse(phrase.trim()).unwrap().to_seed("");
        let account = flamepayments::SpendAccount::from_seed(&seed, Network::Testnet, 0).unwrap();
        let wallet = [0, 1].map(|n| {
            account
                .address_at(RECEIVING, n)
                .unwrap()
                .to_bech32(Network::Testnet)
        });
        assert_eq!(addresses(&phrase).unwrap(), wallet);
        assert_ne!(wallet[0], wallet[1]);
        assert!(addresses("not a phrase").is_err());
    }
}
