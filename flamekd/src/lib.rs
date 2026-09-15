#![deny(missing_docs)]
//! Flame key derivation and Bech32f key/address encoding.
//!
//! ```
//! use flamekd::{Mnemonic, Network, RecvKey, SpendKey, HARDENED};
//!
//! let mnemonic = Mnemonic::from_entropy(&[0u8; 32])?; // Public test entropy only.
//! let root = SpendKey::from_mnemonic(&mnemonic, "")?;
//! let account = root.derive_child(HARDENED)?;
//! let recv: RecvKey = account.to_recv().to_string().parse()?;
//! let address = recv.derive_child(0)?.to_address();
//! assert_eq!(address, account.derive_child(0)?.to_recv().to_address());
//! let testnet = recv.to_bech32(Network::Testnet);
//! assert_eq!(RecvKey::from_bech32(&testnet, Network::Testnet)?, recv);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Extended keys redact `Debug` and erase their stored secrets on drop. `Display`
//! and `to_bytes` deliberately export key material; callers must protect those
//! copies. Normal child scalar disclosure together with an ancestor receiving
//! key can reveal the corresponding ancestor scalar. See `flamekd.md`.
//!
//! `Display` and `FromStr` use mainnet. Use `to_bech32` and `from_bech32` to
//! select a network explicitly. Network selection changes only the encoding.
//! Standard wallet path constants live separately in [`wallet`].

use std::{fmt, str::FromStr};

use curve25519_dalek::{
    constants::RISTRETTO_BASEPOINT_POINT as G,
    ristretto::{CompressedRistretto, RistrettoPoint},
    scalar::Scalar,
    traits::IsIdentity,
};
use merlin::Transcript;
use zeroize::{ZeroizeOnDrop, Zeroizing};

/// BIP39 mnemonic parsing, validation, and seed conversion.
pub use bip39::{Language, Mnemonic};

mod encoding;

pub mod util;

#[cfg(test)]
mod tests;

/// The first hardened index; the low 31 bits select a child within either mode.
pub const HARDENED: u32 = 1 << 31;

/// Selects the network HRP for textual encoding; key material is network-neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// Mainnet HRPs: `spend`, `view`, `recv`, `f`, and `c`.
    Mainnet,
    /// Testnet HRPs: `testspend`, `testview`, `testrecv`, `tf`, and `tc`.
    Testnet,
}

/// Invalid key material, encoding, or derivation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A key or address has the wrong byte or string length.
    #[error("invalid key or address length")]
    InvalidLength,
    /// A scalar is not a canonical little-endian scalar modulo the group order.
    #[error("noncanonical scalar")]
    InvalidScalar,
    /// A point is not a valid compressed Ristretto point.
    #[error("invalid Ristretto point")]
    InvalidPoint,
    /// A spending/viewing scalar is zero, or its public point is the identity.
    #[error("zero spending or viewing key")]
    ZeroKey,
    /// Only an extended spending key can derive a hardened child.
    #[error("hardened derivation requires an extended spending key")]
    HardenedDerivation,
    /// The HRP/network, checksum, case, alphabet, or bit padding is invalid.
    #[error("invalid Bech32f encoding")]
    InvalidEncoding,
    /// The mnemonic does not have a valid BIP39 checksum.
    #[error("invalid BIP39 mnemonic")]
    InvalidMnemonic,
}

/// Extended spending key `(s, v, t)`, with HRP `spend` or `testspend`.
#[derive(Clone, PartialEq, Eq, ZeroizeOnDrop)]
pub struct SpendKey {
    s: Scalar,
    v: Scalar,
    t: Scalar,
}

/// Extended viewing key `(S, v, t)`, with HRP `view` or `testview`.
#[derive(Clone, PartialEq, Eq, ZeroizeOnDrop)]
pub struct ViewKey {
    #[zeroize(skip)]
    spending: RistrettoPoint,
    v: Scalar,
    t: Scalar,
}

/// Extended receiving key `(S, V, t)`, with HRP `recv` or `testrecv`.
///
/// Generates normal descendant addresses and links their activity, without
/// decrypting or spending. The derivation secret still requires privacy.
#[derive(Clone, PartialEq, Eq, ZeroizeOnDrop)]
pub struct RecvKey {
    #[zeroize(skip)]
    address: ReceivingAddress,
    t: Scalar,
}

