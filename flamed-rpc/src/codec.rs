//! How a value looks on the wire, in one place.
//!
//! A 32-byte value is a 64-character lowercase hex string; a byte blob is
//! standard base64. Both encodings are serde `with` modules, so the same code
//! serves a newtype's inner field and a plain struct field. `flamed` writes
//! `genesis.json` through these very modules, which is what keeps a predicate
//! copied between that file and an RPC reply byte for byte the same.

/// `[u8; 32]` as a 64-character lowercase hex string.
pub mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Writes 32 bytes as lowercase hex.
    pub fn serialize<S>(value: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&::hex::encode(value))
    }

    /// Reads 32 bytes from a 64-character hex string.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 32], D::Error>
    where
        D: Deserializer<'de>,
    {
        // `String`, never `&str`: an internally tagged enum deserializes
        // through serde's `ContentDeserializer`, which cannot hand out a
        // borrowed `&str` from owned buffered content.
        let text = String::deserialize(deserializer)?;
        let bytes = ::hex::decode(&text)
            .map_err(|error| serde::de::Error::custom(format_args!("invalid hex: {error}")))?;
        bytes.try_into().map_err(|bytes: Vec<u8>| {
            serde::de::Error::invalid_length(bytes.len(), &"32 bytes, as 64 hex characters")
        })
    }
}

/// A byte blob as standard base64, with padding.
pub mod base64 {
    use ::base64::engine::general_purpose::STANDARD;
    use ::base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    /// Writes bytes as standard base64.
    pub fn serialize<T, S>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        T: AsRef<[u8]> + ?Sized,
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(value.as_ref()))
    }

    /// Reads bytes from a standard base64 string.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        STANDARD
            .decode(text.as_bytes())
            .map_err(|error| serde::de::Error::custom(format_args!("invalid base64: {error}")))
    }
}

/// Stamps out a 32-byte id newtype that rides the wire as lowercase hex.
///
/// `#[serde(transparent)]` alone would emit a JSON array of 32 numbers; with
/// the field's `with` module it emits the bare string. The module is named by
/// an absolute path rather than `$crate`, because `$crate` is not substituted
/// inside a string literal — which is why these macros stay crate-private.
macro_rules! hex32_newtype {
    ($(
        $(#[$attr:meta])*
        $name:ident;
    )*) => {$(
        $(#[$attr])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord,
            ::serde::Serialize, ::serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(#[serde(with = "crate::codec::hex32")] pub [u8; 32]);

        impl ::core::convert::From<[u8; 32]> for $name {
            fn from(bytes: [u8; 32]) -> Self {
                $name(bytes)
            }
        }

        impl ::core::convert::From<$name> for [u8; 32] {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                formatter.write_str(&::hex::encode(self.0))
            }
        }
    )*};
}

/// Stamps out a byte-blob newtype that rides the wire as standard base64.
macro_rules! base64_newtype {
    ($(
        $(#[$attr:meta])*
        $name:ident;
    )*) => {$(
        $(#[$attr])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(#[serde(with = "crate::codec::base64")] pub Vec<u8>);

        impl ::core::convert::From<Vec<u8>> for $name {
            fn from(bytes: Vec<u8>) -> Self {
                $name(bytes)
            }
        }

        impl ::core::convert::From<$name> for Vec<u8> {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl ::core::convert::AsRef<[u8]> for $name {
            fn as_ref(&self) -> &[u8] {
                &self.0
            }
        }
    )*};
}

pub(crate) use base64_newtype;
pub(crate) use hex32_newtype;
