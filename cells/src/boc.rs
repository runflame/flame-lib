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

impl GasMeter for u64 {
    fn charge(&mut self, amount: u64) -> Result<(), CellError> {
        *self = self
            .checked_sub(amount)
            .ok_or(CellError::ResourceExhausted)?;
        Ok(())
    }
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
            pending.extend(
                cell.refs()
                    .iter()
                    .filter_map(CellRef::as_resident_arc)
                    .cloned(),
            );
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

    /// Iterates the exact committed body set in canonical CellID order.
    pub fn iter(&self) -> impl Iterator<Item = (&CellID, &Arc<Cell>)> {
        self.cells.iter()
    }

    /// Adds another explicit body set. This is construction, never implicit
    /// resolution from a global cache or another transaction.
    pub fn extend(&mut self, other: &Self) -> Result<(), CellError> {
        for cell in other.cells.values() {
            self.insert(Arc::clone(cell))?;
        }
        Ok(())
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
            // Account for hashing/checking each significant level, including zero.
            let hash_work = u64::try_from(record_bytes)
                .map_err(|_| CellError::LimitExceeded)?
                .checked_mul(u64::from(cell.commitment().mask().count_ones()) + 1)
                .ok_or(CellError::LimitExceeded)?;
            gas.charge(hash_work)?;
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

        validate_child_commitments(&cells, gas)?;
        validate_acyclic(&cells, gas)?;
        Ok(Self { cells })
    }
}

impl CellResolver for BagOfCells {
    fn resolve(&mut self, reference: &CellRef) -> Result<Arc<Cell>, CellError> {
        match reference {
            CellRef::Resident(cell) => Ok(Arc::clone(cell)),
            CellRef::Unloaded(_) | CellRef::Unresolved(_) => self
                .get(&reference.id())
                .ok_or(CellError::MissingCell(reference.id())),
        }
    }
}

fn detach(cell: Arc<Cell>) -> Arc<Cell> {
    if cell.refs().iter().all(CellRef::is_unloaded) {
        return cell;
    }
    Arc::new(cell.detached())
}

