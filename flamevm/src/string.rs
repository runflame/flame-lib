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
}

impl From<Vec<u8>> for String {
    fn from(v: Vec<u8>) -> Self {
        String { inner: v }
    }
}
