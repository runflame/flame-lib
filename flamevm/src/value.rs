use crate::errors::VMError;
use crate::Contract;
use crate::Dict;
use crate::Merlin;
use crate::MultiscalarMul;
use crate::Point;
use crate::Scalar;
use crate::String;
use crate::{ClearToken, Token, WideToken};
use crate::{Constraint, Expression, SecretConstraint, Variable};
use cells::CellEncode;

/// Possible values on the stack machine.
///
/// `Clone` is the Rust-level deep copy (used for snapshots, storage,
/// witness-bearing pushes). It is *not* VM copyability — that gate is
/// [`Value::try_clone`] / [`Value::is_copyable`], which reject linear
/// types so a script can never duplicate a bearer asset on the stack.
#[derive(Clone, Debug)]
pub enum Value {
    Scalar(Scalar),
    String(String),
    Dict(Dict),
    Point(Point),
    Token(Token),
    WideToken(WideToken),
    ClearToken(ClearToken),
    Contract(Box<Contract>),
    Merlin(Merlin),
    Variable(Variable),
    Expression(Expression),
    Constraint(Constraint),
    MultiscalarMul(MultiscalarMul),
    /// Copyable immutable serialized data; it grants no bearer ownership.
    Cell(crate::CellValue),
    Slice(crate::Slice),
    Builder(cells::CellBuilder),
}

#[rustfmt::skip]
impl Value {
    /// Returns a fresh copy of this value if its type is **copyable** per
    /// spec.md §Stack discipline (plain-data types). Linear types
    /// (tokens, contracts, variables, expressions, constraints, transcripts)
    /// are never copyable and produce [`VMError::TypeNotCopyable`].
    pub fn try_clone(&self) -> Result<Value, VMError> {
        match self {
            Value::Scalar(i) => Ok(Value::Scalar(*i)),
            Value::String(s) => Ok(Value::String(s.clone())),
            Value::Point(p) => Ok(Value::Point(p.clone())),
            Value::Dict(d) => Ok(Value::Dict(d.try_clone()?)),
            Value::Token(_)
            | Value::ClearToken(_)
            | Value::WideToken(_)
            | Value::Contract(_)
            | Value::Merlin(_)
            | Value::Variable(_)
            | Value::Expression(_)
            | Value::Constraint(_)
            | Value::MultiscalarMul(_) => Err(VMError::TypeNotCopyable),
            Value::Cell(cell) => Ok(Value::Cell(cell.clone())),
            Value::Slice(slice) => Ok(Value::Slice(slice.clone())),
            Value::Builder(_) => Err(VMError::TypeNotCopyable),
        }
    }

    /// True iff this value's type permits duplication (`dup`/`getdup`).
    /// Plain-data and copyable containers; never linear types.
    pub fn is_copyable(&self) -> bool {
        match self {
            Value::Scalar(_) | Value::String(_) | Value::Point(_) | Value::Cell(_) | Value::Slice(_) => true,
            Value::Dict(d) => d.is_copyable(),
            _ => false,
        }
    }

    /// True iff this value can survive across VM execution — sealed into
    /// a contract or actor state. Plain-data types are portable; `ClearToken`
    /// is portable iff its centered quantity is non-negative; encrypted in-range
    /// `Token` is portable; everything else (linear, intermediate,
    /// constraint-system-only types) is not.
    ///
    /// Contracts are themselves linear handles, not portable — a contract's
    /// payload is portable, but a contract *value on the stack* may not be
    /// placed into another contract's payload (sealing a contract-in-a-contract).
    pub fn is_portable(&self) -> bool {
        match self {
            Value::Scalar(_) | Value::String(_) | Value::Point(_) | Value::Cell(_) => true,
            Value::Dict(d) => d.is_portable(),
            Value::ClearToken(t) => t.is_portable(),
            // `Token` is portable by construction: public cleartext
            // construction enforces its range, while commitment-based
            // construction is restricted to trusted crate paths.
            Value::Token(_) => true,
            _ => false,
        }
    }