/// Receiving address `(S, V)`, with HRP `f` or `tf`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceivingAddress {
    spending: RistrettoPoint,
    viewing: RistrettoPoint,
}

/// One clear-payment/tracking address `(S)`, with HRP `c` or `tc`.
///
/// This address does not identify or derive other addresses in the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackingAddress {
    spending: RistrettoPoint,
}

impl SpendKey {
    /// Derives a root from the raw 64-byte BIP39 seed, without a BIP32 HMAC.
    /// Returns an error if a resulting spending or viewing scalar is zero.
    pub fn from_seed(seed: &[u8; 64]) -> Result<Self, Error> {
        let mut transcript = Transcript::new(b"FlameKD.from_seed");
        transcript.append_message(b"seed", seed);
        Self::from_parts(
            challenge_scalar(&mut transcript, b"s"),
            challenge_scalar(&mut transcript, b"v"),
            challenge_scalar(&mut transcript, b"t"),
        )
    }

    /// Validates a BIP39 mnemonic and derives a root with the given passphrase.
    /// Pass `""` when no passphrase is used. `Mnemonic::to_seed` exposes the same
    /// intermediate seed used by BIP32; all standard BIP39 languages are enabled.
    pub fn from_mnemonic(mnemonic: &Mnemonic, passphrase: &str) -> Result<Self, Error> {
        let sentence = Zeroizing::new(mnemonic.to_string());
        let mnemonic = Mnemonic::parse_in_normalized(mnemonic.language(), &sentence)
            .map_err(|_| Error::InvalidMnemonic)?;
        let seed = Zeroizing::new(mnemonic.to_seed(passphrase));
        Self::from_seed(&seed)
    }

    /// Removes spending authority while retaining viewing and derivation.
    pub fn to_view(&self) -> ViewKey {
        ViewKey {
            spending: self.s * G,
            v: self.v,
            t: self.t,
        }
    }

    /// Removes spending and viewing authority, retaining address derivation.
    pub fn to_recv(&self) -> RecvKey {
        self.to_view().to_recv()
    }

    /// Returns the spending scalar for this node, not a per-output disclosure key.
    pub fn spending_key(&self) -> &Scalar {
        &self.s
    }

    /// Returns the viewing scalar for this node, not a per-output disclosure key.
    pub fn viewing_key(&self) -> &Scalar {
        &self.v
    }

    /// Derives a normal or hardened child. Failure never advances the index.
    pub fn derive_child(&self, index: u32) -> Result<Self, Error> {
        let (ds, dv, t) = if index >= HARDENED {
            let mut transcript = Transcript::new(b"FlameKD.hardened");
            transcript.append_message(b"s", self.s.as_bytes());
            transcript.append_message(b"v", self.v.as_bytes());
            finish_derivation(transcript, &self.t, index)
        } else {
            normal_derivation(&self.to_recv(), index)?
        };
        Self::from_parts(self.s + ds, self.v + dv, t)
    }

    /// Serializes `(s, v, t)` as three canonical 32-byte scalars.
    pub fn to_bytes(&self) -> [u8; 96] {
        join([self.s.to_bytes(), self.v.to_bytes(), self.t.to_bytes()])
    }

    /// Parses exactly 96 bytes, rejecting noncanonical scalars and zero keys.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let bytes: &[u8; 96] = bytes.try_into().map_err(|_| Error::InvalidLength)?;
        Self::from_parts(
            scalar(&bytes[..32])?,
            scalar(&bytes[32..64])?,
            scalar(&bytes[64..])?,
        )
    }

    fn from_parts(s: Scalar, v: Scalar, t: Scalar) -> Result<Self, Error> {
        if s == Scalar::ZERO || v == Scalar::ZERO {
            return Err(Error::ZeroKey);
        }
        Ok(Self { s, v, t })
    }
}

impl ViewKey {
    /// Removes viewing authority, retaining address derivation and tracking.
    pub fn to_recv(&self) -> RecvKey {
        RecvKey {
            address: ReceivingAddress {
                spending: self.spending,
                viewing: self.v * G,
            },
            t: self.t,
        }
    }

