use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::CellError;

/// Maximum ordinary payload, using all twelve bits of the length field.
pub const MAX_CELL_PAYLOAD: usize = 4095;
pub const MAX_CELL_REFS: usize = 4;
pub const MAX_CELL_LEVEL: u8 = 15;
const PRUNED_FLAG: u16 = 0x8000;
const MASK_BITS: u16 = 0x7fff;
const REF_COUNT_SHIFT: u16 = 12;
const LENGTH_BITS: u16 = 0x0fff;

pub type CellID = [u8; 32];

/// Hashes and depths at significant levels, including the factual (highest) one.
/// An unloaded reference retains this information; a bare ID cannot supply it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellCommitment {
    mask: u16,
    hashes: Box<[CellID]>,
    depths: Box<[u16]>,
}

impl CellCommitment {
    pub fn new(mask: u16, hashes: Vec<CellID>, depths: Vec<u16>) -> Result<Self, CellError> {
        let count = mask.count_ones() as usize + 1;
        if mask & !MASK_BITS != 0 || hashes.len() != count || depths.len() != count {
            return Err(CellError::InvalidFormat);
        }
        Ok(Self {
            mask,
            hashes: hashes.into(),
            depths: depths.into(),
        })
    }
    pub fn mask(&self) -> u16 {
        self.mask
    }
    pub fn level(&self) -> u8 {
        mask_level(self.mask)
    }
    pub fn id(&self) -> CellID {
        *self.hashes.last().expect("nonempty commitment")
    }
    pub fn hashes(&self) -> &[CellID] {
        &self.hashes
    }
    pub fn depths(&self) -> &[u16] {
        &self.depths
    }
    pub fn hash(&self, level: u8) -> Result<CellID, CellError> {
        Ok(self.hashes[hash_index(self.mask, level)?])
    }
    pub fn depth(&self, level: u8) -> Result<u16, CellError> {
        Ok(self.depths[hash_index(self.mask, level)?])
    }
    fn encode(&self, bytes: &mut Vec<u8>) {
        bytes.extend_from_slice(&self.mask.to_le_bytes());
        encode_pairs(bytes, &self.hashes, &self.depths);
    }
    fn encoded_size(&self) -> usize {
        2 + self.hashes.len() * 34
    }
    fn decode(input: &mut &[u8]) -> Result<Self, CellError> {
        let mask = read_u16(input)?;
        if mask & !MASK_BITS != 0 {
            return Err(CellError::InvalidFormat);
        }
        let (hashes, depths) = decode_pairs(input, mask.count_ones() as usize + 1)?;
        Self::new(mask, hashes, depths)
    }
}

/// Physical residency is independent of explicit proof pruning.
#[derive(Clone, Debug)]
pub enum CellRef {
    Resident(Arc<Cell>),
    Unloaded(Arc<CellCommitment>),
    /// Lookup handle only: resolve it before storing it in a Cell.
    Unresolved(CellID),
}
impl From<Cell> for CellRef {
    fn from(cell: Cell) -> Self {
        Self::resident(cell)
    }
}
impl From<Arc<Cell>> for CellRef {
    fn from(cell: Arc<Cell>) -> Self {
        Self::resident(cell)
    }
}
impl CellRef {
    pub fn resident(cell: impl Into<Arc<Cell>>) -> Self {
        Self::Resident(cell.into())
    }
    pub fn unresolved(id: CellID) -> Self {
        Self::Unresolved(id)
    }
    pub fn id(&self) -> CellID {
        match self {
            Self::Resident(cell) => cell.id(),
            Self::Unloaded(commitment) => commitment.id(),
            Self::Unresolved(id) => *id,
        }
    }
    pub fn commitment(&self) -> Result<&CellCommitment, CellError> {
        match self {
            Self::Resident(cell) => Ok(cell.commitment()),
            Self::Unloaded(commitment) => Ok(commitment),
            Self::Unresolved(id) => Err(CellError::MissingCellMetadata(*id)),
        }
    }
    pub fn validate_child(&self) -> Result<(), CellError> {
        if self.commitment()?.depths.contains(&u16::MAX) {
            return Err(CellError::DepthOverflow);
        }
        Ok(())
    }
    pub fn is_unloaded(&self) -> bool {
        matches!(self, Self::Unloaded(_))
    }
    pub fn as_resident(&self) -> Option<&Cell> {
        self.as_resident_arc().map(AsRef::as_ref)
    }
    pub fn as_resident_arc(&self) -> Option<&Arc<Cell>> {
        match self {
            Self::Resident(cell) => Some(cell),
            _ => None,
        }
    }
    pub fn to_unloaded(&self) -> Result<Self, CellError> {
        match self {
            Self::Unloaded(_) => Ok(self.clone()),
            _ => Ok(Self::Unloaded(Arc::new(self.commitment()?.clone()))),
        }
    }
}

