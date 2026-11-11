//! The one output descriptor the minter's wallet imports: `raw(<script hex>)`.

use corepc_client::bitcoin::ScriptBuf;

const INPUT_CHARSET: &str = "0123456789()[],'/*abcdefgh@:$%{}IJKLMNOPQRSTUVWXYZ&+-.;<=>?!^_|~ijklmnopqrstuvwxyzABCDEFGH`#\"\\ ";
const CHECKSUM_CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

/// The script of a `raw(...)` descriptor, with or without its checksum.
pub fn parse_raw(descriptor: &str) -> Option<ScriptBuf> {
    let (body, checksum) = match descriptor.split_once('#') {
        Some((body, checksum)) => (body, Some(checksum)),
        None => (descriptor, None),
    };
    if checksum.is_some_and(|checksum| Some(checksum) != self::checksum(body).as_deref()) {
        return None;
    }
    let hex = body.strip_prefix("raw(")?.strip_suffix(')')?;
    ScriptBuf::from_hex(hex)
        .ok()
        .filter(|script| !script.is_empty())
}

/// The BIP 380 descriptor checksum, or `None` for a character outside the descriptor charset.
pub fn checksum(descriptor: &str) -> Option<String> {
    let mut symbols = Vec::new();
    let mut groups = Vec::new();
    for character in descriptor.chars() {
        let value = INPUT_CHARSET.find(character)? as u64;
        symbols.push(value & 31);
        groups.push(value >> 5);
        if groups.len() == 3 {
            symbols.push(groups[0] * 9 + groups[1] * 3 + groups[2]);
            groups.clear();
        }
    }
    match groups.as_slice() {
        [a] => symbols.push(*a),
        [a, b] => symbols.push(a * 3 + b),
        _ => {}
    }
    symbols.extend([0; 8]);
    let checksum = polymod(&symbols) ^ 1;
    Some(
        (0..8)
            .map(|i| CHECKSUM_CHARSET[((checksum >> (5 * (7 - i))) & 31) as usize] as char)
            .collect(),
    )
}

fn polymod(symbols: &[u64]) -> u64 {
    const GENERATOR: [u64; 5] = [
        0xf5dee51989,
        0xa9fdca3312,
        0x1bab10e32d,
        0x3706b1677a,
        0x644d626ffd,
    ];
    let mut checksum = 1u64;
    for value in symbols {
        let top = checksum >> 35;
        checksum = ((checksum & 0x7ffffffff) << 5) ^ value;
        for (i, generator) in GENERATOR.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                checksum ^= generator;
            }
        }
    }
    checksum
}

#[cfg(test)]
mod tests {
    use super::*;

    // From `bitcoin-cli getdescriptorinfo` on Bitcoin Core 31.
    const CORE: [(&str, &str); 3] = [
        ("raw(deadbeef)", "89f8spxm"),
        ("raw(51)", "8lvh9jxk"),
        (
            "raw(0020abababababababababababababababababababababababababababababababab)",
            "wuxdgrh8",
        ),
    ];

    #[test]
    fn checksums_match_core() {
        for (descriptor, expected) in CORE {
            assert_eq!(
                checksum(descriptor).as_deref(),
                Some(expected),
                "{descriptor}"
            );
        }
    }

    #[test]
    fn parses_raw_descriptors_and_rejects_bad_checksums() {
        assert_eq!(
            parse_raw("raw(51)#8lvh9jxk"),
            Some(ScriptBuf::from_bytes(vec![0x51]))
        );
        assert_eq!(
            parse_raw("raw(51)"),
            Some(ScriptBuf::from_bytes(vec![0x51]))
        );
        assert_eq!(parse_raw("raw(51)#8lvh9jxq"), None);
        assert_eq!(parse_raw("addr(bc1qxyz)"), None);
        assert_eq!(parse_raw("raw()"), None);
    }
}
