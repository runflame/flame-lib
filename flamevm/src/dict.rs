use crate::Integer;
use crate::Value;

pub struct Dict {
    inner: Vec<(Integer, Value)>
}