/// Immutable ordinary or explicit pruned Cell. Clones and virtual views share data.
#[derive(Clone, Debug)]
pub struct Cell {
    data: Arc<CellData>,
}

#[derive(Debug)]
struct CellData {
    pruned: bool,
    payload: Box<[u8]>,
    refs: Box<[CellRef]>,
    commitment: CellCommitment,
}

// Avoid recursive destruction of long resident snake chains.
impl Drop for CellData {
    fn drop(&mut self) {
        let mut pending = std::mem::take(&mut self.refs).into_vec();
        while let Some(reference) = pending.pop() {
            if let CellRef::Resident(cell) = reference
                && let Ok(cell) = Arc::try_unwrap(cell)
                && let Ok(mut data) = Arc::try_unwrap(cell.data)
            {
                pending.extend(std::mem::take(&mut data.refs).into_vec());
            }
        }
    }
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
        let mut mask = 0;
        for reference in &refs {
            reference.validate_child()?;
            mask |= reference.commitment()?.mask;
        }
        let mut hashes = Vec::with_capacity(mask.count_ones() as usize + 1);
        let mut depths = Vec::with_capacity(hashes.capacity());
        for level in significant_levels(mask) {
            let mut depth = 0;
            for reference in &refs {
                depth = depth.max(reference.commitment()?.depth(level)? + 1);
            }
            let preimage = ordinary_preimage(&payload, &refs, mask, level, hashes.last())?;
            hashes.push(Sha256::digest(preimage).into());
            depths.push(depth);
        }
        Ok(Self {
            data: Arc::new(CellData {
                pruned: false,
                payload: payload.into(),
                refs: refs.into(),
                commitment: CellCommitment::new(mask, hashes, depths)?,
            }),
        })
    }

    /// Constructs a pruning record. Retained hashes are claims until checked
    /// against an expected root; they do not recover or authenticate hidden data.
    /// All hashes precede all depths, in increasing significant-level order.
    pub fn from_pruned(
        mask: u16,
        mut hashes: Vec<CellID>,
        mut depths: Vec<u16>,
    ) -> Result<Self, CellError> {
        let count = mask.count_ones() as usize;
        if mask == 0 || mask & !MASK_BITS != 0 || hashes.len() != count || depths.len() != count {
            return Err(CellError::InvalidFormat);
        }
        let mut payload = Vec::with_capacity(count * 34);
        encode_pairs(&mut payload, &hashes, &depths);
        let mut hasher = Sha256::new();
        hasher.update((PRUNED_FLAG | mask).to_le_bytes());
        hasher.update(&payload);
        hashes.push(hasher.finalize().into());
        depths.push(0);
        Ok(Self {
            data: Arc::new(CellData {
                pruned: true,
                payload: payload.into(),
                refs: Box::new([]),
                commitment: CellCommitment::new(mask, hashes, depths)?,
            }),
        })
    }
    pub fn id(&self) -> CellID {
        self.commitment().id()
    }
    pub fn commitment(&self) -> &CellCommitment {
        &self.data.commitment
    }
    pub fn level_mask(&self) -> u16 {
        self.commitment().mask()
    }
    pub fn level(&self) -> u8 {
        self.commitment().level()
    }
    pub fn hash(&self, level: u8) -> Result<CellID, CellError> {
        self.commitment().hash(level)
    }
    pub fn depth(&self, level: u8) -> Result<u16, CellError> {
        self.commitment().depth(level)
    }
    pub fn is_pruned(&self) -> bool {
        self.data.pruned
    }
    /// Physical payload. On pruned cells these are retained hashes/depths,
    /// not application data. Use CellSlice or a view for checked access.
    pub fn payload(&self) -> &[u8] {
        &self.data.payload
    }
    pub fn refs(&self) -> &[CellRef] {
        &self.data.refs
    }
    pub fn into_parts(self) -> (Vec<u8>, Vec<CellRef>) {
        match Arc::try_unwrap(self.data) {
            Ok(mut data) => (
                std::mem::take(&mut data.payload).into_vec(),
                std::mem::take(&mut data.refs).into_vec(),
            ),
            Err(data) => (data.payload.to_vec(), data.refs.to_vec()),
        }
    }

    pub fn detached(&self) -> Self {
        if self.refs().iter().all(CellRef::is_unloaded) {
            return self.clone();
        }
        Self {
            data: Arc::new(CellData {
                pruned: self.is_pruned(),
                payload: self.payload().into(),
                refs: self
                    .refs()
                    .iter()
                    .map(|r| r.to_unloaded().expect("validated Cell reference"))
                    .collect(),
                commitment: self.commitment().clone(),
            }),
        }
    }

    /// Replaces a subtree at `level`, preserving commitments through level - 1.
    /// Choose ONE level throughout a proof. Reusing it continues that proof;
    /// advancing beyond its factual level starts a proof of the proof.
    pub fn prune(&self, level: u8) -> Result<Self, CellError> {
        if level == 0 || level > MAX_CELL_LEVEL || level < self.level() {
            return Err(CellError::InvalidLevel);
        }
        let source_mask = apply_mask(self.level_mask(), level - 1);
        let mask = source_mask | (1 << (level - 1));
        let mut hashes = Vec::new();
        let mut depths = Vec::new();
        for source_level in significant_levels(source_mask) {
            hashes.push(self.hash(source_level)?);
            depths.push(self.depth(source_level)?);
        }
        Self::from_pruned(mask, hashes, depths)
    }
    pub fn virtualize(&self, level: u8) -> Result<CellView, CellError> {
        if level > self.level() {
            return Err(CellError::InvalidLevel);
        }
        Ok(CellView {
            cell: self.clone(),
            level,
        })
    }
    pub fn record_size(&self) -> usize {
        2 + self.payload().len()
            + self
                .refs()
                .iter()
                .map(|r| {
                    r.commitment()
                        .expect("validated Cell reference")
                        .encoded_size()
                })
                .sum::<usize>()
    }
    fn descriptor(&self) -> u16 {
        if self.is_pruned() {
            PRUNED_FLAG | self.level_mask()
        } else {
            descriptor(self.payload().len(), self.refs().len())
        }
    }
    /// Canonical physical record with complete summaries for unloaded children.
    /// Virtual views are deliberately not serializable as physical cells.
    pub fn encode_record(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.record_size());
        bytes.extend_from_slice(&self.descriptor().to_le_bytes());
        bytes.extend_from_slice(self.payload());
        for reference in self.refs() {
            reference
                .commitment()
                .expect("validated Cell reference")
                .encode(&mut bytes);
        }
        bytes
    }
    /// SHA256 preimage at the requested significant hash level. At higher
    /// ordinary levels the previous computed hash replaces the original payload.
    pub fn hash_preimage(&self, level: u8) -> Result<Vec<u8>, CellError> {
        let index = hash_index(self.level_mask(), level)?;
        if self.is_pruned() {
            if index + 1 != self.commitment().hashes.len() {
                return Err(CellError::PrunedCell);
            }
            return Ok(self.encode_record());
        }
        let significant = significant_levels(self.level_mask())
            .nth(index)
            .expect("hash index valid");
        let previous = index.checked_sub(1).map(|i| &self.commitment().hashes[i]);
        ordinary_preimage(
            self.payload(),
            self.refs(),
            self.level_mask(),
            significant,
            previous,
        )
    }
    pub fn decode_record(input: &mut &[u8]) -> Result<Self, CellError> {
        let descriptor = read_u16(input)?;
        if descriptor & PRUNED_FLAG != 0 {
            let mask = descriptor & MASK_BITS;
            if mask == 0 {
                return Err(CellError::InvalidFormat);
            }
            let (hashes, depths) = decode_pairs(input, mask.count_ones() as usize)?;
            return Self::from_pruned(mask, hashes, depths);
        }
        let payload_len = usize::from(descriptor & LENGTH_BITS);
        let ref_count = usize::from(descriptor >> REF_COUNT_SHIFT);
        if payload_len > MAX_CELL_PAYLOAD || ref_count > MAX_CELL_REFS {
            return Err(CellError::InvalidFormat);
        }
        let payload = take(input, payload_len)?.to_vec();
        let mut refs = Vec::with_capacity(ref_count);
        for _ in 0..ref_count {
            refs.push(CellRef::Unloaded(Arc::new(CellCommitment::decode(input)?)));
        }
        Self::new(payload, refs)
    }
    pub fn decode_record_exact(mut bytes: &[u8]) -> Result<Self, CellError> {
        let cell = Self::decode_record(&mut bytes)?;
        if !bytes.is_empty() {
            return Err(CellError::TrailingBytes);
        }
        Ok(cell)
    }
}

