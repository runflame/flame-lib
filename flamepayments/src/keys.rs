//! Accounts: one flamekd seed, or a key below it, and the addresses and keys
//! below that.
//!
//! An [`Account`] is the node at `m/35263'/network'/0'`. Everything the
//! wallet hands out lives under it on the two normal branches — receiving
//! (`util::RECEIVING`) and change (`util::CHANGE`) — so an address is
//! `m/35263'/network'/0'/branch/n`. The path constants come from
//! [`flamekd::util`] and are never copied here.
//!
//! What an account can do is in its type, one per flamekd key:
//!
//! - [`SpendAccount`], `Account<SpendKey>`, comes from a seed: it finds,
//!   opens and spends.
//! - [`ViewAccount`], `Account<ViewKey>`, comes from a view key: it finds
//!   and opens every payment, and has no `spending_key_at`.
//! - [`ReceiveAccount`], `Account<RecvKey>`, comes from a receiving key: it
//!   finds every payment, and has neither `spending_key_at` nor
//!   `viewing_key_at`.
//!
//! `Account<K>` with `K: AccountKey` names any of the three, and with
//! `K: ViewingKey` either of the first two, for code that needs no more.
//! Calling past what the key allows does not compile:
//!
//! ```compile_fail
//! # use flamekd::Network;
//! # use flamepayments::{SpendAccount, ViewAccount};
//! let account = SpendAccount::from_seed(&[7; 64], Network::Testnet, 0).unwrap();
//! let view: ViewAccount = account.to_view_account();
//! view.spending_key_at(0, 0); // no such method on a ViewAccount
//! ```
//!
//! ```compile_fail
//! # use flamekd::Network;
//! # use flamepayments::{ReceiveAccount, SpendAccount};
//! let account = SpendAccount::from_seed(&[7; 64], Network::Testnet, 0).unwrap();
//! let receive: ReceiveAccount = account.to_receive_account();
//! receive.viewing_key_at(0, 0); // no such method on a ReceiveAccount
//! ```

use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar as DalekScalar;
use flamekd::{util, Network, ReceivingAddress, RecvKey, SpendKey, ViewKey, HARDENED};
use flamevm::Predicate;

/// A key could not be derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// A branch or address index was hardened. Both positions are normal in
    /// the standard path, so a hardened index is a caller mistake, not a
    /// derivation the account could perform with more authority.
    #[error("index {0} is hardened; branch and address indices must be normal")]
    HardenedIndex(u32),

    /// flamekd refused the derivation or the seed.
    #[error(transparent)]
    Kd(#[from] flamekd::Error),
}

/// The extended key an [`Account`] holds at `m/35263'/network'/0'`: a
/// [`SpendKey`], a [`ViewKey`] or a [`RecvKey`]. All three derive the same
/// normal children and so the same addresses. Sealed: an account is built
/// from one of these three and nothing else.
pub trait AccountKey: Clone + sealed::Sealed {
    /// The normal or, for a `SpendKey`, hardened child at `index`.
    fn derive_child(&self, index: u32) -> Result<Self, flamekd::Error>;

    /// This node with spending and viewing authority removed.
    fn to_recv(&self) -> RecvKey;
}

/// An [`AccountKey`] that also views: a [`SpendKey`] or a [`ViewKey`]. Both
/// derive the same viewing scalars, so both open the same notes.
pub trait ViewingKey: AccountKey {
    /// The viewing scalar of this node.
    fn viewing_key(&self) -> &DalekScalar;

    /// This node with spending authority removed.
    fn to_view(&self) -> ViewKey;
}

impl AccountKey for SpendKey {
    fn derive_child(&self, index: u32) -> Result<Self, flamekd::Error> {
        SpendKey::derive_child(self, index)
    }

    fn to_recv(&self) -> RecvKey {
        SpendKey::to_recv(self)
    }
}

impl ViewingKey for SpendKey {
    fn viewing_key(&self) -> &DalekScalar {
        SpendKey::viewing_key(self)
    }

    fn to_view(&self) -> ViewKey {
        SpendKey::to_view(self)
    }
}

impl AccountKey for ViewKey {
    fn derive_child(&self, index: u32) -> Result<Self, flamekd::Error> {
        ViewKey::derive_child(self, index)
    }

    fn to_recv(&self) -> RecvKey {
        ViewKey::to_recv(self)
    }
}

