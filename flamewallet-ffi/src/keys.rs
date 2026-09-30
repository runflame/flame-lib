//! Mnemonics, seeds, and the wallet handle that holds an account.

use std::sync::{Arc, Mutex, MutexGuard};

use curve25519_dalek::ristretto::CompressedRistretto;
use flamekd::{Language, Mnemonic, ReceivingAddress};
use flamepayments::Account;
use rand::rngs::OsRng;
use rand::RngCore;
use zeroize::Zeroizing;

use crate::convert::array32;
use crate::error::FlameError;
use crate::note::{self, ReceivedNote};
use crate::transfer::{self, Transfer, TransferRequest};

/// Which network addresses are encoded for: `f1…` or `tf1…`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Network {
    Mainnet,
    Testnet,
}

impl From<Network> for flamekd::Network {
    fn from(network: Network) -> flamekd::Network {
        match network {
            Network::Mainnet => flamekd::Network::Mainnet,
            Network::Testnet => flamekd::Network::Testnet,
        }
    }
}

/// A position below the account: `m/35263'/network'/0'/branch/index`.
/// `branch` is `0` for receiving and `1` for change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct KeyPath {
    pub branch: u32,
    pub index: u32,
}

/// An address the wallet has handed out, with the predicate an indexer is
/// asked about.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct IssuedAddress {
    pub path: KeyPath,
    /// bech32f: `f1…` on mainnet, `tf1…` on testnet.
    pub address: String,
    /// The 32-byte compressed point that locks payments to this address.
    pub predicate: Vec<u8>,
}

/// A fresh English mnemonic of `word_count` words: 12, 15, 18, 21 or 24.
#[uniffi::export]
pub fn generate_mnemonic(word_count: u8) -> Result<String, FlameError> {
    if !matches!(word_count, 12 | 15 | 18 | 21 | 24) {
        return Err(FlameError::InvalidMnemonic {
            reason: format!("{word_count} words; BIP-39 defines 12, 15, 18, 21 or 24"),
        });
    }
    // Every 3 words carry 32 bits of entropy.
    let mut entropy = Zeroizing::new([0u8; 32]);
    let entropy = &mut entropy[..usize::from(word_count) / 3 * 4];
    OsRng.fill_bytes(entropy);
    let mnemonic = Mnemonic::from_entropy_in(Language::English, entropy).map_err(|error| {
        FlameError::InvalidMnemonic {
            reason: error.to_string(),
        }
    })?;
    Ok(mnemonic.to_string())
}

/// Whether `phrase` is a valid mnemonic in any BIP-39 language.
#[uniffi::export]
pub fn validate_mnemonic(phrase: String) -> bool {
    parse_mnemonic(&phrase).is_ok()
}

/// The 64-byte BIP-39 seed of `phrase` under `passphrase` (`""` for none).
/// This seed is what [`Wallet::new`] takes and what an app stores.
#[uniffi::export]
pub fn mnemonic_to_seed(phrase: String, passphrase: String) -> Result<Vec<u8>, FlameError> {
    Ok(parse_mnemonic(&phrase)?.to_seed(passphrase).to_vec())
}

/// The predicate a payment to `address` is locked with: the address's
/// spending point. Refuses an address encoded for the other network.
#[uniffi::export]
pub fn address_to_predicate(address: String, network: Network) -> Result<Vec<u8>, FlameError> {
    let address = ReceivingAddress::from_bech32(&address, network.into()).map_err(|error| {
        FlameError::InvalidAddress {
            reason: error.to_string(),
        }
    })?;
    Ok(address.spending_key().compress().to_bytes().to_vec())
}

fn parse_mnemonic(phrase: &str) -> Result<Mnemonic, FlameError> {
    Mnemonic::parse(phrase).map_err(|error| FlameError::InvalidMnemonic {
        reason: error.to_string(),
    })
}

/// One account, `m/35263'/network'/0'`, behind an opaque handle.
///
/// The handle is the only thing that ever holds key material: every
/// spending key is derived inside a call and dropped before it returns. It
/// is safe to share across threads. Calls that read or advance the counter
/// are serialized; [`Wallet::build_transfer`] works on a copy and holds no
/// lock while it proves.
#[derive(uniffi::Object)]
pub struct Wallet {
    account: Mutex<Account>,
}