    /// Returns true iff this value can be silently discarded by `drop`
    /// without losing embedded asset value. The rule:
    ///
    /// - **Every copyable type is droppable.** Discarding it is a strict
    ///   subset of duplicate-then-discard, and the type holds no
    ///   exclusive content. Plain-data: `Scalar`, `String`, `Point`.
    ///   Dicts: droppable iff their sticky `droppable` flag is set.
    /// - **Zero-quantity `ClearToken`** is droppable. It's a flavor
    ///   nameplate with no balance attached.
    /// - **Pure-computation values** are droppable: `Expression`,
    ///   `Variable`, `Constraint`, `Merlin`, `MultiscalarMul`. They
    ///   carry CS / transcript bookkeeping, not asset value, so
    ///   abandoning them costs the script nothing.
    /// - **Asset-bearing values are not droppable.** `Token` /
    ///   `ClearToken(qty ≠ 0)` / `WideToken` / `Contract` would silently
    ///   destroy value or break linearity.
    pub fn is_droppable(&self) -> bool {
        match self {
            // Plain data; Cell handles are consuming but hold no bearer asset.
            Value::Scalar(_) | Value::String(_) | Value::Point(_) | Value::Cell(_) | Value::Slice(_) | Value::Builder(_) => true,
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
            Value::Token(_) | Value::WideToken(_) | Value::Contract(_) => false,
        }
    }

    /// Equality across two stack values.
    ///
    /// - Cell/Slice/Builder pairs may match across variants; other mixed types
    ///   are unequal except the transitional String/byte-Cell pair.
    /// - Same-variant for plain-data types (`Scalar`, `String`, `Point`):
    ///   bitwise / by-value comparison.
    /// - Cell/Slice/Builder pairs compare factual hashes of their current contents.
    /// - Dicts are not comparable.
    /// - Same-variant for linear types (tokens, contracts, variables,
    ///   expressions, constraints, transcripts): `Err(TypeNotComparable)`
    ///   — linear types have no stable identity for `eq`.
    pub fn try_eq(&self, other: &Value) -> Result<bool, VMError> {
        if self.is_cell_container() && other.is_cell_container() {
            return Ok(self.factual_hash()? == other.factual_hash()?);
        }
        match (self, other) {
            (Value::Scalar(a), Value::Scalar(b)) => Ok(a == b),
            // Compare by canonical wire bytes so witness-bearing
            // variants (Point / Scalar / Script / Contract) match the
            // verifier's view of the same string.
            (Value::String(a), Value::String(b)) => Ok(a.to_bytes_vec() == b.to_bytes_vec()),
            (Value::Point(a), Value::Point(b)) => Ok(a.to_bytes() == b.to_bytes()),
            // Transitional byte consumers still produce String results.
            (Value::Cell(cell), Value::String(bytes)) | (Value::String(bytes), Value::Cell(cell)) => Ok(cell.id() == bytes.to_cell()?.id()),
            // Cross-variant always unequal.
            (sa, sb) if core::mem::discriminant(sa) != core::mem::discriminant(sb) => Ok(false),
            // Same-variant non-primitives (Dict, tokens, contracts, linear types):
            // dicts require recursion + non-trivial gas; linear types have
            // no defined equality. All hard-fail.
            _ => Err(VMError::TypeNotComparable),
        }
    }

    pub(crate) fn is_cell_container(&self) -> bool {
        matches!(self, Self::Cell(_) | Self::Slice(_) | Self::Builder(_))
    }

    fn factual_hash(&self) -> Result<cells::CellID, VMError> {
        match self {
            Self::Cell(cell) => Ok(cell.id()),
            Self::Slice(slice) => Ok(slice.commitment()?.id()),
            Self::Builder(builder) => Ok(builder.commitment()?.id()),
            _ => Err(VMError::TypeNotByteSource),
        }
    }

    pub(crate) fn byte_payload(&self) -> Result<&[u8], VMError> {
        match self {
            Self::Slice(slice) => Ok(slice.bytes()),
            Self::Builder(builder) => Ok(builder.payload()),
            _ => Err(VMError::TypeNotByteSource),
        }
    }

