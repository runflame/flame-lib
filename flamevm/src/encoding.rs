//! Expected-type Cell codecs. Only the `Value` sum type carries a type tag.

use cells::{
    resolve_cell, Cell, CellBuilder, CellDecode, CellEncode, CellError, CellRef, CellResolver,
    CellSlice,
};
use curve25519_dalek::ristretto::CompressedRistretto;

use crate::{ClearToken, Commitment, Dict, Point, Scalar, String, Token, Value};

/// Bound recursive typed decoding independently of witness availability or gas.
pub(crate) const MAX_VALUE_DEPTH: usize = 128;

/// Encodes a complete length-prefixed byte string in a dedicated Cell chain.
pub(crate) fn blob_cell(bytes: &[u8]) -> Result<Cell, CellError> {
    let mut builder = CellBuilder::new();
    builder.store_snake(bytes)?;
    Ok(builder.build())
}

/// Resolves and exactly reads one dedicated byte-string Cell chain.
pub(crate) fn read_blob<R: CellResolver + ?Sized>(
    reference: &CellRef,
    resolver: &mut R,
    limit: usize,
) -> Result<Vec<u8>, CellError> {
    let cell = resolve_cell(resolver, reference)?;
    let mut slice = CellSlice::new(&cell);
    let bytes = slice.load_snake(resolver, limit)?;
    slice.finish()?;
    Ok(bytes)
}

impl CellEncode for Scalar {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder.store_bytes(self.as_bytes())?;
        Ok(())
    }
}

impl CellDecode for Scalar {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        resolver: &mut R,
    ) -> Result<Self, CellError> {
        slice.try_load(|slice| {
            Self::from_bytes(<[u8; 32]>::decode(slice, resolver)?).ok_or(CellError::InvalidFormat)
        })
    }
}

impl CellEncode for Point {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder.store_bytes(&self.to_bytes())?;
        Ok(())
    }
}

impl CellDecode for Point {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        resolver: &mut R,
    ) -> Result<Self, CellError> {
        Ok(Self::from_bytes(<[u8; 32]>::decode(slice, resolver)?))
    }
}

impl CellEncode for Token {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder.store_bytes(self.qty().to_point().as_bytes())?;
        builder.store_bytes(self.flv().to_point().as_bytes())?;
        Ok(())
    }
}

impl CellDecode for Token {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        resolver: &mut R,
    ) -> Result<Self, CellError> {
        slice.try_load(|slice| {
            let qty = CompressedRistretto(<[u8; 32]>::decode(slice, resolver)?);
            let flv = CompressedRistretto(<[u8; 32]>::decode(slice, resolver)?);
            Ok(Self::new(Commitment::Closed(qty), Commitment::Closed(flv)))
        })
    }
}

impl CellEncode for ClearToken {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        builder.store(&self.qty())?.store(&self.flv())?;
        Ok(())
    }
}

impl CellDecode for ClearToken {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        resolver: &mut R,
    ) -> Result<Self, CellError> {
        slice.try_load(|slice| {
            Ok(Self::new(
                Scalar::decode(slice, resolver)?,
                Scalar::decode(slice, resolver)?,
            ))
        })
    }
}

impl CellEncode for String {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        let len = self.len();
        if len > Self::MAX_LEN {
            return Err(CellError::PayloadTooLarge {
                actual: len,
                max: Self::MAX_LEN,
            });
        }
        if let Some(bytes) = self.as_opaque() {
            builder.store_bytes(bytes)?;
        } else {
            // Check the size before compiling/copying private witness bytes.
            builder.store_bytes(&self.to_bytes_vec())?;
        }
        Ok(())
    }
}

impl CellDecode for String {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        _resolver: &mut R,
    ) -> Result<Self, CellError> {
        if slice.remaining_refs() != 0 {
            return Err(CellError::InvalidFormat);
        }
        Ok(Self::from(
            slice.load_bytes(slice.remaining_bytes())?.to_vec(),
        ))
    }
}

impl CellEncode for Value {
    fn encode(&self, builder: &mut CellBuilder) -> Result<(), CellError> {
        encode_value_at(self, builder, 0)
    }
}

