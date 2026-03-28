
use crate::Integer;
use crate::Dict;
use crate::String;
use crate::{Token,WideToken,ClearToken};
use crate::Object;
use crate::Merlin;
use crate::Point;

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

const IntTypecode: u8 = 0;
const StringTypecode: u8 = 66;
const StructTypecode: u8 = 126;
const PointTypecode: u8 = 246;
const TokenTypecode: u8 = 247;
const ClearTokenTypecode: u8 = 248;
const WideTokenTypecode: u8 = 249;
const ObjectTypecode: u8 = 250;
const MerlinTypecode: u8 = 251;
const VariableTypecode: u8 = 252;
const ExpressionTypecode: u8 = 253;
const ConstraintTypecode: u8 = 254;
const MultiscalarMulTypecode: u8 = 255;
