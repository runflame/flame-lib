use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::CellError;

/// Maximum payload carried directly by one [`Cell`].
pub const MAX_CELL_PAYLOAD: usize = 8191;

/// Maximum number of ordered child references in one [`Cell`].
pub const MAX_CELL_REFS: usize = 4;

const REF_COUNT_SHIFT: u16 = 13;

/// Content identity of a [`Cell`].
pub type CellID = [u8; 32];

/// One ordered reference to a resident or unloaded Cell body.
#[derive(Clone, Debug)]
pub enum CellRef {
    Resident(Arc<Cell>),
    Pruned(CellID),
}

impl CellRef {
    pub fn resident(cell: Cell) -> Self {
        Self::Resident(Arc::new(cell))
    }

    pub fn resident_arc(cell: Arc<Cell>) -> Self {
        Self::Resident(cell)
    }

    pub fn pruned(id: CellID) -> Self {
        Self::Pruned(id)
    }

    pub fn id(&self) -> CellID {
        match self {
            Self::Resident(cell) => cell.id(),
            Self::Pruned(id) => *id,
        }
    }

    pub fn is_pruned(&self) -> bool {
        matches!(self, Self::Pruned(_))
    }

    pub fn as_resident(&self) -> Option<&Cell> {
        match self {
            Self::Resident(cell) => Some(cell),
            Self::Pruned(_) => None,
        }
    }

    pub fn as_resident_arc(&self) -> Option<&Arc<Cell>> {
        match self {
            Self::Resident(cell) => Some(cell),
            Self::Pruned(_) => None,
        }
    }

    pub fn to_pruned(&self) -> Self {
        Self::Pruned(self.id())
    }
}

/// Immutable payload plus zero to four ordered child references.
#[derive(Clone, Debug)]
pub struct Cell {
    id: CellID,
    payload: Box<[u8]>,
    refs: Box<[CellRef]>,
}

impl Cell {
    pub fn new(payload: Vec<u8>, refs: Vec<CellRef>) -> Result<Self, CellError> {
        if payload.len() > MAX_CELL_PAYLOAD {
            return Err(CellError::PayloadTooLarge {
                actual: payload.len(),
                max: MAX_CELL_PAYLOAD,
            });
        }
        if refs.len() > MAX_CELL_REFS {
            return Err(CellError::TooManyReferences {
                actual: refs.len(),
                max: MAX_CELL_REFS,
            });
        }

        let id = hash_record(&payload, &refs);
        Ok(Self {
            id,
            payload: payload.into_boxed_slice(),
            refs: refs.into_boxed_slice(),
        })
    }

    pub fn id(&self) -> CellID {
        self.id
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn refs(&self) -> &[CellRef] {
        &self.refs
    }

    pub fn into_parts(self) -> (Vec<u8>, Vec<CellRef>) {
        (self.payload.into_vec(), self.refs.into_vec())
    }

    pub fn encoded_size(&self) -> usize {
        2 + self.payload.len() + self.refs.len() * 32
    }

    /// Returns the canonical self-delimiting Cell record.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.encoded_size());
        bytes.extend_from_slice(&descriptor(self.payload.len(), self.refs.len()).to_le_bytes());
        bytes.extend_from_slice(&self.payload);
        for reference in &self.refs {
            bytes.extend_from_slice(&reference.id());
        }
        bytes
    }

    /// Decodes one record and advances `input` past it.
    pub fn decode_record(input: &mut &[u8]) -> Result<Self, CellError> {
        let descriptor_bytes = take(input, 2)?;
        let descriptor = u16::from_le_bytes([descriptor_bytes[0], descriptor_bytes[1]]);
        let payload_len = usize::from(descriptor & MAX_CELL_PAYLOAD as u16);
        let ref_count = usize::from(descriptor >> REF_COUNT_SHIFT);
        if ref_count > MAX_CELL_REFS {
            return Err(CellError::InvalidFormat);
        }

        let payload = take(input, payload_len)?.to_vec();
        let mut refs = Vec::with_capacity(ref_count);
        for _ in 0..ref_count {
            let bytes = take(input, 32)?;
            let mut id = [0; 32];
            id.copy_from_slice(bytes);
            refs.push(CellRef::Pruned(id));
        }
        Self::new(payload, refs).map_err(|_| CellError::InvalidFormat)
    }

    pub fn decode_exact(bytes: &[u8]) -> Result<Self, CellError> {
        let mut input = bytes;
        let cell = Self::decode_record(&mut input)?;
        if !input.is_empty() {
            return Err(CellError::TrailingBytes);
        }
        Ok(cell)
    }
}

fn descriptor(payload_len: usize, ref_count: usize) -> u16 {
    (payload_len as u16) | ((ref_count as u16) << REF_COUNT_SHIFT)
}

fn hash_record(payload: &[u8], refs: &[CellRef]) -> CellID {
    let mut hash = Sha256::new();
    hash.update(descriptor(payload.len(), refs.len()).to_le_bytes());
    hash.update(payload);
    for reference in refs {
        hash.update(reference.id());
    }
    hash.finalize().into()
}

