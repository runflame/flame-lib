//! Accounts: one flamekd seed, the addresses and keys below it.
//!
//! An [`Account`] is the node at `m/35263'/network'/0'`. Everything the
//! wallet hands out lives under it on the two normal branches — receiving
//! (`util::RECEIVING`) and change (`util::CHANGE`) — so an address is
//! `m/35263'/network'/0'/branch/n`. The path constants come from
//! [`flamekd::util`] and are never copied here.

use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar as DalekScalar;
use flamekd::{util, Network, ReceivingAddress, RecvKey, SpendKey, HARDENED};
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

/// One account: the spending key at `m/35263'/network'/0'`, the network its
/// addresses are encoded for, and the next unissued receiving index.
#[derive(Clone)]
pub struct Account {
    account: SpendKey,
    network: Network,
    next_index: u32,
}

impl Account {
    /// The node at `m/35263'/network'/0'` from a 64-byte seed.
    pub fn from_seed(seed: &[u8; 64], network: Network) -> Result<Account, KeyError> {
        let network_index = match network {
            Network::Mainnet => util::MAINNET,
            Network::Testnet => util::TESTNET,
        };
        let account = SpendKey::from_seed(seed)?
            .derive_child(HARDENED | util::PURPOSE)?
            .derive_child(HARDENED | network_index)?
            .derive_child(HARDENED)?;
        Ok(Account {
            account,
            network,
            next_index: 0,
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

    /// `s_n`, the spending scalar at `m/…/branch/n`, by value: the
    /// [`SpendKey`] it came from zeroizes on drop.
    pub fn spending_key_at(&self, branch: u32, n: u32) -> Result<DalekScalar, KeyError> {
        Ok(*self.spend_key_at(branch, n)?.spending_key())
    }

    /// `v_n`, the viewing scalar at `m/…/branch/n`, by value.
    pub fn viewing_key_at(&self, branch: u32, n: u32) -> Result<DalekScalar, KeyError> {
        Ok(*self.spend_key_at(branch, n)?.viewing_key())
    }

    /// The account-level receiving key: what an indexer is given. It
    /// generates every address below the account and links their activity,
    /// and can neither spend nor read a payment's private contents.
    pub fn recv_key(&self) -> RecvKey {
        self.account.to_recv()
    }

    /// The receiving key for one branch, `m/…/branch`.
    pub fn recv_key_for(&self, branch: u32) -> Result<RecvKey, KeyError> {
        Ok(self.recv_key().derive_child(normal(branch)?)?)
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

    /// The spending key at `m/…/branch/n`. Private, so no extended key
    /// escapes the module: the [`SpendKey`] this returns is dropped — and
    /// zeroized — inside the accessors below. The scalar those accessors
    /// hand out is itself key material and is not zeroized; protecting the
    /// copy is the caller's job.
    fn spend_key_at(&self, branch: u32, n: u32) -> Result<SpendKey, KeyError> {
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
