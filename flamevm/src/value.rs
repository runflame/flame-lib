use crate::errors::VMError;
use crate::Cell;
use crate::Dict;
use crate::Int253;
use crate::Merlin;
use crate::Point;
use crate::String;
use crate::{ClearToken, Token, WideToken};
use crate::{Constraint, Expression, Variable};

#[rustfmt::skip]
impl Value {
    /// Downcast to Int253.
    pub fn to_int253(self)      -> Result<Int253, VMError>     { match self { Value::Int253(x) => Ok(x),     _ => Err(VMError::TypeNotInt253) } }
    /// Downcast to String.
    pub fn to_string(self)      -> Result<String, VMError>     { match self { Value::String(x) => Ok(x),     _ => Err(VMError::TypeNotString) } }
    /// Downcast to Dict.
    pub fn to_dict(self)        -> Result<Dict, VMError>       { match self { Value::Dict(x) => Ok(x),       _ => Err(VMError::TypeNotDict) } }
    /// Downcast to Point.
    pub fn to_point(self)       -> Result<Point, VMError>      { match self { Value::Point(x) => Ok(x),      _ => Err(VMError::TypeNotPoint) } }
    /// Downcast to Cell.
    pub fn to_cell(self)        -> Result<Cell, VMError>       { match self { Value::Cell(x) => Ok(x),       _ => Err(VMError::TypeNotCell) } }
    /// Downcast to Merlin transcript.
    pub fn to_merlin(self)      -> Result<Merlin, VMError>     { match self { Value::Merlin(x) => Ok(x),     _ => Err(VMError::TypeNotMerlin) } }
    /// Downcast to Variable.
    pub fn to_variable(self)    -> Result<Variable, VMError>   { match self { Value::Variable(x) => Ok(x),   _ => Err(VMError::TypeNotVariable) } }
    /// Downcast to ClearToken.
    pub fn to_clear_token(self) -> Result<ClearToken, VMError> { match self { Value::ClearToken(x) => Ok(x), _ => Err(VMError::TypeNotClearToken) } }
    /// Downcast to MultiscalarMul.
    pub fn to_msm(self)         -> Result<crate::MultiscalarMul, VMError> { match self { Value::MultiscalarMul(x) => Ok(x), _ => Err(VMError::TypeNotMsm) } }
}

impl Value {
    /// Lift to Expression — Int253 folds to a constant; Expression passes through.
    pub fn to_expression(self) -> Result<Expression, VMError> {
        match self {
            Value::Expression(e) => Ok(e),
            Value::Int253(i) => Ok(Expression::constant(i)),
            _ => Err(VMError::TypeNotExpression),
        }
    }

    /// Lift to Constraint — Int253 folds to `Cleartext(v != 0)`; Constraint passes through.
    pub fn to_constraint(self) -> Result<Constraint, VMError> {
        match self {
            Value::Constraint(c) => Ok(c),
            Value::Int253(i) => Ok(Constraint::Cleartext(!i.is_zero())),
            _ => Err(VMError::TypeNotConstraint),
        }
    }

    /// _x_ **neg** — sign flip for Int253, LC negate for Expression,
    /// scalar-coefficient negation for Point/MSM (lifts to MSM).
    pub fn neg(self) -> Result<Value, VMError> {
        match self {
            Value::Int253(v) => Ok(Value::Int253(-v)),
            Value::Expression(e) => Ok(Value::Expression(-e)),
            Value::Point(p) => Ok(Value::MultiscalarMul(
                crate::msm::MultiscalarMul::term(
                    -curve25519_dalek::scalar::Scalar::ONE,
                    p.to_compressed(),
                ),
            )),
            Value::MultiscalarMul(m) => Ok(Value::MultiscalarMul(m.negated())),
            _ => Err(VMError::TypeNotInt253),
        }
    }

