//! Signed 256-bit integer with sign-magnitude representation.
//!
//! ## Representation
//!
//! Bit 255 (the high bit of `bytes[31]`) is the sign; the lower 255 bits
//! are a canonical Ristretto scalar magnitude (strictly less than the
//! group order ℓ ≈ 2²⁵²). Negative zero is not representable — every
//! constructor either rejects it (`from_bytes`) or normalizes it to
//! positive zero (`from_parts`, arithmetic ops, `Neg`).
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
//! `Integer` is intended for public VM stack values. Secret witness data
//! should flow through constraint-system types (`Variable`, `Expression`),
//! not through `Integer`.

use core::cmp::Ordering;
use core::ops::{Add, AddAssign, Mul, MulAssign, Neg, Sub, SubAssign};

use curve25519_dalek::scalar::Scalar;
use spacesuit::SignedInteger;

/// Signed 256-bit integer in sign-magnitude form. See module docs for
/// the canonical representation, wraparound semantics, and the
/// non-constant-time disclaimer.
#[derive(Copy, Clone)]
pub struct Integer {
    bytes: [u8; 32],
}

impl Integer {
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
        // The lower 255 bits are canonical by the `Integer` invariant
        // (established in `from_bytes` and preserved everywhere else).
        Scalar::from_canonical_bytes(abs_bytes)
            .expect("Integer invariant: lower 255 bits are a canonical scalar")
    }

    /// Converts to a Ristretto scalar mod ℓ.
    ///
    /// This is a many-to-one mapping: non-negative `Integer(X)` maps
    /// to `Scalar(X)`; negative `Integer(-X)` maps to `Scalar(ℓ − X)`.
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
    pub fn from_bytes(bytes: [u8; 32]) -> Option<Integer> {
        let negative = bytes[31] & 0x80 != 0;
        let mut scalar_bytes = bytes;
        scalar_bytes[31] &= 0x7f;
        // Verify canonical encoding.
        Scalar::from_canonical_bytes(scalar_bytes)?;
        if negative && scalar_bytes == [0u8; 32] {
            return None; // reject negative zero
        }
        Some(Integer { bytes })
    }

    /// Creates an `Integer` from an absolute-value scalar and a sign flag.
    /// Negative zero is normalized to positive zero.
    pub fn from_parts(sign: bool, scalar: Scalar) -> Integer {
        let mut bytes = scalar.to_bytes();
        if sign && scalar != Scalar::zero() {
            bytes[31] |= 0x80;
        }
        Integer { bytes }
    }

    /// Returns the absolute value as an `Integer` (always non-negative).
    pub fn abs(&self) -> Integer {
        let mut abs_bytes = self.bytes;
        abs_bytes[31] &= 0x7f;
        Integer { bytes: abs_bytes }
    }

    /// Returns the additive identity.
    pub fn zero() -> Integer {
        Integer { bytes: [0u8; 32] }
    }

    /// Returns the multiplicative identity.
    pub fn one() -> Integer {
        Integer::from(1u64)
    }

    /// Returns `true` if the value is zero.
    pub fn is_zero(&self) -> bool {
        let mut bytes = self.bytes;
        bytes[31] &= 0x7f;
        bytes == [0u8; 32]
    }

    /// Compares two `Integer`s by magnitude (ignoring sign).
    pub(crate) fn cmp_magnitude(&self, other: &Integer) -> Ordering {
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

impl Neg for Integer {
    type Output = Integer;
    fn neg(self) -> Integer {
        if self.is_zero() {
            return self;
        }
        let mut bytes = self.bytes;
        bytes[31] ^= 0x80;
        Integer { bytes }
    }
}

impl Add for Integer {
    type Output = Integer;
    fn add(self, other: Integer) -> Integer {
        match (self.is_negative(), other.is_negative()) {
            (false, false) | (true, true) => Integer::from_parts(
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
                Integer::from_parts(sign, abs)
            }
        }
    }
}

impl Sub for Integer {
    type Output = Integer;
    fn sub(self, other: Integer) -> Integer {
        self + (-other)
    }
}

impl Mul for Integer {
    type Output = Integer;
    fn mul(self, other: Integer) -> Integer {
        Integer::from_parts(
            self.is_negative() ^ other.is_negative(),
            self.abs_scalar() * other.abs_scalar(),
        )
    }
}

impl AddAssign for Integer {
    fn add_assign(&mut self, other: Integer) { *self = *self + other; }
}
impl SubAssign for Integer {
    fn sub_assign(&mut self, other: Integer) { *self = *self - other; }
}
impl MulAssign for Integer {
    fn mul_assign(&mut self, other: Integer) { *self = *self * other; }
}

impl Ord for Integer {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_negative(), other.is_negative()) {
            (false, false) => self.cmp_magnitude(other),
            (true, true) => other.cmp_magnitude(self),
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
        }
    }
}

