//! Variable-length binary string. Byte-aligned; plain data (copyable and
//! portable). Many opcodes treat strings as input/output for parsing and
//! assembly — see methods on `String` below.

/// Binary string, byte-aligned. Plain data; copyable and portable.
#[derive(Clone, Debug)]
pub struct String {
    inner: Vec<u8>,
}

impl String {
    /// Returns the byte contents.
    pub fn as_bytes(&self) -> &[u8] {
        &self.inner
    }

    /// Returns the length in bytes.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns true if the string is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns `self` with `other` appended (`self || other`). Used by
    /// `0x46 append`.
    pub fn append(mut self, other: &String) -> String {
        self.inner.extend_from_slice(&other.inner);
        self
    }

    /// Returns `self` with `bytes` appended. Used by `0x44 writebits`,
    /// `0x45 writeint`, `0x47 writezeros`.
    pub fn append_bytes(mut self, bytes: &[u8]) -> String {
        self.inner.extend_from_slice(bytes);
        self
    }

    /// Splits off the first `n` bytes. Returns `(remainder, head)` on
    /// success, `None` if the string is too short. Used by `0x42 readstr`.
    pub fn split_at(self, n: usize) -> Option<(String, String)> {
        if n > self.inner.len() {
            return None;
        }
        let (head, tail) = self.inner.split_at(n);
        Some((String::from(tail.to_vec()), String::from(head.to_vec())))
    }

    /// Returns a new String with every bit inverted. Used by `0x48 bitnot`.
    pub fn bit_not(self) -> String {
        String::from(self.inner.iter().map(|b| !b).collect::<Vec<u8>>())
    }

    /// Bytewise OR. Returns `None` if operand lengths differ.
    pub fn bit_or(self, other: &String) -> Option<String> {
        if self.inner.len() != other.inner.len() {
            return None;
        }
        let out: Vec<u8> = self
            .inner
            .iter()
            .zip(&other.inner)
            .map(|(a, b)| a | b)
            .collect();
        Some(String::from(out))
    }

    /// Bytewise AND. Returns `None` if operand lengths differ.
    pub fn bit_and(self, other: &String) -> Option<String> {
        if self.inner.len() != other.inner.len() {
            return None;
        }
        let out: Vec<u8> = self
            .inner
            .iter()
            .zip(&other.inner)
            .map(|(a, b)| a & b)
            .collect();
        Some(String::from(out))
    }

    /// Bytewise XOR. Returns `None` if operand lengths differ.
    pub fn bit_xor(self, other: &String) -> Option<String> {
        if self.inner.len() != other.inner.len() {
            return None;
        }
        let out: Vec<u8> = self
            .inner
            .iter()
            .zip(&other.inner)
            .map(|(a, b)| a ^ b)
            .collect();
        Some(String::from(out))
    }

    /// Shifts bits left by `n` (treating the string as a big-endian
    /// bigint: byte 0 holds the most significant bits). Returns
    /// `(shifted, removed)` where:
    /// - `shifted` is the same length as `self`, with bits shifted toward
    ///   byte 0 and zero-filled at the low end (bytes near the tail).
    /// - `removed` holds the `n` bits that fell off the top of `self`,
    ///   in their original MSB-first order, **zero-padded on the left**
    ///   to the nearest byte boundary.
    ///
    /// Used by `0x4c shiftleft`.
    pub fn shift_left(self, n: usize) -> (String, String) {
        let total = 8 * self.inner.len();
        let mut shifted = vec![0u8; self.inner.len()];
        for i in 0..total {
            if let Some(src) = i.checked_add(n) {
                if src < total {
                    let bit = bit_at(&self.inner, src);
                    set_bit(&mut shifted, i, bit);
                }
            }
        }
        let removed_bytes = (n + 7) / 8;
        let pad = removed_bytes * 8 - n;
        let mut removed = vec![0u8; removed_bytes];
        for i in 0..n {
            if i < total {
                let bit = bit_at(&self.inner, i);
                set_bit(&mut removed, pad + i, bit);
            }
        }
        (String::from(shifted), String::from(removed))
    }

    /// Shifts bits right by `n`. Returns `(shifted, removed)` where:
    /// - `shifted` is the same length as `self`, bits shifted away from
    ///   byte 0, zero-filled at the high end.
    /// - `removed` holds the `n` bits that fell off the low end of
    ///   `self`, **zero-padded on the right**.
    ///
    /// Used by `0x4d shiftright`.
    pub fn shift_right(self, n: usize) -> (String, String) {
        let total = 8 * self.inner.len();
        let mut shifted = vec![0u8; self.inner.len()];
        for i in 0..total {
            if let Some(src) = i.checked_sub(n) {
                let bit = bit_at(&self.inner, src);
                set_bit(&mut shifted, i, bit);
            }
        }
        let removed_bytes = (n + 7) / 8;
        let mut removed = vec![0u8; removed_bytes];
        for i in 0..n {
            // Source bit position in original = total - n + i.
            if let Some(src) = (total + i).checked_sub(n) {
                if src < total {
                    let bit = bit_at(&self.inner, src);
                    set_bit(&mut removed, i, bit);
                }
            }
        }
        (String::from(shifted), String::from(removed))
    }
}

impl From<Vec<u8>> for String {
    fn from(v: Vec<u8>) -> Self {
        String { inner: v }
    }
}

// ── Internal bit helpers (MSB-first numbering) ───────────────────

fn bit_at(bytes: &[u8], pos: usize) -> u8 {
    let byte = pos / 8;
    let bit = 7 - (pos % 8);
    if byte >= bytes.len() {
        0
    } else {
        (bytes[byte] >> bit) & 1
    }
}

fn set_bit(bytes: &mut [u8], pos: usize, value: u8) {
    let byte = pos / 8;
    let bit = 7 - (pos % 8);
    if byte >= bytes.len() {
        return;
    }
    if value & 1 != 0 {
        bytes[byte] |= 1 << bit;
    } else {
        bytes[byte] &= !(1 << bit);
    }
}
