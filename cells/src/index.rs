use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use crate::{
    Cell, CellBuilder, CellCommitment, CellError, CellID, CellRef, CellResolver, CellSlice,
    GasMeter, MAX_CELL_LEVEL, Trie, resolve_cell,
};

/// An exact in-memory availability index. Network transport belongs to a Cell root.
#[derive(Clone, Debug, Default)]
pub struct CellIndex {
    cells: BTreeMap<CellID, Arc<Cell>>,
}

impl CellIndex {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.cells.len()
    }
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
    pub fn get(&self, id: &CellID) -> Option<Arc<Cell>> {
        self.cells.get(id).cloned()
    }
    pub fn contains(&self, id: &CellID) -> bool {
        self.cells.contains_key(id)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&CellID, &Arc<Cell>)> {
        self.cells.iter()
    }
    pub fn insert(&mut self, cell: Arc<Cell>) -> Result<(), CellError> {
        if !self.contains(&cell.id()) && self.len() == u32::MAX as usize {
            return Err(CellError::CellCountOverflow);
        }
        self.cells
            .entry(cell.id())
            .or_insert_with(|| Arc::new(cell.detached()));
        Ok(())
    }
    pub fn extend(&mut self, other: &Self) -> Result<(), CellError> {
        for cell in other.cells.values() {
            self.insert(cell.clone())?;
        }
        Ok(())
    }
    pub fn collect(root: Arc<Cell>) -> Result<Self, CellError> {
        let mut result = Self::new();
        let mut seen = BTreeSet::new();
        let mut pending = vec![root];
        while let Some(cell) = pending.pop() {
            if !seen.insert(Arc::as_ptr(&cell)) {
                continue;
            }
            pending.extend(
                cell.refs()
                    .iter()
                    .filter_map(CellRef::as_resident_arc)
                    .cloned(),
            );
            result.insert(cell)?;
        }
        Ok(result)
    }

    /// A normal Cell hierarchy committing exactly the indexed bodies. Unknown
    /// descendants become explicit cuts at a fresh level; the header stores the
    /// original cap so decoding can restore their original lookup identities.
    pub fn to_cell(&self) -> Result<Cell, CellError> {
        let cap = self.cells.values().map(|c| c.level()).max().unwrap_or(0);
        let mut proofs = BTreeMap::new();
        let mut trie = Trie::new(32)?;
        for (id, cell) in &self.cells {
            let proof = prove(cell.clone(), self, cap, &mut proofs)?;
            trie.insert_ref(id, proof.into(), &mut ())?;
        }
        let mut b = CellBuilder::new();
        b.store_u32(u32::try_from(self.len()).map_err(|_| CellError::CellCountOverflow)?)?
            .store_u8(cap)?;
        if let Some(root) = trie.into_root() {
            b.store_ref(root)?;
        }
        Ok(b.build())
    }

    pub fn id(&self) -> Result<CellID, CellError> {
        Ok(self.to_cell()?.id())
    }

    pub fn from_cell<R: CellResolver + ?Sized>(
        cell: &Cell,
        resolver: &mut R,
    ) -> Result<Self, CellError> {
        let mut s = CellSlice::new(cell);
        let count = s.load_u32()?;
        let cap = s.load_u8()?;
        if cap > MAX_CELL_LEVEL {
            return Err(CellError::InvalidLevel);
        }
        let mut result = Self::new();
        if count > 0 {
            let trie = Trie::from_cell(s.load_ref()?, 32)?;
            let mut key = trie.first_key(resolver)?;
            for _ in 0..count {
                let k = key.take().ok_or(CellError::InvalidFormat)?;
                let body = trie.get(&k, resolver)?.ok_or(CellError::InvalidFormat)?;
                let original = project(&body, cap)?;
                if original.id().as_slice() != k.as_slice() {
                    return Err(CellError::InvalidFormat);
                }
                result.insert(Arc::new(original))?;
                key = trie.next_key_after(&k, resolver)?;
            }
            if key.is_some() {
                return Err(CellError::InvalidFormat);
            }
        }
        s.finish()?;
        // The reconstruction also pins the cut level and excludes hidden extras.
        if result.to_cell()?.id() != cell.id() {
            return Err(CellError::InvalidFormat);
        }
        Ok(result)
    }
}