impl PartialOrd for Integer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl core::fmt::Debug for Integer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_negative() {
            write!(f, "Integer(-{:?})", self.abs_scalar())
        } else {
            write!(f, "Integer({:?})", self.abs_scalar())
        }
    }
}

impl From<u64> for Integer {
    fn from(v: u64) -> Self {
        Integer {
            bytes: Scalar::from(v).to_bytes(),
        }
    }
}

impl From<i64> for Integer {
    fn from(v: i64) -> Self {
        if v < 0 {
            // `unsigned_abs` handles i64::MIN correctly (its magnitude is 2^63,
            // which doesn't fit in i64 but does fit in u64). Plain `-v` would
            // overflow in debug builds.
            let mut bytes = Scalar::from(v.unsigned_abs()).to_bytes();
            bytes[31] |= 0x80;
            Integer { bytes }
        } else {
            Integer {
                bytes: Scalar::from(v as u64).to_bytes(),
            }
        }
    }
}

impl From<Scalar> for Integer {
    fn from(s: Scalar) -> Self {
        Integer {
            bytes: s.to_bytes(),
        }
    }
}

impl From<SignedInteger> for Integer {
    fn from(si: SignedInteger) -> Self {
        match si.to_u64() {
            Some(v) => Integer::from(v),
            None => {
                let neg = -si;
                let abs_val = neg
                    .to_u64()
                    .expect("negation of negative SignedInteger is non-negative");
                let mut bytes = Scalar::from(abs_val).to_bytes();
                bytes[31] |= 0x80;
                Integer { bytes }
            }
        }
    }
}

impl Into<Scalar> for Integer {
    fn into(self) -> Scalar {
        self.to_scalar_mod_order()
    }
}

impl PartialEq for Integer {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for Integer {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_u64() {
        let i = Integer::from(42u64);
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::from(42u64));
    }