impl ViewingKey for ViewKey {
    fn viewing_key(&self) -> &DalekScalar {
        ViewKey::viewing_key(self)
    }

    fn to_view(&self) -> ViewKey {
        self.clone()
    }
}

impl AccountKey for RecvKey {
    fn derive_child(&self, index: u32) -> Result<Self, flamekd::Error> {
        RecvKey::derive_child(self, index)
    }

    fn to_recv(&self) -> RecvKey {
        self.clone()
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for flamekd::SpendKey {}
    impl Sealed for flamekd::ViewKey {}
    impl Sealed for flamekd::RecvKey {}
}

/// One account: the extended key at `m/35263'/network'/0'`, the network
/// its addresses are encoded for, and the next unissued receiving index.
/// `K` says which key, and so what the account can do: see
/// [`SpendAccount`], [`ViewAccount`] and [`ReceiveAccount`].
#[derive(Clone)]
pub struct Account<K> {
    account: K,
    network: Network,
    next_index: u32,
}

/// A spending account: built from a seed, it finds, opens and spends.
pub type SpendAccount = Account<SpendKey>;

/// A view account: built from a view key, it finds and opens every payment
/// the spending account does, and cannot spend any of them.
pub type ViewAccount = Account<ViewKey>;

/// A receive account: built from a receiving key, it generates every
/// address and finds every payment, and can neither open nor spend one.
/// What an indexer holds.
pub type ReceiveAccount = Account<RecvKey>;

impl SpendAccount {
    /// The node at `m/35263'/network'/0'` from a 64-byte seed, with
    /// `next_index` receiving addresses already issued. The seed holds the
    /// keys, not how many addresses were handed out, so a reopened wallet
    /// passes back the counter it stored; a new one passes 0. Refuses a
    /// hardened index — no address at or past it can ever be issued.
    pub fn from_seed(
        seed: &[u8; 64],
        network: Network,
        next_index: u32,
    ) -> Result<SpendAccount, KeyError> {
        let network_index = match network {
            Network::Mainnet => util::MAINNET,
            Network::Testnet => util::TESTNET,
        };
        let account = SpendKey::from_seed(seed)?
            .derive_child(HARDENED | util::PURPOSE)?
            .derive_child(HARDENED | network_index)?
            .derive_child(HARDENED)?;
        Account::new(account, network, next_index)
    }

    /// `s_n`, the spending scalar at `m/…/branch/n`, by value: the
    /// [`SpendKey`] it came from zeroizes on drop.
    pub fn spending_key_at(&self, branch: u32, n: u32) -> Result<DalekScalar, KeyError> {
        Ok(*self.key_at(branch, n)?.spending_key())
    }
}

impl ViewAccount {
    /// A view account from the view key at `m/35263'/network'/0'`, as
    /// [`Account::view_key`] exports it, with `next_index` as in
    /// [`SpendAccount::from_seed`]. Nothing here can check that `view` is
    /// the account node rather than some other one: a key from elsewhere
    /// just owns nothing.
    pub fn from_view_key(
        view: ViewKey,
        network: Network,
        next_index: u32,
    ) -> Result<ViewAccount, KeyError> {
        Account::new(view, network, next_index)
    }
}

impl ReceiveAccount {
    /// A receive account from the receiving key at `m/35263'/network'/0'`,
    /// as [`Account::recv_key`] exports it, with `next_index` as in
    /// [`SpendAccount::from_seed`]. As with
    /// [`ViewAccount::from_view_key`], a key from another node just owns
    /// nothing.
    pub fn from_recv_key(
        recv: RecvKey,
        network: Network,
        next_index: u32,
    ) -> Result<ReceiveAccount, KeyError> {
        Account::new(recv, network, next_index)
    }
}

impl<K: ViewingKey> Account<K> {
    /// `v_n`, the viewing scalar at `m/…/branch/n`, by value: the extended
    /// key it came from zeroizes on drop.
    pub fn viewing_key_at(&self, branch: u32, n: u32) -> Result<DalekScalar, KeyError> {
        Ok(*self.key_at(branch, n)?.viewing_key())
    }

    /// The account-level view key: what a view account is built from. It
    /// generates every address below the account and opens their notes, so
    /// it reads every amount and memo, but it holds no spending scalar.
    pub fn view_key(&self) -> ViewKey {
        self.account.to_view()
    }

