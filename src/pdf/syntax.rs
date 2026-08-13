//! Shared PDF lexical helpers.
//!
//! Object syntax, content streams, and CMaps all tokenise the same byte
//! classes (ISO 32000 whitespace and delimiters) and the same hex-string
//! nibble rules. Keep those tables in one place so the parsers cannot drift.

/// ISO 32000 whitespace: NUL, tab, LF, FF, CR, space.
#[inline]
pub(crate) fn is_ws(b: u8) -> bool {
    matches!(b, 0x00 | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

/// ISO 32000 delimiters: `( ) < > [ ] { } / %`.
#[inline]
pub(crate) fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

#[inline]
pub(crate) fn is_ws_or_delim(b: u8) -> bool {
    is_ws(b) || is_delim(b)
}

#[inline]
pub(crate) fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode a hex string, skipping non-digits. A trailing `>` ends the run
/// (ASCIIHexDecode). An odd final nibble is padded with zero, per the spec.
pub(crate) fn decode_hex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut nibble: Option<u8> = None;
    for &b in data {
        if b == b'>' {
            break;
        }
        let Some(v) = hex_digit(b) else {
            continue;
        };
        match nibble {
            Some(prev) => {
                out.push((prev << 4) | v);
                nibble = None;
            }
            None => nibble = Some(v),
        }
    }
    if let Some(prev) = nibble {
        out.push(prev << 4);
    }
    out
}

/// Advance `pos` past whitespace and `%…` comments. Stops at EOF or the next
/// non-comment token.
pub(crate) fn skip_ws_and_comments(bytes: &[u8], pos: &mut usize) {
    loop {
        while bytes.get(*pos).copied().is_some_and(is_ws) {
            *pos += 1;
        }
        if bytes.get(*pos) != Some(&b'%') {
            return;
        }
        while !matches!(bytes.get(*pos), None | Some(&b'\n') | Some(&b'\r')) {
            *pos += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_and_delimiter_classes() {
        for b in [0x00, b'\t', b'\n', 0x0C, b'\r', b' '] {
            assert!(is_ws(b), "{b}");
            assert!(is_ws_or_delim(b), "{b}");
        }
        for b in [b'(', b')', b'<', b'>', b'[', b']', b'{', b'}', b'/', b'%'] {
            assert!(is_delim(b), "{b}");
            assert!(is_ws_or_delim(b), "{b}");
        }
        assert!(!is_ws(b'A') && !is_delim(b'A') && !is_ws_or_delim(b'A'));
    }

    #[test]
    fn hex_digit_covers_each_range() {
        assert_eq!(hex_digit(b'0'), Some(0));
        assert_eq!(hex_digit(b'9'), Some(9));
        assert_eq!(hex_digit(b'a'), Some(10));
        assert_eq!(hex_digit(b'f'), Some(15));
        assert_eq!(hex_digit(b'A'), Some(10));
        assert_eq!(hex_digit(b'F'), Some(15));
        assert_eq!(hex_digit(b'g'), None);
        assert_eq!(hex_digit(b' '), None);
    }

    #[test]
    fn decode_hex_skips_junk_pads_and_stops_at_gt() {
        assert_eq!(decode_hex(b"4869"), b"Hi");
        assert_eq!(decode_hex(b"48 6 9> trailing"), b"Hi");
        assert_eq!(decode_hex(b"4"), vec![0x40]);
        assert_eq!(decode_hex(b"deADBeEf"), vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(decode_hex(b"!!"), Vec::<u8>::new());
        assert!(decode_hex(b"").is_empty());
    }

    #[test]
    fn skip_ws_and_comments_eats_comments_and_whitespace() {
        let bytes = b"  % comment\n\t/Name";
        let mut pos = 0;
        skip_ws_and_comments(bytes, &mut pos);
        assert_eq!(bytes[pos], b'/');

        let bytes = b"%eof comment";
        let mut pos = 0;
        skip_ws_and_comments(bytes, &mut pos);
        assert_eq!(pos, bytes.len());

        let bytes = b"%cr\rX";
        let mut pos = 0;
        skip_ws_and_comments(bytes, &mut pos);
        assert_eq!(bytes[pos], b'X');
    }
}
