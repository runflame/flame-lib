
use crate::Integer;
use crate::Dict;
use crate::String;

pub enum Value {
    Int(Integer),
    String(String),
    Dict(Dict),
}