/// A shared, read-only cap on a physical Cell. Missing bodies are not recreated.
#[derive(Clone, Debug)]
pub struct CellView {
    cell: Cell,
    level: u8,
}
impl CellView {
    /// The cap may lie in a gap of the sparse mask.
    pub fn level(&self) -> u8 {
        self.level
    }
    pub fn level_mask(&self) -> u16 {
        apply_mask(self.cell.level_mask(), self.level)
    }
    pub fn id(&self) -> CellID {
        self.cell.hash(self.level).expect("valid view cap")
    }
    pub fn depth(&self) -> u16 {
        self.cell.depth(self.level).expect("valid view cap")
    }
    pub fn hash(&self, level: u8) -> Result<CellID, CellError> {
        if level > MAX_CELL_LEVEL {
            return Err(CellError::InvalidLevel);
        }
        self.cell.hash(level.min(self.level))
    }
    pub fn is_available(&self) -> bool {
        !self.cell.is_pruned() || self.level >= self.cell.level()
    }
    pub fn is_pruned(&self) -> Result<bool, CellError> {
        self.check_available()?;
        Ok(self.cell.is_pruned())
    }
    pub fn payload(&self) -> Result<&[u8], CellError> {
        self.check_available()?;
        Ok(self.cell.payload())
    }
    pub fn reference_count(&self) -> Result<usize, CellError> {
        self.check_available()?;
        Ok(self.cell.refs().len())
    }
    /// Resolve by factual ID, validate all commitments, then inherit the cap.
    pub fn reference<R: CellResolver + ?Sized>(
        &self,
        index: usize,
        resolver: &mut R,
    ) -> Result<Self, CellError> {
        self.check_available()?;
        let reference = self
            .cell
            .refs()
            .get(index)
            .ok_or(CellError::InsufficientReferences)?;
        let cell = resolve_cell(resolver, reference)?;
        Ok(Self {
            cell: cell.as_ref().clone(),
            level: self.level,
        })
    }
    pub fn virtualize(&self, level: u8) -> Result<Self, CellError> {
        if level > self.level {
            return Err(CellError::InvalidLevel);
        }
        Ok(Self {
            cell: self.cell.clone(),
            level,
        })
    }
    fn check_available(&self) -> Result<(), CellError> {
        if self.is_available() {
            Ok(())
        } else {
            Err(CellError::PrunedCell)
        }
    }
}

