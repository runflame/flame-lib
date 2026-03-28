use crate::Dict;
use crate::Integer;
use crate::Merlin;
use crate::Object;
use crate::Point;
use crate::String;
use crate::{ClearToken, Token, WideToken};

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
    //Variable(Variable),
    //Expression(Expression),
    //Constraint(Constraint),
    //MultiscalarMul(MultiscalarMul),
}

const INT_TYPECODE: u8 = 0;
const STRING_TYPECODE: u8 = 66;
const STRUCT_TYPECODE: u8 = 126;
const POINT_TYPECODE: u8 = 246;
const TOKEN_TYPECODE: u8 = 247;
const CLEAR_TOKEN_TYPECODE: u8 = 248;
const WIDE_TOKEN_TYPECODE: u8 = 249;
const OBJECT_TYPECODE: u8 = 250;
const MERLIN_TYPECODE: u8 = 251;
const VARIABLE_TYPECODE: u8 = 252;
const EXPRESSION_TYPECODE: u8 = 253;
const CONSTRAINT_TYPECODE: u8 = 254;
const MultiscalarMulTypecode: u8 = 255;