impl CellResolver for CellIndex {
    fn resolve(&mut self, reference: &CellRef) -> Result<Arc<Cell>, CellError> {
        reference
            .as_resident_arc()
            .cloned()
            .or_else(|| self.get(&reference.id()))
            .ok_or(CellError::MissingCell(reference.id()))
    }
}

fn cut(reference: &CellRef, cap: u8) -> Result<Cell, CellError> {
    if cap >= MAX_CELL_LEVEL {
        return Err(CellError::InvalidLevel);
    }
    let c = reference.commitment()?;
    if c.level() > cap {
        return Err(CellError::InvalidLevel);
    }
    Cell::from_pruned(
        c.mask() | (1 << cap),
        c.hashes().to_vec(),
        c.depths().to_vec(),
    )
}

fn prove(
    root: Arc<Cell>,
    index: &CellIndex,
    cap: u8,
    done: &mut BTreeMap<CellID, Arc<Cell>>,
) -> Result<Arc<Cell>, CellError> {
    if let Some(root) = done.get(&root.id()) {
        return Ok(root.clone());
    }
    let root_id = root.id();
    let mut active = BTreeSet::from([root.id()]);
    let mut stack = vec![(root, 0usize, Vec::<CellRef>::new())];
    while let Some((cell, next, refs)) = stack.last_mut() {
        if *next < cell.refs().len() {
            let r = &cell.refs()[*next];
            if let Some(body) = index.get(&r.id()) {
                if r.commitment()? != body.commitment() {
                    return Err(CellError::CellCommitmentMismatch(r.id()));
                }
                if let Some(proof) = done.get(&r.id()) {
                    refs.push(proof.clone().into());
                    *next += 1;
                } else {
                    if !active.insert(body.id()) {
                        return Err(CellError::Cycle(body.id()));
                    }
                    stack.push((body, 0, Vec::new()));
                }
            } else {
                refs.push(cut(r, cap)?.into());
                *next += 1;
            }
        } else {
            let (original, _, refs) = stack.pop().expect("nonempty stack");
            let proof = if original.is_pruned() {
                original.as_ref().clone()
            } else {
                Cell::new(original.payload().to_vec(), refs)?
            };
            if proof.hash(cap)? != original.id() {
                return Err(CellError::CellCommitmentMismatch(original.id()));
            }
            active.remove(&original.id());
            done.insert(original.id(), Arc::new(proof));
        }
    }
    Ok(done[&root_id].clone())
}

fn project(cell: &Cell, cap: u8) -> Result<Cell, CellError> {
    if cap > MAX_CELL_LEVEL {
        return Err(CellError::InvalidLevel);
    }
    if cell.is_pruned() {
        if cell.level() > cap {
            return Err(CellError::PrunedCell);
        }
        return Ok(cell.clone());
    }
    let mut refs = Vec::new();
    for r in cell.refs() {
        let c = r.commitment()?;
        let mask = c.mask() & ((1u16 << cap) - 1);
        let levels = (0..=cap)
            .filter(|&l| l == 0 || mask & (1 << (l - 1)) != 0)
            .collect::<Vec<_>>();
        let hashes = levels
            .iter()
            .map(|&l| c.hash(l))
            .collect::<Result<_, _>>()?;
        let depths = levels
            .iter()
            .map(|&l| c.depth(l))
            .collect::<Result<_, _>>()?;
        refs.push(CellRef::Unloaded(Arc::new(CellCommitment::new(
            mask, hashes, depths,
        )?)));
    }
    let original = Cell::new(cell.payload().to_vec(), refs)?;
    if original.id() != cell.hash(cap)? {
        return Err(CellError::InvalidFormat);
    }
    Ok(original)
}

/// A typed-object snapshot is itself an ordinary Cell: cap plus one proof root.
/// The transport root is factual; `root()` names the original content view.
#[derive(Clone, Debug)]
pub struct CellEnvelope {
    wire: Cell,
    root: CellID,
    cells: CellIndex,
}