fn descriptor(payload_len: usize, ref_count: usize) -> u16 {
    (payload_len as u16) | ((ref_count as u16) << REF_COUNT_SHIFT)
}
fn mask_level(mask: u16) -> u8 {
    (u16::BITS - mask.leading_zeros()) as u8
}
fn apply_mask(mask: u16, level: u8) -> u16 {
    mask & ((1u16 << level) - 1)
}
fn significant_levels(mask: u16) -> impl Iterator<Item = u8> {
    (0..=MAX_CELL_LEVEL).filter(move |&level| level == 0 || mask & (1 << (level - 1)) != 0)
}
fn hash_index(mask: u16, level: u8) -> Result<usize, CellError> {
    if level > MAX_CELL_LEVEL {
        return Err(CellError::InvalidLevel);
    }
    Ok(apply_mask(mask, level).count_ones() as usize)
}
fn ordinary_preimage(
    payload: &[u8],
    refs: &[CellRef],
    mask: u16,
    level: u8,
    previous: Option<&CellID>,
) -> Result<Vec<u8>, CellError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&descriptor(payload.len(), refs.len()).to_le_bytes());
    bytes.extend_from_slice(&apply_mask(mask, level).to_le_bytes());
    bytes.extend_from_slice(previous.map_or(payload, |hash| hash.as_slice()));
    for reference in refs {
        bytes.extend_from_slice(&reference.commitment()?.depth(level)?.to_le_bytes());
    }
    for reference in refs {
        bytes.extend_from_slice(&reference.commitment()?.hash(level)?);
    }
    Ok(bytes)
}
fn encode_pairs(bytes: &mut Vec<u8>, hashes: &[CellID], depths: &[u16]) {
    for hash in hashes {
        bytes.extend_from_slice(hash);
    }
    for depth in depths {
        bytes.extend_from_slice(&depth.to_le_bytes());
    }
}
fn decode_pairs(input: &mut &[u8], count: usize) -> Result<(Vec<CellID>, Vec<u16>), CellError> {
    if input.len() < count * 34 {
        return Err(CellError::InsufficientBytes);
    }
    let mut hashes = Vec::with_capacity(count);
    let mut depths = Vec::with_capacity(count);
    for _ in 0..count {
        hashes.push(take(input, 32)?.try_into().expect("length checked"));
    }
    for _ in 0..count {
        depths.push(read_u16(input)?);
    }
    Ok((hashes, depths))
}
fn read_u16(input: &mut &[u8]) -> Result<u16, CellError> {
    Ok(u16::from_le_bytes(
        take(input, 2)?.try_into().expect("length checked"),
    ))
}
fn take<'a>(input: &mut &'a [u8], len: usize) -> Result<&'a [u8], CellError> {
    if input.len() < len {
        return Err(CellError::InsufficientBytes);
    }
    let (head, tail) = input.split_at(len);
    *input = tail;
    Ok(head)
}

