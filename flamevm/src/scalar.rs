//! Canonical Ristretto scalars, with arithmetic modulo the group order ℓ.
//!
//! Encoding, equality, and ordering use the unsigned representative in [0, ℓ).
//! Only the explicitly centered helpers interpret residues above (ℓ − 1) / 2
//! as negative integers. Integer division is not field inversion.
//! Ordering, conversions, and centered helpers are not constant-time.

use core::cmp::Ordering;
use core::ops::{Add, AddAssign, Mul, MulAssign, Neg, Sub, SubAssign};

use curve25519_dalek::scalar::Scalar as DalekScalar;
use spacesuit::SignedInteger;

/// A canonical little-endian scalar in [0, ℓ).
///
/// Arithmetic wraps modulo ℓ. `Ord` compares unsigned numeric values, so
/// `Scalar::from(-1i64)` sorts after every other scalar.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Scalar {
    bytes: [u8; 32],
}

impl Scalar {
    /// The additive identity.
    pub const ZERO: Self = Self { bytes: [0u8; 32] };

    /// The multiplicative identity.
    pub const ONE: Self = Self {
        bytes: {
            let mut bytes = [0u8; 32];
            bytes[0] = 1;
            bytes
        },
    };

    /// Decodes a canonical little-endian scalar, rejecting integers ≥ ℓ.
    pub fn from_bytes(bytes: [u8; 32]) -> Option<Self> {
        Option::<DalekScalar>::from(DalekScalar::from_canonical_bytes(bytes))?;
        Some(Self { bytes })
    }

    /// Returns the canonical little-endian encoding.
    pub fn to_bytes(self) -> [u8; 32] {
        self.bytes
    }

    /// Borrows the canonical little-endian encoding.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }

    /// Returns the same field element as a Dalek scalar.
    pub fn to_dalek(&self) -> DalekScalar {
        DalekScalar::from_canonical_bytes(self.bytes).expect("Scalar invariant: canonical encoding")
    }

    /// Returns whether this is the additive identity.
    pub fn is_zero(&self) -> bool {
        self.bytes == [0u8; 32]
    }

    /// Returns the unsigned representative if it fits in `u64`.
    pub fn to_u64(&self) -> Option<u64> {
        if self.bytes[8..].iter().any(|&byte| byte != 0) {
            return None;
        }
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&self.bytes[..8]);
        Some(u64::from_le_bytes(bytes))
    }

    /// Returns the unsigned representative if it fits in `u128`.
    pub fn to_u128(&self) -> Option<u128> {
        if self.bytes[16..].iter().any(|&byte| byte != 0) {
            return None;
        }
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&self.bytes[..16]);
        Some(u128::from_le_bytes(bytes))
    }

    /// Returns whether the centered representative in [−H, H] is negative,
    /// where H = (ℓ − 1) / 2. Zero is non-negative.
    pub fn is_centered_negative(&self) -> bool {
        // For nonzero r, −r is ℓ − r, so r > −r precisely when r > H.
        *self > -*self
    }

    /// Returns the magnitude of the centered representative, always in [0, H].
    pub fn centered_abs(&self) -> Self {
        if self.is_centered_negative() {
            -*self
        } else {
            *self
        }
    }

    /// Divides centered integer representatives, truncating toward zero.
    ///
    /// Returns scalars encoding q and r such that the decoded integers satisfy
    /// n = q*d + r and |r| < |d|. A nonzero remainder has the numerator's sign.
    /// Returns `None` only for a zero divisor. The symmetric centered range
    /// permits (−H) / (−1) = H without overflow.
    pub fn div_rem(self, other: Self) -> Option<(Self, Self)> {
        if other.is_zero() {
            return None;
        }
        let (q, r) = divmod_u256(
            bytes_to_limbs(self.centered_abs().to_bytes()),
            bytes_to_limbs(other.centered_abs().to_bytes()),
        );
        let q =
            Self::from_bytes(limbs_to_bytes(q)).expect("centered quotient magnitude is at most H");
        let r =
            Self::from_bytes(limbs_to_bytes(r)).expect("centered remainder magnitude is below H");
        Some((
            if self.is_centered_negative() ^ other.is_centered_negative() {
                -q
            } else {
                q
            },
            if self.is_centered_negative() { -r } else { r },
        ))
    }
}

impl Neg for Scalar {
    type Output = Self;
    fn neg(self) -> Self {
        (-self.to_dalek()).into()
    }
}

impl Add for Scalar {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        (self.to_dalek() + other.to_dalek()).into()
    }
}

impl Sub for Scalar {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        (self.to_dalek() - other.to_dalek()).into()
    }
}

impl Mul for Scalar {
    type Output = Self;
    fn mul(self, other: Self) -> Self {
        (self.to_dalek() * other.to_dalek()).into()
    }
}