    /// Returns the viewing scalar for this node, not a per-output disclosure key.
    pub fn viewing_key(&self) -> &Scalar {
        &self.v
    }

    /// Derives a normal child; hardened indices fail without advancing the index.
    pub fn derive_child(&self, index: u32) -> Result<Self, Error> {
        let (ds, dv, t) = normal_derivation(&self.to_recv(), index)?;
        Self::from_parts(self.spending + ds * G, self.v + dv, t)
    }

    /// Serializes `(S, v, t)` in that order as three 32-byte elements.
    pub fn to_bytes(&self) -> [u8; 96] {
        join([
            self.spending.compress().to_bytes(),
            self.v.to_bytes(),
            self.t.to_bytes(),
        ])
    }

    /// Parses exactly 96 bytes, validating the point and canonical scalars.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let bytes: &[u8; 96] = bytes.try_into().map_err(|_| Error::InvalidLength)?;
        Self::from_parts(
            point(&bytes[..32])?,
            scalar(&bytes[32..64])?,
            scalar(&bytes[64..])?,
        )
    }

    fn from_parts(spending: RistrettoPoint, v: Scalar, t: Scalar) -> Result<Self, Error> {
        if spending.is_identity() || v == Scalar::ZERO {
            return Err(Error::ZeroKey);
        }
        Ok(Self { spending, v, t })
    }
}

impl RecvKey {
    /// Drops the derivation secret to expose only this node's receiving address.
    pub fn to_address(&self) -> ReceivingAddress {
        self.address
    }

    /// Derives a normal child; hardened indices fail without advancing the index.
    pub fn derive_child(&self, index: u32) -> Result<Self, Error> {
        let (ds, dv, t) = normal_derivation(self, index)?;
        Ok(Self {
            address: ReceivingAddress::from_parts(
                self.address.spending + ds * G,
                self.address.viewing + dv * G,
            )?,
            t,
        })
    }

    /// Serializes `(S, V, t)` in that order as three 32-byte elements.
    pub fn to_bytes(&self) -> [u8; 96] {
        join([
            self.address.spending.compress().to_bytes(),
            self.address.viewing.compress().to_bytes(),
            self.t.to_bytes(),
        ])
    }

    /// Parses exactly 96 bytes, validating both points and the derivation scalar.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let bytes: &[u8; 96] = bytes.try_into().map_err(|_| Error::InvalidLength)?;
        Ok(Self {
            address: ReceivingAddress::from_bytes(&bytes[..64])?,
            t: scalar(&bytes[64..])?,
        })
    }
}

impl ReceivingAddress {
    /// Drops the viewing public key to expose this node's clear-payment address.
    pub fn to_tracking_address(&self) -> TrackingAddress {
        TrackingAddress {
            spending: self.spending,
        }
    }

    /// Returns the public key that authorizes spending from this address.
    pub fn spending_key(&self) -> &RistrettoPoint {
        &self.spending
    }

    /// Returns the public key used to encrypt output contents to this address.
    pub fn viewing_key(&self) -> &RistrettoPoint {
        &self.viewing
    }

    /// Serializes `(S, V)` as two canonical compressed Ristretto points.
    pub fn to_bytes(&self) -> [u8; 64] {
        join([
            self.spending.compress().to_bytes(),
            self.viewing.compress().to_bytes(),
        ])
    }

    /// Parses exactly 64 bytes, rejecting invalid or identity points.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let bytes: &[u8; 64] = bytes.try_into().map_err(|_| Error::InvalidLength)?;
        Self::from_parts(point(&bytes[..32])?, point(&bytes[32..])?)
    }

    fn from_parts(spending: RistrettoPoint, viewing: RistrettoPoint) -> Result<Self, Error> {
        if spending.is_identity() || viewing.is_identity() {
            return Err(Error::ZeroKey);
        }
        Ok(Self { spending, viewing })
    }
}

impl TrackingAddress {
    /// Returns the public key that authorizes spending from this address.
    pub fn spending_key(&self) -> &RistrettoPoint {
        &self.spending
    }

