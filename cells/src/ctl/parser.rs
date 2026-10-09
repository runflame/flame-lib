use super::*;

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_DECLARATIONS: usize = 256;
const MAX_ARRAY_DEPTH: usize = 64;

pub(super) fn parse(source: &str) -> Result<Vec<Decl>, Error> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(Error::at(
            source,
            0,
            "schema exceeds the 1 MiB source limit",
        ));
    }
    let mut parser = Parser { source, offset: 0 };
    let mut declarations = Vec::new();
    loop {
        parser.skip();
        if parser.offset == source.len() {
            return Ok(declarations);
        }
        if declarations.len() == MAX_DECLARATIONS {
            return Err(parser.error("schema exceeds the 256 declaration limit"));
        }
        declarations.push(parser.declaration()?);
    }
}

struct Parser<'a> {
    source: &'a str,
    offset: usize,
}

impl Parser<'_> {
    fn error(&self, message: impl Into<String>) -> Error {
        Error::at(self.source, self.offset, message)
    }

    fn skip(&mut self) {
        let bytes = self.source.as_bytes();
        loop {
            while bytes.get(self.offset).is_some_and(u8::is_ascii_whitespace) {
                self.offset += 1;
            }
            if bytes.get(self.offset..self.offset + 2) != Some(b"//") {
                return;
            }
            while bytes.get(self.offset).is_some_and(|&byte| byte != b'\n') {
                self.offset += 1;
            }
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        self.skip();
        if self.source.as_bytes().get(self.offset) == Some(&byte) {
            self.offset += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), Error> {
        if self.eat(byte) {
            Ok(())
        } else {
            Err(self.error(format!("expected '{}'", char::from(byte))))
        }
    }

    fn ident(&mut self) -> Result<Ident, Error> {
        self.skip();
        let start = self.offset;
        let bytes = self.source.as_bytes();
        if !bytes.get(start).is_some_and(u8::is_ascii_alphabetic) {
            return Err(self.error("expected an ASCII identifier starting with a letter"));
        }
        self.offset += 1;
        while bytes
            .get(self.offset)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            self.offset += 1;
        }
        Ok(Ident {
            name: self.source[start..self.offset].to_owned(),
            offset: start,
        })
    }

    fn declared_ident(&mut self, type_name: bool) -> Result<Ident, Error> {
        let ident = self.ident()?;
        if rust_keyword(&ident.name)
            || (type_name
                && (matches!(ident.name.as_str(), "Cell" | "u8" | "u16" | "u32" | "u64")
                    || Integer::parse(&ident.name).is_some()))
        {
            return Err(Error::at(
                self.source,
                ident.offset,
                format!("'{}' is a reserved name", ident.name),
            ));
        }
        Ok(ident)
    }

    fn declaration(&mut self) -> Result<Decl, Error> {
        let keyword = self.ident()?;
        if !matches!(keyword.name.as_str(), "type" | "struct" | "enum") {
            return Err(Error::at(
                self.source,
                keyword.offset,
                "expected 'type', 'struct', or 'enum'",
            ));
        }
        let name = self.declared_ident(true)?;
        let kind = match keyword.name.as_str() {
            "type" => {
                self.expect(b'=')?;
                let ty = self.ty(0)?;
                self.expect(b';')?;
                Kind::Alias(ty)
            }
            "struct" => Kind::Struct(self.fields()?),
            "enum" => {
                self.expect(b':')?;
                let tag = self.ident()?;
                let tag = Integer::parse(&tag.name).ok_or_else(|| {
                    Error::at(
                        self.source,
                        tag.offset,
                        "enum tag type must be U8, U16, U32, U64, or V128",
                    )
                })?;
                self.expect(b'{')?;
                let mut variants = Vec::new();
                while !self.eat(b'}') {
                    let name = self.declared_ident(false)?;
                    self.skip();
                    let fields = if self.source.as_bytes().get(self.offset) == Some(&b'{') {
                        self.fields()?
                    } else {
                        Vec::new()
                    };
                    self.expect(b'=')?;
                    let tag = self.number()?;
                    variants.push(Variant { name, fields, tag });
                    if !self.eat(b',') {
                        self.expect(b'}')?;
                        break;
                    }
                }
                Kind::Enum { tag, variants }
            }
            _ => unreachable!(),
        };
        Ok(Decl { name, kind })
    }

    fn fields(&mut self) -> Result<Vec<Field>, Error> {
        self.expect(b'{')?;
        let mut fields = Vec::new();
        while !self.eat(b'}') {
            let name = self.declared_ident(false)?;
            self.expect(b':')?;
            let ty = self.ty(0)?;
            fields.push(Field { name, ty });
            if !self.eat(b',') {
                self.expect(b'}')?;
                break;
            }
        }
        Ok(fields)
    }

    fn ty(&mut self, depth: usize) -> Result<Ty, Error> {
        self.skip();
        let offset = self.offset;
        let kind = if self.eat(b'[') {
            if depth == MAX_ARRAY_DEPTH {
                return Err(Error::at(
                    self.source,
                    offset,
                    "array nesting exceeds the limit of 64",
                ));
            }
            let item = self.ty(depth + 1)?;
            self.expect(b';')?;
            self.skip();
            let length = if self
                .source
                .as_bytes()
                .get(self.offset)
                .is_some_and(u8::is_ascii_digit)
            {
                let start = self.offset;
                Length::Fixed(
                    usize::try_from(self.number()?)
                        .map_err(|_| Error::at(self.source, start, "array length exceeds usize"))?,
                )
            } else {
                Length::Field(self.ident()?)
            };
            self.expect(b']')?;
            Type::Array(Box::new(item), length)
        } else if self.eat(b'^') {
            Type::Ref(self.ident()?)
        } else {
            let name = self.ident()?;
            match Integer::parse(&name.name) {
                Some(integer) => Type::Integer(integer),
                None => Type::Named(name),
            }
        };
        Ok(Ty { kind, offset })
    }

    fn number(&mut self) -> Result<u64, Error> {
        self.skip();
        let start = self.offset;
        let bytes = self.source.as_bytes();
        let radix = if matches!(bytes.get(start..start + 2), Some(b"0x" | b"0X")) {
            self.offset += 2;
            16
        } else {
            10
        };
        let digits_start = self.offset;
        let mut number = 0_u64;
        while let Some(digit) = bytes
            .get(self.offset)
            .and_then(|&byte| char::from(byte).to_digit(radix))
        {
            number = number
                .checked_mul(u64::from(radix))
                .and_then(|n| n.checked_add(u64::from(digit)))
                .ok_or_else(|| Error::at(self.source, start, "integer literal exceeds U64"))?;
            self.offset += 1;
        }
        if self.offset == digits_start {
            return Err(Error::at(
                self.source,
                start,
                "expected an unsigned integer literal",
            ));
        }
        if bytes
            .get(self.offset)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            return Err(Error::at(self.source, start, "invalid integer literal"));
        }
        Ok(number)
    }
}

