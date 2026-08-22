mod codable;
mod reader;
mod writer;

pub use codable::{Codable, Decodable, Encodable, ExactSizeEncodable};
pub use reader::{ReadError, Reader};
pub use writer::{SizeWriter, WriteError, Writer};

// `merlin_support` only provides `impl Writer for Transcript` — no
// items to re-export. Declaring the module is enough to bring the
// impl into scope when the `merlin` feature is enabled.
#[cfg(feature = "merlin")]
mod merlin_support;

#[cfg(feature = "bytes")]
mod bytes_support;
#[cfg(feature = "bytes")]
pub use bytes_support::*;
