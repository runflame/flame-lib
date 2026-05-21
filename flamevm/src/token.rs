//! Token value types.
//!
//! All three variants are **linear**: never copyable, droppable only when
//! the quantity is zero (per spec.md §Stack operations `drop`). Quantity
//! and flavor representations differ between variants:
//!
//! - [`Token`]      — encrypted quantity & flavor (Pedersen commitments);
//!                    proven non-negative via range-proof. Portable.
//! - [`ClearToken`] — cleartext quantity & flavor (`Int253`s); may be
//!                    negative; portable only when non-negative.
//! - [`WideToken`]  — encrypted quantity & flavor; may be negative; not
//!                    portable. Intermediate type during cloak/mix.
//!
//! For Phase 1 only `ClearToken` carries fields, used by `pushtoken` /
//! `drop`. The encrypted variants gain their `Point` commitments in
//! later phases (8, 13) when issuance and mix are wired up.

use crate::Int253;

pub struct Token {}

pub struct WideToken {}

pub struct ClearToken {
    pub(crate) qty: Int253,
    pub(crate) flv: Int253,
}

impl ClearToken {
    pub fn new(qty: Int253, flv: Int253) -> Self {
        ClearToken { qty, flv }
    }

    pub fn qty(&self) -> Int253 {
        self.qty
    }

    pub fn flv(&self) -> Int253 {
        self.flv
    }

    /// A cleartoken is droppable iff its quantity is zero
    /// (per spec.md `drop` "including ... zero-tokens").
    pub fn is_zero_qty(&self) -> bool {
        self.qty.is_zero()
    }
}
