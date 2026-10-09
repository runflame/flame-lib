//! Compile real generated Rust, not just snapshots of its text.

use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        // This path was created exclusively by this test, never supplied by a user.
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn compiler_output_builds_and_roundtrips() {
    let path = std::env::temp_dir().join(format!(
        "cells-ctl-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&path).unwrap();
    let scratch = Scratch(path);
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // A separate target avoids the lock held by the outer Cargo test process.
    // This tiny fixture depends only on the local cells crate and its dependencies.
    fs::write(scratch.0.join("Cargo.toml"), format!(
        "[package]\nname = \"ctl-generated-check\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n[dependencies]\ncells = {{ path = {:?} }}\ncurve25519-dalek = \"4\"\n[[bin]]\nname = \"check\"\npath = \"main.rs\"\n",
        manifest.to_str().unwrap()
    )).unwrap();

    let mut source = include_str!("../examples/types.ctl").to_owned();
    source.push_str(include_str!("../examples/point.ctl"));
    source.push_str(
        r#"
        type Count = V128;
        type Counts = [Count; 2];
        type Byte = U8;
        struct Packet {
            count: Count,
            words: [U16; count],
            nested: [[U8; 2]; 2],
            bytes: [Byte; 2],
            next: ^Count,
        }
        struct Refs { count: U8, children: [^Cell; count] }
        enum Wide: U16 { First=0x1234, Second { value:U64 }=65535 }
        enum Variable: V128 { First=128, Last=18446744073709551615 }
    "#,
    );
    // Generated local and standard-library names must not shadow user fields/types.
    source.push_str("struct Vec { b:U8, s:U8, cells:U8, count:U8, data:[U8;count] } struct Empty {} type Nothing=[Empty;0];");
    source.push_str("type Ok=U8; type Err=U8; struct Sized {} ");
    source.push_str("type Result=U8; type StdResult=U8; type StdVec=U8; type Ref=U8; type CellEncode=U8; type a=U8; type b=U8; type x=U8; struct R {} ");
    source.push_str(
        "struct Collision { value:CellEncode, child:^Ref, a:U8, b:U8, x:U8, y:U8, z:U8 }",
    );
    let generated = cells::ctl::compile(&source).unwrap();
    assert!(!generated.contains("__ctl_"));
    assert!(!generated.contains("::core::"));
    assert!(!generated.contains("::std::vec::"));
    assert!(generated.contains("pub struct Point(pub [u8; 32])"));
    assert!(generated.contains("impl CellDecode for Point"));
    let cli_path = scratch.0.join("test.ctl");
    fs::write(&cli_path, &source).unwrap();
    let cli = Command::new(env!("CARGO_BIN_EXE_ctlc"))
        .arg(&cli_path)
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let generated_path = scratch.0.join("types.rs");
    fs::write(&generated_path, &cli.stdout).unwrap();
    let formatted = Command::new("rustfmt")
        .args(["--edition", "2024", "--check"])
        .arg(&generated_path)
        .output()
        .unwrap();
    assert!(
        formatted.status.success(),
        "ctlc output was not formatted:\n{}",
        String::from_utf8_lossy(&formatted.stdout)
    );
    fs::write(
        scratch.0.join("main.rs"),
        format!("mod types;\nuse types::{{Blob, Header, Hash, Count, Counts, Byte, Packet, Node, Refs, Wide, Variable, Vec, Nothing, Collision}};\n{ROUNDTRIP}"),
    )
    .unwrap();
    let result = Command::new(env!("CARGO"))
        .env("RUSTFLAGS", "-D unused-variables")
        .args(["run", "--offline", "--quiet", "--manifest-path"])
        .arg(scratch.0.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(scratch.0.join("target"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "generated Rust failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    fs::write(&cli_path, "struct Broken { data:[U8; unknown] }").unwrap();
    let cli = Command::new(env!("CARGO_BIN_EXE_ctlc"))
        .arg(cli_path)
        .output()
        .unwrap();
    assert!(!cli.status.success());
    assert!(cli.stdout.is_empty());
    assert!(String::from_utf8_lossy(&cli.stderr).contains(":1:"));
}

const ROUNDTRIP: &str = r#"
use cells::{Cell, CellBuilder, CellSlice, CellEncode, CellDecode, CellError, CellRef, CellIndex};
use cells::ctl::Ref;
use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};

#[derive(Clone, Debug)]
pub struct Point(RistrettoPoint);

impl TryFrom<types::Point> for Point {
    type Error = CellError;

    fn try_from(x: types::Point) -> Result<Self, CellError> {
        Ok(Self(CompressedRistretto(x.0).decompress().ok_or(CellError::InvalidFormat)?))
    }
}

impl Point {
    fn to_wire(&self) -> types::Point {
        types::Point(self.0.compress().to_bytes())
    }
}

impl CellEncode for Point {
    fn encode(&self, b: &mut CellBuilder) -> Result<(), CellError> {
        self.to_wire().encode(b)
    }
}

impl CellDecode for Point {
    fn decode<R: cells::CellResolver + ?Sized>(a: &mut CellSlice<'_>, b: &mut R) -> Result<Self, CellError> {
        a.try_load(|a| types::Point::decode(a, b)?.try_into())
    }
}

#[derive(Clone, Debug)]
struct Key { point: Point }

impl TryFrom<types::Key> for Key {
    type Error = CellError;

    fn try_from(x: types::Key) -> Result<Self, CellError> {
        Ok(Self { point: x.point.try_into()? })
    }
}

impl CellEncode for Key {
    fn encode(&self, b: &mut CellBuilder) -> Result<(), CellError> {
        types::Key { point: self.point.to_wire() }.encode(b)
    }
}

impl CellDecode for Key {
    fn decode<R: cells::CellResolver + ?Sized>(a: &mut CellSlice<'_>, b: &mut R) -> Result<Self, CellError> {
        a.try_load(|a| types::Key::decode(a, b)?.try_into())
    }
}

#[derive(Clone, Debug)]
struct Points { items: std::vec::Vec<Point>, commitment: Ref<Point> }

impl TryFrom<types::Points> for Points {
    type Error = CellError;

    fn try_from(x: types::Points) -> Result<Self, CellError> {
        Ok(Self {
            items: x.items.into_iter().map(Point::try_from).collect::<Result<_,_>>()?,
            commitment: Ref::from_reference(x.commitment.into_reference()),
        })
    }
}

impl CellEncode for Points {
    fn encode(&self, b: &mut CellBuilder) -> Result<(), CellError> {
        let count = u8::try_from(self.items.len()).map_err(|_| CellError::InvalidFormat)?;
        types::Points {
            count,
            items: self.items.iter().map(Point::to_wire).collect(),
            commitment: Ref::from_reference(self.commitment.reference().clone()),
        }.encode(b)
    }
}

impl CellDecode for Points {
    fn decode<R: cells::CellResolver + ?Sized>(a: &mut CellSlice<'_>, b: &mut R) -> Result<Self, CellError> {
        a.try_load(|a| types::Points::decode(a, b)?.try_into())
    }
}

fn main() -> ::core::result::Result<(), CellError> {
    let blob = Blob { count:3, data:vec![0xaa, 0xbb, 0xcc] };
    let cell = blob.to_cell()?;
    assert_eq!(cell.payload(), &[3, 0xaa, 0xbb, 0xcc]);
    assert_eq!(Blob::from_cell(&cell, &mut ())?.data, blob.data);
    let header = Header { version: 0x1234, height:0x12345678, parent:Hash([0x42;32]) };
    let encoded = header.to_cell()?;
    assert_eq!(&encoded.payload()[..6], &[0x34,0x12,0x78,0x56,0x34,0x12]);
    assert_eq!(Header::from_cell(&encoded, &mut ())?.parent.0, [0x42;32]);

    let count = Count(128);
    assert_eq!(count.to_cell()?.payload(), &[0x80,1]);
    let packet = Packet {
        count:Count(2), words:vec![0x1234, 0xabcd], nested:[[1,2],[3,4]],
        bytes:[Byte(5),Byte(6)], next:Ref::from_value(&count)?,
    };
    let root = packet.to_cell()?;
    assert_eq!(root.payload(), &[2,0x34,0x12,0xcd,0xab,1,2,3,4,5,6]);
    let restored = Packet::from_cell(&root, &mut ())?;
    assert_eq!(restored.words, packet.words);
    assert_eq!(restored.next.load(&mut ())?.0, 128);
    assert_eq!(Counts([Count(127),Count(128)]).to_cell()?.payload(), &[127,0x80,1]);

    let data = Node::Data { size:3, data:vec![1,2,3] };
    let node = Node::Branch { left:Ref::from_value(&data)?, right:Ref::from_value(&Node::Empty)? };
    let root = node.to_cell()?;
    let mut bag = CellIndex::collect(std::sync::Arc::new(root.clone()))?;
    let detached = root.detached();
    let Node::Branch { left, right } = Node::from_cell(&detached, &mut ())? else { panic!() };
    assert!(matches!(left.load(&mut ()), ::core::result::Result::Err(CellError::MissingCell(_))));
    assert!(matches!(left.load(&mut bag)?, Node::Data { size:3, .. }));
    assert!(matches!(right.load(&mut bag)?, Node::Empty));
    let pruned = data.to_cell()?.prune(1)?;
    let hidden = Node::Branch { left:Ref::from_reference(pruned.into()), right };
    let Node::Branch { left, .. } = Node::from_cell(&hidden.to_cell()?, &mut ())? else { panic!() };
    assert!(matches!(left.load(&mut ()), ::core::result::Result::Err(CellError::PrunedCell)));

    let mut b = CellBuilder::new();
    b.store_u8(9)?;
    assert_eq!(b.store(&Blob { count:2, data:vec![1] }).unwrap_err(), CellError::InvalidFormat);
    assert_eq!(b.used_bytes(), 1);
    let malformed = Cell::new(vec![2,1], vec![])?;
    let mut s = CellSlice::new(&malformed);
    assert!(Blob::decode(&mut s, &mut ()).is_err());
    assert_eq!(s.remaining_bytes(), 2);
    let mut huge = CellBuilder::new();
    huge.store_leb128(u64::MAX)?;
    assert!(Blob::from_cell(&huge.build(), &mut ()).is_err());
    let excessive_refs = Cell::new(vec![255], vec![])?;
    assert!(Refs::from_cell(&excessive_refs, &mut ()).is_err());
    let trailing = Cell::new(vec![0,0], vec![])?;
    assert_eq!(Node::from_cell(&trailing, &mut ()).unwrap_err(), CellError::TrailingBytes);
    let unknown = Cell::new(vec![255], vec![])?;
    assert_eq!(Node::from_cell(&unknown, &mut ()).unwrap_err(), CellError::InvalidFormat);

    let refs = Refs { count:1, children:vec![CellRef::from(cell)] };
    assert_eq!(refs.to_cell()?.refs().len(), 1);
    assert_eq!(Wide::First.to_cell()?.payload(), &[0x34,0x12]);
    assert!(matches!(Wide::from_cell(&Wide::Second { value:42 }.to_cell()?, &mut ())?, Wide::Second { value:42 }));
    assert_eq!(Variable::First.to_cell()?.payload(), &[0x80,1]);
    assert!(matches!(Variable::from_cell(&Variable::Last.to_cell()?, &mut ())?, Variable::Last));
    let noncanonical = Cell::new(vec![0x80,0x81,0], vec![])?;
    assert!(Variable::from_cell(&noncanonical, &mut ()).is_err());
    let named = Vec { b:1,s:2,cells:3,count:2,data:vec![4,5] };
    assert_eq!(Vec::from_cell(&named.to_cell()?, &mut ())?.data, [4,5]);
    assert!(Nothing([]).to_cell()?.payload().is_empty());
    let collision = Collision {
        value:types::CellEncode(7), child:Ref::from_value(&types::Ref(8))?,
        a:1, b:2, x:3, y:4, z:5,
    };
    let decoded = Collision::from_cell(&collision.to_cell()?, &mut ())?;
    assert_eq!(decoded.value.0, 7);
    assert_eq!(decoded.child.load(&mut ())?.0, 8);
    assert_eq!((decoded.a, decoded.b, decoded.x, decoded.y, decoded.z), (1,2,3,4,5));

    let point = Point(curve25519_dalek::constants::RISTRETTO_BASEPOINT_POINT);
    let key = Key { point: point.clone() };
    let cell = key.to_cell()?;
    assert_eq!(cell.payload(), point.0.compress().as_bytes());
    assert_eq!(Key::from_cell(&cell, &mut ())?.point.0, point.0);
    let points = Points { items:vec![point.clone(), point.clone()], commitment:Ref::from_value(&point)? };
    let cell = points.to_cell()?;
    let decoded = Points::from_cell(&cell, &mut ())?;
    assert_eq!(decoded.items.len(), 2);
    assert_eq!(decoded.items[0].0, point.0);
    assert_eq!(decoded.commitment.load(&mut ())?.0, point.0);
    let invalid_items = types::Points {
        count:1,
        items:vec![types::Point([0xff;32])],
        commitment:Ref::from_value(&point.to_wire())?,
    }.to_cell()?;
    assert_eq!(Points::from_cell(&invalid_items, &mut ()).unwrap_err(), CellError::InvalidFormat);
    let invalid = Cell::new(vec![0xff;32], vec![])?;
    // The wire parser accepts any 32 bytes; domain construction validates them.
    let raw = types::Key::from_cell(&invalid, &mut ())?;
    assert_eq!(raw.point.0, [0xff;32]);
    assert_eq!(Key::try_from(raw).unwrap_err(), CellError::InvalidFormat);
    assert_eq!(Key::from_cell(&invalid, &mut ()).unwrap_err(), CellError::InvalidFormat);
    let mut slice = CellSlice::new(&invalid);
    assert_eq!(Key::decode(&mut slice, &mut ()).unwrap_err(), CellError::InvalidFormat);
    assert_eq!(slice.remaining_bytes(), 32);
    let lazy = Points { items:vec![], commitment:Ref::from_reference(invalid.into()) };
    let decoded = Points::from_cell(&lazy.to_cell()?, &mut ())?;
    assert_eq!(decoded.commitment.load(&mut ()).unwrap_err(), CellError::InvalidFormat);
    let impossible = Cell::new(vec![255], vec![])?;
    assert_eq!(Points::from_cell(&impossible, &mut ()).unwrap_err(), CellError::InsufficientBytes);
    ::core::result::Result::Ok(())
}
"#;
