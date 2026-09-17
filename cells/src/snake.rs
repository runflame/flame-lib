use std::sync::Arc;

use crate::{
    BagOfCells, Cell, CellBuilder, CellError, CellRef, CellResolver, CellSlice, GasMeter,
    MAX_CELL_PAYLOAD, MAX_CELL_REFS,
};

fn root(length: u32, inline: usize, refs: Vec<CellRef>) -> Cell {
    let mut payload = length.to_le_bytes().to_vec();
    payload.resize(4 + inline, 0);
    Cell::new(payload, refs).unwrap()
}

fn child(length: usize, refs: Vec<CellRef>) -> CellRef {
    CellRef::resident(Cell::new(vec![0; length], refs).unwrap())
}

#[test]
fn canonical_boundaries_round_trip() {
    for (length, segments) in [
        (0, vec![4]),
        (1, vec![5]),
        (8186, vec![8190]),
        (8187, vec![8191]),
        (8188, vec![8191, 1]),
        (8191, vec![8191, 4]),
        (8192, vec![8191, 5]),
        (16378, vec![8191, 8191]),
        (16379, vec![8191, 8191, 1]),
    ] {
        let bytes: Vec<_> = (0..length).map(|i| i as u8).collect();
        let mut builder = CellBuilder::new();
        builder.store_snake(&bytes).unwrap();
        let cell = builder.build();
        assert_eq!(&cell.payload()[..4], &(length as u32).to_le_bytes());
        let mut node = &cell;
        for (index, expected) in segments.iter().enumerate() {
            assert_eq!(node.payload().len(), *expected, "length {length}");
            if index + 1 < segments.len() {
                assert_eq!(node.refs().len(), 1);
                node = node.refs()[0].as_resident().unwrap();
            } else {
                assert!(node.refs().is_empty());
            }
        }
        let mut slice = CellSlice::new(&cell);
        assert_eq!(slice.load_snake(&mut (), length).unwrap(), bytes);
        slice.finish().unwrap();
    }
}

#[test]
fn header_offset_and_parent_references_are_preserved() {
    for header_length in [0, 1, 7, MAX_CELL_PAYLOAD - 4] {
        for length in [0, 1, 9000] {
            let header = vec![0xaa; header_length];
            let bytes = vec![0xbb; length];
            let before = child(1, vec![]);
            let after = child(2, vec![]);
            let mut builder = CellBuilder::new();
            builder.store_bytes(&header).unwrap();
            builder.store_ref(before.clone()).unwrap();
            builder.store_snake(&bytes).unwrap();
            builder.store_ref(after.clone()).unwrap();
            let cell = builder.build();
            let payload_length = header_length + 4 + length;
            assert_eq!(cell.payload().len(), payload_length.min(MAX_CELL_PAYLOAD));
            assert_eq!(
                cell.refs().len(),
                2 + usize::from(payload_length > MAX_CELL_PAYLOAD)
            );
            let mut slice = CellSlice::new(&cell);
            assert_eq!(slice.load_bytes(header_length).unwrap(), header);
            assert_eq!(slice.load_ref().unwrap().id(), before.id());
            assert_eq!(slice.load_snake(&mut (), bytes.len()).unwrap(), bytes);
            assert_eq!(slice.load_ref().unwrap().id(), after.id());
            slice.finish().unwrap();
        }
    }
}

