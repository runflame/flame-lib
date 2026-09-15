//! Constants for `m/35263'/network'/account'/change/n`.
//!
//! These are raw component values, without the hardened bit. Combine the
//! purpose, network, and account values with [`crate::HARDENED`]; use branch
//! and address indices directly. Keys do not store or enforce this convention.
//!
//! ```
//! use flamekd::{util, Network, SpendKey, HARDENED};
//!
//! let root = SpendKey::from_seed(&[0u8; 64])?; // Public test seed only.
//! let account = root
//!     .derive_child(HARDENED | util::PURPOSE)?
//!     .derive_child(HARDENED | util::TESTNET)?
//!     .derive_child(HARDENED)?; // Account 0.
//! let address = account.to_recv()
//!     .derive_child(util::RECEIVING)?
//!     .derive_child(0)?
//!     .to_address();
//! assert!(address.to_bech32(Network::Testnet).starts_with("tf1"));
//! # Ok::<(), flamekd::Error>(())
//! ```

/// Flame's private purpose identifier: `FLAME` on a telephone keypad.
/// Raw value; combine with [`crate::HARDENED`] when deriving.
pub const PURPOSE: u32 = 35_263;

/// Raw mainnet network number; combine with [`crate::HARDENED`] when deriving.
pub const MAINNET: u32 = 0;

/// Raw testnet network number; combine with [`crate::HARDENED`] when deriving.
pub const TESTNET: u32 = 1;

/// Receiving branch number, used with normal derivation.
pub const RECEIVING: u32 = 0;

/// Change branch number, used with normal derivation.
pub const CHANGE: u32 = 1;