    /// _x y_ **add** — cleartext if both Int253; Point/MSM operands
    /// produce a `MultiscalarMul`; otherwise lifts to Expression
    /// (caller must be in external context).
    pub fn add(self, other: Value, can_constrain: bool) -> Result<Value, VMError> {
        use crate::msm::MultiscalarMul;
        match (self, other) {
            (Value::Int253(x), Value::Int253(y)) => Ok(Value::Int253(x + y)),
            // Point + Point → MSM with two unit-scalar terms.
            (Value::Point(a), Value::Point(b)) => Ok(Value::MultiscalarMul(
                MultiscalarMul::from_point(&a).push_point(&b),
            )),
            // Point + MSM / MSM + Point → append.
            (Value::Point(p), Value::MultiscalarMul(m))
            | (Value::MultiscalarMul(m), Value::Point(p)) => {
                Ok(Value::MultiscalarMul(m.push_point(&p)))
            }
            // MSM + MSM → concat term lists.
            (Value::MultiscalarMul(a), Value::MultiscalarMul(b)) => {
                Ok(Value::MultiscalarMul(a.append(b)))
            }
            (a, b) if can_constrain => {
                Ok(Value::Expression(a.to_expression()? + b.to_expression()?))
            }
            _ => Err(VMError::TypeNotInt253),
        }
    }


    /// _x_ **not** — cleartext if Int253; structural if Constraint.
    pub fn not(self) -> Result<Value, VMError> {
        match self {
            Value::Int253(v) => {
                let r = if v.is_zero() { 1u64 } else { 0u64 };
                Ok(Value::Int253(Int253::from(r)))
            }
            Value::Constraint(c) => Ok(Value::Constraint(Constraint::not(c))),
            _ => Err(VMError::TypeNotInt253),
        }
    }

    /// _a b_ **and** — cleartext if both Int253; structural if a
    /// Constraint is involved (caller in external context).
    pub fn and(self, other: Value, can_constrain: bool) -> Result<Value, VMError> {
        let either_constraint = matches!(self, Value::Constraint(_))
            || matches!(other, Value::Constraint(_));
        if can_constrain && either_constraint {
            let a = self.to_constraint()?;
            let b = other.to_constraint()?;
            Ok(Value::Constraint(Constraint::and(a, b)))
        } else {
            let a = self.to_int253()?;
            let b = other.to_int253()?;
            let r = if !a.is_zero() && !b.is_zero() { 1u64 } else { 0u64 };
            Ok(Value::Int253(Int253::from(r)))
        }
    }

    /// _a b_ **or** — mirror of [`Value::and`].
    pub fn or(self, other: Value, can_constrain: bool) -> Result<Value, VMError> {
        let either_constraint = matches!(self, Value::Constraint(_))
            || matches!(other, Value::Constraint(_));
        if can_constrain && either_constraint {
            let a = self.to_constraint()?;
            let b = other.to_constraint()?;
            Ok(Value::Constraint(Constraint::or(a, b)))
        } else {
            let a = self.to_int253()?;
            let b = other.to_int253()?;
            let r = if !a.is_zero() || !b.is_zero() { 1u64 } else { 0u64 };
            Ok(Value::Int253(Int253::from(r)))
        }
    }
}