/// Called for resident accesses too, so callers can meter logical accesses.
pub trait CellResolver {
    fn resolve(&mut self, reference: &CellRef) -> Result<Arc<Cell>, CellError>;
}
pub fn resolve_cell<R: CellResolver + ?Sized>(
    resolver: &mut R,
    reference: &CellRef,
) -> Result<Arc<Cell>, CellError> {
    let cell = resolver.resolve(reference)?;
    if cell.id() != reference.id() {
        return Err(CellError::CellHashMismatch {
            expected: reference.id(),
            actual: cell.id(),
        });
    }
    if let Ok(expected) = reference.commitment()
        && expected != cell.commitment()
    {
        return Err(CellError::CellCommitmentMismatch(reference.id()));
    }
    Ok(cell)
}
impl CellResolver for () {
    fn resolve(&mut self, reference: &CellRef) -> Result<Arc<Cell>, CellError> {
        reference
            .as_resident_arc()
            .cloned()
            .ok_or_else(|| CellError::MissingCell(reference.id()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(n: u8) -> Cell {
        Cell::new(vec![n], vec![]).unwrap()
    }
    fn parent(children: Vec<Cell>) -> Cell {
        Cell::new(vec![42], children.into_iter().map(CellRef::from).collect()).unwrap()
    }

    #[test]
    fn descriptor_space_is_exhaustively_validated() {
        let mut ordinary = 0;
        let mut pruned = 0;
        for descriptor in 0..=u16::MAX {
            let mut bytes = descriptor.to_le_bytes().to_vec();
            let valid = if descriptor & PRUNED_FLAG != 0 {
                let mask = descriptor & MASK_BITS;
                bytes.resize(2 + mask.count_ones() as usize * 34, 0);
                if mask != 0 {
                    pruned += 1;
                }
                mask != 0
            } else {
                let n = descriptor >> 12;
                let len = descriptor & LENGTH_BITS;
                let valid = n <= 4 && len <= MAX_CELL_PAYLOAD as u16;
                if valid {
                    ordinary += 1;
                    bytes.resize(2 + len as usize + n as usize * 36, 0);
                }
                valid
            };
            let decoded = Cell::decode_record_exact(&bytes);
            assert_eq!(decoded.is_ok(), valid, "{descriptor:04x}");
            if let Ok(cell) = decoded {
                assert_eq!(cell.encode_record(), bytes);
            }
        }
        assert_eq!(ordinary, 5 * 4096);
        assert_eq!(pruned, 32767);
    }

    #[test]
    fn ordinary_payload_uses_all_twelve_length_bits() {
        for length in [2048, 4095] {
            let cell = Cell::new(vec![0x5a; length], vec![]).unwrap();
            let encoded = cell.encode_record();
            assert_eq!(&encoded[..2], &(length as u16).to_le_bytes());
            assert_eq!(
                Cell::decode_record_exact(&encoded).unwrap().payload(),
                cell.payload()
            );
        }
        assert!(matches!(
            Cell::new(vec![0; 4096], vec![]),
            Err(CellError::PayloadTooLarge {
                actual: 4096,
                max: 4095
            })
        ));
    }

    #[test]
    fn pruned_wire_has_all_hashes_then_all_little_endian_depths() {
        let cell =
            Cell::from_pruned(0b101, vec![[0x11; 32], [0x22; 32]], vec![0x1234, 0x5678]).unwrap();
        let bytes = cell.encode_record();
        assert_eq!(&bytes[..2], &[5, 0x80]);
        assert_eq!(&bytes[2..34], &[0x11; 32]);
        assert_eq!(&bytes[34..66], &[0x22; 32]);
        assert_eq!(&bytes[66..], &[0x34, 0x12, 0x78, 0x56]);
        assert_eq!(cell.hash(0).unwrap(), [0x11; 32]);
        assert_eq!(cell.hash(1).unwrap(), [0x22; 32]);
        assert_eq!(cell.hash(2).unwrap(), [0x22; 32]);
        assert_eq!(cell.depth(2).unwrap(), 0x5678);
        assert_eq!(cell.depth(3).unwrap(), 0);
        assert_eq!(cell.id(), <CellID>::from(Sha256::digest(&bytes)));
        assert_eq!(
            Cell::decode_record_exact(&bytes).unwrap().commitment(),
            cell.commitment()
        );
        assert!(Cell::from_pruned(0, vec![], vec![]).is_err());
        assert!(Cell::from_pruned(0x8000, vec![[0; 32]], vec![0]).is_err());
        assert!(Cell::from_pruned(3, vec![[0; 32]], vec![0]).is_err());
    }

    #[test]
    fn canonical_preimages_and_reference_metadata_survive_detachment() {
        let unique = leaf(9);
        let original_allocation = unique.payload().as_ptr();
        let (bytes, refs) = unique.into_parts();
        assert_eq!(bytes.as_ptr(), original_allocation);
        assert!(refs.is_empty());
        let child = leaf(7);
        let payload = child.payload().as_ptr();
        let reference = CellRef::from(child);
        assert_eq!(reference.as_resident().unwrap().payload().as_ptr(), payload);
        let root = Cell::new(vec![3], vec![reference.clone()]).unwrap();
        assert_eq!(&root.encode_record()[..2], &[1, 0x10]);
        assert_eq!(root.depth(0).unwrap(), 1);
        let expected_preimage = [&[1, 0x10, 0, 0, 3, 0, 0][..], &reference.id()].concat();
        assert_eq!(root.hash_preimage(0).unwrap(), expected_preimage);
        assert_eq!(root.id(), <CellID>::from(Sha256::digest(expected_preimage)));
        let detached = Cell::new(vec![3], vec![reference.to_unloaded().unwrap()]).unwrap();
        assert_eq!(detached.encode_record(), root.encode_record());
        assert_eq!(
            Cell::decode_record_exact(&root.encode_record())
                .unwrap()
                .commitment(),
            root.commitment()
        );
        assert!(matches!(
            Cell::new(vec![], vec![CellRef::unresolved(root.id())]),
            Err(CellError::MissingCellMetadata(_))
        ));
    }

    #[test]
    fn outer_proof_preserves_transaction_and_old_taproot_pruning() {
        let hidden_script = parent(vec![leaf(1)]);
        let options = parent(vec![leaf(2), hidden_script.clone()]);
        let disclosed_options = parent(vec![leaf(2), hidden_script.prune(1).unwrap()]);
        assert_eq!(options.id(), disclosed_options.hash(0).unwrap());
        assert_ne!(options.id(), disclosed_options.id());
        let other_tx_data = parent(vec![leaf(3), leaf(4)]);
        let transaction = parent(vec![disclosed_options.clone(), other_tx_data.clone()]);
        let proof = parent(vec![
            disclosed_options.clone(),
            other_tx_data.prune(2).unwrap(),
        ]);
        assert_eq!(proof.hash(1).unwrap(), transaction.id());
        assert_eq!(proof.hash(0).unwrap(), transaction.hash(0).unwrap());
        let view = proof.virtualize(1).unwrap();
        assert_eq!(view.id(), transaction.id());
        let old_cut = view
            .reference(0, &mut ())
            .unwrap()
            .reference(1, &mut ())
            .unwrap();
        assert!(old_cut.is_pruned().unwrap());
        assert!(old_cut.payload().is_ok());
        assert!(matches!(
            old_cut.virtualize(0).unwrap().payload(),
            Err(CellError::PrunedCell)
        ));
        let new_cut = view.reference(1, &mut ()).unwrap();
        assert!(!new_cut.is_available());
        assert_eq!(new_cut.id(), other_tx_data.id());
        assert!(matches!(new_cut.payload(), Err(CellError::PrunedCell)));
        assert!(view.virtualize(2).is_err());
        assert_eq!(proof.virtualize(2).unwrap().id(), proof.id());
        assert_eq!(
            proof.virtualize(2).unwrap().payload().unwrap().as_ptr(),
            proof.payload().as_ptr()
        );
        let smaller = parent(vec![
            disclosed_options.prune(2).unwrap(),
            other_tx_data.prune(2).unwrap(),
        ]);
        assert_eq!(smaller.hash(1).unwrap(), transaction.id());
        let outer_cut = proof.prune(3).unwrap();
        assert_eq!(outer_cut.hash(2).unwrap(), proof.id());
        assert_eq!(outer_cut.hash(1).unwrap(), transaction.id());
        for level in 0..=proof.level() {
            assert_eq!(
                proof.hash(level).unwrap(),
                <CellID>::from(Sha256::digest(proof.hash_preimage(level).unwrap()))
            );
        }
    }

    #[test]
    fn sparse_masks_and_maximum_level() {
        let original = parent(vec![leaf(1)]);
        let cut = original.prune(15).unwrap();
        assert_eq!(cut.level_mask(), 1 << 14);
        for level in 0..15 {
            assert_eq!(cut.hash(level).unwrap(), original.id());
        }
        let view = cut.virtualize(7).unwrap();
        assert_eq!(view.level(), 7);
        assert_eq!(view.level_mask(), 0);
        assert!(!view.is_available());
        assert!(cut.prune(16).is_err());
        assert!(cut.hash(16).is_err());
        assert!(view.hash(16).is_err());
        assert!(cut.prune(0).is_err());
        let dense = Cell::from_pruned(0x7fff, vec![[8; 32]; 15], vec![0; 15]).unwrap();
        assert_eq!(dense.encode_record().len(), 512);
        assert_eq!(dense.commitment().hashes().len(), 16);
    }

    #[test]
    fn invalid_and_overflowing_depths_are_rejected_without_panics() {
        let maximal = CellRef::Unloaded(Arc::new(
            CellCommitment::new(0, vec![[1; 32]], vec![u16::MAX]).unwrap(),
        ));
        assert!(matches!(
            Cell::new(vec![], vec![maximal]),
            Err(CellError::DepthOverflow)
        ));
        let near = CellRef::Unloaded(Arc::new(
            CellCommitment::new(0, vec![[1; 32]], vec![u16::MAX - 1]).unwrap(),
        ));
        let root = Cell::new(vec![], vec![near]).unwrap();
        assert_eq!(root.depth(0).unwrap(), u16::MAX);
        assert!(matches!(
            Cell::new(vec![], vec![root.into()]),
            Err(CellError::DepthOverflow)
        ));
    }

    #[test]
    fn resolver_checks_every_claimed_hash_and_depth_and_counts_resident_reads() {
        struct Fixed(Cell, usize);
        impl CellResolver for Fixed {
            fn resolve(&mut self, _: &CellRef) -> Result<Arc<Cell>, CellError> {
                self.1 += 1;
                Ok(Arc::new(self.0.clone()))
            }
        }
        let cell = leaf(1).prune(1).unwrap();
        let forged = CellCommitment::new(1, vec![[0x99; 32], cell.id()], vec![0, 0]).unwrap();
        let reference = CellRef::Unloaded(Arc::new(forged));
        assert!(matches!(
            resolve_cell(&mut Fixed(cell.clone(), 0), &reference),
            Err(CellError::CellCommitmentMismatch(_))
        ));
        assert!(resolve_cell(&mut Fixed(cell.clone(), 0), &CellRef::unresolved(cell.id())).is_ok());
        assert!(matches!(
            resolve_cell(&mut Fixed(leaf(2), 0), &CellRef::unresolved(cell.id())),
            Err(CellError::CellHashMismatch { .. })
        ));
        let mut counter = Fixed(cell.clone(), 0);
        resolve_cell(&mut counter, &cell.into()).unwrap();
        assert_eq!(counter.1, 1);
    }

    #[test]
    fn decoder_rejects_truncation_trailing_bytes_and_reference_mask_overflow() {
        let cell = parent(vec![leaf(1).prune(1).unwrap()]);
        let bytes = cell.encode_record();
        for end in 0..bytes.len() {
            assert!(Cell::decode_record_exact(&bytes[..end]).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            Cell::decode_record_exact(&trailing),
            Err(CellError::TrailingBytes)
        ));
        let mut bad_mask = bytes;
        bad_mask[4] |= 0x80;
        assert!(Cell::decode_record_exact(&bad_mask).is_err());
    }

    #[test]
    fn deep_resident_chains_are_dropped_iteratively() {
        let mut cell = leaf(0);
        for _ in 0..10_000 {
            cell = parent(vec![cell]);
        }
        assert_eq!(cell.depth(0).unwrap(), 10_000);
        drop(cell);
    }
}
