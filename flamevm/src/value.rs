use crate::errors::VMError;
use crate::Dict;
use crate::Int253;
use crate::Merlin;
use crate::Object;
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
    Object(Object),
    Merlin(Merlin),
    Variable(Variable),
    Expression(Expression),
    Constraint(Constraint),
    //MultiscalarMul(MultiscalarMul),
}

impl Value {
    /// Returns a fresh copy of this value if its type is **copyable** per
    /// spec.md §Stack discipline (plain-data types). Linear types
    /// (tokens, objects, variables, expressions, constraints, transcripts,
    /// …) are never copyable and produce [`VMError::TypeNotCopyable`].
    ///
    /// Phase 1 covers the types currently constructible by stack-literal
    /// opcodes (`Int253`, `String`, `Point`). `Dict` becomes copyable in
    /// Phase 5 once member-flag propagation lands.
    pub fn try_clone(&self) -> Result<Value, VMError> {
        match self {
            Value::Int253(i) => Ok(Value::Int253(*i)),
            Value::String(s) => Ok(Value::String(s.clone())),
            Value::Point(p) => Ok(Value::Point(*p)),
            Value::Dict(d) => Ok(Value::Dict(d.try_clone()?)),
            Value::Token(_)
            | Value::ClearToken(_)
            | Value::WideToken(_)
            | Value::Object(_)
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
    pub fn is_portable(&self) -> bool {
        match self {
            Value::Int253(_) | Value::String(_) | Value::Point(_) => true,
            Value::Dict(d) => d.is_portable(),
            Value::ClearToken(t) => !t.qty().is_negative(),
            // Phase 1 / 5: `Token` (encrypted, in-range) is a placeholder
            // empty struct. Treat as portable once it gains real fields
            // in Phase 13. For now, `Token` instances cannot be created,
            // so this case is unreachable in practice.
            Value::Token(_) => true,
            _ => false,
        }
    }

    /// Returns true iff this value can be silently discarded by `drop`.
    /// Plain-data types are always droppable; cleartokens are droppable
    /// when quantity is zero; empty dicts are droppable; everything else
    /// is not.
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
    /// - Same-variant for linear types (tokens, objects, variables,
    ///   expressions, constraints, transcripts): `Err(TypeNotComparable)`
    ///   until later phases define their equality. Linear types should
    ///   rarely need on-stack equality; the constraint-system path uses
    ///   `0x51 eq` differently (yields a `Constraint`).
    pub fn try_eq(&self, other: &Value) -> Result<bool, VMError> {
        match (self, other) {
            (Value::Int253(a), Value::Int253(b)) => Ok(a == b),
            (Value::String(a), Value::String(b)) => Ok(a.as_bytes() == b.as_bytes()),
            (Value::Point(a), Value::Point(b)) => Ok(a.as_bytes() == b.as_bytes()),
            (Value::Dict(a), Value::Dict(b)) => {
                if a.len() != b.len() {
                    return Ok(false);
                }
                for ((ka, va), (kb, vb)) in a.entries().zip(b.entries()) {
                    if ka != kb {
                        return Ok(false);
                    }
                    if !va.try_eq(vb)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            // Cross-variant always unequal.
            (sa, sb) if core::mem::discriminant(sa) != core::mem::discriminant(sb) => Ok(false),
            // Same-variant linear / constraint / token types: not yet defined.
            _ => Err(VMError::TypeNotComparable),
        }
    }

    /// Returns the type code used by the `type` opcode.
    ///
    /// Wire-encodable types use the base tag of their wire-encoding range
    /// from spec.md §Types (`Int253` → 0, `String` → 68, `Dict` → 128,
    /// `Point` → 248, …). Stack-only types (no wire encoding) are assigned
    /// codes in `0xc0..` that don't collide with any wire tag; these are
    /// implementation-defined for now and will be confirmed with Architect
    /// before any cross-implementation use.
    pub fn type_code(&self) -> u8 {
        match self {
            Value::Int253(_) => 0,
            Value::String(_) => 68,
            Value::Dict(_) => 128,
            Value::Point(_) => 248,
            Value::Token(_) => 249,
            Value::ClearToken(_) => 250,
            Value::WideToken(_) => 251,
            Value::Object(_) => 252,
            Value::Merlin(_) => 253,
            // Stack-only — code TBC by Architect.
            Value::Variable(_) => 0xc0,
            Value::Expression(_) => 0xc1,
            Value::Constraint(_) => 0xc2,
        }
    }
}