/// Possible values on the stack machine.
///
/// `Clone` is the Rust-level deep copy (used for snapshots, storage,
/// witness-bearing pushes). It is *not* VM copyability — that gate is
/// [`Value::try_clone`] / [`Value::is_copyable`], which reject linear
/// types so a script can never duplicate a bearer asset on the stack.
#[derive(Clone, Debug)]
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
    /// Deferred multi-scalar multiplication; consumed by `verify`
    /// which adds it to the same batch as Schnorr/Musig sigs.
    MultiscalarMul(crate::msm::MultiscalarMul),
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
            Value::Point(p) => Ok(Value::Point(p.clone())),
            Value::Dict(d) => Ok(Value::Dict(d.try_clone()?)),
            Value::Token(_)
            | Value::ClearToken(_)
            | Value::WideToken(_)
            | Value::Cell(_)
            | Value::Merlin(_)
            | Value::Variable(_)
            | Value::Expression(_)
            | Value::Constraint(_)
            | Value::MultiscalarMul(_) => Err(VMError::TypeNotCopyable),
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
            // `Token` is portable per design — every `Token`
            // instance is range-proven non-negative at construction
            // (via the encrypted opcode paths or `Token::cleartext`),
            // so the portability invariant holds by construction.
            Value::Token(_) => true,
            _ => false,
        }
    }

    /// Returns true iff this value can be silently discarded by `drop`
    /// without losing embedded asset value. The rule:
    ///
    /// - **Every copyable type is droppable.** Discarding it is a strict
    ///   subset of duplicate-then-discard, and the type holds no
    ///   exclusive content. Plain-data: `Int253`, `String`, `Point`.
    ///   Dicts: droppable iff their sticky `droppable` flag is set.
    /// - **Zero-quantity `ClearToken`** is droppable. It's a flavor
    ///   nameplate with no balance attached.
    /// - **Pure-computation values** are droppable: `Expression`,
    ///   `Variable`, `Constraint`, `Merlin`, `MultiscalarMul`. They
    ///   carry CS / transcript bookkeeping, not asset value, so
    ///   abandoning them costs the script nothing.
    /// - **Asset-bearing values are not droppable.** `Token` /
    ///   `ClearToken(qty ≠ 0)` / `WideToken` / `Cell` would silently
    ///   destroy value or break linearity.
    pub fn is_droppable(&self) -> bool {
        match self {
            // Copyable plain-data.
            Value::Int253(_) | Value::String(_) | Value::Point(_) => true,
            // Dict: droppable iff its sticky flag is set (every value
            // ever inserted was droppable).
            Value::Dict(d) => d.is_droppable(),
            // Zero-qty CT is the empty-flavor case.
            Value::ClearToken(t) => t.is_zero_qty(),
            // Pure computation — no embedded asset value to lose.
            Value::Expression(_)
            | Value::Variable(_)
            | Value::Constraint(_)
            | Value::Merlin(_)
            | Value::MultiscalarMul(_) => true,
            // Asset-bearing values: dropping would lose value.
            Value::Token(_) | Value::WideToken(_) | Value::Cell(_) => false,
        }
    }

    /// Equality across two stack values.
    ///
    /// - Cross-variant: always `Ok(false)` (different types are not equal).
    /// - Same-variant for plain-data types (`Int253`, `String`, `Point`):
    ///   bitwise / by-value comparison.
    /// - Same-variant for `Dict`: recursive entry-wise comparison.
    /// - Same-variant for linear types (tokens, cells, variables,
    ///   expressions, constraints, transcripts): `Err(TypeNotComparable)`
    ///   — linear types have no stable identity for `eq`.
    pub fn try_eq(&self, other: &Value) -> Result<bool, VMError> {
        match (self, other) {
            (Value::Int253(a), Value::Int253(b)) => Ok(a == b),
            // Use `bytes_view` (Cow) so witness-bearing variants
            // (Commitment / Scalar / Predicate / Script) are
            // compared by their canonical wire form, the same way
            // the verifier would see them.
            (Value::String(a), Value::String(b)) => {
                Ok(a.bytes_view().as_ref() == b.bytes_view().as_ref())
            }
            (Value::Point(a), Value::Point(b)) => Ok(a.to_bytes() == b.to_bytes()),
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
            // `Cell` carries the wire tag previously assigned to
            // `Object` (renamed to `Cell` per ADR 0001).
            Value::Cell(_) => 252,
            Value::Merlin(_) => 253,
            // Stack-only — code TBC by Architect.
            Value::Variable(_) => 0xc0,
            Value::Expression(_) => 0xc1,
            Value::Constraint(_) => 0xc2,
            Value::MultiscalarMul(_) => 0xc3,
        }
    }
}