fn validate_child_commitments(
    cells: &BTreeMap<CellID, Arc<Cell>>,
    gas: &mut impl GasMeter,
) -> Result<(), CellError> {
    for cell in cells.values() {
        for reference in cell.refs() {
            if let Some(child) = cells.get(&reference.id()) {
                gas.charge(u64::from(child.commitment().mask().count_ones()) + 1)?;
                if reference.commitment()? != child.commitment() {
                    return Err(CellError::CellCommitmentMismatch(reference.id()));
                }
            }
        }
    }
    Ok(())
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

    pub fn into_parts(self) -> (CellID, BagOfCells) {
        (self.root, self.cells)
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
                .all(CellRef::is_unloaded)
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
        malformed.extend_from_slice(&(5u16 << 12).to_le_bytes());
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
        let child = CellRef::from(Cell::new(vec![7], vec![]).unwrap());
        let missing = child.id();
        let root = Arc::new(Cell::new(vec![], vec![child.to_unloaded().unwrap()]).unwrap());
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
        assert!(stored_root.refs()[0].is_unloaded());
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
        let poor = Arc::new(rich.detached());
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
        let a_ref = CellRef::from(Cell::new(vec![1], vec![]).unwrap())
            .to_unloaded()
            .unwrap();
        let b_ref = CellRef::from(Cell::new(vec![2], vec![]).unwrap())
            .to_unloaded()
            .unwrap();
        let a = a_ref.id();
        let b = b_ref.id();
        let mut cells = BTreeMap::new();
        cells.insert(a, Arc::new(Cell::new(vec![], vec![b_ref]).unwrap()));
        cells.insert(b, Arc::new(Cell::new(vec![], vec![a_ref]).unwrap()));
        assert!(matches!(
            validate_acyclic(&cells, &mut plenty_of_gas()),
            Err(CellError::Cycle(id)) if id == a.min(b)
        ));
    }

    #[test]
    fn explicit_pruned_cells_and_nested_levels_round_trip() {
        let full = Cell::new(vec![7], vec![]).unwrap();
        let inner = full.prune(1).unwrap();
        let transaction = Cell::new(vec![8], vec![CellRef::from(inner.clone())]).unwrap();
        let outer = transaction.prune(2).unwrap();
        let root = Arc::new(
            Cell::new(
                vec![9],
                vec![CellRef::from(inner.clone()), CellRef::from(outer.clone())],
            )
            .unwrap(),
        );
        let bag = BagOfCells::collect(root.clone()).unwrap();
        assert_eq!(bag.len(), 3);
        let bytes = bag.encode();
        let decoded = BagOfCells::decode(&bytes, bytes.len(), &mut plenty_of_gas()).unwrap();
        assert_eq!(decoded.encode(), bytes);
        for original in [&inner, &outer] {
            let restored = decoded.get(&original.id()).unwrap();
            assert!(restored.is_pruned());
            assert_eq!(restored.commitment(), original.commitment());
            assert_eq!(restored.encode(), original.encode());
        }
        assert_eq!(inner.hash(0).unwrap(), full.id());
        assert_eq!(outer.hash(1).unwrap(), transaction.id());
        assert_eq!(
            decoded.get(&root.id()).unwrap().commitment(),
            root.commitment()
        );
    }

    #[test]
    fn rejects_forged_child_summary_despite_matching_child_id() {
        let child = Arc::new(Cell::new(vec![7], vec![]).unwrap());
        let root = Cell::new(vec![], vec![CellRef::resident(child.clone())]).unwrap();
        let mut forged = root.encode();
        // The final LE16 is the sole child's declared level-zero depth.
        let depth_offset = forged.len() - 2;
        forged[depth_offset..].copy_from_slice(&1u16.to_le_bytes());
        let forged = Cell::decode_exact(&forged).unwrap();
        assert_eq!(forged.refs()[0].id(), child.id());

        let mut bag = BagOfCells::new();
        bag.insert(Arc::new(forged)).unwrap();
        bag.insert(child.clone()).unwrap();
        assert!(matches!(
            BagOfCells::decode(&bag.encode(), usize::MAX, &mut plenty_of_gas()),
            Err(CellError::CellCommitmentMismatch(id)) if id == child.id()
        ));
    }

    #[test]
    fn rejects_forged_lower_hash_and_charges_all_significant_levels() {
        let child = Arc::new(Cell::new(vec![7], vec![]).unwrap().prune(1).unwrap());
        let root = Cell::new(vec![], vec![CellRef::resident(child.clone())]).unwrap();
        let mut forged = root.encode();
        // Descriptor, child mask, then the lower hash; the child's top ID stays intact.
        forged[4] ^= 1;
        let forged = Cell::decode_exact(&forged).unwrap();
        assert_eq!(forged.refs()[0].id(), child.id());
        let mut bag = BagOfCells::new();
        bag.insert(Arc::new(forged)).unwrap();
        bag.insert(child.clone()).unwrap();
        assert!(matches!(
            BagOfCells::decode(&bag.encode(), usize::MAX, &mut plenty_of_gas()),
            Err(CellError::CellCommitmentMismatch(id)) if id == child.id()
        ));

        let nested = Arc::new(child.prune(2).unwrap());
        let old_single_level_budget = nested.encoded_size() as u64 + 2;
        let bag = BagOfCells::collect(nested).unwrap();
        assert!(matches!(
            BagOfCells::decode(
                &bag.encode(),
                usize::MAX,
                &mut LimitedGas(old_single_level_budget)
            ),
            Err(CellError::ResourceExhausted)
        ));
    }

    #[test]
    fn decoded_block_proof_preserves_taproot_pruning_in_transaction_view() {
        let script_a = Cell::new(vec![1], vec![]).unwrap();
        let script_b = Cell::new(vec![2], vec![]).unwrap();
        let full_options = Cell::new(
            vec![],
            vec![script_a.clone().into(), script_b.clone().into()],
        )
        .unwrap();
        let taproot_cut = script_b.prune(1).unwrap();
        let options = Cell::new(vec![], vec![script_a.into(), taproot_cut.clone().into()]).unwrap();
        let other_payload = Cell::new(vec![3], vec![]).unwrap();
        let transaction = Cell::new(
            vec![4],
            vec![options.clone().into(), other_payload.clone().into()],
        )
        .unwrap();
        let other_transaction = Cell::new(vec![5], vec![]).unwrap();
        let block = Cell::new(
            vec![6],
            vec![transaction.clone().into(), other_transaction.clone().into()],
        )
        .unwrap();

        // Both fresh cuts use the block's target level + 1, even though the
        // omitted subtrees themselves are level zero.
        let partial_transaction = Cell::new(
            vec![4],
            vec![options.into(), other_payload.prune(2).unwrap().into()],
        )
        .unwrap();
        let proof = Cell::new(
            vec![6],
            vec![
                partial_transaction.into(),
                other_transaction.prune(2).unwrap().into(),
            ],
        )
        .unwrap();
        assert_eq!(proof.hash(1).unwrap(), block.id());
        assert_ne!(proof.id(), block.id());

        let bytes = BagOfCells::collect(Arc::new(proof.clone()))
            .unwrap()
            .encode();
        let mut bag = BagOfCells::decode(&bytes, bytes.len(), &mut plenty_of_gas()).unwrap();
        let root = bag.get(&proof.id()).unwrap();
        let block_view = root.virtualize(1).unwrap();
        assert_eq!(block_view.id(), block.id());
        assert!(!block_view.reference(1, &mut bag).unwrap().is_available());

        let tx_view = block_view.reference(0, &mut bag).unwrap();
        assert_eq!(tx_view.id(), transaction.id());
        assert!(!tx_view.reference(1, &mut bag).unwrap().is_available());
        let options_view = tx_view.reference(0, &mut bag).unwrap();
        let old_cut = options_view.reference(1, &mut bag).unwrap();
        assert_eq!(old_cut.is_pruned(), Ok(true));
        assert_eq!(old_cut.payload().unwrap(), taproot_cut.payload());

        let semantic_options = options_view.virtualize(0).unwrap();
        assert_eq!(semantic_options.id(), full_options.id());
        let hidden_script = semantic_options.reference(1, &mut bag).unwrap();
        assert!(!hidden_script.is_available());
        assert_eq!(hidden_script.payload(), Err(CellError::PrunedCell));
    }
}