    pub(crate) fn byte_refs(&self) -> Result<usize, VMError> {
        match self {
            Self::Slice(slice) => Ok(slice.remaining_refs()),
            Self::Builder(builder) => Ok(builder.used_refs()),
            _ => Err(VMError::TypeNotByteSource),
        }
    }

    /// Stable type code for the `type` opcode — a sequential
    /// enumeration of the variants (enum-declaration order), independent
    /// of the wire-encoding tags in `encoding.rs` (which keep their own
    /// compact scheme).
    pub fn type_code(&self) -> u8 {
        match self {
            Value::Scalar(_) => 0,
            Value::String(_) => 1,
            Value::Dict(_) => 2,
            Value::Point(_) => 3,
            Value::Token(_) => 4,
            Value::WideToken(_) => 5,
            Value::ClearToken(_) => 6,
            Value::Contract(_) => 7,
            Value::Merlin(_) => 8,
            Value::Variable(_) => 9,
            Value::Expression(_) => 10,
            Value::Constraint(_) => 11,
            Value::MultiscalarMul(_) => 12,
            Value::Cell(_) => 13,
            Value::Slice(_) => 14,
            Value::Builder(_) => 15,
        }
    }

    /// Logical heap work needed for a Rust-level clone. The unit is gas, not
    /// allocator bytes: every variable byte, container member, expression
    /// term, and constraint node contributes at least one unit. This keeps
    /// rollback snapshots bounded without making consensus depend on a Rust
    /// allocator's layout.
    pub(crate) fn clone_gas(&self) -> u64 {
        fn expression_gas(expr: &Expression) -> u64 {
            match expr {
                Expression::Constant(_) => 0,
                Expression::LinearCombination(terms, _) => terms.len() as u64,
            }
        }

        fn secret_constraint_gas(constraint: &SecretConstraint) -> u64 {
            match constraint {
                SecretConstraint::Eq(a, b) => 2u64
                    .saturating_add(expression_gas(a))
                    .saturating_add(expression_gas(b)),
                SecretConstraint::And(a, b) | SecretConstraint::Or(a, b) => 2u64
                    .saturating_add(secret_constraint_gas(a))
                    .saturating_add(secret_constraint_gas(b)),
                SecretConstraint::Not(value) => {
                    1u64.saturating_add(secret_constraint_gas(value))
                }
            }
        }

        match self {
            Value::Scalar(_) | Value::ClearToken(_) | Value::Variable(_) => 0,
            Value::String(s) => s.len() as u64,
            Value::Dict(d) => d.clone_gas(),
            Value::Point(_) | Value::Token(_) | Value::Merlin(_) => 1,
            Value::Cell(_) | Value::Slice(_) => 1,
            Value::Builder(builder) => (builder.used_bytes() + builder.used_refs()) as u64,
            // Charge the same logical item on prover (assigned) and verifier
            // (unassigned); gas must never depend on secret witness presence.
            Value::WideToken(_) => 1,
            Value::Contract(contract) => 1u64.saturating_add(contract.clone_gas()),
            Value::Expression(expr) => expression_gas(expr),
            Value::Constraint(Constraint::Cleartext(_)) => 0,
            Value::Constraint(Constraint::Secret(constraint)) => {
                secret_constraint_gas(constraint)
            }
            Value::MultiscalarMul(msm) => msm.len() as u64,
        }
    }