fn encode_value_at(
    value: &Value,
    builder: &mut CellBuilder,
    depth: usize,
) -> Result<(), CellError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(CellError::LimitExceeded);
    }
    match value {
        Value::Scalar(value) => {
            builder.store_u8(0)?.store(value)?;
        }
        Value::String(value) => {
            builder
                .store_u8(1)?
                .store_ref(CellRef::resident(value.to_cell()?))?;
        }
        Value::Dict(value) => {
            let mut child = CellBuilder::new();
            value.encode_at(&mut child, depth + 1)?;
            builder
                .store_u8(2)?
                .store_ref(CellRef::resident(child.build()))?;
        }
        Value::Point(value) => {
            builder.store_u8(3)?.store(value)?;
        }
        Value::Token(value) => {
            builder.store_u8(4)?.store(value)?;
        }
        Value::ClearToken(value) => {
            builder.store_u8(6)?.store(value)?;
        }
        // No encoding exists for computation-only witnesses, WideToken, or
        // the linear Contract handle. Portability of encodable types is a
        // separate domain-boundary check: negative ClearTokens still encode.
        _ => return Err(CellError::InvalidFormat),
    }
    Ok(())
}

pub(crate) fn value_cell_at(value: &Value, depth: usize) -> Result<Cell, CellError> {
    let mut builder = CellBuilder::new();
    encode_value_at(value, &mut builder, depth)?;
    Ok(builder.build())
}

impl CellDecode for Value {
    fn decode<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        resolver: &mut R,
    ) -> Result<Self, CellError> {
        decode_value_at(slice, resolver, 0, false)
    }
}

impl Value {
    /// Logical work for a Cell-encoding attempt, without allocating encoded
    /// bodies. Runtime-only values may still be placed in Dict caches; their
    /// placeholder costs are included, but this does not admit them to storage.
    pub(crate) fn encoding_gas(&self) -> Result<u64, crate::VMError> {
        let mut gas = 0u64;
        let mut pending = vec![(self, 0usize)];
        while let Some((value, depth)) = pending.pop() {
            // The encoder rejects here before inspecting descendants. Keep
            // this estimator iterative and bounded even for runtime-only Dicts.
            let work = if depth > MAX_VALUE_DEPTH {
                1
            } else {
                match value {
                    Value::Scalar(_) | Value::Point(_) => 35,
                    Value::Token(_) | Value::ClearToken(_) => 67,
                    Value::String(string) => {
                        string.check_len()?;
                        // Copy/hash one raw String Cell, plus the tagged Value
                        // Cell containing its single reference.
                        2 * (string.len() as u64 + 2) + 35
                    }
                    Value::Dict(dict) if depth < MAX_VALUE_DEPTH => {
                        pending.extend(dict.cached_values().map(|value| (value, depth + 1)));
                        dict.encoding_path_gas()?
                            .checked_add(78)
                            .ok_or(crate::VMError::OutOfGas)?
                    }
                    _ => 1,
                }
            };
            gas = gas.checked_add(work).ok_or(crate::VMError::OutOfGas)?;
        }
        Ok(gas)
    }

    /// Decodes a Value from authenticated state, retaining lazy Dict branches.
    /// Call only after the owning Contract/Actor/state transition establishes
    /// the validity of its committed capability summaries and entry counts.
    pub fn decode_trusted<R: CellResolver + ?Sized>(
        slice: &mut CellSlice<'_>,
        resolver: &mut R,
    ) -> Result<Self, CellError> {
        decode_value_at(slice, resolver, 0, true)
    }

    /// Exactly decodes an authenticated Value Cell without eagerly loading Dicts.
    pub fn from_trusted_cell<R: CellResolver + ?Sized>(
        cell: &Cell,
        resolver: &mut R,
    ) -> Result<Self, CellError> {
        value_from_cell_at(cell, resolver, 0, true)
    }
}

pub(crate) fn value_from_cell_at<R: CellResolver + ?Sized>(
    cell: &Cell,
    resolver: &mut R,
    depth: usize,
    trusted: bool,
) -> Result<Value, CellError> {
    let mut slice = CellSlice::new(cell);
    let value = decode_value_at(&mut slice, resolver, depth, trusted)?;
    slice.finish()?;
    Ok(value)
}

