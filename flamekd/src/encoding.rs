use bech32::{primitives::decode::CheckedHrpstring, Bech32m, ByteIterExt, Fe32IterExt, Hrp};
use zeroize::Zeroizing;

use crate::Error;

fn payload_len(hrp: &str) -> Option<usize> {
    match hrp {
        "spend" | "view" | "recv" | "testspend" | "testview" | "testrecv" => Some(96),
        "f" | "tf" => Some(64),
        "c" | "tc" => Some(32),
        _ => None,
    }
}

pub(crate) fn encode(hrp: &str, bytes: &[u8]) -> String {
    assert_eq!(
        payload_len(hrp),
        Some(bytes.len()),
        "invalid Flame key format"
    );
    let hrp = Hrp::parse(hrp).expect("known Flame HRP");
    let len = bech32::encoded_length::<Bech32m>(hrp, bytes).expect("bounded Flame key length");
    // Write directly into the returned allocation, without temporary secret strings.
    let mut text = String::with_capacity(len);
    text.extend(
        bytes
            .iter()
            .copied()
            .bytes_to_fes()
            .with_checksum::<Bech32m>(&hrp)
            .chars(),
    );
    text
}

pub(crate) fn decode<const N: usize>(hrp: &str, text: &str) -> Result<[u8; N], Error> {
    let expected_len = payload_len(hrp).ok_or(Error::InvalidEncoding)?;
    if text.len() > 170 {
        return Err(Error::InvalidLength);
    }
    let checked = CheckedHrpstring::new::<Bech32m>(text).map_err(|_| Error::InvalidEncoding)?;
    if !checked.hrp().lowercase_byte_iter().eq(hrp.bytes()) {
        return Err(Error::InvalidEncoding);
    }
    // This checks generic 5-to-8-bit padding; Flame has no witness-version character to remove.
    checked
        .validate_segwit_padding()
        .map_err(|_| Error::InvalidEncoding)?;
    if checked.byte_iter().len() != expected_len || N != expected_len {
        return Err(Error::InvalidLength);
    }
    let mut bytes = Zeroizing::new([0u8; N]);
    for (out, byte) in bytes.iter_mut().zip(checked.byte_iter()) {
        *out = byte;
    }
    Ok(*bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bech32::{Bech32, Fe32};

    fn round_trip<const N: usize>(hrp: &str, length: usize) {
        let bytes = [0x42; N];
        let text = encode(hrp, &bytes);
        assert_eq!(text.len(), length);
        assert_eq!(text, text.to_ascii_lowercase());
        assert_eq!(decode::<N>(hrp, &text), Ok(bytes));
        assert_eq!(decode::<N>(hrp, &text.to_ascii_uppercase()), Ok(bytes));
        let mut mixed = text.clone();
        mixed[..1].make_ascii_uppercase();
        assert_eq!(decode::<N>(hrp, &mixed), Err(Error::InvalidEncoding));
    }

    #[test]
    fn exact_lengths_and_case_rules() {
        round_trip::<96>("spend", 166);
        round_trip::<96>("view", 165);
        round_trip::<96>("recv", 165);
        round_trip::<64>("f", 111);
        round_trip::<32>("c", 60);
        round_trip::<96>("testspend", 170);
        round_trip::<96>("testview", 169);
        round_trip::<96>("testrecv", 169);
        round_trip::<64>("tf", 112);
        round_trip::<32>("tc", 61);
    }

    #[test]
    fn rejects_wrong_formats_and_lengths() {
        let spend = encode("spend", &[0x42; 96]);
        assert_eq!(decode::<96>("view", &spend), Err(Error::InvalidEncoding));
        assert_eq!(decode::<96>("other", &spend), Err(Error::InvalidEncoding));
        assert_eq!(decode::<32>("spend", &spend), Err(Error::InvalidLength));
        let unknown = bech32::encode::<Bech32m>(Hrp::parse("other").unwrap(), &[0; 96]).unwrap();
        assert_eq!(decode::<96>("spend", &unknown), Err(Error::InvalidEncoding));
        for length in [0, 31, 33, 100] {
            let wrong =
                bech32::encode::<Bech32m>(Hrp::parse("c").unwrap(), &vec![0; length]).unwrap();
            assert_eq!(decode::<32>("c", &wrong), Err(Error::InvalidLength));
        }
        for malformed in ["", "c", "c1", "c1!", " c1qqqqqq", "c1qqqqqq\n"] {
            assert_eq!(decode::<32>("c", malformed), Err(Error::InvalidEncoding));
        }
        let wrong_checksum = bech32::encode::<Bech32>(Hrp::parse("c").unwrap(), &[0; 32]).unwrap();
        assert_eq!(
            decode::<32>("c", &wrong_checksum),
            Err(Error::InvalidEncoding)
        );
    }

    #[test]
    fn rejects_noncanonical_padding_with_valid_checksums() {
        let encode_symbols = |hrp: &str, symbols: &[Fe32]| -> String {
            symbols
                .iter()
                .copied()
                .with_checksum::<Bech32m>(&Hrp::parse(hrp).unwrap())
                .chars()
                .collect()
        };
        let mut symbols = [Fe32::Q; 52];
        symbols[51] = Fe32::P; // Nonzero padding after 32 bytes.
        let nonzero = encode_symbols("c", &symbols);
        assert!(CheckedHrpstring::new::<Bech32m>(&nonzero).is_ok());
        assert_eq!(decode::<32>("c", &nonzero), Err(Error::InvalidEncoding));
        let excess = encode_symbols("recv", &[Fe32::Q; 155]); // Seven padding bits after 96 bytes.
        assert!(CheckedHrpstring::new::<Bech32m>(&excess).is_ok());
        assert_eq!(decode::<96>("recv", &excess), Err(Error::InvalidEncoding));
    }

    #[test]
    fn rejects_single_symbol_changes_throughout_longest_encoding() {
        let text = encode("testspend", &[0x42; 96]);
        for position in 10..text.len() {
            let mut changed = text.clone().into_bytes();
            changed[position] = if changed[position] == b'q' {
                b'p'
            } else {
                b'q'
            };
            assert_eq!(
                decode::<96>("testspend", std::str::from_utf8(&changed).unwrap()),
                Err(Error::InvalidEncoding)
            );
        }
    }
}