impl AddAssign for Scalar {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl SubAssign for Scalar {
    fn sub_assign(&mut self, other: Self) {
        *self = *self - other;
    }
}

impl MulAssign for Scalar {
    fn mul_assign(&mut self, other: Self) {
        *self = *self * other;
    }
}

impl Ord for Scalar {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes.iter().rev().cmp(other.bytes.iter().rev())
    }
}

impl PartialOrd for Scalar {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl From<u64> for Scalar {
    fn from(value: u64) -> Self {
        Self::from(value as u128)
    }
}

impl From<u128> for Scalar {
    fn from(value: u128) -> Self {
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&value.to_le_bytes());
        Self { bytes }
    }
}

impl From<i64> for Scalar {
    fn from(value: i64) -> Self {
        Self::from(value as i128)
    }
}

impl From<i128> for Scalar {
    fn from(value: i128) -> Self {
        let magnitude = Self::from(value.unsigned_abs());
        if value < 0 {
            -magnitude
        } else {
            magnitude
        }
    }
}

impl From<DalekScalar> for Scalar {
    fn from(value: DalekScalar) -> Self {
        // Dalek's legacy `from_bits` can produce noncanonical encodings.
        Self {
            bytes: DalekScalar::from_bytes_mod_order(value.to_bytes()).to_bytes(),
        }
    }
}

impl From<Scalar> for DalekScalar {
    fn from(value: Scalar) -> Self {
        value.to_dalek()
    }
}

impl From<SignedInteger> for Scalar {
    fn from(value: SignedInteger) -> Self {
        Self::from(DalekScalar::from(value))
    }
}

