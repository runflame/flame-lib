/// Represents a usize with value in the range [0,64]
#[derive(Copy, Clone, Debug)]
pub struct BitRange(usize);

impl BitRange {
    /// Returns Some(BitRange) if `n` is ≤ 64.
    /// Otherwise returns None.
    pub fn new(n: usize) -> Option<Self> {
        if n > 64 {
            None
        } else {
            Some(BitRange(n))
        }
    }

    /// Returns 64-bit range
    pub fn max() -> Self {
        BitRange(64)
    }
}

impl From<BitRange> for usize {
    fn from(bit_range: BitRange) -> usize {
        bit_range.0
    }
}

impl From<BitRange> for u8 {
    fn from(bit_range: BitRange) -> u8 {
        bit_range.0 as u8
    }
}
