use crate::Dict;
use crate::Integer;
use crate::Merlin;
use crate::Object;
use crate::Point;
use crate::String;
use crate::{ClearToken, Token, WideToken};
use crate::{Constraint, Expression, Variable};

/// Possible values on the stack machine
pub enum Value {
    Int(Integer),
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
