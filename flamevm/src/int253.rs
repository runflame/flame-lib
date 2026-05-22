//! Signed sign-magnitude integer with magnitude bounded by the
//! Ristretto group order ℓ ≈ 2²⁵². The name `Int253` reflects the
//! effective conceptual width — ⌈log₂(ℓ)⌉ = 253 bits of magnitude
//! plus an explicit sign bit — even though the on-the-wire encoding
//! occupies 32 bytes.
//!
//! ## Representation
//!
//! Bit 255 (the high bit of `bytes[31]`) is the sign; the lower 255 bits
//! are a canonical Ristretto scalar magnitude (strictly less than ℓ).
//! Negative zero is not representable — every constructor either rejects
//! it (`from_bytes`) or normalizes it to positive zero (`from_parts`,
//! arithmetic ops, `Neg`).
//!
//! ## Arithmetic wraps modulo ℓ
//!
//! `Add` / `Sub` / `Mul` operate on magnitudes via `Scalar` arithmetic,
//! which is mod ℓ. Small inputs whose results stay below ℓ behave like
//! ordinary signed integers; results that would exceed ℓ wrap. Callers
//! that need overflow detection should bound their inputs themselves;
//! no `checked_*` variants are provided.
//!
//! ## Not constant-time
//!
//! Sign branching, ordering, and magnitude comparison are data-dependent.
//! `Int253` is intended for public VM stack values. Secret witness data
//! should flow through constraint-system types (`Variable`, `Expression`),
//! not through `Int253`.

use core::cmp::Ordering;
use core::ops::{Add, AddAssign, Mul, MulAssign, Neg, Sub, SubAssign};

use curve25519_dalek::scalar::Scalar;
use spacesuit::SignedInteger;

/// Signed sign-magnitude integer; magnitude is a canonical Ristretto
/// scalar (< ℓ ≈ 2²⁵²). See module docs for the representation,
/// wraparound semantics, and the non-constant-time disclaimer.
#[derive(Copy, Clone)]
pub struct Int253 {
    bytes: [u8; 32],
}

impl Int253 {
    /// Returns the raw 32-byte representation (sign bit included).
    pub fn to_bytes(self) -> [u8; 32] {
        self.bytes
    }

