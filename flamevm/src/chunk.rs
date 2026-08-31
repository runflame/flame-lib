//! Small immutable binary objects with ordered links to more Chunks.

use std::convert::TryFrom;
use std::sync::Arc;

use merlin::Transcript;
use readerwriter::{
    Decodable, Encodable, ExactSizeEncodable, ReadError, Reader, WriteError, Writer,
};

use crate::errors::VMError;

/// Maximum payload carried directly by one [`Chunk`].
pub const MAX_CHUNK_PAYLOAD: usize = 1024;

/// Maximum number of ordered child references in one [`Chunk`].
pub const MAX_CHUNK_REFS: usize = 4;

/// Content identity of a [`Chunk`].
pub type ChunkID = [u8; 32];

/// One ordered child reference.
///
/// Residency is deliberately not part of the referenced identity. Replacing a
/// resident child with its pruned ID therefore leaves every ancestor ID intact.
#[derive(Clone, Debug)]
pub enum ChunkRef {
    /// The complete child body is available locally.
    Resident(Arc<Chunk>),
    /// Only the child's content identity is available.
    Pruned(ChunkID),
}

impl ChunkRef {
    /// Wraps a resident child.
    pub fn resident(chunk: Chunk) -> Self {
        Self::Resident(Arc::new(chunk))
    }

    /// Returns the referenced content identity.
    pub fn id(&self) -> ChunkID {
        match self {
            Self::Resident(chunk) => chunk.id(),
            Self::Pruned(id) => *id,
        }
    }

    /// Returns true when the child body is unavailable.
    pub fn is_pruned(&self) -> bool {
        matches!(self, Self::Pruned(_))
    }

    /// Borrows the resident child, if present.
    pub fn as_resident(&self) -> Option<&Chunk> {
        match self {
            Self::Resident(chunk) => Some(chunk),
            Self::Pruned(_) => None,
        }
    }

    /// Drops a resident body while retaining its identity.
    pub fn prune(&mut self) {
        if let Self::Resident(chunk) = self {
            *self = Self::Pruned(chunk.id());
        }
    }

    /// Replaces a pruned reference with its verified body.
    pub fn hydrate(&mut self, chunk: Chunk) -> Result<(), VMError> {
        if chunk.id() != self.id() {
            return Err(VMError::ChunkReferenceHashMismatch);
        }
        if matches!(self, Self::Pruned(_)) {
            *self = Self::resident(chunk);
        }
        Ok(())
    }
}

/// Immutable payload plus zero to four ordered child references.
///
/// The cached ID commits to the payload and the ordered child IDs, but not to
/// whether those child bodies are currently resident.
#[derive(Clone, Debug)]
pub struct Chunk {
    id: ChunkID,
    payload: Box<[u8]>,
    refs: Box<[ChunkRef]>,
}

impl Chunk {
    /// Constructs a checked Chunk and computes its content identity.
    pub fn new(payload: Vec<u8>, refs: Vec<ChunkRef>) -> Result<Self, VMError> {
        if payload.len() > MAX_CHUNK_PAYLOAD {
            return Err(VMError::ChunkPayloadTooLarge);
        }
        if refs.len() > MAX_CHUNK_REFS {
            return Err(VMError::ChunkTooManyReferences);
        }

        let mut chunk = Self {
            id: [0u8; 32],
            payload: payload.into_boxed_slice(),
            refs: refs.into_boxed_slice(),
        };
        let encoded = chunk.encode_to_vec();
        let mut transcript = Transcript::new(b"flamevm.chunk.id");
        transcript.append_message(b"chunk", &encoded);
        transcript.challenge_bytes(b"id", &mut chunk.id);
        Ok(chunk)
    }

    /// Returns this Chunk's cached content identity.
    pub fn id(&self) -> ChunkID {
        self.id
    }

    /// Borrows the inline payload.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Borrows the ordered child references.
    pub fn refs(&self) -> &[ChunkRef] {
        &self.refs
    }

    /// Consumes the Chunk into its payload and references.
    pub fn into_parts(self) -> (Vec<u8>, Vec<ChunkRef>) {
        (self.payload.into_vec(), self.refs.into_vec())
    }
}

