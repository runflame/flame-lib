use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

use sha2::{Digest, Sha256};

use crate::{Cell, CellError, CellID, CellRef, CellResolver};

pub type BoCID = [u8; 32];

pub trait GasMeter {
    fn charge(&mut self, amount: u64) -> Result<(), CellError>;
}

#[derive(Clone, Debug, Default)]
pub struct BagOfCells {
    cells: BTreeMap<CellID, Arc<Cell>>,
}

impl BagOfCells {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn collect(root: Arc<Cell>) -> Result<Self, CellError> {
        let mut bag = Self::new();
        let mut pending = vec![root];
        let mut visited = HashSet::new();

        while let Some(cell) = pending.pop() {
            if !visited.insert(Arc::as_ptr(&cell)) {
                continue;
            }
            pending.extend(cell.refs().iter().filter_map(|reference| match reference {
                CellRef::Resident(cell) => Some(Arc::clone(cell)),
                CellRef::Pruned(_) => None,
            }));
            bag.insert(cell)?;
        }

        Ok(bag)
    }

    pub fn insert(&mut self, cell: Arc<Cell>) -> Result<(), CellError> {
        let id = cell.id();
        if self.cells.contains_key(&id) {
            return Ok(());
        }
        if self.cells.len() == u32::MAX as usize {
            return Err(CellError::CellCountOverflow);
        }
        self.cells.insert(id, detach(cell));
        Ok(())
    }

    pub fn get(&self, id: &CellID) -> Option<Arc<Cell>> {
        self.cells.get(id).cloned()
    }

    pub fn contains(&self, id: &CellID) -> bool {
        self.cells.contains_key(id)
    }

    pub fn id(&self) -> BoCID {
        Sha256::digest(self.encode()).into()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(self.cells.len() as u32).to_le_bytes());
        for cell in self.cells.values() {
            bytes.extend_from_slice(&cell.encode());
        }
        bytes
    }

    pub fn decode(
        bytes: &[u8],
        max_bytes: usize,
        gas: &mut impl GasMeter,
    ) -> Result<Self, CellError> {
        if bytes.len() > max_bytes {
            return Err(CellError::LimitExceeded);
        }

        let count_bytes: [u8; 4] = bytes
            .get(..4)
            .ok_or(CellError::InsufficientBytes)?
            .try_into()
            .expect("slice length checked");
        let count = u32::from_le_bytes(count_bytes);
        gas.charge(u64::from(count))?;

        let mut remaining = &bytes[4..];
        let mut cells = BTreeMap::new();
        let mut previous = None;

        for _ in 0..count {
            let before = remaining.len();
            let cell = Cell::decode_record(&mut remaining)?;
            let record_bytes = before - remaining.len();
            gas.charge(u64::try_from(record_bytes).map_err(|_| CellError::LimitExceeded)?)?;
            gas.charge(cell.refs().len() as u64)?;

            let id = cell.id();
            if previous.is_some_and(|previous| previous >= id) {
                return Err(CellError::InvalidFormat);
            }
            previous = Some(id);
            cells.insert(id, Arc::new(cell));
        }

        if !remaining.is_empty() {
            return Err(CellError::TrailingBytes);
        }

        validate_acyclic(&cells, gas)?;
        Ok(Self { cells })
    }
}

impl CellResolver for BagOfCells {
    fn resolve(&mut self, reference: &CellRef) -> Result<Arc<Cell>, CellError> {
        match reference {
            CellRef::Resident(cell) => Ok(Arc::clone(cell)),
            CellRef::Pruned(id) => self.get(id).ok_or(CellError::MissingCell(*id)),
        }
    }
}

fn detach(cell: Arc<Cell>) -> Arc<Cell> {
    if cell.refs().iter().all(CellRef::is_pruned) {
        return cell;
    }
    Arc::new(
        Cell::new(
            cell.payload().to_vec(),
            cell.refs().iter().map(CellRef::to_pruned).collect(),
        )
        .expect("an existing Cell remains valid when detached"),
    )
}