#[uniffi::export]
impl Wallet {
    /// The account below a 64-byte seed, as [`mnemonic_to_seed`] gives it,
    /// with `next_index` receiving addresses already issued. The seed holds
    /// keys, not history: an app stores [`Wallet::next_index`] after each
    /// [`Wallet::next_address`] and passes it back here; a new wallet
    /// passes 0.
    #[uniffi::constructor]
    pub fn new(
        seed: Vec<u8>,
        network: Network,
        next_index: u32,
    ) -> Result<Arc<Wallet>, FlameError> {
        let seed = Zeroizing::new(<[u8; 64]>::try_from(seed.as_slice()).map_err(|_| {
            FlameError::InvalidSeed {
                reason: format!("{} bytes, expected 64", seed.len()),
            }
        })?);
        let account = Account::from_seed(&seed, network.into(), next_index)?;
        Ok(Arc::new(Wallet {
            account: Mutex::new(account),
        }))
    }

    /// The account below `phrase` and `passphrase`, without the caller ever
    /// seeing the seed; `next_index` as in [`Wallet::new`].
    #[uniffi::constructor]
    pub fn from_mnemonic(
        phrase: String,
        passphrase: String,
        network: Network,
        next_index: u32,
    ) -> Result<Arc<Wallet>, FlameError> {
        let seed = Zeroizing::new(parse_mnemonic(&phrase)?.to_seed(passphrase));
        let account = Account::from_seed(&seed, network.into(), next_index)?;
        Ok(Arc::new(Wallet {
            account: Mutex::new(account),
        }))
    }

    /// The network this wallet's addresses are encoded for.
    pub fn network(&self) -> Network {
        match self.account().network() {
            flamekd::Network::Mainnet => Network::Mainnet,
            flamekd::Network::Testnet => Network::Testnet,
        }
    }

    /// The address and predicate at `path`, without issuing anything.
    pub fn address(&self, path: KeyPath) -> Result<IssuedAddress, FlameError> {
        issued(&self.account(), path)
    }

    /// Issues the next receiving address and advances the counter.
    pub fn next_address(&self) -> Result<IssuedAddress, FlameError> {
        let mut account = self.account();
        let (index, _) = account.next_address()?;
        issued(
            &account,
            KeyPath {
                branch: flamekd::util::RECEIVING,
                index,
            },
        )
    }

    /// The next receiving index not yet issued.
    pub fn next_index(&self) -> u32 {
        self.account().next_index()
    }

    /// Which path below `next_index + gap`, on either branch, owns
    /// `predicate`; `None` if none does. The scan re-derives every address
    /// it covers, so `gap` is the caller's cost budget.
    pub fn owns(&self, predicate: Vec<u8>, gap: u32) -> Result<Option<KeyPath>, FlameError> {
        let point = CompressedRistretto(array32("predicate", &predicate)?);
        Ok(self
            .account()
            .owns(&point, gap)
            .map(|(branch, index)| KeyPath { branch, index }))
    }

    /// The account's receiving key in bech32f (`recv1…` / `testrecv1…`):
    /// what an indexer is given to find this wallet's payments. It derives
    /// every address and links their activity, and can neither spend nor
    /// read a payment's amount.
    pub fn receiving_key(&self) -> String {
        let account = self.account();
        account.recv_key().to_bech32(account.network())
    }

    /// Opens the note that followed `contract` in its log, as a scan serves
    /// the pair; `note` is `None` when the scan served none. `path` is what
    /// [`Wallet::owns`] found for the contract's predicate. The opening it
    /// gives is what a later [`crate::TransferInput`] spends the output with.
    pub fn open_note(
        &self,
        contract: Vec<u8>,
        note: Option<Vec<u8>>,
        path: KeyPath,
    ) -> Result<ReceivedNote, FlameError> {
        note::open(&self.account(), &contract, note.as_deref(), path)
    }

    /// Builds, proves and signs a transfer. The keys come from the paths
    /// the inputs name and never leave this call.
    ///
    /// Proving takes seconds on a phone, so it runs on a copy of the
    /// account with the lock released: other calls on this handle, from a
    /// UI thread say, do not wait for it. The build never touches the
    /// counter, so nothing the copy does needs writing back.
    pub fn build_transfer(&self, request: TransferRequest) -> Result<Transfer, FlameError> {
        let account = self.account().clone();
        transfer::build(&account, request)
    }
}

impl Wallet {
    /// A poisoned lock means another call panicked mid-way; the account
    /// holds only derivation state and a counter, and neither is left
    /// half-written by any method, so the guard is taken regardless.
    fn account(&self) -> MutexGuard<'_, Account> {
        self.account
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn issued(account: &Account, path: KeyPath) -> Result<IssuedAddress, FlameError> {
    let address = account.address_at(path.branch, path.index)?;
    Ok(IssuedAddress {
        path,
        address: address.to_bech32(account.network()),
        predicate: address.spending_key().compress().to_bytes().to_vec(),
    })
}