#[test]
fn inline_strings_allow_later_payload_and_do_not_consume_refs() {
    let mut builder = CellBuilder::new();
    builder.store_snake(b"").unwrap();
    builder.store_snake(b"hello").unwrap();
    builder.store_u32(0x12345678).unwrap();
    builder.store_snake(b"world").unwrap();
    let later = CellRef::pruned([7; 32]);
    builder.store_ref(later.clone()).unwrap();
    let cell = builder.build();
    let mut slice = CellSlice::new(&cell);
    assert_eq!(slice.load_snake(&mut (), 0).unwrap(), b"");
    assert_eq!(slice.load_snake(&mut (), 5).unwrap(), b"hello");
    assert_eq!(slice.load_u32().unwrap(), 0x12345678);
    assert_eq!(slice.load_snake(&mut (), 5).unwrap(), b"world");
    assert_eq!(slice.load_ref().unwrap().id(), later.id());
    slice.finish().unwrap();

    let cell = root(8187, 8187, vec![later.clone()]);
    let mut slice = CellSlice::new(&cell);
    assert_eq!(slice.load_snake(&mut (), 8187).unwrap().len(), 8187);
    assert_eq!(slice.load_ref().unwrap().id(), later.id());
    slice.finish().unwrap();
}

#[test]
fn capacity_errors_leave_the_builder_unchanged() {
    for free in 0..4 {
        let payload = vec![0xaa; MAX_CELL_PAYLOAD - free];
        let mut builder = CellBuilder::new();
        builder.store_bytes(&payload).unwrap();
        builder.store_ref(child(0, vec![])).unwrap();
        assert!(matches!(
            builder.store_snake(b""),
            Err(CellError::PayloadCapacity)
        ));
        let cell = builder.build();
        assert_eq!(cell.payload(), payload);
        assert_eq!(cell.refs().len(), 1);
    }

    let mut builder = CellBuilder::new();
    builder.store_u8(0xaa).unwrap();
    for _ in 0..MAX_CELL_REFS {
        builder.store_ref(child(0, vec![])).unwrap();
    }
    assert!(matches!(
        builder.store_snake(&vec![0; MAX_CELL_PAYLOAD]),
        Err(CellError::ReferenceCapacity)
    ));
    assert_eq!(builder.used_bytes(), 1);
    assert_eq!(builder.used_refs(), MAX_CELL_REFS);
    // Inline strings do not need a spare reference slot.
    builder.store_snake(b"ok").unwrap();
    let cell = builder.build();
    assert_eq!(cell.payload(), &[0xaa, 2, 0, 0, 0, b'o', b'k']);
}

#[test]
fn noncanonical_roots_and_continuations_are_rejected_atomically() {
    let tail = child(1, vec![]);
    let malformed = [
        root(2, 1, vec![tail.clone()]), // Short parent cannot be crossed.
        root(1, 0, vec![]),
        root(8188, 8187, vec![child(0, vec![])]), // Truncated tail.
        root(8188, 8187, vec![child(2, vec![])]), // Overlong tail.
        root(8188, 8187, vec![child(1, vec![tail.clone()])]),
        root(16379, 8187, vec![child(8190, vec![tail.clone()])]),
        root(16379, 8187, vec![child(8191, vec![])]),
        root(16379, 8187, vec![child(8191, vec![tail.clone(), tail])]),
    ];
    for cell in malformed {
        let mut slice = CellSlice::new(&cell);
        assert_eq!(
            slice.load_snake(&mut (), usize::MAX),
            Err(CellError::InvalidFormat)
        );
        assert_eq!(slice.remaining_bytes(), cell.payload().len());
        assert_eq!(slice.remaining_refs(), cell.refs().len());
    }

    for prefix_bytes in 0..4 {
        let cell = Cell::new(vec![0; prefix_bytes], vec![]).unwrap();
        let mut slice = CellSlice::new(&cell);
        assert_eq!(
            slice.load_snake(&mut (), usize::MAX),
            Err(CellError::InsufficientBytes)
        );
        assert_eq!(slice.remaining_bytes(), prefix_bytes);
    }
    let cell = root(8188, 8187, vec![]);
    let mut slice = CellSlice::new(&cell);
    assert_eq!(
        slice.load_snake(&mut (), 8188),
        Err(CellError::InsufficientReferences)
    );
    assert_eq!(slice.remaining_bytes(), MAX_CELL_PAYLOAD);
}