fn take<'a>(input: &mut &'a [u8], len: usize) -> Result<&'a [u8], CellError> {
    if input.len() < len {
        return Err(CellError::InsufficientBytes);
    }
    let (head, tail) = input.split_at(len);
    *input = tail;
    Ok(head)
}

/// Resolves a reference within the caller's allowed execution context.
///
/// This is called for resident references too, allowing the context to charge
/// every logical Cell access independently of where its body came from.
pub trait CellResolver {
    fn resolve(&mut self, reference: &CellRef) -> Result<Arc<Cell>, CellError>;
}

/// Resolves a reference and verifies that the returned body has its committed ID.
pub fn resolve_cell<R: CellResolver + ?Sized>(
    resolver: &mut R,
    reference: &CellRef,
) -> Result<Arc<Cell>, CellError> {
    let expected = reference.id();
    let cell = resolver.resolve(reference)?;
    let actual = cell.id();
    if actual != expected {
        return Err(CellError::CellHashMismatch { expected, actual });
    }
    Ok(cell)
}

/// Resolver useful for fully resident graphs.
impl CellResolver for () {
    fn resolve(&mut self, reference: &CellRef) -> Result<Arc<Cell>, CellError> {
        match reference {
            CellRef::Resident(cell) => Ok(Arc::clone(cell)),
            CellRef::Pruned(id) => Err(CellError::MissingCell(*id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(byte: u8) -> Cell {
        Cell::new(vec![byte], vec![]).unwrap()
    }

    #[test]
    fn limits_and_invalid_descriptors_are_enforced() {
        assert!(Cell::new(vec![0; MAX_CELL_PAYLOAD], vec![]).is_ok());
        assert!(matches!(
            Cell::new(vec![0; MAX_CELL_PAYLOAD + 1], vec![]),
            Err(CellError::PayloadTooLarge { .. })
        ));
        let refs = (0..=MAX_CELL_REFS)
            .map(|n| CellRef::resident(leaf(n as u8)))
            .collect();
        assert!(matches!(
            Cell::new(vec![], refs),
            Err(CellError::TooManyReferences { .. })
        ));

        for count in 5u16..=7 {
            let bytes = (count << REF_COUNT_SHIFT).to_le_bytes();
            assert!(matches!(
                Cell::decode_exact(&bytes),
                Err(CellError::InvalidFormat)
            ));
        }
    }

    #[test]
    fn encoding_and_plain_sha256_identity_are_canonical() {
        let cell = Cell::new(
            vec![0xaa, 0xbb, 0xcc],
            vec![CellRef::pruned([0x11; 32]), CellRef::pruned([0x22; 32])],
        )
        .unwrap();
        let bytes = cell.encode();
        assert_eq!(&bytes[..2], &0x4003u16.to_le_bytes());
        assert_eq!(cell.id(), <[u8; 32]>::from(Sha256::digest(&bytes)));
        let decoded = Cell::decode_exact(&bytes).unwrap();
        assert_eq!(decoded.encode(), bytes);
        assert_eq!(decoded.id(), cell.id());
        assert!(decoded.refs().iter().all(CellRef::is_pruned));
    }

    #[test]
    fn residency_does_not_change_parent_identity() {
        let child = leaf(7);
        let resident = Cell::new(vec![1], vec![CellRef::resident(child.clone())]).unwrap();
        let pruned = Cell::new(vec![1], vec![CellRef::pruned(child.id())]).unwrap();
        assert_eq!(resident.encode(), pruned.encode());
        assert_eq!(resident.id(), pruned.id());
    }

    #[test]
    fn decoder_is_exact() {
        let bytes = leaf(1).encode();
        assert!(matches!(
            Cell::decode_exact(&bytes[..bytes.len() - 1]),
            Err(CellError::InsufficientBytes)
        ));
        let mut trailing = bytes;
        trailing.push(0);
        assert!(matches!(
            Cell::decode_exact(&trailing),
            Err(CellError::TrailingBytes)
        ));
    }

    #[test]
    fn resolver_output_is_hash_checked() {
        struct Wrong(Cell);
        impl CellResolver for Wrong {
            fn resolve(&mut self, _: &CellRef) -> Result<Arc<Cell>, CellError> {
                Ok(Arc::new(self.0.clone()))
            }
        }

        let expected = leaf(1);
        let reference = CellRef::pruned(expected.id());
        let mut wrong = Wrong(leaf(2));
        assert!(matches!(
            resolve_cell(&mut wrong, &reference),
            Err(CellError::CellHashMismatch { .. })
        ));
    }

    #[test]
    fn resident_access_still_passes_through_the_resolver() {
        struct Counting(usize);
        impl CellResolver for Counting {
            fn resolve(&mut self, reference: &CellRef) -> Result<Arc<Cell>, CellError> {
                self.0 += 1;
                reference
                    .as_resident_arc()
                    .cloned()
                    .ok_or_else(|| CellError::MissingCell(reference.id()))
            }
        }

        let reference = CellRef::resident(leaf(1));
        let mut resolver = Counting(0);
        resolve_cell(&mut resolver, &reference).unwrap();
        assert_eq!(resolver.0, 1);
    }
}