impl CellEnvelope {
    pub fn new(root: CellID, cells: CellIndex) -> Result<Self, CellError> {
        let body = cells.get(&root).ok_or(CellError::MissingCell(root))?;
        let cap = body.level();
        let mut proofs = BTreeMap::new();
        let proof = prove(body, &cells, cap, &mut proofs)?;
        if proofs.len() != cells.len() {
            return Err(CellError::InvalidFormat);
        }
        let mut b = CellBuilder::new();
        b.store_u8(cap)?.store_ref(proof.into())?;
        Ok(Self {
            wire: b.build(),
            root,
            cells,
        })
    }
    pub fn root(&self) -> CellID {
        self.root
    }
    pub fn transport_root(&self) -> &Cell {
        &self.wire
    }
    pub fn cells(&self) -> &CellIndex {
        &self.cells
    }
    pub fn into_parts(self) -> (CellID, CellIndex) {
        (self.root, self.cells)
    }
    pub fn encode(&self) -> Vec<u8> {
        self.wire
            .encode_transport(&mut ())
            .expect("constructed snapshot is a complete DAG")
    }

    pub fn decode(
        bytes: &[u8],
        max_bytes: usize,
        gas: &mut impl GasMeter,
    ) -> Result<Self, CellError> {
        let wire = Cell::decode_transport(bytes, max_bytes, gas)?;
        let mut s = CellSlice::new(&wire);
        let cap = s.load_u8()?;
        let proof = resolve_cell(&mut (), &s.load_ref()?)?;
        s.finish()?;
        if cap > proof.level() {
            return Err(CellError::InvalidLevel);
        }
        let mut cells = CellIndex::new();
        let mut seen = BTreeSet::new();
        let mut pending = vec![proof];
        while let Some(cell) = pending.pop() {
            if !seen.insert(cell.id()) {
                continue;
            }
            if cell.is_pruned() && cell.level() > cap {
                continue;
            }
            gas.charge(
                (cell.payload().len() as u64 + cell.refs().len() as u64 * 34 + 36)
                    * (u64::from(cell.level_mask().count_ones()) + 1),
            )?;
            cells.insert(Arc::new(project(&cell, cap)?))?;
            for r in cell.refs() {
                pending.push(resolve_cell(&mut (), r)?);
            }
        }
        let root = wire.refs()[0]
            .as_resident()
            .expect("transport bodies are resident")
            .hash(cap)?;
        let envelope = Self::new(root, cells)?;
        if envelope.wire.id() != wire.id() {
            return Err(CellError::InvalidFormat);
        }
        Ok(envelope)
    }
}