#[test]
fn length_and_resolution_failures_preserve_the_cursor() {
    struct Budget(usize);
    impl CellResolver for Budget {
        fn resolve(&mut self, _: &CellRef) -> Result<Arc<Cell>, CellError> {
            self.0 += 1;
            Err(CellError::ResourceExhausted)
        }
    }
    let missing_id = [7; 32];
    let cell = root(8188, 8187, vec![CellRef::pruned(missing_id)]);
    let mut slice = CellSlice::new(&cell);
    let mut budget = Budget(0);
    assert_eq!(
        slice.load_snake(&mut budget, 8187),
        Err(CellError::LimitExceeded)
    );
    assert_eq!(budget.0, 0);
    assert_eq!(
        slice.load_snake(&mut budget, 8188),
        Err(CellError::ResourceExhausted)
    );
    assert_eq!(budget.0, 1);
    assert_eq!(
        slice.load_snake(&mut (), 8188),
        Err(CellError::MissingCell(missing_id))
    );
    assert_eq!(slice.remaining_bytes(), MAX_CELL_PAYLOAD);
    assert_eq!(slice.remaining_refs(), 1);

    struct Wrong(Arc<Cell>);
    impl CellResolver for Wrong {
        fn resolve(&mut self, _: &CellRef) -> Result<Arc<Cell>, CellError> {
            Ok(self.0.clone())
        }
    }
    let wrong = Arc::new(Cell::new(vec![0], vec![]).unwrap());
    assert_eq!(
        slice.load_snake(&mut Wrong(wrong.clone()), 8188),
        Err(CellError::CellHashMismatch {
            expected: missing_id,
            actual: wrong.id()
        })
    );
    assert_eq!(slice.remaining_bytes(), MAX_CELL_PAYLOAD);
    assert_eq!(slice.remaining_refs(), 1);
    let cell = root(u32::MAX, 0, vec![]);
    assert_eq!(
        CellSlice::new(&cell).load_snake(&mut budget, 100),
        Err(CellError::LimitExceeded)
    );
    assert_eq!(budget.0, 1);

    // Roll back to the current cursor, not the beginning of its parent Cell.
    let mut payload = vec![0xaa];
    payload.extend_from_slice(&8187u32.to_le_bytes());
    payload.resize(MAX_CELL_PAYLOAD, 0);
    let cell = Cell::new(payload, vec![child(0, vec![]), CellRef::pruned(missing_id)]).unwrap();
    let mut slice = CellSlice::new(&cell);
    assert_eq!(slice.load_u8().unwrap(), 0xaa);
    slice.load_ref().unwrap();
    assert_eq!(
        slice.load_snake(&mut budget, 8187),
        Err(CellError::ResourceExhausted)
    );
    assert_eq!(slice.remaining_bytes(), MAX_CELL_PAYLOAD - 1);
    assert_eq!(slice.remaining_refs(), 1);
    assert_eq!(slice.load_u32().unwrap(), 8187);
}

#[test]
fn boc_round_trip_resolves_pruned_continuations() {
    struct FreeGas;
    impl GasMeter for FreeGas {
        fn charge(&mut self, _: u64) -> Result<(), CellError> {
            Ok(())
        }
    }
    let bytes: Vec<_> = (0..25000).map(|i| i as u8).collect();
    let mut builder = CellBuilder::new();
    builder.store_snake(&bytes).unwrap();
    let cell = Arc::new(builder.build());
    let encoded = BagOfCells::collect(cell.clone()).unwrap().encode();
    let mut bag = BagOfCells::decode(&encoded, encoded.len(), &mut FreeGas).unwrap();
    let detached = bag.get(&cell.id()).unwrap();
    assert!(detached.refs().iter().all(CellRef::is_pruned));
    let mut slice = CellSlice::new(&detached);
    assert_eq!(slice.load_snake(&mut bag, bytes.len()).unwrap(), bytes);
    slice.finish().unwrap();
}