    /// Serializes this address as one canonical compressed Ristretto point.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.spending.compress().to_bytes()
    }

    /// Parses exactly 32 bytes, rejecting invalid or identity points.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 32 {
            return Err(Error::InvalidLength);
        }
        let spending = point(bytes)?;
        if spending.is_identity() {
            return Err(Error::ZeroKey);
        }
        Ok(Self { spending })
    }
}

fn scalar(bytes: &[u8]) -> Result<Scalar, Error> {
    let bytes = bytes.try_into().map_err(|_| Error::InvalidLength)?;
    Option::from(Scalar::from_canonical_bytes(bytes)).ok_or(Error::InvalidScalar)
}

fn point(bytes: &[u8]) -> Result<RistrettoPoint, Error> {
    CompressedRistretto(bytes.try_into().map_err(|_| Error::InvalidLength)?)
        .decompress()
        .ok_or(Error::InvalidPoint)
}

fn challenge_scalar(transcript: &mut Transcript, label: &'static [u8]) -> Scalar {
    let mut bytes = Zeroizing::new([0u8; 64]);
    transcript.challenge_bytes(label, bytes.as_mut());
    Scalar::from_bytes_mod_order_wide(&bytes)
}

fn normal_derivation(key: &RecvKey, index: u32) -> Result<(Scalar, Scalar, Scalar), Error> {
    if index >= HARDENED {
        return Err(Error::HardenedDerivation);
    }
    let mut transcript = Transcript::new(b"FlameKD.derivation");
    transcript.append_message(b"S", key.address.spending.compress().as_bytes());
    transcript.append_message(b"V", key.address.viewing.compress().as_bytes());
    Ok(finish_derivation(transcript, &key.t, index))
}

fn finish_derivation(
    mut transcript: Transcript,
    t: &Scalar,
    index: u32,
) -> (Scalar, Scalar, Scalar) {
    transcript.append_message(b"t", t.as_bytes());
    transcript.append_message(b"i", &index.to_le_bytes());
    (
        challenge_scalar(&mut transcript, b"ds"),
        challenge_scalar(&mut transcript, b"dv"),
        challenge_scalar(&mut transcript, b"t"),
    )
}

fn join<const N: usize, const M: usize>(parts: [[u8; 32]; M]) -> [u8; N] {
    assert_eq!(N, 32 * M);
    let parts = Zeroizing::new(parts);
    let mut bytes = [0u8; N];
    for (chunk, part) in bytes.chunks_exact_mut(32).zip(parts.iter()) {
        chunk.copy_from_slice(part);
    }
    bytes
}

macro_rules! bech32f {
    ($type:ty, $mainnet:literal, $testnet:literal, $len:literal) => {
        impl $type {
            /// Encodes for the selected network, exporting any secret key material.
            pub fn to_bech32(&self, network: Network) -> String {
                let hrp = match network {
                    Network::Mainnet => $mainnet,
                    Network::Testnet => $testnet,
                };
                let bytes = Zeroizing::new(self.to_bytes());
                encoding::encode(hrp, bytes.as_ref())
            }

            /// Parses for the expected network, rejecting any other type or network.
            pub fn from_bech32(text: &str, network: Network) -> Result<Self, Error> {
                let hrp = match network {
                    Network::Mainnet => $mainnet,
                    Network::Testnet => $testnet,
                };
                let bytes = Zeroizing::new(encoding::decode::<$len>(hrp, text)?);
                Self::from_bytes(bytes.as_ref())
            }
        }
        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let encoded = Zeroizing::new(self.to_bech32(Network::Mainnet));
                f.write_str(&encoded)
            }
        }
        impl FromStr for $type {
            type Err = Error;
            fn from_str(text: &str) -> Result<Self, Self::Err> {
                Self::from_bech32(text, Network::Mainnet)
            }
        }
    };
}

bech32f!(SpendKey, "spend", "testspend", 96);
bech32f!(ViewKey, "view", "testview", 96);
bech32f!(RecvKey, "recv", "testrecv", 96);
bech32f!(ReceivingAddress, "f", "tf", 64);
bech32f!(TrackingAddress, "c", "tc", 32);

macro_rules! redacted_debug {
    ($($type:ty),+) => { $(
        impl fmt::Debug for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($type), "([REDACTED])"))
            }
        }
    )+ };
}

redacted_debug!(SpendKey, ViewKey, RecvKey);
