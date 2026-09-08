use std::{collections::HashSet, sync::Arc};

use crate::{Cell, CellError, CellID, CellRef, CellResolver, MAX_CELL_PAYLOAD, resolve_cell};

/// An arbitrary byte string stored as a linear chain of Cells.
pub struct Snake {
    root: CellRef,
}

impl Snake {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut next = None;
        for chunk in bytes.chunks(MAX_CELL_PAYLOAD).rev() {
            let refs = next.into_iter().collect();
            next = Some(CellRef::resident(
                Cell::new(chunk.to_vec(), refs).expect("Snake chunks fit in a Cell"),
            ));
        }

        Self {
            root: next
                .unwrap_or_else(|| CellRef::resident(Cell::new(Vec::new(), Vec::new()).unwrap())),
        }
    }

    pub fn from_root(root: CellRef) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &CellRef {
        &self.root
    }

    pub fn reader<'a, R: CellResolver + ?Sized>(&'a self, cells: &'a mut R) -> SnakeReader<'a, R> {
        SnakeReader {
            cells,
            next: Some(self.root.clone()),
            current: None,
            offset: 0,
            seen: HashSet::new(),
        }
    }

    pub fn to_bytes<R: CellResolver + ?Sized>(
        &self,
        cells: &mut R,
        limit: usize,
    ) -> Result<Vec<u8>, CellError> {
        let mut reader = self.reader(cells);
        let mut bytes = Vec::with_capacity(limit.min(MAX_CELL_PAYLOAD));
        let mut buffer = [0; MAX_CELL_PAYLOAD];

        loop {
            let remaining = limit.saturating_sub(bytes.len());
            let wanted = remaining.saturating_add(1).min(MAX_CELL_PAYLOAD);
            let read = reader.read(&mut buffer[..wanted])?;
            if read == 0 {
                return Ok(bytes);
            }
            if read > remaining {
                return Err(CellError::LimitExceeded);
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
    }
}

/// Incremental, non-recursive Snake reader.
pub struct SnakeReader<'a, R: CellResolver + ?Sized> {
    cells: &'a mut R,
    next: Option<CellRef>,
    current: Option<Arc<Cell>>,
    offset: usize,
    seen: HashSet<CellID>,
}

impl<R: CellResolver + ?Sized> SnakeReader<'_, R> {
    pub fn read(&mut self, output: &mut [u8]) -> Result<usize, CellError> {
        if output.is_empty() {
            return Ok(0);
        }

        loop {
            if let Some(cell) = &self.current {
                if self.offset < cell.payload().len() {
                    let count = output.len().min(cell.payload().len() - self.offset);
                    output[..count]
                        .copy_from_slice(&cell.payload()[self.offset..self.offset + count]);
                    self.offset += count;
                    return Ok(count);
                }
                self.current = None;
            }

            let Some(reference) = self.next.as_ref().cloned() else {
                return Ok(0);
            };
            let id = reference.id();
            if self.seen.contains(&id) {
                return Err(CellError::Cycle(id));
            }

            let cell = resolve_cell(self.cells, &reference)?;
            let next = match cell.refs() {
                [] if cell.payload().is_empty() && !self.seen.is_empty() => {
                    return Err(CellError::InvalidFormat);
                }
                [] => None,
                [next] if cell.payload().len() == MAX_CELL_PAYLOAD => Some(next.clone()),
                _ => return Err(CellError::InvalidFormat),
            };

            self.seen.insert(id);
            self.next = next;
            self.offset = 0;
            self.current = Some(cell);
        }
    }
}

/// A minimal in-memory Snake builder.
#[derive(Default)]
pub struct SnakeWriter {
    bytes: Vec<u8>,
}