    /// Downcast to Scalar.
    pub fn to_scalar(self)      -> Result<Scalar, VMError>     { match self { Value::Scalar(x) => Ok(x),     _ => Err(VMError::TypeNotScalar) } }
    /// Downcast to String.
    pub fn to_string(self)      -> Result<String, VMError>     { match self { Value::String(x) => { x.check_len()?; Ok(x) }, _ => Err(VMError::TypeNotString) } }
    /// Downcast to Dict.
    pub fn to_dict(self)        -> Result<Dict, VMError>       { match self { Value::Dict(x) => Ok(x),       _ => Err(VMError::TypeNotDict) } }
    /// Downcast to Point.
    pub fn to_point(self)       -> Result<Point, VMError>      { match self { Value::Point(x) => Ok(x),      _ => Err(VMError::TypeNotPoint) } }
    /// Downcast to Contract.
    pub fn to_contract(self)        -> Result<Contract, VMError>       { match self { Value::Contract(x) => Ok(*x),      _ => Err(VMError::TypeNotContract) } }
    /// Downcast to the raw serialized Cell handle, without resolving its body.
    pub fn into_cell_ref(self) -> Result<cells::CellRef, VMError> { match self { Value::Cell(x) => Ok(x.reference), _ => Err(VMError::TypeNotCell) } }
    pub fn into_slice(self) -> Result<crate::Slice, VMError> { match self { Value::Slice(x) => Ok(x), _ => Err(VMError::TypeNotSlice) } }
    pub fn into_builder(self) -> Result<cells::CellBuilder, VMError> { match self { Value::Builder(x) => Ok(x), _ => Err(VMError::TypeNotBuilder) } }
    /// Downcast to Merlin transcript.
    pub fn to_merlin(self)      -> Result<Merlin, VMError>     { match self { Value::Merlin(x) => Ok(x),     _ => Err(VMError::TypeNotMerlin) } }
    /// Downcast to Variable.
    pub fn to_variable(self)    -> Result<Variable, VMError>   { match self { Value::Variable(x) => Ok(x),   _ => Err(VMError::TypeNotVariable) } }
    /// Downcast to ClearToken.
    pub fn to_clear_token(self) -> Result<ClearToken, VMError> { match self { Value::ClearToken(x) => Ok(x), _ => Err(VMError::TypeNotClearToken) } }
}

impl Value {
    /// Lift to Expression — Scalar folds to a constant; Expression passes through.
    pub fn to_expression(self) -> Result<Expression, VMError> {
        match self {
            Value::Expression(e) => Ok(e),
            Value::Scalar(i) => Ok(Expression::constant(i)),
            _ => Err(VMError::TypeNotExpression),
        }
    }

    /// Lift to Constraint — Scalar folds to `Cleartext(v != 0)`; Constraint passes through.
    pub fn to_constraint(self) -> Result<Constraint, VMError> {
        match self {
            Value::Constraint(c) => Ok(c),
            Value::Scalar(i) => Ok(Constraint::Cleartext(!i.is_zero())),
            _ => Err(VMError::TypeNotConstraint),
        }
    }

    /// _x_ **neg** — modular additive inverse for Scalar, LC negate for Expression,
    /// scalar-coefficient negation for Point/MSM (lifts to MSM).
    #[allow(clippy::should_implement_trait)]
    pub fn neg(self) -> Result<Value, VMError> {
        match self {
            Value::Scalar(v) => Ok(Value::Scalar(-v)),
            Value::Expression(e) => Ok(Value::Expression(-e)),
            Value::Point(p) => Ok(Value::MultiscalarMul(MultiscalarMul::term(
                -curve25519_dalek::scalar::Scalar::ONE,
                p.to_compressed(),
            ))),
            Value::MultiscalarMul(m) => Ok(Value::MultiscalarMul(m.negated())),
            _ => Err(VMError::TypeNotScalar),
        }
    }

    /// _x y_ **add** — cleartext if both Scalar; Point/MSM operands
    /// produce a `MultiscalarMul`; otherwise lifts to Expression
    /// (caller must be in external context).
    pub fn add(self, other: Value, can_constrain: bool) -> Result<Value, VMError> {
        match (self, other) {
            (Value::Scalar(x), Value::Scalar(y)) => Ok(Value::Scalar(x + y)),
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
            _ => Err(VMError::TypeNotScalar),
        }
    }

    /// _x_ **not** — cleartext if Scalar; structural if Constraint.
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Result<Value, VMError> {
        match self {
            Value::Scalar(v) => {
                let r = if v.is_zero() { 1u64 } else { 0u64 };
                Ok(Value::Scalar(Scalar::from(r)))
            }
            Value::Constraint(c) => Ok(Value::Constraint(Constraint::not(c))),
            _ => Err(VMError::TypeNotScalar),
        }
    }