    /// Returns the raw 32-byte representation (sign bit included).
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }

    /// Returns `true` if the sign bit is set.
    pub fn is_negative(&self) -> bool {
        self.bytes[31] & 0x80 != 0
    }

    /// Returns the sign bit and the magnitude as a `Scalar`.
    pub fn to_parts(self) -> (bool, Scalar) {
        (self.is_negative(), self.abs_scalar())
    }

    /// Returns the value as a non-negative `u64` if it fits; otherwise `None`.
    /// Negative values always return `None`.
    pub fn to_u64(&self) -> Option<u64> {
        if self.is_negative() {
            return None;
        }
        if self.bytes[8..].iter().any(|&b| b != 0) {
            return None;
        }
        let mut lo = [0u8; 8];
        lo.copy_from_slice(&self.bytes[..8]);
        Some(u64::from_le_bytes(lo))
    }

    /// Returns the absolute value as a `Scalar`.
    pub fn abs_scalar(&self) -> Scalar {
        let mut abs_bytes = self.bytes;
        abs_bytes[31] &= 0x7f;
        // The lower 255 bits are canonical by the `Int253` invariant
        // (established in `from_bytes` and preserved everywhere else).
        Scalar::from_canonical_bytes(abs_bytes)
            .expect("Int253 invariant: lower 255 bits are a canonical scalar")
    }

    /// Converts to a Ristretto scalar mod ℓ.
    ///
    /// This is a many-to-one mapping: non-negative `Int253(X)` maps
    /// to `Scalar(X)`; negative `Int253(-X)` maps to `Scalar(ℓ − X)`.
    /// Round-tripping through `From<Scalar>` recovers only the
    /// non-negative representative — sign information is lost.
    pub fn to_scalar_mod_order(&self) -> Scalar {
        let abs = self.abs_scalar();
        if self.is_negative() {
            -abs
        } else {
            abs
        }
    }

    /// Decodes from 32 bytes where bit 255 is the sign bit
    /// and the lower 255 bits are a canonical scalar.
    /// Returns `None` if the absolute value is not a canonical scalar
    /// or if negative zero is encountered.
    pub fn from_bytes(bytes: [u8; 32]) -> Option<Int253> {
        let negative = bytes[31] & 0x80 != 0;
        let mut scalar_bytes = bytes;
        scalar_bytes[31] &= 0x7f;
        // Verify canonical encoding.
        Scalar::from_canonical_bytes(scalar_bytes)?;
        if negative && scalar_bytes == [0u8; 32] {
            return None; // reject negative zero
        }
        Some(Int253 { bytes })
    }

    /// Creates an `Int253` from an absolute-value scalar and a sign flag.
    /// Negative zero is normalized to positive zero.
    pub fn from_parts(sign: bool, scalar: Scalar) -> Int253 {
        let mut bytes = scalar.to_bytes();
        if sign && scalar != Scalar::zero() {
            bytes[31] |= 0x80;
        }
        Int253 { bytes }
    }

    /// Returns the absolute value as an `Int253` (always non-negative).
    pub fn abs(&self) -> Int253 {
        let mut abs_bytes = self.bytes;
        abs_bytes[31] &= 0x7f;
        Int253 { bytes: abs_bytes }
    }

    /// Returns the additive identity.
    pub fn zero() -> Int253 {
        Int253 { bytes: [0u8; 32] }
    }

    /// Returns the multiplicative identity.
    pub fn one() -> Int253 {
        Int253::from(1u64)
    }

    /// Returns `true` if the value is zero.
    pub fn is_zero(&self) -> bool {
        let mut bytes = self.bytes;
        bytes[31] &= 0x7f;
        bytes == [0u8; 32]
    }

    /// Truncated division and remainder.
    ///
    /// Returns `Some((q, r))` such that `self = q * other + r`, with
    /// `sign(q) = sign(self) XOR sign(other)` and `sign(r) = sign(self)`
    /// (the convention Rust's i64 division uses). Sign on zero results
    /// is normalized to positive.
    ///
    /// Returns `None` iff `other` is zero.
    pub fn div_rem(self, other: Int253) -> Option<(Int253, Int253)> {
        if other.is_zero() {
            return None;
        }
        let n = bytes_to_limbs(self.abs_scalar().to_bytes());
        let d = bytes_to_limbs(other.abs_scalar().to_bytes());
        let (q_limbs, r_limbs) = divmod_u256(n, d);
        let q_scalar = Scalar::from_canonical_bytes(limbs_to_bytes(q_limbs))
            .expect("Int253 invariant: quotient magnitude < ℓ");
        let r_scalar = Scalar::from_canonical_bytes(limbs_to_bytes(r_limbs))
            .expect("Int253 invariant: remainder magnitude < ℓ");
        let q_sign = self.is_negative() ^ other.is_negative();
        let r_sign = self.is_negative();
        Some((
            Int253::from_parts(q_sign, q_scalar),
            Int253::from_parts(r_sign, r_scalar),
        ))
    }

    /// Compares two `Int253`s by magnitude (ignoring sign).
    pub(crate) fn cmp_magnitude(&self, other: &Int253) -> Ordering {
        let mut a = self.bytes;
        let mut b = other.bytes;
        a[31] &= 0x7f;
        b[31] &= 0x7f;
        // Bytes are little-endian: walk from most-significant to least.
        for i in (0..32).rev() {
            match a[i].cmp(&b[i]) {
                Ordering::Equal => continue,
                ord => return ord,
            }
        }
        Ordering::Equal
    }
}

// ── Arithmetic ─────────────────────────────────────────────────────
//
// Magnitudes wrap modulo the Ristretto group order ℓ via `Scalar`
// arithmetic. Sign rules:
//   * `-a`        flips the sign; zero stays positive.
//   * same-sign add: keep the sign, magnitudes add (may wrap).
//   * mixed-sign add: sign of the larger magnitude operand; cannot wrap.
//   * multiplication: sign = XOR of input signs; magnitudes multiply
//     (may wrap). A zero result always normalizes to positive.

