use crate::errors::VMError;
use crate::Cell;
use crate::Dict;
use crate::Int253;
use crate::Merlin;
use crate::Point;
use crate::String;
use crate::{ClearToken, Token, WideToken};
use crate::{Constraint, Expression, Variable};

/// Possible values on the stack machine
pub enum Value {
    Int253(Int253),
    String(String),
    Dict(Dict),
    Point(Point),
    Token(Token),
    WideToken(WideToken),
    ClearToken(ClearToken),
    Cell(Cell),
    Merlin(Merlin),
    Variable(Variable),
    Expression(Expression),
    Constraint(Constraint),
    //MultiscalarMul(MultiscalarMul),
}

impl Value {
    /// Returns a fresh copy of this value if its type is **copyable** per
    /// spec.md §Stack discipline (plain-data types). Linear types
    /// (tokens, cells, variables, expressions, constraints, transcripts,
    /// …) are never copyable and produce [`VMError::TypeNotCopyable`].
    pub fn try_clone(&self) -> Result<Value, VMError> {
        match self {
            Value::Int253(i) => Ok(Value::Int253(*i)),
            Value::String(s) => Ok(Value::String(s.clone())),
            Value::Point(p) => Ok(Value::Point(*p)),
            Value::Dict(d) => Ok(Value::Dict(d.try_clone()?)),
            Value::Token(_)
            | Value::ClearToken(_)
            | Value::WideToken(_)
            | Value::Cell(_)
            | Value::Merlin(_)
            | Value::Variable(_)
            | Value::Expression(_)
            | Value::Constraint(_) => Err(VMError::TypeNotCopyable),
        }
    }

    /// True iff this value's type permits duplication (`dup`/`getdup`).
    /// Plain-data and copyable containers; never linear types.
    pub fn is_copyable(&self) -> bool {
        match self {
            Value::Int253(_) | Value::String(_) | Value::Point(_) => true,
            Value::Dict(d) => d.is_copyable(),
            _ => false,
        }
    }

    /// True iff this value can survive across VM execution — sealed into
    /// a cell or actor state. Plain-data types are portable; `ClearToken`
    /// is portable iff its quantity is non-negative; encrypted in-range
    /// `Token` is portable; everything else (linear, intermediate,
    /// constraint-system-only types) is not.
    ///
    /// Cells are themselves linear handles, not portable — a cell's
    /// payload is portable, but a cell *value on the stack* may not be
    /// placed into another cell's payload (sealing a cell-in-a-cell).
    pub fn is_portable(&self) -> bool {
        match self {
            Value::Int253(_) | Value::String(_) | Value::Point(_) => true,
            Value::Dict(d) => d.is_portable(),
            Value::ClearToken(t) => !t.qty().is_negative(),
            // `Token` is portable per design (encrypted in-range token).
            // The struct is currently empty; once Phase 13 wires fields,
            // every `Token` instance is portable by construction.
            Value::Token(_) => true,
            _ => false,
        }
    }

    /// Returns true iff this value can be silently discarded by `drop`.
    /// Plain-data types are always droppable; cleartokens are droppable
    /// when quantity is zero; empty dicts are droppable; everything else
    /// (including cells — they're linear) is not.
    pub fn is_droppable(&self) -> bool {
        match self {
            Value::Int253(_) | Value::String(_) | Value::Point(_) => true,
            Value::Dict(d) => d.is_empty(),
            Value::ClearToken(t) => t.is_zero_qty(),
            _ => false,
        }
    }

    /// Equality across two stack values.
    ///
    /// - Cross-variant: always `Ok(false)` (different types are not equal).
    /// - Same-variant for plain-data types (`Int253`, `String`, `Point`):
    ///   bytewise / by-value comparison.
    /// - Same-variant for `Dict`: recursive entry-wise comparison.
    /// - Same-variant for linear types (tokens, cells, variables,
    ///   expressions, constraints, transcripts): `Err(TypeNotComparable)`
    ///   — linear types have no stable identity for `eq`.
    pub fn try_eq(&self, other: &Value) -> Result<bool, VMError> {
        match (self, other) {
            (Value::Int253(a), Value::Int253(b)) => Ok(a == b),
            (Value::String(a), Value::String(b)) => Ok(a.as_bytes() == b.as_bytes()),
            (Value::Point(a), Value::Point(b)) => Ok(a.as_bytes() == b.as_bytes()),
            // Cross-variant always unequal.
            (sa, sb) if core::mem::discriminant(sa) != core::mem::discriminant(sb) => Ok(false),
            // Same-variant non-primitives (Dict, tokens, cells, linear types):
            // dicts require recursion + non-trivial gas; linear types have
            // no defined equality. All hard-fail.
            _ => Err(VMError::TypeNotComparable),
        }
    }

    /// Returns the type code used by the `type` opcode.
    pub fn type_code(&self) -> u8 {
        match self {
            Value::Int253(_) => 0,
            Value::String(_) => 68,
            Value::Dict(_) => 128,
            Value::Point(_) => 248,
            Value::Token(_) => 249,
            Value::ClearToken(_) => 250,
            Value::WideToken(_) => 251,
            // `Cell` reuses the wire tag previously assigned to `Object`
            // (the value type was renamed in Phase 9; the wire-tag table
            // stays).
            Value::Cell(_) => 252,
            Value::Merlin(_) => 253,
            // Stack-only — code TBC by Architect.
            Value::Variable(_) => 0xc0,
            Value::Expression(_) => 0xc1,
            Value::Constraint(_) => 0xc2,
        }
    }
}
