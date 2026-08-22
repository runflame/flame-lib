use std::fmt::Formatter;

#[derive(Debug, Clone, PartialEq)]
pub enum WriteError {
    InsufficientCapacity,
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            WriteError::InsufficientCapacity => write!(f, "insufficient capacity"),
        }
    }
}

impl std::error::Error for WriteError {}

/// Interface for writing binary data.
pub trait Writer {
    /// Writes bytes with the given label. If there is no sufficient capacity,
    /// performs no modifications and returns WriteError::InsufficientCapacity.
    fn write(&mut self, label: &'static [u8], src: &[u8]) -> Result<(), WriteError>;

    /// Remaining number of bytes that can be written.
    fn remaining_capacity(&self) -> usize;

    /// Writes a single byte.
    #[inline]
    fn write_u8(&mut self, label: &'static [u8], x: u8) -> Result<(), WriteError> {
        self.write(label, &[x])
    }

    /// Writes a LE32-encoded integer.
    #[inline]
    fn write_u32(&mut self, label: &'static [u8], x: u32) -> Result<(), WriteError> {
        self.write(label, &x.to_le_bytes())
    }

    /// Writes a LE64-encoded integer.
    #[inline]
    fn write_u64(&mut self, label: &'static [u8], x: u64) -> Result<(), WriteError> {
        self.write(label, &x.to_le_bytes())
    }
}

/// Allocation-free writer that only counts encoded bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SizeWriter {
    len: usize,
}

impl SizeWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Writer for SizeWriter {
    fn write(&mut self, _label: &'static [u8], src: &[u8]) -> Result<(), WriteError> {
        self.len = self
            .len
            .checked_add(src.len())
            .ok_or(WriteError::InsufficientCapacity)?;
        Ok(())
    }

    fn remaining_capacity(&self) -> usize {
        usize::MAX.saturating_sub(self.len)
    }
}

impl Writer for Vec<u8> {
    #[inline]
    fn write(&mut self, _label: &'static [u8], src: &[u8]) -> Result<(), WriteError> {
        self.extend_from_slice(src);
        Ok(())
    }

    #[inline]
    fn write_u8(&mut self, _label: &'static [u8], x: u8) -> Result<(), WriteError> {
        self.push(x);
        Ok(())
    }

    #[inline]
    fn remaining_capacity(&self) -> usize {
        usize::MAX
    }
}

impl Writer for &mut [u8] {
    #[inline]
    fn write(&mut self, _label: &'static [u8], src: &[u8]) -> Result<(), WriteError> {
        let n = src.len();
        if n <= self.remaining_capacity() {
            let (a, b) = std::mem::take(self).split_at_mut(n);
            a.copy_from_slice(&src[..n]);
            *self = b;
            Ok(())
        } else {
            Err(WriteError::InsufficientCapacity)
        }
    }

    #[inline]
    fn remaining_capacity(&self) -> usize {
        self.len()
    }
}