/// Canonical encoding of one Chunk record:
///
/// 1. payload length as little-endian `u64`;
/// 2. payload bytes;
/// 3. reference count as little-endian `u64`;
/// 4. each ordered child [`ChunkID`].
///
/// Child bodies and residency are not encoded. Decoding therefore yields
/// pruned references, while a graph store can encode every resident Chunk as
/// its own record.
impl Encodable for Chunk {
    fn encode(&self, w: &mut impl Writer) -> Result<(), WriteError> {
        w.write_u64(b"chunk.payload.len", self.payload.len() as u64)?;
        w.write(b"chunk.payload", &self.payload)?;
        w.write_u64(b"chunk.refs.len", self.refs.len() as u64)?;
        for reference in self.refs.iter() {
            w.write(b"chunk.ref", &reference.id())?;
        }
        Ok(())
    }

    fn encoded_size_hint(&self) -> Option<usize> {
        Some(ExactSizeEncodable::encoded_size(self))
    }
}

impl ExactSizeEncodable for Chunk {
    fn encoded_size(&self) -> usize {
        16 + self.payload.len() + self.refs.len() * 32
    }
}

impl Decodable for Chunk {
    fn decode(r: &mut impl Reader) -> Result<Self, ReadError> {
        let payload_len = usize::try_from(r.read_u64()?).map_err(|_| ReadError::InvalidFormat)?;
        if payload_len > MAX_CHUNK_PAYLOAD {
            return Err(ReadError::InvalidFormat);
        }
        let payload = r.read_bytes(payload_len)?;

        let ref_count = usize::try_from(r.read_u64()?).map_err(|_| ReadError::InvalidFormat)?;
        if ref_count > MAX_CHUNK_REFS {
            return Err(ReadError::InvalidFormat);
        }
        let mut refs = Vec::with_capacity(ref_count);
        for _ in 0..ref_count {
            refs.push(ChunkRef::Pruned(r.read_u8x32()?));
        }
        Chunk::new(payload, refs).map_err(|_| ReadError::InvalidFormat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty(payload: u8) -> Chunk {
        Chunk::new(vec![payload], vec![]).unwrap()
    }

    #[test]
    fn enforces_limits() {
        assert!(Chunk::new(vec![0; 1024], vec![]).is_ok());
        assert!(matches!(
            Chunk::new(vec![0; 1025], vec![]),
            Err(VMError::ChunkPayloadTooLarge)
        ));

        let four = (0..4).map(|n| ChunkRef::resident(empty(n))).collect();
        let max = Chunk::new(vec![0; 1024], four).unwrap();
        let wire = max.encode_to_vec();
        let mut encoded = wire.as_slice();
        let decoded = encoded.read_all(Chunk::decode).unwrap();
        assert_eq!(decoded.id(), max.id());
        let five = (0..5).map(|n| ChunkRef::resident(empty(n))).collect();
        assert!(matches!(
            Chunk::new(vec![], five),
            Err(VMError::ChunkTooManyReferences)
        ));
    }

    #[test]
    fn pruning_and_hydration_preserve_identity() {
        let child = empty(7);
        let child_copy = child.clone();
        let mut child_ref = ChunkRef::resident(child);
        let parent_id = Chunk::new(vec![1], vec![child_ref.clone()]).unwrap().id();

        child_ref.prune();
        assert!(child_ref.is_pruned());
        assert_eq!(
            Chunk::new(vec![1], vec![child_ref.clone()]).unwrap().id(),
            parent_id
        );

        child_ref.hydrate(child_copy).unwrap();
        assert!(!child_ref.is_pruned());
        assert_eq!(
            Chunk::new(vec![1], vec![child_ref]).unwrap().id(),
            parent_id
        );
    }

    #[test]
    fn hydration_checks_the_committed_id() {
        let mut child_ref = ChunkRef::Pruned(empty(1).id());
        assert!(matches!(
            child_ref.hydrate(empty(2)),
            Err(VMError::ChunkReferenceHashMismatch)
        ));
        assert!(child_ref.is_pruned());
    }

    #[test]
    fn hydration_never_replaces_a_more_resident_body() {
        let grandchild = empty(1);
        let full = Chunk::new(vec![2], vec![ChunkRef::resident(grandchild.clone())]).unwrap();
        let partial = Chunk::new(vec![2], vec![ChunkRef::Pruned(grandchild.id())]).unwrap();
        assert_eq!(full.id(), partial.id());

        let mut reference = ChunkRef::resident(full);
        reference.hydrate(partial).unwrap();
        let child = reference.as_resident().unwrap();
        assert!(!child.refs()[0].is_pruned());
    }

    #[test]
    fn payload_and_reference_order_are_committed() {
        let a = ChunkRef::resident(empty(1));
        let b = ChunkRef::resident(empty(2));
        assert_ne!(
            Chunk::new(vec![0], vec![a.clone(), b.clone()])
                .unwrap()
                .id(),
            Chunk::new(vec![0], vec![b, a]).unwrap().id()
        );
        assert_ne!(
            Chunk::new(vec![0], vec![]).unwrap().id(),
            Chunk::new(vec![1], vec![]).unwrap().id()
        );
    }

    #[test]
    fn canonical_encoding_roundtrips_as_pruned_references() {
        let refs = vec![ChunkRef::Pruned([0x11; 32]), ChunkRef::Pruned([0x22; 32])];
        let chunk = Chunk::new(vec![0xaa, 0xbb, 0xcc], refs).unwrap();
        let bytes = chunk.encode_to_vec();

        let mut expected = Vec::new();
        expected.extend_from_slice(&3u64.to_le_bytes());
        expected.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
        expected.extend_from_slice(&2u64.to_le_bytes());
        expected.extend_from_slice(&[0x11; 32]);
        expected.extend_from_slice(&[0x22; 32]);
        assert_eq!(bytes, expected);
        assert_eq!(chunk.encoded_size(), bytes.len());

        let mut input = bytes.as_slice();
        let decoded = input.read_all(Chunk::decode).unwrap();
        assert_eq!(decoded.encode_to_vec(), bytes);
        assert_eq!(decoded.id(), chunk.id());
        assert!(decoded.refs().iter().all(ChunkRef::is_pruned));

        let mut transcript = Transcript::new(b"flamevm.chunk.id");
        transcript.append_message(b"chunk", &bytes);
        let mut expected_id = [0u8; 32];
        transcript.challenge_bytes(b"id", &mut expected_id);
        assert_eq!(chunk.id(), expected_id);
        assert_eq!(
            chunk.id(),
            [
                0xb1, 0x01, 0x69, 0x3c, 0x32, 0xf0, 0xf8, 0x50, 0x45, 0xcd, 0xd6, 0xbc, 0x8b, 0xdc,
                0xbb, 0x8b, 0xc5, 0xcf, 0xec, 0x09, 0x3d, 0x87, 0xf4, 0x8b, 0x09, 0x43, 0x93, 0x61,
                0x3a, 0x48, 0xde, 0xd5,
            ]
        );
    }

    #[test]
    fn resident_and_pruned_references_have_identical_wire_bytes() {
        let child = empty(7);
        let resident = Chunk::new(vec![1], vec![ChunkRef::resident(child.clone())]).unwrap();
        let pruned = Chunk::new(vec![1], vec![ChunkRef::Pruned(child.id())]).unwrap();
        assert_eq!(resident.encode_to_vec(), pruned.encode_to_vec());
        assert_eq!(resident.id(), pruned.id());
    }

    #[test]
    fn decoder_enforces_bounds_and_record_shape() {
        let mut oversized_payload = 1025u64.to_le_bytes().to_vec();
        oversized_payload.extend_from_slice(&vec![0; 1025]);
        oversized_payload.extend_from_slice(&0u64.to_le_bytes());
        assert!(matches!(
            Chunk::decode(&mut oversized_payload.as_slice()),
            Err(ReadError::InvalidFormat)
        ));

        let mut too_many_refs = 0u64.to_le_bytes().to_vec();
        too_many_refs.extend_from_slice(&5u64.to_le_bytes());
        assert!(matches!(
            Chunk::decode(&mut too_many_refs.as_slice()),
            Err(ReadError::InvalidFormat)
        ));

        let mut truncated = empty(1).encode_to_vec();
        truncated.pop();
        assert!(matches!(
            Chunk::decode(&mut truncated.as_slice()),
            Err(ReadError::InsufficientBytes)
        ));

        let mut trailing = empty(1).encode_to_vec();
        trailing.push(0);
        assert!(matches!(
            trailing.as_slice().read_all(Chunk::decode),
            Err(ReadError::TrailingBytes)
        ));
    }
}
