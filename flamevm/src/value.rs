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
            Value::Dict(_)
            | Value::Token(_)
            | Value::ClearToken(_)
            | Value::WideToken(_)
            | Value::Object(_)
            | Value::Merlin(_)
            | Value::Variable(_)
            | Value::Expression(_)
            | Value::Constraint(_) => Err(VMError::TypeNotCopyable),
        }
    }

    /// Returns true iff this value can be silently discarded by `drop`.
    /// Plain-data types are always droppable; cleartokens are droppable
    /// when quantity is zero; everything else (linear types, non-empty
    /// containers in later phases) is not.
    pub fn is_droppable(&self) -> bool {
        match self {
            Value::Int253(_) | Value::String(_) | Value::Point(_) => true,
            Value::ClearToken(t) => t.is_zero_qty(),
            _ => false,
        }
    }
}