    #[test]
    fn from_i64_positive() {
        let i = Integer::from(42i64);
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::from(42u64));
    }

    #[test]
    fn from_i64_negative() {
        let i = Integer::from(-42i64);
        assert!(i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), -Scalar::from(42u64));
    }

    #[test]
    fn from_i64_zero() {
        let i = Integer::from(0i64);
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::zero());
    }

    #[test]
    fn from_signed_integer_positive() {
        let si = SignedInteger::from(100u64);
        let i = Integer::from(si);
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::from(100u64));
    }

    #[test]
    fn from_signed_integer_negative() {
        let si = -SignedInteger::from(100u64);
        let i = Integer::from(si);
        assert!(i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), -Scalar::from(100u64));
    }

    #[test]
    fn roundtrip_bytes_positive() {
        let i = Integer::from(12345u64);
        let bytes = i.to_bytes();
        let i2 = Integer::from_bytes(bytes).unwrap();
        assert_eq!(i, i2);
    }

    #[test]
    fn roundtrip_bytes_negative() {
        let i = Integer::from(-12345i64);
        let bytes = i.to_bytes();
        assert_eq!(bytes[31] & 0x80, 0x80);
        let i2 = Integer::from_bytes(bytes).unwrap();
        assert_eq!(i, i2);
    }

    #[test]
    fn reject_negative_zero() {
        let mut bytes = [0u8; 32];
        bytes[31] = 0x80;
        assert!(Integer::from_bytes(bytes).is_none());
    }

    #[test]
    fn from_scalar() {
        let s = Scalar::from(99u64);
        let i = Integer::from(s);
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), s);
    }

    #[test]
    fn abs_returns_non_negative() {
        let i = Integer::from(-77i64).abs();
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::from(77u64));

        let i = Integer::from(77u64).abs();
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::from(77u64));

        // abs(0) is 0.
        let i = Integer::from(0u64).abs();
        assert_eq!(i.to_scalar_mod_order(), Scalar::zero());
    }

    #[test]
    fn abs_scalar() {
        let i = Integer::from(-99i64);
        assert_eq!(i.abs_scalar(), Scalar::from(99u64));

        let i = Integer::from(99u64);
        assert_eq!(i.abs_scalar(), Scalar::from(99u64));
    }

    #[test]
    fn from_parts() {
        let i = Integer::from_parts(true, Scalar::from(50u64));
        assert!(i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), -Scalar::from(50u64));

        let i = Integer::from_parts(false, Scalar::from(50u64));
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::from(50u64));

        // Negative zero normalizes to positive zero.
        let i = Integer::from_parts(true, Scalar::zero());
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar_mod_order(), Scalar::zero());
    }

    #[test]
    fn to_parts() {
        let i = Integer::from(-42i64);
        let (sign, abs) = i.to_parts();
        assert!(sign);
        assert_eq!(abs, Scalar::from(42u64));

        let i = Integer::from(42u64);
        let (sign, abs) = i.to_parts();
        assert!(!sign);
        assert_eq!(abs, Scalar::from(42u64));
    }

    // ── Constants & predicates ─────────────────────────────────────

    #[test]
    fn zero_and_one() {
        assert!(Integer::zero().is_zero());
        assert_eq!(Integer::zero(), Integer::from(0u64));
        assert_eq!(Integer::one(), Integer::from(1u64));
        assert!(!Integer::one().is_zero());
    }

    #[test]
    fn is_zero_after_neg() {
        let z = -Integer::zero();
        assert!(z.is_zero());
        assert!(!z.is_negative());
    }

    // ── Negation ───────────────────────────────────────────────────

    #[test]
    fn neg_basic() {
        assert_eq!(-Integer::from(5i64), Integer::from(-5i64));
        assert_eq!(-Integer::from(-5i64), Integer::from(5i64));
        assert_eq!(-Integer::zero(), Integer::zero());
        assert_eq!(-(-Integer::from(7i64)), Integer::from(7i64));
    }

    #[test]
    fn neg_never_produces_negative_zero() {
        let z = -Integer::from(0i64);
        assert!(!z.is_negative());
        assert!(z.is_zero());
    }

    // ── Addition: i64 cross-check ──────────────────────────────────

    #[test]
    fn add_matches_i64() {
        for a in [-5i64, -1, 0, 1, 5, 100, -100] {
            for b in [-5i64, -1, 0, 1, 5, 100, -100] {
                let got = Integer::from(a) + Integer::from(b);
                let want = Integer::from(a + b);
                assert_eq!(got, want, "{} + {}", a, b);
            }
        }
    }

    #[test]
    fn add_zero_is_identity() {
        let a = Integer::from(42i64);
        assert_eq!(a + Integer::zero(), a);
        assert_eq!(Integer::zero() + a, a);
    }

    #[test]
    fn add_inverse_is_zero() {
        let a = Integer::from(123i64);
        assert!((a + (-a)).is_zero());
    }

    #[test]
    fn add_commutative_small() {
        let a = Integer::from(17i64);
        let b = Integer::from(-25i64);
        assert_eq!(a + b, b + a);
    }

    // ── Subtraction ────────────────────────────────────────────────

    #[test]
    fn sub_matches_i64() {
        for a in [-5i64, -1, 0, 1, 5, 100] {
            for b in [-5i64, -1, 0, 1, 5, 100] {
                let got = Integer::from(a) - Integer::from(b);
                let want = Integer::from(a - b);
                assert_eq!(got, want, "{} - {}", a, b);
            }
        }
    }

    #[test]
    fn sub_self_is_zero() {
        let a = Integer::from(-99i64);
        assert!((a - a).is_zero());
    }

    // ── Multiplication: i64 cross-check ────────────────────────────

    #[test]
    fn mul_matches_i64() {
        for a in [-5i64, -1, 0, 1, 5, 100] {
            for b in [-5i64, -1, 0, 1, 5, 100] {
                let got = Integer::from(a) * Integer::from(b);
                let want = Integer::from(a * b);
                assert_eq!(got, want, "{} * {}", a, b);
            }
        }
    }

    #[test]
    fn mul_zero_is_zero() {
        let a = Integer::from(42i64);
        assert!((a * Integer::zero()).is_zero());
        assert!((Integer::zero() * a).is_zero());

        let a = Integer::from(-42i64);
        let z = a * Integer::zero();
        assert!(z.is_zero());
        assert!(!z.is_negative());
    }

    #[test]
    fn mul_one_is_identity() {
        let a = Integer::from(-77i64);
        assert_eq!(a * Integer::one(), a);
        assert_eq!(Integer::one() * a, a);
    }

    #[test]
    fn mul_signs_xor() {
        // Spot-check each sign combination's sign bit.
        assert!(!(Integer::from(3i64) * Integer::from(4i64)).is_negative());
        assert!((Integer::from(-3i64) * Integer::from(4i64)).is_negative());
        assert!((Integer::from(3i64) * Integer::from(-4i64)).is_negative());
        assert!(!(Integer::from(-3i64) * Integer::from(-4i64)).is_negative());
    }

    // ── Assignment forms ───────────────────────────────────────────

    #[test]
    fn assign_ops() {
        let mut a = Integer::from(10i64);
        a += Integer::from(5i64);
        assert_eq!(a, Integer::from(15i64));
        a -= Integer::from(20i64);
        assert_eq!(a, Integer::from(-5i64));
        a *= Integer::from(-3i64);
        assert_eq!(a, Integer::from(15i64));
    }

    // ── Wrap semantics at ℓ ────────────────────────────────────────

    fn l_minus_one() -> Integer {
        // -1 mod ℓ = ℓ - 1 as a Scalar; wrap that into a non-negative Integer.
        Integer::from(-Scalar::one())
    }

    #[test]
    fn add_wraps_at_l() {
        // (ℓ-1) + 1 == 0 (mod ℓ).
        let got = l_minus_one() + Integer::one();
        assert!(got.is_zero());
    }

    #[test]
    fn add_two_large_positives_wrap() {
        // (ℓ-1) + (ℓ-1) == ℓ-2 (mod ℓ), still positive.
        let got = l_minus_one() + l_minus_one();
        assert!(!got.is_negative());
        let want = l_minus_one() - Integer::one();
        assert_eq!(got, want);
    }

    #[test]
    fn add_two_large_negatives_wrap() {
        // (-(ℓ-1)) + (-(ℓ-1)) == -(ℓ-2) (mod ℓ).
        let got = -l_minus_one() + -l_minus_one();
        assert!(got.is_negative());
        let want = -(l_minus_one() - Integer::one());
        assert_eq!(got, want);
    }

    #[test]
    fn mul_wraps_at_l() {
        // (ℓ-1) * 2 == ℓ-2 (mod ℓ), positive.
        let got = l_minus_one() * Integer::from(2u64);
        assert!(!got.is_negative());
        let want = l_minus_one() - Integer::one();
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
        assert_eq!(got, Integer::one());
    }

    #[test]
    fn mul_wrap_is_content_dependent() {
        // Two distinct multipliers must produce distinct wrapped results:
        //   (ℓ-1) * 2 ≡ ℓ - 2 (mod ℓ)
        //   (ℓ-1) * 3 ≡ ℓ - 3 (mod ℓ)
        let by_two = l_minus_one() * Integer::from(2u64);
        let by_three = l_minus_one() * Integer::from(3u64);
        assert_ne!(by_two, by_three);
        assert!(!by_two.is_zero());
        assert!(!by_three.is_zero());
        assert_eq!(by_two, l_minus_one() - Integer::one());
        assert_eq!(by_three, l_minus_one() - Integer::from(2u64));
    }

    // ── Ordering ───────────────────────────────────────────────────

    #[test]
    fn ordering_small() {
        let xs = [
            Integer::from(-3i64),
            Integer::from(-1i64),
            Integer::zero(),
            Integer::from(1i64),
            Integer::from(3i64),
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
        assert_eq!(Integer::zero().cmp(&Integer::zero()), Ordering::Equal);
    }

    #[test]
    fn ordering_across_sign() {
        assert!(Integer::from(1i64) > Integer::from(-1_000_000i64));
        assert!(Integer::from(-1i64) < Integer::from(1_000_000i64));
    }

    #[test]
    fn cmp_magnitude_ignores_sign() {
        let a = Integer::from(-1000i64);
        let b = Integer::from(1000i64);
        assert_eq!(a.cmp_magnitude(&b), Ordering::Equal);

        let a = Integer::from(-5i64);
        let b = Integer::from(10i64);
        assert_eq!(a.cmp_magnitude(&b), Ordering::Less);
    }
}
