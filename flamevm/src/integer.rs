use curve25519_dalek::scalar::Scalar;
use spacesuit::SignedInteger;

/// Integer type backed by `[u8; 32]` with the sign bit in bit 255
/// (the highest bit of `bytes[31]`). The lower 255 bits are a
/// canonical Ristretto scalar (strictly less than the group order ℓ).
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

    /// Returns the absolute value as a `Scalar`.
    pub fn abs_scalar(&self) -> Scalar {
        let mut abs_bytes = self.bytes;
        abs_bytes[31] &= 0x7f;
        // The lower 255 bits are canonical by the `Integer` invariant
        // (established in `from_bytes` and preserved everywhere else).
        Scalar::from_canonical_bytes(abs_bytes)
            .expect("Integer invariant: lower 255 bits are a canonical scalar")
    }

    /// Converts to a Ristretto scalar.
    /// Non-negative values map directly; negative values map to `-abs mod l`.
    pub fn to_scalar(&self) -> Scalar {
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
            let mut bytes = Scalar::from((-v) as u64).to_bytes();
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
        self.to_scalar()
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
        assert_eq!(i.to_scalar(), Scalar::from(42u64));
    }

    #[test]
    fn from_i64_positive() {
        let i = Integer::from(42i64);
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar(), Scalar::from(42u64));
    }

    #[test]
    fn from_i64_negative() {
        let i = Integer::from(-42i64);
        assert!(i.is_negative());
        assert_eq!(i.to_scalar(), -Scalar::from(42u64));
    }

    #[test]
    fn from_i64_zero() {
        let i = Integer::from(0i64);
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar(), Scalar::zero());
    }

    #[test]
    fn from_signed_integer_positive() {
        let si = SignedInteger::from(100u64);
        let i = Integer::from(si);
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar(), Scalar::from(100u64));
    }

    #[test]
    fn from_signed_integer_negative() {
        let si = -SignedInteger::from(100u64);
        let i = Integer::from(si);
        assert!(i.is_negative());
        assert_eq!(i.to_scalar(), -Scalar::from(100u64));
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
        assert_eq!(i.to_scalar(), s);
    }

    #[test]
    fn abs_returns_non_negative() {
        let i = Integer::from(-77i64).abs();
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar(), Scalar::from(77u64));

        let i = Integer::from(77u64).abs();
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar(), Scalar::from(77u64));

        // abs(0) is 0.
        let i = Integer::from(0u64).abs();
        assert_eq!(i.to_scalar(), Scalar::zero());
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
        assert_eq!(i.to_scalar(), -Scalar::from(50u64));

        let i = Integer::from_parts(false, Scalar::from(50u64));
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar(), Scalar::from(50u64));

        // Negative zero normalizes to positive zero.
        let i = Integer::from_parts(true, Scalar::zero());
        assert!(!i.is_negative());
        assert_eq!(i.to_scalar(), Scalar::zero());
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
}