    /// _a b_ **and** — cleartext if both Scalar; structural if a
    /// Constraint is involved (caller in external context).
    pub fn and(self, other: Value, can_constrain: bool) -> Result<Value, VMError> {
        let either_constraint =
            matches!(self, Value::Constraint(_)) || matches!(other, Value::Constraint(_));
        if can_constrain && either_constraint {
            let a = self.to_constraint()?;
            let b = other.to_constraint()?;
            Ok(Value::Constraint(Constraint::and(a, b)))
        } else {
            let a = self.to_scalar()?;
            let b = other.to_scalar()?;
            let r = if !a.is_zero() && !b.is_zero() {
                1u64
            } else {
                0u64
            };
            Ok(Value::Scalar(Scalar::from(r)))
        }
    }

    /// _a b_ **or** — mirror of [`Value::and`].
    pub fn or(self, other: Value, can_constrain: bool) -> Result<Value, VMError> {
        let either_constraint =
            matches!(self, Value::Constraint(_)) || matches!(other, Value::Constraint(_));
        if can_constrain && either_constraint {
            let a = self.to_constraint()?;
            let b = other.to_constraint()?;
            Ok(Value::Constraint(Constraint::or(a, b)))
        } else {
            let a = self.to_scalar()?;
            let b = other.to_scalar()?;
            let r = if !a.is_zero() || !b.is_zero() {
                1u64
            } else {
                0u64
            };
            Ok(Value::Scalar(Scalar::from(r)))
        }
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use core::mem::size_of;

    use super::Value;
    use crate::{Merlin, SecretConstraint, String, Token, WideToken};

    #[test]
    fn large_value_variants_stay_indirect() {
        assert_eq!(size_of::<Merlin>(), size_of::<Box<()>>());
        assert_eq!(size_of::<String>(), size_of::<Vec<u8>>());
        assert!(size_of::<SecretConstraint>() <= 3 * size_of::<usize>());
        assert_eq!(size_of::<WideToken>(), 40);
        assert_eq!(size_of::<Value>(), size_of::<Token>());
    }
}

#[cfg(test)]
mod capability_tests {
    use curve25519_dalek::constants::RISTRETTO_BASEPOINT_COMPRESSED;
    use curve25519_dalek::scalar::Scalar as DalekScalar;

    use super::Value;
    use crate::{
        ClearToken, Commitment, Constraint, Dict, Expression, Merlin, MultiscalarMul, Scalar,
        Variable,
    };

    #[test]
    fn droppability_matrix_matches_asset_ownership() {
        for value in [
            Value::Merlin(Merlin::new(b"drop-test")),
            Value::Variable(Variable {
                commitment: Commitment::unblinded(Scalar::ONE),
            }),
            Value::Expression(Expression::Constant(Scalar::ONE)),
            Value::Constraint(Constraint::Cleartext(true)),
            Value::MultiscalarMul(MultiscalarMul::term(
                DalekScalar::ONE,
                RISTRETTO_BASEPOINT_COMPRESSED,
            )),
            Value::ClearToken(ClearToken::new(Scalar::ZERO, Scalar::from(7u64))),
        ] {
            assert!(value.is_droppable());
            assert!(!value.is_copyable());
        }

        let mut dict = Dict::new();
        dict.insert(Scalar::ZERO, Value::Scalar(Scalar::ONE));
        assert!(dict.is_droppable());
        dict.insert(
            Scalar::ONE,
            Value::ClearToken(ClearToken::new(Scalar::ONE, Scalar::from(7u64))),
        );
        assert!(!dict.is_droppable());
        dict.remove(&Scalar::ONE);
        assert!(
            !dict.is_droppable(),
            "a non-empty tainted dict stays non-droppable"
        );
        dict.remove(&Scalar::ZERO);
        assert!(dict.is_droppable(), "a fully drained dict is droppable");
    }
}