impl CellResolver for CellEnvelope {
    fn resolve(&mut self, reference: &CellRef) -> Result<Arc<Cell>, CellError> {
        self.cells.resolve(reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn leaf(n: u8) -> Cell {
        Cell::new(vec![n], vec![]).unwrap()
    }

    #[test]
    fn sparse_snapshot_is_a_cell_tree_and_preserves_exact_availability() {
        let a = leaf(1);
        let b = leaf(2);
        let root = Cell::new(vec![3], vec![a.clone().into(), b.clone().into()]).unwrap();
        let mut sparse = CellIndex::new();
        sparse.insert(Arc::new(root.clone())).unwrap();
        sparse.insert(Arc::new(a.clone())).unwrap();
        let snapshot = sparse.to_cell().unwrap();
        let encoded = snapshot.encode_transport(&mut ()).unwrap();
        let mut gas = u64::MAX;
        let decoded = Cell::decode_transport(&encoded, encoded.len(), &mut gas).unwrap();
        let mut restored = CellIndex::from_cell(&decoded, &mut ()).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored.id().unwrap(), sparse.id().unwrap());
        assert_eq!(
            restored.get(&root.id()).unwrap().commitment(),
            root.commitment()
        );
        assert!(resolve_cell(&mut restored, &root.refs()[0]).is_ok());
        let missing = restored.get(&root.id()).unwrap().refs()[1].clone();
        assert!(
            matches!(resolve_cell(&mut restored,&missing), Err(CellError::MissingCell(id)) if id == b.id())
        );
    }

    #[test]
    fn typed_snapshot_is_rooted_transport_and_rejects_unreachable_extras() {
        let child = leaf(2);
        let root = Cell::new(vec![1], vec![child.clone().into()]).unwrap();
        let mut sparse = CellIndex::new();
        sparse.insert(Arc::new(root.clone())).unwrap();
        let envelope = CellEnvelope::new(root.id(), sparse.clone()).unwrap();
        let bytes = envelope.encode();
        let mut gas = u64::MAX;
        let decoded = CellEnvelope::decode(&bytes, bytes.len(), &mut gas).unwrap();
        assert_eq!(decoded.root(), root.id());
        assert_eq!(decoded.cells.len(), 1);
        assert_eq!(decoded.encode(), bytes);
        assert_ne!(decoded.transport_root().id(), root.id());
        sparse.insert(Arc::new(child)).unwrap();
        let complete = CellEnvelope::new(root.id(), sparse.clone()).unwrap();
        assert_ne!(
            complete.transport_root().id(),
            envelope.transport_root().id()
        );
        sparse.insert(Arc::new(leaf(9))).unwrap();
        assert!(matches!(
            CellEnvelope::new(root.id(), sparse),
            Err(CellError::InvalidFormat)
        ));
    }

    #[test]
    fn existing_pruning_is_preserved_and_empty_indexes_have_a_cell_root() {
        let old = leaf(1).prune(1).unwrap();
        let full = Cell::new(vec![2], vec![old.clone().into(), leaf(3).into()]).unwrap();
        let mut index = CellIndex::new();
        index.insert(Arc::new(old.clone())).unwrap();
        index.insert(Arc::new(full.clone())).unwrap();
        let root = index.to_cell().unwrap();
        let decoded = CellIndex::from_cell(&root, &mut ()).unwrap();
        assert_eq!(
            decoded.get(&old.id()).unwrap().commitment(),
            old.commitment()
        );
        assert_eq!(
            decoded.get(&full.id()).unwrap().commitment(),
            full.commitment()
        );
        let empty = CellIndex::new();
        let root = empty.to_cell().unwrap();
        assert!(CellIndex::from_cell(&root, &mut ()).unwrap().is_empty());
    }

    #[test]
    fn exhausted_pruning_levels_fail_without_changing_the_index() {
        let old = leaf(1).prune(15).unwrap();
        let root = Cell::new(vec![2], vec![old.clone().into(), leaf(3).into()]).unwrap();
        let mut index = CellIndex::collect(Arc::new(root.clone())).unwrap();
        assert!(index.to_cell().is_ok());
        index.cells.remove(&root.refs()[1].id());
        assert_eq!(index.to_cell().unwrap_err(), CellError::InvalidLevel);
        assert_eq!(index.len(), 2);
        assert!(index.contains(&root.id()));
        assert!(index.contains(&old.id()));
    }

    #[test]
    fn snapshot_rejects_wrong_count_cap_and_identity() {
        let body = leaf(1);
        let mut index = CellIndex::new();
        index.insert(Arc::new(body.clone())).unwrap();
        let snapshot = index.to_cell().unwrap();
        for payload in [
            vec![0, 0, 0, 0, 0],
            vec![1, 0, 0, 0, 1],
            vec![1, 0, 0, 0, 16],
        ] {
            let forged = Cell::new(payload, snapshot.refs().to_vec()).unwrap();
            assert!(CellIndex::from_cell(&forged, &mut ()).is_err());
        }
        let mut trie = Trie::new(32).unwrap();
        trie.insert_ref(&[9; 32], body.into(), &mut ()).unwrap();
        let forged = Cell::new(vec![1, 0, 0, 0, 0], vec![trie.into_root().unwrap()]).unwrap();
        assert_eq!(
            CellIndex::from_cell(&forged, &mut ()).unwrap_err(),
            CellError::InvalidFormat
        );
    }
}
