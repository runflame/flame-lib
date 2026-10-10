use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use crate::{Cell, CellError, CellReader, CellRef, MAX_CELL_REFS, read_cell};

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

pub(crate) fn put_var(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let b = (n & 127) as u8;
        n >>= 7;
        out.push(b | if n == 0 { 0 } else { 128 });
        if n == 0 {
            return;
        }
    }
}

fn get_var(input: &mut &[u8]) -> Result<u64, CellError> {
    let mut n = 0;
    for i in 0..10 {
        let b = take(input, 1)?[0];
        if i == 9 && b > 1 {
            return Err(CellError::InvalidFormat);
        }
        n |= u64::from(b & 127) << (7 * i);
        if b & 128 == 0 {
            if i > 0 && b == 0 {
                return Err(CellError::InvalidFormat);
            }
            return Ok(n);
        }
    }
    Err(CellError::InvalidFormat)
}

fn take<'a>(input: &mut &'a [u8], n: usize) -> Result<&'a [u8], CellError> {
    let result = input.get(..n).ok_or(CellError::InsufficientBytes)?;
    *input = &input[n..];
    Ok(result)
}

// One canonical ordered postorder, independent of Arc identity and cache residency.
pub(crate) fn ordered<R: CellReader + ?Sized>(
    root: Arc<Cell>,
    resolver: &mut R,
) -> Result<Vec<Arc<Cell>>, CellError> {
    let mut completed = BTreeSet::new();
    let mut active = BTreeSet::from([root.id()]);
    let mut stack = vec![(root, 0usize)];
    let mut result = Vec::new();
    while let Some((cell, next)) = stack.last_mut() {
        if *next < cell.refs().len() {
            let reference = &cell.refs()[*next];
            *next += 1;
            let child = read_cell(resolver, reference)?;
            if completed.contains(&child.id()) {
                continue;
            }
            if !active.insert(child.id()) {
                return Err(CellError::Cycle(child.id()));
            }
            stack.push((child, 0));
        } else {
            let (cell, _) = stack.pop().expect("nonempty stack");
            active.remove(&cell.id());
            completed.insert(cell.id());
            result.push(cell);
        }
    }
    Ok(result)
}

impl Cell {
    /// Transport this Cell's complete rooted graph, including shared descendants.
    pub fn encode(&self) -> Result<Vec<u8>, CellError> {
        self.encode_transport(&mut ())
    }