fn validate_acyclic(
    cells: &BTreeMap<CellID, Arc<Cell>>,
    gas: &mut impl GasMeter,
) -> Result<(), CellError> {
    // true means visiting; false means completely visited.
    let mut states = BTreeMap::<CellID, bool>::new();

    for &start in cells.keys() {
        if states.contains_key(&start) {
            continue;
        }

        gas.charge(1)?;
        states.insert(start, true);
        let mut stack = vec![(start, 0usize)];

        while let Some((id, next_ref)) = stack.last_mut() {
            let cell = cells.get(id).expect("stack contains bag IDs");
            if *next_ref == cell.refs().len() {
                states.insert(*id, false);
                stack.pop();
                continue;
            }

            let child = cell.refs()[*next_ref].id();
            *next_ref += 1;
            gas.charge(1)?;

            if !cells.contains_key(&child) {
                continue;
            }
            match states.get(&child) {
                Some(true) => return Err(CellError::Cycle(child)),
                Some(false) => continue,
                None => {
                    gas.charge(1)?;
                    states.insert(child, true);
                    stack.push((child, 0));
                }
            }
        }
    }

    Ok(())
}

#[derive(Clone, Debug)]
pub struct CellEnvelope {
    root: CellID,
    cells: BagOfCells,
}

impl CellEnvelope {
    pub fn new(root: CellID, cells: BagOfCells) -> Result<Self, CellError> {
        if !cells.contains(&root) {
            return Err(CellError::MissingCell(root));
        }
        Ok(Self { root, cells })
    }

    pub fn root(&self) -> CellID {
        self.root
    }

