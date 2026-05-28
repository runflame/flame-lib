//! Overflow-safe per-tx fee accumulator.

use crate::errors::VMError;

/// Maximum fee an external transaction may accumulate across all
/// `fee` opcodes. Allows overflow-safe `size_bytes × fee` math in any
/// downstream fee-rate computation: `MAX_FEE = 2^24` flames means a
/// 2^40-byte transaction (~1 TB) still leaves 24 bits of `u64` headroom.
pub const MAX_FEE: u64 = 1 << 24;

/// Per-transaction fee accumulator. Constructed at `VM::new`, mutated
/// only by [`Self::add`] (called from `op_fee`), and surfaced through
/// the eventual `TxResult.total_fee`. Carries no flavor
/// information: the flavor is recorded separately in each
/// `TxEntry::Fee` and the matching `WideToken` returned to the
/// stack.
///
/// The implementation is intentionally a thin newtype over `u64`. The
/// `Copy` derive keeps API ergonomics — the field is a value, not a
/// resource. Linear-type discipline does not apply here: this isn't
/// a bearer token, it's a metering counter.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckedFee {
    inner: u64,
}

impl CheckedFee {
    /// Constructs an accumulator at zero. Used by `VM::new`.
    pub const fn zero() -> Self {
        Self { inner: 0 }
    }

    /// Adds `fee` to the accumulator, returning `Err(FeeTooHigh)` if
    /// `fee` itself exceeds [`MAX_FEE`] or if the running total would
    /// after the add. Both checks needed: a single oversized opcode
    /// arg and an aggregate overflow are distinct failure modes that
    /// share the same error tag.
    pub fn add(&mut self, fee: u64) -> Result<(), VMError> {
        if fee > MAX_FEE {
            return Err(VMError::FeeTooHigh);
        }
        // `inner + fee` cannot overflow `u64` here because both
        // operands are ≤ MAX_FEE = 2^24 ≪ 2^64; the check is the
        // policy boundary, not arithmetic safety.
        let next = self.inner + fee;
        if next > MAX_FEE {
            return Err(VMError::FeeTooHigh);
        }
        self.inner = next;
        Ok(())
    }

    /// Read-only accessor. Exposed for `TxResult.total_fee` and tests.
    pub fn total(&self) -> u64 {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_starts_at_zero() {
        assert_eq!(CheckedFee::zero().total(), 0);
    }

    #[test]
    fn add_accumulates() {
        let mut f = CheckedFee::zero();
        f.add(10).unwrap();
        f.add(20).unwrap();
        f.add(30).unwrap();
        assert_eq!(f.total(), 60);
    }

    #[test]
    fn add_at_cap_succeeds() {
        let mut f = CheckedFee::zero();
        f.add(MAX_FEE).unwrap();
        assert_eq!(f.total(), MAX_FEE);
    }

    #[test]
    fn add_single_over_cap_rejects() {
        let mut f = CheckedFee::zero();
        let err = f.add(MAX_FEE + 1).unwrap_err();
        assert!(matches!(err, VMError::FeeTooHigh));
        // Accumulator unchanged on rejection.
        assert_eq!(f.total(), 0);
    }

    #[test]
    fn add_aggregate_over_cap_rejects() {
        let mut f = CheckedFee::zero();
        f.add(MAX_FEE / 2).unwrap();
        f.add(MAX_FEE / 2).unwrap();
        // Now at MAX_FEE - (MAX_FEE % 2). Add 2 → overflows.
        let err = f.add(2).unwrap_err();
        assert!(matches!(err, VMError::FeeTooHigh));
    }

    #[test]
    fn add_u64_max_rejects() {
        let mut f = CheckedFee::zero();
        let err = f.add(u64::MAX).unwrap_err();
        assert!(matches!(err, VMError::FeeTooHigh));
    }
}
