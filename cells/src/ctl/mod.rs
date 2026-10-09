//! 🎻 Cell Type Language: compile byte-oriented `.ctl` schemas to Rust codecs.

mod parser;
mod runtime;
mod rust;

pub use runtime::Ref;

/// A schema error at a one-based line and column.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{line}:{column}: {message}")]
pub struct Error {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl Error {
    fn at(source: &str, offset: usize, message: impl Into<String>) -> Self {
        let before = &source[..offset.min(source.len())];
        Self {
            line: before.bytes().filter(|&b| b == b'\n').count() + 1,
            column: before.rsplit('\n').next().unwrap_or("").chars().count() + 1,
            message: message.into(),
        }
    }
}

/// Compiles a complete schema into Rust definitions and Cell codecs.
/// No schema identifiers, versions, or implicit constructor tags are encoded.
pub fn compile(source: &str) -> Result<String, Error> {
    let schema = parser::parse(source)?;
    rust::generate(source, &schema)
}

#[derive(Clone, Debug)]
struct Ident {
    name: String,
    offset: usize,
}

#[derive(Clone, Debug)]
struct Decl {
    name: Ident,
    kind: Kind,
}

#[derive(Clone, Debug)]
enum Kind {
    Struct(Vec<Field>),
    Enum {
        tag: Integer,
        variants: Vec<Variant>,
    },
    Alias(Ty),
}

#[derive(Clone, Debug)]
struct Field {
    name: Ident,
    ty: Ty,
}

#[derive(Clone, Debug)]
struct Variant {
    name: Ident,
    fields: Vec<Field>,
    tag: u64,
}

#[derive(Clone, Debug)]
struct Ty {
    kind: Type,
    offset: usize,
}

#[derive(Clone, Debug)]
enum Type {
    Integer(Integer),
    Named(Ident),
    Ref(Ident),
    Array(Box<Ty>, Length),
}

#[derive(Clone, Debug)]
enum Length {
    Fixed(usize),
    Field(Ident),
}

#[derive(Clone, Copy, Debug)]
enum Integer {
    U8,
    U16,
    U32,
    U64,
    V128,
}

impl Integer {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "U8" => Self::U8,
            "U16" => Self::U16,
            "U32" => Self::U32,
            "U64" => Self::U64,
            "V128" => Self::V128,
            _ => return None,
        })
    }
    fn method(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::V128 => "leb128",
        }
    }
    fn rust(self) -> &'static str {
        match self {
            Self::V128 => "u64",
            _ => self.method(),
        }
    }
    fn bytes(self) -> usize {
        match self {
            Self::U8 | Self::V128 => 1,
            Self::U16 => 2,
            Self::U32 => 4,
            Self::U64 => 8,
        }
    }
    fn max(self) -> u64 {
        match self {
            Self::U8 => u8::MAX.into(),
            Self::U16 => u16::MAX.into(),
            Self::U32 => u32::MAX.into(),
            Self::U64 | Self::V128 => u64::MAX,
        }
    }
}