impl SnakeWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write(&mut self, input: &[u8]) {
        self.bytes.extend_from_slice(input);
    }

    pub fn finish(self) -> Snake {
        Snake::from_bytes(&self.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload_lengths(snake: &Snake) -> Vec<usize> {
        let mut lengths = Vec::new();
        let mut reference = snake.root();
        loop {
            let cell = reference.as_resident().unwrap();
            lengths.push(cell.payload().len());
            match cell.refs() {
                [] => return lengths,
                [next] => reference = next,
                _ => panic!("invalid constructed Snake"),
            }
        }
    }

    #[test]
    fn canonical_boundaries_round_trip() {
        for (length, segments) in [
            (0, vec![0]),
            (1, vec![1]),
            (8190, vec![8190]),
            (8191, vec![8191]),
            (8192, vec![8191, 1]),
            (16382, vec![8191, 8191]),
        ] {
            let bytes: Vec<_> = (0..length).map(|i| i as u8).collect();
            let snake = Snake::from_bytes(&bytes);
            assert_eq!(payload_lengths(&snake), segments);
            assert_eq!(snake.to_bytes(&mut (), length).unwrap(), bytes);
        }
    }

    #[test]
    fn reader_and_writer_are_incremental() {
        let bytes: Vec<_> = (0..MAX_CELL_PAYLOAD + 17).map(|i| i as u8).collect();
        let mut writer = SnakeWriter::new();
        for chunk in bytes.chunks(7) {
            writer.write(chunk);
        }
        let snake = writer.finish();
        let mut resolver = ();
        let mut reader = snake.reader(&mut resolver);
        let mut decoded = Vec::new();
        let mut buffer = [0; 3];
        loop {
            let count = reader.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            decoded.extend_from_slice(&buffer[..count]);
        }
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn malformed_cells_are_rejected() {
        let tail = CellRef::resident(Cell::new(Vec::new(), Vec::new()).unwrap());
        let short = Snake::from_root(CellRef::resident(
            Cell::new(vec![0; MAX_CELL_PAYLOAD - 1], vec![tail.clone()]).unwrap(),
        ));
        assert_eq!(
            short.to_bytes(&mut (), usize::MAX),
            Err(CellError::InvalidFormat)
        );

        let extra = Snake::from_root(CellRef::resident(
            Cell::new(vec![0; MAX_CELL_PAYLOAD], vec![tail.clone(), tail]).unwrap(),
        ));
        assert_eq!(
            extra.to_bytes(&mut (), usize::MAX),
            Err(CellError::InvalidFormat)
        );

        let empty_sentinel = Snake::from_root(CellRef::resident(
            Cell::new(
                vec![0; MAX_CELL_PAYLOAD],
                vec![CellRef::resident(Cell::new(vec![], vec![]).unwrap())],
            )
            .unwrap(),
        ));
        assert_eq!(
            empty_sentinel.to_bytes(&mut (), MAX_CELL_PAYLOAD),
            Err(CellError::InvalidFormat)
        );
    }

    #[test]
    fn missing_repeated_and_over_limit_chains_are_rejected() {
        let missing_id = [7; 32];
        let missing = Snake::from_root(CellRef::resident(
            Cell::new(vec![0; MAX_CELL_PAYLOAD], vec![CellRef::pruned(missing_id)]).unwrap(),
        ));
        assert_eq!(
            missing.to_bytes(&mut (), MAX_CELL_PAYLOAD + 1),
            Err(CellError::MissingCell(missing_id))
        );

        let snake = Snake::from_bytes(&[1]);
        let root_id = snake.root().id();
        let mut resolver = ();
        let mut reader = snake.reader(&mut resolver);
        reader.seen.insert(root_id);
        assert_eq!(reader.read(&mut [0]), Err(CellError::Cycle(root_id)));

        assert_eq!(
            Snake::from_bytes(&[1, 2]).to_bytes(&mut (), 1),
            Err(CellError::LimitExceeded)
        );
        assert_eq!(Snake::from_bytes(&[]).to_bytes(&mut (), 0).unwrap(), b"");
    }
}