// Unsigned long division over four little-endian limbs. The centered operand
// magnitudes are at most H < 2²⁵², so shifting the remainder cannot overflow.
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

    fn centered_max() -> Scalar {
        Scalar::from(DalekScalar::from(2u64).invert() - DalekScalar::ONE)
    }

    #[test]
    fn canonical_encoding_and_conversions() {
        for value in [Scalar::ZERO, Scalar::ONE, centered_max(), -Scalar::ONE] {
            assert_eq!(Scalar::from_bytes(value.to_bytes()), Some(value));
            assert_eq!(value.as_bytes(), &value.to_bytes());
            assert_eq!(Scalar::from(value.to_dalek()), value);
            assert_eq!(DalekScalar::from(value).to_bytes(), value.to_bytes());
        }
        let mut order = (-DalekScalar::ONE).to_bytes();
        order[0] += 1;
        assert!(Scalar::from_bytes(order).is_none());
        order[0] += 1;
        assert!(Scalar::from_bytes(order).is_none());
        assert!(Scalar::from_bytes([0xff; 32]).is_none());
        let mut out_of_range = Scalar::ONE.to_bytes();
        out_of_range[31] |= 0x80;
        assert!(Scalar::from_bytes(out_of_range).is_none());

        assert_eq!(Scalar::from(u64::MAX).to_u64(), Some(u64::MAX));
        assert_eq!(Scalar::from(u128::MAX).to_u128(), Some(u128::MAX));
        assert_eq!(Scalar::from(u64::MAX as u128 + 1).to_u64(), None);
        assert_eq!((Scalar::from(u128::MAX) + Scalar::ONE).to_u128(), None);
        assert_eq!(Scalar::from(-1i64).to_u128(), None);
        assert_eq!(Scalar::from(-1i64).to_u64(), None);
        assert_eq!(
            Scalar::from(i64::MIN).centered_abs(),
            Scalar::from(1u64 << 63)
        );
        assert_eq!(
            Scalar::from(i128::MIN).centered_abs(),
            Scalar::from(1u128 << 127)
        );
        for value in [
            SignedInteger::from(u64::MAX),
            -SignedInteger::from(u64::MAX),
        ] {
            assert_eq!(Scalar::from(value).to_dalek(), DalekScalar::from(value));
        }
    }

    #[test]
    #[allow(deprecated)]
    fn legacy_dalek_input_is_canonicalized() {
        let legacy = DalekScalar::from_bits([0xff; 32]);
        let value = Scalar::from(legacy);
        assert_eq!(Scalar::from_bytes(value.to_bytes()), Some(value));
        assert_eq!(
            value.to_dalek(),
            DalekScalar::from_bytes_mod_order(legacy.to_bytes())
        );
    }

    #[test]
    fn unsigned_order_and_centered_boundaries() {
        let h = centered_max();
        let ordered = [
            Scalar::ZERO,
            Scalar::ONE,
            Scalar::from(255u64),
            Scalar::from(256u64),
            Scalar::from(u64::MAX),
            Scalar::from(u128::MAX),
            h,
            h + Scalar::ONE,
            Scalar::from(-2i64),
            Scalar::from(-1i64),
        ];
        for (i, a) in ordered.iter().enumerate() {
            for (j, b) in ordered.iter().enumerate() {
                assert_eq!(a.cmp(b), i.cmp(&j));
            }
        }
        assert!(!Scalar::ZERO.is_centered_negative());
        assert!(!h.is_centered_negative());
        assert_eq!(h.centered_abs(), h);
        assert!((h + Scalar::ONE).is_centered_negative());
        assert_eq!((h + Scalar::ONE).centered_abs(), h);
        assert_eq!(h + Scalar::ONE, -h);
        assert!((-Scalar::ONE).is_centered_negative());
        assert_eq!((-Scalar::ONE).centered_abs(), Scalar::ONE);
        assert_eq!(-Scalar::ZERO, Scalar::ZERO);
    }

    #[test]
    fn field_laws_and_dalek_agreement() {
        let values = [
            Scalar::ZERO,
            Scalar::ONE,
            -Scalar::ONE,
            centered_max(),
            -centered_max(),
            Scalar::from(u128::MAX),
            Scalar::from(DalekScalar::from_bytes_mod_order([0x55; 32])),
        ];
        for a in values {
            assert_eq!(a + -a, Scalar::ZERO);
            assert_eq!(a * Scalar::ONE, a);
            assert_eq!(-(-a), a);
            for b in values {
                assert_eq!((a + b).to_dalek(), a.to_dalek() + b.to_dalek());
                assert_eq!((a - b).to_dalek(), a.to_dalek() - b.to_dalek());
                assert_eq!((a * b).to_dalek(), a.to_dalek() * b.to_dalek());
                assert_eq!(a + b, b + a);
                assert_eq!(a * b, b * a);
                for c in values {
                    assert_eq!((a + b) + c, a + (b + c));
                    assert_eq!((a * b) * c, a * (b * c));
                    assert_eq!(a * (b + c), a * b + a * c);
                }
            }
        }
        let mut value = Scalar::from(10i64);
        value += Scalar::from(5i64);
        value -= Scalar::from(20i64);
        value *= Scalar::from(-3i64);
        assert_eq!(value, Scalar::from(15i64));
        assert!((-Scalar::ONE + Scalar::ONE).is_zero());
    }

    #[test]
    fn div_rem_matches_native_signed_integers() {
        let numerators = [
            i128::MIN + 1,
            i64::MIN as i128,
            -1_000_000,
            -7,
            -1,
            0,
            1,
            7,
            1_000_000,
            i64::MAX as i128,
            i128::MAX,
        ];
        for n in numerators {
            for d in [-1_000_000i128, -7, -1, 1, 7, 1_000_000, i128::MAX] {
                let (q, r) = Scalar::from(n).div_rem(Scalar::from(d)).unwrap();
                assert_eq!(q, Scalar::from(n / d), "{n} / {d}");
                assert_eq!(r, Scalar::from(n % d), "{n} % {d}");
            }
        }
        assert_eq!(
            Scalar::from(i128::MIN).div_rem(Scalar::from(-1i64)),
            Some((Scalar::from(1u128 << 127), Scalar::ZERO))
        );
        assert_eq!(Scalar::ONE.div_rem(Scalar::ZERO), None);
        assert_eq!(Scalar::ZERO.div_rem(Scalar::ZERO), None);
        assert_eq!(
            Scalar::ONE.div_rem(Scalar::from(2u64)),
            Some((Scalar::ZERO, Scalar::ONE))
        );
    }

    #[test]
    fn div_rem_full_width_and_centered_minimum() {
        let h = centered_max();
        assert_eq!((-h).div_rem(-Scalar::ONE), Some((h, Scalar::ZERO)));
        assert_eq!((-h).div_rem(h), Some((-Scalar::ONE, Scalar::ZERO)));
        assert_eq!(h.div_rem(-h), Some((-Scalar::ONE, Scalar::ZERO)));

        let mut numerator = [0u8; 32];
        numerator[30] = 1; // 2^240 + 123.
        numerator[0] = 123;
        let n = Scalar::from_bytes(numerator).unwrap();
        let d = Scalar::from(u128::MAX) + Scalar::ONE; // 2^128.
        assert_eq!(
            n.div_rem(d),
            Some((Scalar::from(1u128 << 112), Scalar::from(123u64)))
        );
        for n in [h, -h, n, -n] {
            for d in [h, -h, d, -d, Scalar::from(7i64), Scalar::from(-3i64)] {
                let (q, r) = n.div_rem(d).unwrap();
                assert_eq!(q * d + r, n);
                assert!(r.centered_abs() < d.centered_abs());
                assert!(q.centered_abs() <= n.centered_abs());
                if !q.is_zero() {
                    assert_eq!(
                        q.is_centered_negative(),
                        n.is_centered_negative() ^ d.is_centered_negative()
                    );
                }
                if !r.is_zero() {
                    assert_eq!(r.is_centered_negative(), n.is_centered_negative());
                }
            }
        }
        // An integer quotient check detects even a wrapped positive product.
        let wrapped = h * Scalar::from(3u64);
        assert!(!wrapped.is_centered_negative());
        assert_ne!(wrapped.div_rem(h).unwrap().0, Scalar::from(3u64));
    }
}