fn rust_keyword(name: &str) -> bool {
    matches!(
        name,
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "abstract"
            | "become"
            | "box"
            | "do"
            | "final"
            | "gen"
            | "macro"
            | "override"
            | "priv"
            | "try"
            | "typeof"
            | "unsized"
            | "virtual"
            | "yield"
            | "macro_rules"
            | "raw"
            | "safe"
            | "union"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_declarations_comments_and_offsets() {
        let source = "// Unicode comments: 🎻\n\
            type Bytes = [U8; 0x20];\n\
            enum Message: V128 {\n\
                Empty = 0,\n\
                Data { n: V128, bytes: [U8; n], child: ^Other, } = 0x01,\n\
            }\n\
            struct Other { bytes: Bytes, cell: ^Cell }";
        let schema = parse(source).unwrap();
        assert_eq!(schema.len(), 3);
        assert_eq!(schema[0].name.offset, source.find("Bytes").unwrap());
        let Kind::Alias(ty) = &schema[0].kind else {
            panic!("expected alias")
        };
        assert!(matches!(ty.kind, Type::Array(_, Length::Fixed(32))));
        let Kind::Enum { tag, variants } = &schema[1].kind else {
            panic!("expected enum")
        };
        assert!(matches!(tag, Integer::V128));
        assert_eq!(variants[0].tag, 0);
        assert_eq!(variants[1].tag, 1);
        let fields = &variants[1].fields;
        let Type::Array(_, Length::Field(length)) = &fields[1].ty.kind else {
            panic!("expected array")
        };
        assert_eq!(length.name, "n");
        assert_eq!(&source[fields[1].ty.offset..][..7], "[U8; n]");
        assert!(matches!(&fields[2].ty.kind, Type::Ref(name) if name.name == "Other"));
    }

    #[test]
    fn rejects_invalid_syntax_with_positions() {
        for source in [
            "type Name = U8",
            "struct Name { a: U8 b: U8 }",
            "enum Name: U8 { A = 0 B = 1 }",
            "enum Name: U8 { A }",
            "enum Name { A = 0 }",
            "enum Name: Name { A = 0 }",
            "enum Name: U8 { A = -1 }",
            "enum Name: U8 { A = 0x }",
            "enum Name: U8 { A = 1u8 }",
            "type Name = ^[U8; 1];",
            "type Name = ^^Name;",
            "type Name = [U8; -1];",
            "type Name = [U8; 1 + 2];",
            "struct _Name {}",
            "struct Café {}",
            "struct Name { café: U8 }",
            "struct Name { type: U8 }",
            "struct u8 {}",
            "struct U8 {}",
            "struct U16 {}",
            "struct U32 {}",
            "struct U64 {}",
            "struct V128 {}",
            "struct Cell {}",
            "struct gen {}",
            "/* comments are line-only */",
            "struct Name {} @",
            "#[other] type Name = U8;",
            "ext type Name = U8;",
        ] {
            assert!(parse(source).is_err(), "unexpectedly accepted {source:?}");
        }
        let error = parse("// 🎻\nstruct Name {\n  value U8\n}").unwrap_err();
        assert_eq!((error.line, error.column), (3, 9));
        assert_eq!(error.message, "expected ':'");
        let error = parse("// 🎻\nstruct 名 {}").unwrap_err();
        assert_eq!((error.line, error.column), (2, 8));
    }

    #[test]
    fn checks_integer_overflow() {
        for literal in ["18446744073709551616", "0x10000000000000000"] {
            let error = parse(&format!("enum Name: U64 {{ A = {literal} }}")).unwrap_err();
            assert_eq!(error.message, "integer literal exceeds U64");
            assert!(parse(&format!("type Name = [U8; {literal}];")).is_err());
        }
        let schema = parse("enum Name: U64 { A = 0xffffffffffffffff }").unwrap();
        let Kind::Enum { variants, .. } = &schema[0].kind else {
            panic!("expected enum")
        };
        assert_eq!(variants[0].tag, u64::MAX);
    }

    #[test]
    fn bounds_parser_resources() {
        let nested = |depth| {
            format!(
                "type Name = {}U8{};",
                "[".repeat(depth),
                "; 1]".repeat(depth)
            )
        };
        assert!(parse(&nested(MAX_ARRAY_DEPTH)).is_ok());
        assert!(parse(&nested(MAX_ARRAY_DEPTH + 1)).is_err());
        assert!(parse(&" ".repeat(MAX_SOURCE_BYTES)).is_ok());
        assert!(parse(&" ".repeat(MAX_SOURCE_BYTES + 1)).is_err());
        let mut source: String = (0..MAX_DECLARATIONS)
            .map(|n| format!("struct Name{n} {{}}\n"))
            .collect();
        assert_eq!(parse(&source).unwrap().len(), MAX_DECLARATIONS);
        source.push_str("struct OneTooMany {}");
        assert!(parse(&source).is_err());
    }
}