impl Neg for Int253 {
    type Output = Int253;
    fn neg(self) -> Int253 {
        if self.is_zero() {
            return self;
        }
        let mut bytes = self.bytes;
        bytes[31] ^= 0x80;
        Int253 { bytes }
    }
}

impl Add for Int253 {
    type Output = Int253;
    fn add(self, other: Int253) -> Int253 {
        match (self.is_negative(), other.is_negative()) {
            (false, false) | (true, true) => Int253::from_parts(
                self.is_negative(),
                self.abs_scalar() + other.abs_scalar(),
            ),
            _ => {
                let (sign, abs) = match self.cmp_magnitude(&other) {
                    Ordering::Less => (
                        other.is_negative(),
                        other.abs_scalar() - self.abs_scalar(),
                    ),
                    _ => (
                        self.is_negative(),
                        self.abs_scalar() - other.abs_scalar(),
                    ),
                };
                Int253::from_parts(sign, abs)
            }
        }
    }
}

impl Sub for Int253 {
    type Output = Int253;
    fn sub(self, other: Int253) -> Int253 {
        self + (-other)
    }
}

impl Mul for Int253 {
    type Output = Int253;
    fn mul(self, other: Int253) -> Int253 {
        Int253::from_parts(
            self.is_negative() ^ other.is_negative(),
            self.abs_scalar() * other.abs_scalar(),
        )
    }
}

impl AddAssign for Int253 {
    fn add_assign(&mut self, other: Int253) { *self = *self + other; }
}
impl SubAssign for Int253 {
    fn sub_assign(&mut self, other: Int253) { *self = *self - other; }
}
impl MulAssign for Int253 {
    fn mul_assign(&mut self, other: Int253) { *self = *self * other; }
}

impl Ord for Int253 {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_negative(), other.is_negative()) {
            (false, false) => self.cmp_magnitude(other),
            (true, true) => other.cmp_magnitude(self),
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
        }
    }
}

impl PartialOrd for Int253 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl core::fmt::Debug for Int253 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_negative() {
            write!(f, "Int253(-{:?})", self.abs_scalar())
        } else {
            write!(f, "Int253({:?})", self.abs_scalar())
        }
    }
}

impl From<u64> for Int253 {
    fn from(v: u64) -> Self {
        Int253 {
            bytes: Scalar::from(v).to_bytes(),
        }
    }
}

impl From<i64> for Int253 {
    fn from(v: i64) -> Self {
        if v < 0 {
            // `unsigned_abs` handles i64::MIN correctly (its magnitude is 2^63,
            // which doesn't fit in i64 but does fit in u64). Plain `-v` would
            // overflow in debug builds.
            let mut bytes = Scalar::from(v.unsigned_abs()).to_bytes();
            bytes[31] |= 0x80;
            Int253 { bytes }
        } else {
            Int253 {
                bytes: Scalar::from(v as u64).to_bytes(),
            }
        }
    }
}

impl From<Scalar> for Int253 {
    fn from(s: Scalar) -> Self {
        Int253 {
            bytes: s.to_bytes(),
        }
    }
}

impl From<SignedInteger> for Int253 {
    fn from(si: SignedInteger) -> Self {
        match si.to_u64() {
            Some(v) => Int253::from(v),
            None => {
                let neg = -si;
                let abs_val = neg
                    .to_u64()
                    .expect("negation of negative SignedInteger is non-negative");
                let mut bytes = Scalar::from(abs_val).to_bytes();
                bytes[31] |= 0x80;
                Int253 { bytes }
            }
        }
    }
}

impl Into<Scalar> for Int253 {
    fn into(self) -> Scalar {
        self.to_scalar_mod_order()
    }
}

impl PartialEq for Int253 {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for Int253 {}

// ── Unsigned 256-bit divmod (private helpers for `div_rem`) ────────
//
// Both operands fed in are canonical Ristretto-scalar magnitudes
// (< ℓ < 2²⁵³), so 4 u64 LE limbs suffice. Not constant-time, matching
// `Int253`'s module-level disclaimer.

fn bytes_to_limbs(bytes: [u8; 32]) -> [u64; 4] {
    let mut limbs = [0u64; 4];
    for (i, chunk) in bytes.chunks_exact(8).enumerate() {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(chunk);
        limbs[i] = u64::from_le_bytes(buf);
    }
    limbs
}

fn limbs_to_bytes(limbs: [u64; 4]) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    for (i, &limb) in limbs.iter().enumerate() {
        bytes[i * 8..(i + 1) * 8].copy_from_slice(&limb.to_le_bytes());
    }
    bytes
}