    /// Packs the complete physical DAG, with children first and this Cell last.
    /// Unloaded references must resolve; explicit pruning records are terminal data.
    pub fn encode_transport<R: CellReader + ?Sized>(
        &self,
        resolver: &mut R,
    ) -> Result<Vec<u8>, CellError> {
        // Union resident frontiers before traversal: two references may carry
        // rich and detached views of the same child, in either reference order.
        let mut attached = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let mut pending = vec![Arc::new(self.clone())];
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
            attached.entry(cell.id()).or_insert(cell);
        }
        struct EncodingCells<'a, R: ?Sized> {
            attached: BTreeMap<crate::CellID, Arc<Cell>>,
            fallback: &'a mut R,
        }
        impl<R: CellReader + ?Sized> CellReader for EncodingCells<'_, R> {
            fn read(
                &mut self,
                id: crate::CellID,
                resident: Option<&Arc<Cell>>,
            ) -> Result<Arc<Cell>, CellError> {
                self.fallback.read(id, self.attached.get(&id).or(resident))
            }
        }
        let records = ordered(
            Arc::new(self.clone()),
            &mut EncodingCells {
                attached,
                fallback: resolver,
            },
        )?;
        let mut indexes = BTreeMap::new();
        let mut bytes = Vec::new();
        put_var(
            u64::try_from(records.len()).map_err(|_| CellError::CellCountOverflow)?,
            &mut bytes,
        );
        for (i, cell) in records.iter().enumerate() {
            let descriptor = if cell.is_pruned() {
                0x8000 | cell.level_mask()
            } else {
                cell.payload().len() as u16 | ((cell.refs().len() as u16) << 12)
            };
            bytes.extend_from_slice(&descriptor.to_le_bytes());
            bytes.extend_from_slice(cell.payload());
            for reference in cell.refs() {
                let child = indexes
                    .get(&reference.id())
                    .copied()
                    .ok_or(CellError::InvalidFormat)?;
                put_var((i - child) as u64, &mut bytes);
            }
            indexes.insert(cell.id(), i);
        }
        Ok(bytes)
    }

    /// Decodes one bounded, canonical rooted DAG. Its root is the final record.
    pub fn decode_transport(
        bytes: &[u8],
        max_bytes: usize,
        gas: &mut impl GasMeter,
    ) -> Result<Self, CellError> {
        if bytes.len() > max_bytes {
            return Err(CellError::LimitExceeded);
        }
        let mut input = bytes;
        let count = usize::try_from(get_var(&mut input)?).map_err(|_| CellError::LimitExceeded)?;
        if count == 0 || count > input.len() / 2 {
            return Err(CellError::InvalidFormat);
        }
        gas.charge(count as u64)?;
        let mut records: Vec<Arc<Cell>> = Vec::new();
        let mut ids = BTreeSet::new();
        for i in 0..count {
            let raw: [u8; 2] = take(&mut input, 2)?.try_into().expect("two bytes");
            let descriptor = u16::from_le_bytes(raw);
            let cell = if descriptor & 0x8000 != 0 {
                let mask = descriptor & 0x7fff;
                if mask == 0 {
                    return Err(CellError::InvalidFormat);
                }
                let n = mask.count_ones() as usize;
                gas.charge((2 + n * 34) as u64)?;
                let body = take(&mut input, n * 34)?;
                let hashes = body[..n * 32]
                    .chunks_exact(32)
                    .map(|x| x.try_into().expect("32 bytes"))
                    .collect();
                let depths = body[n * 32..]
                    .chunks_exact(2)
                    .map(|x| u16::from_le_bytes(x.try_into().expect("2 bytes")))
                    .collect();
                Cell::from_pruned(mask, hashes, depths)?
            } else {
                let n = usize::from(descriptor >> 12);
                if n > MAX_CELL_REFS {
                    return Err(CellError::InvalidFormat);
                }
                let len = usize::from(descriptor & 4095);
                gas.charge((len + n + 2) as u64)?;
                let payload = take(&mut input, len)?;
                let mut refs = Vec::new();
                let mut mask = 0u16;
                for _ in 0..n {
                    let distance = usize::try_from(get_var(&mut input)?)
                        .map_err(|_| CellError::InvalidFormat)?;
                    if distance == 0 || distance > i {
                        return Err(CellError::InvalidFormat);
                    }
                    let child = Arc::clone(&records[i - distance]);
                    mask |= child.level_mask();
                    refs.push(CellRef::resident(child));
                }
                // Price hash preimages and allocations before constructing the Cell.
                gas.charge((u64::from(mask.count_ones()) + 1) * (len as u64 + n as u64 * 34 + 36))?;
                Cell::new(payload.to_vec(), refs)?
            };
            if !ids.insert(cell.id()) {
                return Err(CellError::InvalidFormat);
            }
            records.push(Arc::new(cell));
        }
        if !input.is_empty() {
            return Err(CellError::TrailingBytes);
        }
        let root = records.last().expect("positive count").clone();
        // Validate both reachability and the deterministic first-visit order.
        gas.charge((records.len() * 5) as u64)?;
        let canonical = ordered(root.clone(), &mut ())?;
        if canonical.len() != records.len()
            || !canonical
                .iter()
                .zip(&records)
                .all(|(a, b)| a.id() == b.id())
        {
            return Err(CellError::InvalidFormat);
        }
        Ok(root.as_ref().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn leaf(n: u8) -> Cell {
        Cell::new(vec![n], vec![]).unwrap()
    }
    fn decode(bytes: &[u8]) -> Result<Cell, CellError> {
        let mut gas = u64::MAX;
        Cell::decode_transport(bytes, bytes.len(), &mut gas)
    }

    #[test]
    fn shared_dag_has_backward_indexes_and_root_last() {
        let child = leaf(7);
        let a = Cell::new(vec![1], vec![child.clone().into()]).unwrap();
        let b = Cell::new(vec![2], vec![child.into()]).unwrap();
        let root = Cell::new(vec![3], vec![a.into(), b.into()]).unwrap();
        let bytes = root.encode_transport(&mut ()).unwrap();
        assert_eq!(
            bytes,
            [4, 1, 0, 7, 1, 0x10, 1, 1, 1, 0x10, 2, 2, 1, 0x20, 3, 2, 1]
        );
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.commitment(), root.commitment());
        assert_eq!(decoded.encode_transport(&mut ()).unwrap(), bytes);
    }

    #[test]
    fn pruning_preserves_every_level_through_transport() {
        let full = Cell::new(vec![9], vec![leaf(1).into(), leaf(2).into()]).unwrap();
        let proof = Cell::new(
            vec![9],
            vec![leaf(1).into(), leaf(2).prune(1).unwrap().into()],
        )
        .unwrap();
        let decoded = decode(&proof.encode_transport(&mut ()).unwrap()).unwrap();
        assert_eq!(decoded.id(), proof.id());
        assert_eq!(decoded.hash(0).unwrap(), full.id());
        assert_eq!(decoded.commitment(), proof.commitment());
    }

    #[test]
    fn a_proof_of_a_pruned_transaction_retains_its_older_cuts() {
        let inner = Cell::new(
            vec![1],
            vec![leaf(2).into(), leaf(3).prune(1).unwrap().into()],
        )
        .unwrap();
        let other = leaf(4);
        let transaction =
            Cell::new(vec![5], vec![inner.clone().into(), other.clone().into()]).unwrap();
        let proof = Cell::new(vec![5], vec![inner.into(), other.prune(2).unwrap().into()]).unwrap();
        let encoded = proof.encode().unwrap();
        let restored = decode(&encoded).unwrap();
        assert_eq!(restored.hash(1).unwrap(), transaction.id());
        let view = restored.virtualize(1).unwrap();
        let old = view
            .reference(0, &mut ())
            .unwrap()
            .reference(1, &mut ())
            .unwrap();
        assert_eq!(old.is_pruned(), Ok(true));
        assert!(old.payload().is_ok());
        assert!(!view.reference(1, &mut ()).unwrap().is_available());
    }

    #[test]
    fn resident_frontiers_are_unioned_before_encoding() {
        let rich = Cell::new(vec![1], vec![leaf(2).into()]).unwrap();
        let poor = rich.detached();
        let root = Cell::new(vec![3], vec![poor.into(), rich.into()]).unwrap();
        assert_eq!(decode(&root.encode().unwrap()).unwrap().id(), root.id());
    }

    #[test]
    fn rejects_invalid_indexes_orders_counts_and_unloaded_children() {
        for bytes in [
            vec![],
            vec![0],
            vec![0x81, 0, 0, 0],
            vec![1, 0, 0x10, 0],
            vec![1, 0, 0x10, 1],
            vec![1, 0, 0x50],
            vec![1, 0, 0x80],
            vec![2, 1, 0, 1, 1, 0, 2],
            vec![2, 1, 0, 1, 1, 0, 1],
        ] {
            assert!(decode(&bytes).is_err(), "{bytes:?}");
        }
        let c = leaf(1);
        let root = Cell::new(vec![], vec![CellRef::resident(c).to_unloaded()]).unwrap();
        assert!(matches!(
            root.encode_transport(&mut ()),
            Err(CellError::MissingCell(_))
        ));
        let mut trailing = leaf(1).encode_transport(&mut ()).unwrap();
        trailing.push(0);
        assert_eq!(decode(&trailing).unwrap_err(), CellError::TrailingBytes);
    }

    #[test]
    fn count_byte_and_gas_bounds_fail_before_building_a_graph() {
        let bytes = leaf(1).encode_transport(&mut ()).unwrap();
        let mut gas = u64::MAX;
        assert_eq!(
            Cell::decode_transport(&bytes, bytes.len() - 1, &mut gas).unwrap_err(),
            CellError::LimitExceeded
        );
        assert_eq!(
            Cell::decode_transport(&bytes, bytes.len(), &mut 0).unwrap_err(),
            CellError::ResourceExhausted
        );
    }

    #[test]
    fn rejects_valid_topological_order_that_is_not_ordered_postorder() {
        // The two leaves are swapped, but both are reachable through valid indexes.
        let bytes = [3, 1, 0, 2, 1, 0, 1, 0, 0x20, 1, 2];
        assert_eq!(decode(&bytes).unwrap_err(), CellError::InvalidFormat);
        let canonical = [3, 1, 0, 1, 1, 0, 2, 0, 0x20, 2, 1];
        assert_eq!(decode(&canonical).unwrap().encode().unwrap(), canonical);
    }

    #[test]
    fn maximum_payload_references_and_level_mask_roundtrip() {
        let pruned =
            Cell::from_pruned(0x7fff, (0..15).map(|i| [i; 32]).collect(), vec![0; 15]).unwrap();
        let root = Cell::new(vec![7; 4095], vec![pruned.clone().into(); 4]).unwrap();
        let bytes = root.encode().unwrap();
        assert_eq!(bytes[0], 2);
        let restored = decode(&bytes).unwrap();
        assert_eq!(restored.commitment(), root.commitment());
        assert_eq!(restored.refs()[0].commitment(), pruned.commitment());
        assert_eq!(restored.encode().unwrap(), bytes);
    }

    #[test]
    fn deep_chains_are_iterative_and_distance_one_stays_one_byte() {
        let mut root = leaf(1);
        for _ in 0..10_000 {
            root = Cell::new(vec![], vec![root.into()]).unwrap();
        }
        let bytes = root.encode_transport(&mut ()).unwrap();
        assert_eq!(decode(&bytes).unwrap().id(), root.id());
        assert_eq!(bytes.len(), 2 + 3 + 3 * 10_000);
    }
}