fn decode_value_at<R: CellResolver + ?Sized>(
    slice: &mut CellSlice<'_>,
    resolver: &mut R,
    depth: usize,
    trusted: bool,
) -> Result<Value, CellError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(CellError::LimitExceeded);
    }
    slice.try_load(|slice| {
        Ok(match slice.load_u8()? {
            0 => Value::Scalar(Scalar::decode(slice, resolver)?),
            1 => {
                let cell = resolve_cell(resolver, &slice.load_ref()?)?;
                Value::String(String::from_cell(&cell, resolver)?)
            }
            2 => {
                let cell = resolve_cell(resolver, &slice.load_ref()?)?;
                let mut child = CellSlice::new(&cell);
                let dict = if trusted {
                    Dict::decode_trusted(&mut child, resolver)?
                } else {
                    Dict::decode_at(&mut child, resolver, depth + 1)?
                };
                child.finish()?;
                Value::Dict(dict)
            }
            3 => Value::Point(Point::decode(slice, resolver)?),
            4 => Value::Token(Token::decode(slice, resolver)?),
            6 => Value::ClearToken(ClearToken::decode(slice, resolver)?),
            _ => return Err(CellError::InvalidFormat),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Merlin;
    use cells::{BagOfCells, Trie, MAX_CELL_PAYLOAD};
    use std::sync::Arc;

    #[test]
    fn primitive_layouts_have_no_tags_and_value_adds_one_byte() {
        let scalar = Scalar::from(256u64);
        assert_eq!(scalar.to_cell().unwrap().payload(), scalar.as_bytes());
        assert_eq!(
            Scalar::from_cell(&scalar.to_cell().unwrap(), &mut ()).unwrap(),
            scalar
        );
        assert_eq!(
            Point::from_bytes([7; 32]).to_cell().unwrap().payload(),
            &[7; 32]
        );
        let token = Token::cleartext(Scalar::ONE, scalar).unwrap();
        let cell = token.to_cell().unwrap();
        assert_eq!(cell.payload().len(), 64);
        assert!(cell.refs().is_empty());
        assert_eq!(
            Token::from_cell(&cell, &mut ())
                .unwrap()
                .to_cell()
                .unwrap()
                .id(),
            cell.id()
        );
        let value = Value::Token(token).to_cell().unwrap();
        assert_eq!(value.payload()[0], 4);
        assert_eq!(&value.payload()[1..], cell.payload());
    }

    #[test]
    fn encoding_work_counts_nested_values_and_stops_at_the_format_depth() {
        let small = Value::Dict(Dict::from_values(vec![Value::Scalar(Scalar::ONE)]));
        let large = Value::Dict(Dict::from_values(vec![Value::String(String::from(
            vec![7; MAX_CELL_PAYLOAD],
        ))]));
        assert!(large.encoding_gas().unwrap() > small.encoding_gas().unwrap() + 16_000);
        let mut nested = Value::Scalar(Scalar::ONE);
        for _ in 0..=MAX_VALUE_DEPTH {
            nested = Value::Dict(Dict::from_values(vec![nested]));
        }
        assert!(nested.encoding_gas().unwrap() > 0);
        assert!(matches!(nested.to_cell(), Err(CellError::LimitExceeded)));
    }

    #[test]
    fn encoding_does_not_reject_negative_clear_tokens() {
        let token = ClearToken::new(-Scalar::ONE, Scalar::from(17u64));
        assert!(!token.is_portable());
        let cell = token.to_cell().unwrap();
        assert_eq!(cell.payload().len(), 64);
        assert_eq!(&cell.payload()[..32], token.qty().as_bytes());
        assert_eq!(&cell.payload()[32..], token.flv().as_bytes());
        let value = Value::ClearToken(token).to_cell().unwrap();
        assert!(
            matches!(Value::from_cell(&value, &mut ()).unwrap(), Value::ClearToken(t) if t.qty() == -Scalar::ONE)
        );
        assert!(matches!(
            Value::Merlin(Merlin::new(b"unsupported")).to_cell(),
            Err(CellError::InvalidFormat)
        ));
    }

    #[test]
    fn strings_use_one_raw_payload_cell_and_load_from_boc() {
        for length in [0, 1, MAX_CELL_PAYLOAD - 1, MAX_CELL_PAYLOAD] {
            let bytes = vec![0xa5; length];
            let string_cell = String::from(bytes.clone()).to_cell().unwrap();
            assert_eq!(string_cell.payload(), bytes);
            assert!(string_cell.refs().is_empty());
            assert_eq!(string_cell.encoded_size(), length + 2);
            let cell = Value::String(String::from(bytes.clone()))
                .to_cell()
                .unwrap();
            assert_eq!(cell.payload(), &[1]);
            assert_eq!(cell.refs().len(), 1);
            assert_eq!(cell.refs()[0].id(), string_cell.id());
            let mut bag = BagOfCells::collect(Arc::new(cell.clone())).unwrap();
            let root = Cell::decode_exact(&cell.encode()).unwrap();
            assert!(
                matches!(Value::from_cell(&root, &mut bag).unwrap(), Value::String(s) if s.to_bytes_vec() == bytes)
            );
        }
        let witness = String::scalar(Scalar::ONE).to_cell().unwrap();
        assert_eq!(witness.payload(), Scalar::ONE.as_bytes());
        assert!(witness.refs().is_empty());
    }

    #[test]
    fn strings_reject_oversized_payloads_and_all_references() {
        let oversized = String::from(vec![0; MAX_CELL_PAYLOAD + 1]);
        assert!(matches!(
            oversized.to_cell(),
            Err(CellError::PayloadTooLarge { .. })
        ));
        assert!(Value::String(oversized).encoding_gas().is_err());
        let script = String::script(vec![crate::ops::Instruction::Nop; MAX_CELL_PAYLOAD + 1]);
        assert!(matches!(
            script.to_cell(),
            Err(CellError::PayloadTooLarge { .. })
        ));

        for reference in [
            CellRef::pruned([1; 32]),
            CellRef::resident(Cell::new(vec![], vec![]).unwrap()),
        ] {
            let cell = Cell::new(vec![0xaa], vec![reference]).unwrap();
            let mut slice = CellSlice::new(&cell);
            assert!(matches!(
                String::decode(&mut slice, &mut ()),
                Err(CellError::InvalidFormat)
            ));
            assert_eq!(slice.remaining_bytes(), 1);
            assert_eq!(slice.remaining_refs(), 1);
            let wrapped = Cell::new(vec![1], vec![CellRef::resident(cell)]).unwrap();
            assert!(matches!(
                Value::from_cell(&wrapped, &mut ()),
                Err(CellError::InvalidFormat)
            ));
        }
    }

    #[test]
    fn arbitrary_blobs_still_use_snake_chains() {
        let bytes = vec![0xa5; 20_000];
        let cell = blob_cell(&bytes).unwrap();
        assert_eq!(cell.payload().len(), MAX_CELL_PAYLOAD);
        assert_eq!(cell.refs().len(), 1);
        let mut bag = BagOfCells::collect(Arc::new(cell.clone())).unwrap();
        assert_eq!(
            read_blob(&CellRef::pruned(cell.id()), &mut bag, bytes.len()).unwrap(),
            bytes
        );
    }

    #[test]
    fn malformed_data_is_rejected_without_consuming_the_slice() {
        for payload in [
            vec![0],
            vec![5],
            vec![255],
            [vec![0], vec![255; 32]].concat(),
        ] {
            let cell = Cell::new(payload, vec![]).unwrap();
            let mut slice = CellSlice::new(&cell);
            assert!(Value::decode(&mut slice, &mut ()).is_err());
            assert_eq!(slice.remaining_bytes(), cell.payload().len());
        }
        let scalar = Scalar::ONE.to_cell().unwrap();
        let trailing = Cell::new([scalar.payload(), &[0]].concat(), vec![]).unwrap();
        assert!(matches!(
            Scalar::from_cell(&trailing, &mut ()),
            Err(CellError::TrailingBytes)
        ));
    }

    fn wrap_dict(value: Cell) -> Cell {
        let mut trie = Trie::new(32).unwrap();
        trie.insert(&[0; 32], value, &mut ()).unwrap();
        let mut dict = CellBuilder::new();
        dict.store_u64(1)
            .unwrap()
            .store_u8(3)
            .unwrap()
            .store_ref(trie.into_root().unwrap())
            .unwrap();
        Cell::new(vec![2], vec![CellRef::resident(dict.build())]).unwrap()
    }

    #[test]
    fn nested_untrusted_dicts_have_an_explicit_depth_bound() {
        let mut cell = Value::Scalar(Scalar::ONE).to_cell().unwrap();
        for _ in 0..MAX_VALUE_DEPTH {
            cell = wrap_dict(cell);
        }
        assert!(Value::from_cell(&cell, &mut ()).is_ok());
        let too_deep = wrap_dict(cell);
        assert!(matches!(
            Value::from_cell(&too_deep, &mut ()),
            Err(CellError::LimitExceeded)
        ));
        // Already authenticated state may remain lazy regardless of how much
        // deeper data exists; a single access does not recurse through it.
        assert!(Value::from_trusted_cell(&too_deep, &mut ()).is_ok());
    }
}