    pub fn cells(&self) -> &BagOfCells {
        &self.cells
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&self.root);
        bytes.extend_from_slice(&self.cells.encode());
        bytes
    }

    pub fn decode(
        bytes: &[u8],
        max_bytes: usize,
        gas: &mut impl GasMeter,
    ) -> Result<Self, CellError> {
        if bytes.len() > max_bytes {
            return Err(CellError::LimitExceeded);
        }
        let root: CellID = bytes
            .get(..32)
            .ok_or(CellError::InsufficientBytes)?
            .try_into()
            .expect("slice length checked");
        let cells = BagOfCells::decode(&bytes[32..], max_bytes - 32, gas)?;
        Self::new(root, cells)
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

    struct LimitedGas(u64);

    impl GasMeter for LimitedGas {
        fn charge(&mut self, amount: u64) -> Result<(), CellError> {
            self.0 = self
                .0
                .checked_sub(amount)
                .ok_or(CellError::ResourceExhausted)?;
            Ok(())
        }
    }

    fn plenty_of_gas() -> LimitedGas {
        LimitedGas(u64::MAX)
    }

    #[test]
    fn canonical_round_trip_and_direct_id() {
        let left = Arc::new(Cell::new(vec![1], vec![]).unwrap());
        let right = Arc::new(Cell::new(vec![2], vec![]).unwrap());
        let root = Arc::new(
            Cell::new(
                vec![3],
                vec![
                    CellRef::Resident(Arc::clone(&left)),
                    CellRef::Resident(Arc::clone(&right)),
                ],
            )
            .unwrap(),
        );

        let collected = BagOfCells::collect(Arc::clone(&root)).unwrap();
        let mut inserted = BagOfCells::new();
        inserted.insert(right).unwrap();
        inserted.insert(root.clone()).unwrap();
        inserted.insert(left).unwrap();
        assert_eq!(collected.encode(), inserted.encode());
        assert_eq!(
            collected.id(),
            <[u8; 32]>::from(Sha256::digest(collected.encode()))
        );

        let mut gas = plenty_of_gas();
        let decoded = BagOfCells::decode(&collected.encode(), usize::MAX, &mut gas).unwrap();
        assert_eq!(decoded.encode(), collected.encode());
        assert!(
            decoded
                .get(&root.id())
                .unwrap()
                .refs()
                .iter()
                .all(|reference| matches!(reference, CellRef::Pruned(_)))
        );
    }

    #[test]
    fn rejects_noncanonical_and_trailing_records() {
        let a = Cell::new(vec![1], vec![]).unwrap();
        let b = Cell::new(vec![2], vec![]).unwrap();
        let (high, low) = if a.id() > b.id() { (a, b) } else { (b, a) };
        let mut noncanonical = 2u32.to_le_bytes().to_vec();
        noncanonical.extend_from_slice(&high.encode());
        noncanonical.extend_from_slice(&low.encode());
        assert!(matches!(
            BagOfCells::decode(&noncanonical, usize::MAX, &mut plenty_of_gas()),
            Err(CellError::InvalidFormat)
        ));

        let mut duplicate = 2u32.to_le_bytes().to_vec();
        duplicate.extend_from_slice(&high.encode());
        duplicate.extend_from_slice(&high.encode());
        assert!(matches!(
            BagOfCells::decode(&duplicate, usize::MAX, &mut plenty_of_gas()),
            Err(CellError::InvalidFormat)
        ));

        let mut trailing = BagOfCells::collect(Arc::new(high)).unwrap().encode();
        trailing.push(0);
        assert!(matches!(
            BagOfCells::decode(&trailing, usize::MAX, &mut plenty_of_gas()),
            Err(CellError::TrailingBytes)
        ));

        let mut malformed = 1u32.to_le_bytes().to_vec();
        malformed.extend_from_slice(&(5u16 << 13).to_le_bytes());
        assert!(matches!(
            BagOfCells::decode(&malformed, usize::MAX, &mut plenty_of_gas()),
            Err(CellError::InvalidFormat)
        ));
    }

    #[test]
    fn bounds_and_prepaid_count_stop_early() {
        let empty = BagOfCells::new().encode();
        assert!(matches!(
            BagOfCells::decode(&empty, empty.len() - 1, &mut plenty_of_gas()),
            Err(CellError::LimitExceeded)
        ));

        let declared_flood = 100u32.to_le_bytes();
        assert!(matches!(
            BagOfCells::decode(&declared_flood, usize::MAX, &mut LimitedGas(99)),
            Err(CellError::ResourceExhausted)
        ));
    }

    #[test]
    fn partial_bag_and_envelope() {
        let missing = [7; 32];
        let root = Arc::new(Cell::new(vec![], vec![CellRef::Pruned(missing)]).unwrap());
        let bag = BagOfCells::collect(Arc::clone(&root)).unwrap();
        assert!(bag.contains(&root.id()));
        assert!(!bag.contains(&missing));

        let envelope = CellEnvelope::new(root.id(), bag).unwrap();
        let bytes = envelope.encode();
        let mut gas = plenty_of_gas();
        let decoded = CellEnvelope::decode(&bytes, bytes.len(), &mut gas).unwrap();
        assert_eq!(decoded.root(), root.id());
        assert!(decoded.cells().contains(&root.id()));

        let mut missing_root = bytes;
        missing_root[..32].copy_from_slice(&missing);
        assert!(matches!(
            CellEnvelope::decode(&missing_root, missing_root.len(), &mut plenty_of_gas()),
            Err(CellError::MissingCell(id)) if id == missing
        ));

        assert!(matches!(
            CellEnvelope::new(missing, BagOfCells::new()),
            Err(CellError::MissingCell(id)) if id == missing
        ));
    }

    #[test]
    fn bag_membership_is_the_exact_availability_boundary() {
        let child = Arc::new(Cell::new(vec![1], vec![]).unwrap());
        let root =
            Arc::new(Cell::new(vec![2], vec![CellRef::Resident(Arc::clone(&child))]).unwrap());
        let mut bag = BagOfCells::new();
        bag.insert(Arc::clone(&root)).unwrap();

        let stored_root = bag.get(&root.id()).unwrap();
        assert!(stored_root.refs()[0].is_pruned());
        assert!(matches!(
            crate::resolve_cell(&mut bag, &stored_root.refs()[0]),
            Err(CellError::MissingCell(id)) if id == child.id()
        ));

        let mut complete = BagOfCells::collect(root).unwrap();
        assert!(crate::resolve_cell(&mut complete, &stored_root.refs()[0]).is_ok());
    }

    #[test]
    fn collect_unions_resident_frontiers_of_duplicate_cells() {
        let grandchild = Arc::new(Cell::new(vec![1], vec![]).unwrap());
        let rich =
            Arc::new(Cell::new(vec![2], vec![CellRef::Resident(Arc::clone(&grandchild))]).unwrap());
        let poor = Arc::new(Cell::new(vec![2], vec![CellRef::pruned(grandchild.id())]).unwrap());
        assert_eq!(rich.id(), poor.id());

        let root = Arc::new(
            Cell::new(
                vec![3],
                vec![CellRef::Resident(rich), CellRef::Resident(poor)],
            )
            .unwrap(),
        );
        let bag = BagOfCells::collect(root).unwrap();
        assert_eq!(bag.len(), 3);
        assert!(bag.contains(&grandchild.id()));
    }

    #[test]
    fn detects_included_cycles() {
        let a = [1; 32];
        let b = [2; 32];
        let mut cells = BTreeMap::new();
        cells.insert(
            a,
            Arc::new(Cell::new(vec![], vec![CellRef::Pruned(b)]).unwrap()),
        );
        cells.insert(
            b,
            Arc::new(Cell::new(vec![], vec![CellRef::Pruned(a)]).unwrap()),
        );
        assert!(matches!(
            validate_acyclic(&cells, &mut plenty_of_gas()),
            Err(CellError::Cycle(id)) if id == a
        ));
    }
}