    /// This account narrowed to its view key, with the same network and
    /// counter.
    pub fn to_view_account(&self) -> ViewAccount {
        Account {
            account: self.account.to_view(),
            network: self.network,
            next_index: self.next_index,
        }
    }
}

impl<K: AccountKey> Account<K> {
    /// Every constructor ends here, so each refuses a hardened counter the
    /// same way.
    fn new(account: K, network: Network, next_index: u32) -> Result<Account<K>, KeyError> {
        Ok(Account {
            account,
            network,
            next_index: normal(next_index)?,
        })
    }

    /// The network this account's addresses are encoded for.
    pub fn network(&self) -> Network {
        self.network
    }

    /// `(S_n, V_n)` at `m/…/branch/n`, derived through the receiving key:
    /// no secret is touched.
    pub fn address_at(&self, branch: u32, n: u32) -> Result<ReceivingAddress, KeyError> {
        Ok(self
            .recv_key_for(branch)?
            .derive_child(normal(n)?)?
            .to_address())
    }

    /// The predicate that locks a payment to `m/…/branch/n`:
    /// `Predicate::opaque(S_n.compress())`, with no Taproot tree behind it.
    /// A spend of such a contract authorizes through `signtx`.
    pub fn predicate_at(&self, branch: u32, n: u32) -> Result<Predicate, KeyError> {
        Ok(Predicate::opaque(
            self.address_at(branch, n)?.spending_key().compress(),
        ))
    }

    /// The account-level receiving key: what a receive account is built
    /// from. It generates every address below the account and links their
    /// activity, and can neither spend nor read a payment's private
    /// contents.
    pub fn recv_key(&self) -> RecvKey {
        self.account.to_recv()
    }

    /// The receiving key for one branch, `m/…/branch`.
    pub fn recv_key_for(&self, branch: u32) -> Result<RecvKey, KeyError> {
        Ok(self.recv_key().derive_child(normal(branch)?)?)
    }

    /// This account narrowed to its receiving key, with the same network
    /// and counter.
    pub fn to_receive_account(&self) -> ReceiveAccount {
        Account {
            account: self.account.to_recv(),
            network: self.network,
            next_index: self.next_index,
        }
    }

    /// Reserves the next receiving address and returns it with its index.
    pub fn next_address(&mut self) -> Result<(u32, ReceivingAddress), KeyError> {
        let index = self.next_index;
        let address = self.address_at(util::RECEIVING, index)?;
        self.next_index = index.saturating_add(1);
        Ok((index, address))
    }

    /// The next receiving index this account has not issued yet.
    pub fn next_index(&self) -> u32 {
        self.next_index
    }

    /// Which `(branch, n)` below `next_index + gap` owns `point`, the
    /// receiving branch first. The gap covers addresses handed out by
    /// another copy of the wallet that this one has not issued itself.
    ///
    /// The scan re-derives `2 × (next_index + gap)` addresses and caches
    /// nothing, so `gap` is the caller's cost budget.
    pub fn owns(&self, point: &CompressedRistretto, gap: u32) -> Option<(u32, u32)> {
        let limit = self.next_index.saturating_add(gap);
        for branch in [util::RECEIVING, util::CHANGE] {
            // An `Option` return leaves nowhere to report a derivation
            // failure, and neither skip hides a wrong answer: the branch
            // constants are normal indices, and the only reachable child
            // failure is `n >= HARDENED`, which needs `next_index + gap`
            // past 2^31 and is not an address this account ever issued.
            let Ok(branch_key) = self.recv_key_for(branch) else {
                continue;
            };
            for n in 0..limit {
                let Ok(child) = branch_key.derive_child(n) else {
                    continue;
                };
                if child.to_address().spending_key().compress() == *point {
                    return Some((branch, n));
                }
            }
        }
        None
    }

    /// The extended key at `m/…/branch/n`. Private, so no extended key
    /// escapes the module: the key this returns is dropped — and zeroized —
    /// inside the accessors above. The scalar those accessors hand out is
    /// itself key material and is not zeroized; protecting the copy is the
    /// caller's job.
    fn key_at(&self, branch: u32, n: u32) -> Result<K, KeyError> {
        Ok(self
            .account
            .derive_child(normal(branch)?)?
            .derive_child(normal(n)?)?)
    }
}

/// Passes a normal index through; rejects a hardened one.
fn normal(index: u32) -> Result<u32, KeyError> {
    if index >= HARDENED {
        return Err(KeyError::HardenedIndex(index));
    }
    Ok(index)
}