fn ge_u256(a: &[u64; 4], b: &[u64; 4]) -> bool {
    for i in (0..4).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

fn sub_in_place_u256(a: &mut [u64; 4], b: &[u64; 4]) {
    let mut borrow: u64 = 0;
    for i in 0..4 {
        let (t1, b1) = a[i].overflowing_sub(b[i]);
        let (t2, b2) = t1.overflowing_sub(borrow);
        a[i] = t2;
        // b1 && b2 is unreachable: b1 implies t1 >= 1, so t1.sub(1) cannot
        // underflow. OR is therefore equivalent to addition.
        borrow = (b1 as u64) | (b2 as u64);
    }
}

/// Unsigned 256-bit divmod by shift-and-subtract long division.
/// Precondition: `d != [0; 4]`.
fn divmod_u256(n: [u64; 4], d: [u64; 4]) -> ([u64; 4], [u64; 4]) {
    let mut q = [0u64; 4];
    let mut r = [0u64; 4];
    for i in (0..256).rev() {
        // r <<= 1
        let mut carry = 0u64;
        for limb in &mut r {
            let next = *limb >> 63;
            *limb = (*limb << 1) | carry;
            carry = next;
        }
        // r |= bit i of n
        r[0] |= (n[i >> 6] >> (i & 63)) & 1;
        // if r >= d: r -= d; set bit i of q
        if ge_u256(&r, &d) {
            sub_in_place_u256(&mut r, &d);
            q[i >> 6] |= 1u64 << (i & 63);
        }
    }
    (q, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_u64() {
        let i = Int253::from(42u64);
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::from(42u64));
    }

    #[test]
    fn from_i64_positive() {
        let i = Int253::from(42i64);
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::from(42u64));
    }

    #[test]
    fn from_i64_negative() {
        let i = Int253::from(-42i64);
        assert!(i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), -Scalar::from(42u64));
    }

    #[test]
    fn from_i64_min_does_not_overflow() {
        // i64::MIN's magnitude is 2^63 (doesn't fit in i64). Plain `-v`
        // would overflow in debug builds; `unsigned_abs` handles it.
        let i = Int253::from(i64::MIN);
        assert!(i.is_negative());
        assert_eq!(i.abs_scalar(), Scalar::from(1u64 << 63));
    }

    #[test]
    fn from_i64_zero() {
        let i = Int253::from(0i64);
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::zero());
    }

    #[test]
    fn from_signed_integer_positive() {
        let si = SignedInteger::from(100u64);
        let i = Int253::from(si);
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::from(100u64));
    }

    #[test]
    fn from_signed_integer_negative() {
        let si = -SignedInteger::from(100u64);
        let i = Int253::from(si);
        assert!(i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), -Scalar::from(100u64));
    }

    #[test]
    fn roundtrip_bytes_positive() {
        let i = Int253::from(12345u64);
        let bytes = i.to_bytes();
        let i2 = Int253::from_bytes(bytes).unwrap();
        assert_eq!(i, i2);
    }

    #[test]
    fn roundtrip_bytes_negative() {
        let i = Int253::from(-12345i64);
        let bytes = i.to_bytes();
        assert_eq!(bytes[31] & 0x80, 0x80);
        let i2 = Int253::from_bytes(bytes).unwrap();
        assert_eq!(i, i2);
    }

    #[test]
    fn reject_negative_zero() {
        let mut bytes = [0u8; 32];
        bytes[31] = 0x80;
        assert!(Int253::from_bytes(bytes).is_none());
    }

    #[test]
    fn from_scalar() {
        let s = Scalar::from(99u64);
        let i = Int253::from(s);
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), s);
    }

    #[test]
    fn abs_returns_non_negative() {
        let i = Int253::from(-77i64).abs();
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::from(77u64));

        let i = Int253::from(77u64).abs();
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::from(77u64));

        // abs(0) is 0.
        let i = Int253::from(0u64).abs();
        assert_eq!(i.to_scalar_mod_order(), Scalar::zero());
    }

    #[test]
    fn abs_scalar() {
        let i = Int253::from(-99i64);
        assert_eq!(i.abs_scalar(), Scalar::from(99u64));

        let i = Int253::from(99u64);
        assert_eq!(i.abs_scalar(), Scalar::from(99u64));
    }

    #[test]
    fn from_parts() {
        let i = Int253::from_parts(true, Scalar::from(50u64));
        assert!(i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), -Scalar::from(50u64));

        let i = Int253::from_parts(false, Scalar::from(50u64));
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::from(50u64));

        // Negative zero normalizes to positive zero.
        let i = Int253::from_parts(true, Scalar::zero());
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::zero());
    }

    #[test]
    fn to_parts() {
        let i = Int253::from(-42i64);
        let (sign, abs) = i.to_parts();
        assert!(sign);
        assert_eq!(abs, Scalar::from(42u64));

        let i = Int253::from(42u64);
        let (sign, abs) = i.to_parts();
        assert!(!sign);
        assert_eq!(abs, Scalar::from(42u64));
    }

    // ── Constants & predicates ─────────────────────────────────────

    #[test]
    fn zero_and_one() {
        assert!(Int253::zero().is_zero());
        assert_eq!(Int253::zero(), Int253::from(0u64));
        assert_eq!(Int253::one(), Int253::from(1u64));
        assert!(!Int253::one().is_zero());
    }

    #[test]
    fn is_zero_after_neg() {
        let z = -Int253::zero();
        assert!(z.is_zero());
        assert!(!z.is_negative());
    }

    // ── Negation ───────────────────────────────────────────────────

    #[test]
    fn neg_basic() {
        assert_eq!(-Int253::from(5i64), Int253::from(-5i64));
        assert_eq!(-Int253::from(-5i64), Int253::from(5i64));
        assert_eq!(-Int253::zero(), Int253::zero());
        assert_eq!(-(-Int253::from(7i64)), Int253::from(7i64));
    }

    #[test]
    fn neg_never_produces_negative_zero() {
        let z = -Int253::from(0i64);
        assert!(!z.is_negative());
        assert!(z.is_zero());
    }

    // ── Addition: i64 cross-check ──────────────────────────────────

    #[test]
    fn add_matches_i64() {
        for a in [-5i64, -1, 0, 1, 5, 100, -100] {
            for b in [-5i64, -1, 0, 1, 5, 100, -100] {
                let got = Int253::from(a) + Int253::from(b);
                let want = Int253::from(a + b);
                assert_eq!(got, want, "{} + {}", a, b);
            }
        }
    }

    #[test]
    fn add_zero_is_identity() {
        let a = Int253::from(42i64);
        assert_eq!(a + Int253::zero(), a);
        assert_eq!(Int253::zero() + a, a);
    }

    #[test]
    fn add_inverse_is_zero() {
        let a = Int253::from(123i64);
        assert!((a + (-a)).is_zero());
    }

    #[test]
    fn add_commutative_small() {
        let a = Int253::from(17i64);
        let b = Int253::from(-25i64);
        assert_eq!(a + b, b + a);
    }

    // ── Subtraction ────────────────────────────────────────────────

    #[test]
    fn sub_matches_i64() {
        for a in [-5i64, -1, 0, 1, 5, 100] {
            for b in [-5i64, -1, 0, 1, 5, 100] {
                let got = Int253::from(a) - Int253::from(b);
                let want = Int253::from(a - b);
                assert_eq!(got, want, "{} - {}", a, b);
            }
        }
    }

    #[test]
    fn sub_self_is_zero() {
        let a = Int253::from(-99i64);
        assert!((a - a).is_zero());
    }

    // ── Multiplication: i64 cross-check ────────────────────────────

    #[test]
    fn mul_matches_i64() {
        for a in [-5i64, -1, 0, 1, 5, 100] {
            for b in [-5i64, -1, 0, 1, 5, 100] {
                let got = Int253::from(a) * Int253::from(b);
                let want = Int253::from(a * b);
                assert_eq!(got, want, "{} * {}", a, b);
            }
        }
    }

    #[test]
    fn mul_zero_is_zero() {
        let a = Int253::from(42i64);
        assert!((a * Int253::zero()).is_zero());
        assert!((Int253::zero() * a).is_zero());

        let a = Int253::from(-42i64);
        let z = a * Int253::zero();
        assert!(z.is_zero());
        assert!(!z.is_negative());
    }

    #[test]
    fn mul_one_is_identity() {
        let a = Int253::from(-77i64);
        assert_eq!(a * Int253::one(), a);
        assert_eq!(Int253::one() * a, a);
    }

    #[test]
    fn mul_signs_xor() {
        // Spot-check each sign combination's sign bit.
        assert!(!(Int253::from(3i64) * Int253::from(4i64)).is_negative());
        assert!((Int253::from(-3i64) * Int253::from(4i64)).is_negative());
        assert!((Int253::from(3i64) * Int253::from(-4i64)).is_negative());
        assert!(!(Int253::from(-3i64) * Int253::from(-4i64)).is_negative());
    }

    // ── Assignment forms ───────────────────────────────────────────

    #[test]
    fn assign_ops() {
        let mut a = Int253::from(10i64);
        a += Int253::from(5i64);
        assert_eq!(a, Int253::from(15i64));
        a -= Int253::from(20i64);
        assert_eq!(a, Int253::from(-5i64));
        a *= Int253::from(-3i64);
        assert_eq!(a, Int253::from(15i64));
    }

    // ── Wrap semantics at ℓ ────────────────────────────────────────

    fn l_minus_one() -> Int253 {
        // -1 mod ℓ = ℓ - 1 as a Scalar; wrap that into a non-negative Int253.
        Int253::from(-Scalar::one())
    }

    #[test]
    fn add_wraps_at_l() {
        // (ℓ-1) + 1 == 0 (mod ℓ).
        let got = l_minus_one() + Int253::one();
        assert!(got.is_zero());
    }

    #[test]
    fn add_two_large_positives_wrap() {
        // (ℓ-1) + (ℓ-1) == ℓ-2 (mod ℓ), still positive.
        let got = l_minus_one() + l_minus_one();
        assert!(!got.is_negative());
        let want = l_minus_one() - Int253::one();
        assert_eq!(got, want);
    }

    #[test]
    fn add_two_large_negatives_wrap() {
        // (-(ℓ-1)) + (-(ℓ-1)) == -(ℓ-2) (mod ℓ).
        let got = -l_minus_one() + -l_minus_one();
        assert!(got.is_negative());
        let want = -(l_minus_one() - Int253::one());
        assert_eq!(got, want);
    }

    #[test]
    fn mul_wraps_at_l() {
        // (ℓ-1) * 2 == ℓ-2 (mod ℓ), positive.
        let got = l_minus_one() * Int253::from(2u64);
        assert!(!got.is_negative());
        let want = l_minus_one() - Int253::one();
        assert_eq!(got, want);
    }

    #[test]
    fn mul_large_neg_times_large_pos() {
        // (-(ℓ-1)) * (ℓ-1) — sign should be negative,
        // magnitude is (ℓ-1)^2 mod ℓ = 1.
        let got = -l_minus_one() * l_minus_one();
        assert!(got.is_negative());
        assert_eq!(got.abs_scalar(), Scalar::one());
    }

    #[test]
    fn mul_large_neg_times_large_neg() {
        // (-(ℓ-1)) * (-(ℓ-1)) — signs XOR to positive,
        // magnitude is (ℓ-1)^2 mod ℓ = 1.
        let got = -l_minus_one() * -l_minus_one();
        assert!(!got.is_negative());
        assert_eq!(got, Int253::one());
    }

    #[test]
    fn mul_wrap_is_content_dependent() {
        // Two distinct multipliers must produce distinct wrapped results:
        //   (ℓ-1) * 2 ≡ ℓ - 2 (mod ℓ)
        //   (ℓ-1) * 3 ≡ ℓ - 3 (mod ℓ)
        let by_two = l_minus_one() * Int253::from(2u64);
        let by_three = l_minus_one() * Int253::from(3u64);
        assert_ne!(by_two, by_three);
        assert!(!by_two.is_zero());
        assert!(!by_three.is_zero());
        assert_eq!(by_two, l_minus_one() - Int253::one());
        assert_eq!(by_three, l_minus_one() - Int253::from(2u64));
    }

    // ── Ordering ───────────────────────────────────────────────────

    #[test]
    fn ordering_small() {
        let xs = [
            Int253::from(-3i64),
            Int253::from(-1i64),
            Int253::zero(),
            Int253::from(1i64),
            Int253::from(3i64),
        ];
        for i in 0..xs.len() {
            for j in 0..xs.len() {
                let want = i.cmp(&j);
                let got = xs[i].cmp(&xs[j]);
                assert_eq!(got, want, "cmp index {} vs {}", i, j);
            }
        }
    }

    #[test]
    fn ordering_zero_signs() {
        // -0 cannot be constructed, but zero must equal itself.
        assert_eq!(Int253::zero().cmp(&Int253::zero()), Ordering::Equal);
    }

    #[test]
    fn ordering_across_sign() {
        assert!(Int253::from(1i64) > Int253::from(-1_000_000i64));
        assert!(Int253::from(-1i64) < Int253::from(1_000_000i64));
    }

    #[test]
    fn cmp_magnitude_ignores_sign() {
        let a = Int253::from(-1000i64);
        let b = Int253::from(1000i64);
        assert_eq!(a.cmp_magnitude(&b), Ordering::Equal);

        let a = Int253::from(-5i64);
        let b = Int253::from(10i64);
        assert_eq!(a.cmp_magnitude(&b), Ordering::Less);
    }

    // ── div_rem ────────────────────────────────────────────────────

    #[test]
    fn div_rem_zero_divisor_is_none() {
        assert!(Int253::from(1i64).div_rem(Int253::zero()).is_none());
        assert!(Int253::zero().div_rem(Int253::zero()).is_none());
    }

    #[test]
    fn div_rem_matches_i64() {
        let xs = [i64::MIN + 1, -1_000_000, -7, -1, 0, 1, 7, 1_000_000, i64::MAX];
        let ys = [-1_000_000i64, -7, -1, 1, 7, 1_000_000];
        for &a in &xs {
            for &b in &ys {
                let (q, r) = Int253::from(a).div_rem(Int253::from(b)).unwrap();
                assert_eq!(q, Int253::from(a / b), "{} / {}", a, b);
                assert_eq!(r, Int253::from(a % b), "{} % {}", a, b);
            }
        }
    }

    #[test]
    fn div_rem_full_width_identity() {
        // For magnitudes beyond u64::MAX (where there is no native operator
        // to cross-check against), verify the contract directly:
        //   n == q*d + r   (Int253 arithmetic preserves this mod ℓ; since
        //                   |n| < ℓ, the mod-ℓ equality implies integer
        //                   equality)
        //   |r| < |d|
        //   sign(q) == sign(n) ^ sign(d)   (when q != 0)
        //   sign(r) == sign(n)              (when r != 0)
        let big = Int253::from(-Scalar::one()); // magnitude ℓ-1, positive
        let huge = Int253::from(Scalar::from(u64::MAX) * Scalar::from(u64::MAX));
        for &n in &[big, -big, huge, -huge] {
            for &d in &[
                Int253::from(7i64),
                Int253::from(-3i64),
                Int253::from(u64::MAX),
                -Int253::from(u64::MAX),
            ] {
                let (q, r) = n.div_rem(d).unwrap();
                assert_eq!(q * d + r, n, "identity n={:?} d={:?}", n, d);
                assert!(r.abs() < d.abs(), "|r| < |d| n={:?} d={:?}", n, d);
                if !q.is_zero() {
                    assert_eq!(
                        q.is_negative(),
                        n.is_negative() ^ d.is_negative(),
                        "q sign n={:?} d={:?}", n, d,
                    );
                }
                if !r.is_zero() {
                    assert_eq!(r.is_negative(), n.is_negative(),
                               "r sign n={:?} d={:?}", n, d);
                }
            }
        }
    }
}
