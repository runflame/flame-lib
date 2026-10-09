# 🎻 Cell Type Language (CTL)

CTL is a byte-oriented wire-layout language for Flame Cells. It compiles
every declaration in a `.ctl` file into Rust structs, enums, alias wrappers, and
`CellEncode`/`CellDecode` implementations. It takes inspiration from
[TON's TL-B](https://docs.ton.org/foundations/tlb/overview), with Rust-like
syntax and whole bytes instead of bits.

## Type reference

Payload bytes and Cell references are separate ordered streams. Fields append
to those streams in declaration order. CTL adds no padding, schema identifier,
or implicit tag.

| Type or declaration | Encoding and meaning | Rust representation |
| --- | --- | --- |
| `U8` | Unsigned integer, one byte; also a raw byte. | `u8` |
| `U16` | Unsigned integer, two bytes, little-endian. | `u16` |
| `U32` | Unsigned integer, four bytes, little-endian. | `u32` |
| `U64` | Unsigned integer, eight bytes, little-endian. | `u64` |
| `V128` | Unsigned 64-bit integer in shortest unsigned LEB128 form, 1–10 bytes. The name denotes base 128. | `u64` |
| `[T; N]` | Exactly `N` consecutive elements; no length prefix. | `[T; N]` |
| `[T; count]` | Consecutive elements sized by an earlier unsigned field; no additional length prefix. | `Vec<T>` |
| `^T` | One ordered reference to a child containing `T`; its body is decoded when explicitly loaded. | `cells::ctl::Ref<T>` |
| `^Cell` | One ordered reference to a child whose interpretation is supplied by the caller. | `CellRef` |
| `type Name = T;` | A name for the declared layout, with no extra wire bytes. | Generated tuple struct `Name(pub ...)` and codecs |
| `struct Name { ... }` | Inline fields in declaration order, without a tag. | Generated struct and codecs |
| `enum Name: U8 { ... }` | An explicit integer tag followed by the selected variant's fields. Tag types may be `U8`, `U16`, `U32`, `U64`, or `V128`. | Generated enum and codecs |

`N` is a nonnegative constant. `count` names an earlier unsigned field in the
same struct or enum variant, including generated integer aliases. Encoding
checks that the array length agrees with `count`.

## Compile and use

From the repository root:

```sh
cargo run -p cells --bin ctlc -- cells/examples/types.ctl > types.rs
```

`ctlc` accepts one source path and writes formatted Rust to standard output,
using `rustfmt` from the pinned Rust toolchain. The library
entry point is `cells::ctl::compile(source: &str) -> Result<String,
cells::ctl::Error>`; it returns source directly, and the CLI applies formatting.
Schema errors carry a one-based line and column. Generated
code imports the standard prelude and Cells types, and uses short local names
such as `a`, `b`, `x`, and `y`. Imports and local names are adjusted when a
schema declares a conflicting name. Code depends on the `cells` crate and
can be included in a Rust module; no
build script or runtime schema parser is required.

For the [example schema](examples/types.ctl), place the generated `types.rs`
next to this Rust source:

```rust
use cells::{CellDecode, CellEncode, CellError};
use cells::ctl::Ref;

mod types;
use types::Node;

fn main() -> Result<(), CellError> {
    let leaf = Node::Data { size: 3, data: b"abc".to_vec() };
    let child = Ref::from_value(&leaf)?;
    let branch = Node::Branch { left: child.clone(), right: child };
    let cell = branch.to_cell()?;

    // This reads the tag and references, without opening either child.
    let decoded = Node::from_cell(&cell, &mut ())?;
    if let Node::Branch { left, .. } = decoded {
        // () resolves resident children; use a CellResolver for unloaded ones.
        let leaf = left.load(&mut ())?;
        assert!(matches!(leaf, Node::Data { size: 3, .. }));
    }
    Ok(())
}
```

Use `to_cell` and `from_cell` for a complete typed Cell. Inside another codec,
`CellBuilder::store` and the generated `CellDecode::decode` append to or read
from the current Cell. `from_cell` requires complete consumption of both
payload and references; an inline `decode` leaves subsequent fields unread.

## Examples, from bytes to graphs

A struct writes its fields in declaration order, with no padding or implicit
tag. Fixed arrays become Rust arrays:

```ctl
type Hash = [U8; 32];

struct Header {
    version: U16,
    height: U32,
    parent: Hash,
}
```

An alias adds no wire bytes. In Rust it becomes a public tuple struct, for
example `pub struct Hash(pub [u8; 32])`, with its own codec. This preserves the
declared encoding even for aliases of `V128`, whose Rust value is a `u64`
but whose wire encoding differs from fixed-width `U64`.

An array may use an earlier unsigned scalar field in the same struct or enum
variant as its element count. Such arrays become `Vec<T>`; the count remains
an explicit Rust field, and encoding rejects a count/length mismatch.
Arrays have no additional length prefix:

```ctl
struct Blob {
    count: V128,
    data: [U8; count],
}
```

An enum requires an unsigned tag type and an explicit, unique tag for every
variant. Each tag must fit the declared tag type. Its payload begins with that
tag, followed by the selected variant's fields. Recursive graphs use `^`:

```ctl
enum Node: U8 {
    Empty = 0,
    Data { size: V128, data: [U8; size] } = 1,
    Branch { left: ^Node, right: ^Node } = 2,
}
```

`^Node` consumes one ordered reference slot, with no bytes added to the
parent payload. The Rust field is `cells::ctl::Ref<Node>`: a lazy reference
whose body is decoded only when `load` is called. Inline named structs and
enums use the parent's payload and reference slots. Inline type cycles are
rejected; recursion through references is allowed.

The final example composes inline data, typed references, and an opaque Cell:

```ctl
struct Envelope {
    header: Header,
    body: ^Node,
    attachment: ^Cell,
}
```

`^Cell` generates a raw `CellRef`, leaving interpretation of that child's
body to the caller. Bare `Cell` is not an inline type. This `Envelope` is an
illustration, not a production transaction schema.

## Custom APIs and validation

Put generated code in a `wire` module. Add handwritten `impl` blocks to its
types for convenience methods, or convert wire records into private domain
types when construction must enforce invariants. For example:

```ctl
type Point = [U8; 32];

struct Key {
    point: Point,
}
```

The compiler generates `wire::Point(pub [u8; 32])`, its codecs, and a `wire::Key`
containing that wire type. Any 32-byte value is valid for this layout.
The application supplies a separate `Point` whose private contents are a
validated Ristretto point.

The following application implementation validates Ristretto encodings. Its
application crate depends on `cells` and `curve25519-dalek`; Cells itself
does not require the latter. Generate the wire module from
[point.ctl](examples/point.ctl):

```sh
cargo run -p cells --bin ctlc -- cells/examples/point.ctl > wire.rs
```

```rust
use cells::{CellBuilder, CellDecode, CellEncode, CellError, CellResolver, CellSlice};
use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};

mod wire;

#[derive(Clone, Debug)]
pub struct Point(RistrettoPoint);

impl TryFrom<wire::Point> for Point {
    type Error = CellError;

    fn try_from(x: wire::Point) -> Result<Self, CellError> {
        let point = CompressedRistretto(x.0).decompress()
            .ok_or(CellError::InvalidFormat)?;
        Ok(Self(point))
    }
}

impl Point {
    pub fn as_ristretto(&self) -> &RistrettoPoint {
        &self.0
    }
}

impl CellEncode for Point {
    fn encode(&self, b: &mut CellBuilder) -> Result<(), CellError> {
        wire::Point(self.0.compress().to_bytes()).encode(b)
    }
}

impl CellDecode for Point {
    fn decode<R: CellResolver + ?Sized>(
        a: &mut CellSlice<'_>,
        b: &mut R,
    ) -> Result<Self, CellError> {
        a.try_load(|a| wire::Point::decode(a, b)?.try_into())
    }
}
```

The domain codec delegates layout parsing to the generated codec and performs
validation through `TryFrom`. Encoding delegates back to the wire type.
The wrapper or conversion adds no serialized bytes.

Composite domain types use the same pattern: convert each wire field into
its domain type and validate relationships between them. Generated containers
continue to contain wire types, so that conversion is explicit.

A `Ref<wire::Point>` can be transferred into a `Ref<Point>` with
`Ref::from_reference(raw.into_reference())`. This transfers the same Cell
handle without opening it; `load` then uses the domain codec and validates
the body. It preserves lazy loading and the existing resolver checks.

Generated fields are public. Adding a `validate()` method to a generated
type does not make decoding call it, nor prevent later field mutation.
Private domain fields and checked constructors enforce that distinction.

## Validation

The compiler currently checks layout rules: integer widths, canonical `V128`,
enum tags, dependent array lengths, Cell capacities, and exact consumption
when reading a complete Cell. Specialized validation lives in handwritten
domain conversions and codecs, as in the Ristretto example above. References
keep body parsing and validation lazy until explicitly opened.

## Wire layout

Payload bytes and references are separate ordered streams. Traversing fields
in declaration order appends bytes to the payload and references to the
reference list; a reference does not interrupt the payload with a hash.
Inline nested values follow the same rule. The ordinary Cell record wraps
these streams with the [existing descriptor and child commitment summaries](../docs/cells.md#canonical-cell-record).

For the examples above:

| Value | Payload | Payload length | References |
| --- | --- | --- | --- |
| `Header`, version 1, height `0x01020304`, zero parent | `01 00 04 03 02 01`, then 32 zero bytes | 38 bytes | 0 |
| `Blob`, count 3, data `abc` | `03 61 62 63` | 4 bytes | 0 |
| `Node::Empty` | `00` | 1 byte | 0 |
| `Node::Data`, size 3, data `abc` | `01 03 61 62 63` | 5 bytes | 0 |
| `Node::Branch` | `02` | 1 byte | 2, left then right |
| `Envelope` | Its inline `Header` bytes | 38 bytes | 2, body then attachment |

Lengths in the table exclude the Cell descriptor and child summaries.
CTL adds no schema ID, struct tag, implicit enum tag, schema version, or
domain separator. A field named `version`, as in `Header`, is ordinary
explicit data. The caller must already know which type to decode.

## Grammar and validation

The compact grammar below omits whitespace and `//` line comments. Field and
variant lists are comma-separated and allow a trailing comma.

```text
schema      = declaration*
declaration = "type" name "=" type ";"
            | "struct" name "{" fields "}"
            | "enum" name ":" integer "{" variants "}"
fields      = (name ":" type ("," name ":" type)* ","?)?
variants    = variant ("," variant)* ","?
variant     = name ("{" fields "}")? "=" number
type        = integer | name | "^" name | "[" type ";" length "]"
integer     = "U8" | "U16" | "U32" | "U64" | "V128"
length      = number | name
```

Identifiers start with an ASCII letter, followed by ASCII letters, digits,
or underscores. Rust keywords are reserved, and declarations cannot reuse
built-in type names or the underlying Rust integer names (`u8`, `u16`, `u32`,
`u64`). Primitive spellings are case-sensitive. Names of
declarations, fields in a field list, and variants in an enum must be unique
in their respective scopes. Integer literals are decimal or hexadecimal
(`0x`/`0X` prefix), without signs or digit separators.

Array lengths are nonnegative integer literals or earlier unsigned fields
in the same field list; they are not arbitrary expressions. Named types may
be declared later in the file. An earlier unsigned field may itself use an
integer alias. `^T` requires a declared type (including an alias) or the
built-in `Cell`; direct references to primitive integers are unsupported.

Compilation rejects undefined names, invalid lengths, duplicate or
out-of-range tags, inline cycles, and arrays whose repeated element can
consume zero payload bytes and zero references. Fixed zero-length arrays
are allowed. Sources are limited to 1 MiB and 256 declarations, with
array syntax nesting bounded to 64 levels and combined inline type expansion
bounded to 128 levels, including across named types. The prototype has no generics,
conditions, arithmetic length expressions, imports, bit fields, or TL-B
linear inversion (`~`).

## Canonical decoding and trust

Generated codecs use the existing bounded `CellBuilder` and `CellSlice`.
Each ordinary Cell holds at most 4095 payload bytes and four references.
Counts use checked conversion/arithmetic and are checked against the minimum
remaining payload/reference requirements before array allocation or iteration.
Oversized values fail; the compiler does not split them across Cells.

`V128` carries seven value bits per byte, low groups first, with the high bit
indicating continuation. Zero is `00`, 127 is `7f`, and 128 is `80 01`.
Decoding rejects unknown enum tags, truncated data, overflowing or overlong
LEB128, and impossible array counts. Whole-Cell decoding also rejects
trailing payload or references and inaccessible pruned bodies. Encoding
rejects mismatched dependent counts, capacity overflow, and references
without the metadata required by a Cell. These failures use `CellError`;
schema compilation failures use `cells::ctl::Error`.

`Ref<T>::from_value(&value)` encodes a resident child.
`Ref<T>::from_reference(reference)` wraps an existing `CellRef`;
`reference()` borrows it and `into_reference()` returns it.
Cloning a `Ref<T>` clones the handle and does not require `T: Clone`.
Decoding `^T` only consumes a reference: it neither invokes a resolver nor
claims that the child body has already been validated as `T`. Unloaded and
pruned references can therefore be retained and re-encoded.

`load(&mut resolver)` resolves the child, checks its ID and complete committed
hash/depth metadata, and decodes exactly one `T`. Missing or pruned bodies,
mismatched commitments, and invalid typed contents fail at that point.
Resolving a parent does not recursively validate its descendants; each lazy
child must be opened separately. The caller controls the resolver and any
application traversal budget.

A type parameter is an expectation, not an authenticated type tag. Two CTL
types with identical layouts can accept identical bytes. CTL does not decide
which root or commitment level is trusted, prove retained pruning claims,
enforce application invariants, or select VM portability/linearity rules.
Those remain the consuming protocol's responsibility.

There is no implicit Snake encoding, automatic spilling, or "remaining
bytes" built-in in this version. Factor larger data into explicit child Cells
and describe each child's layout; the separate existing Snake APIs remain
available to handwritten codecs.
