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

/// Decode a hex string, rejecting any byte that is neither whitespace nor a
/// hex digit. An odd final nibble is padded with zero, as in [`decode_hex`].
pub(crate) fn decode_hex_strict(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut nibble: Option<u8> = None;
    for &b in data {
        if is_ws(b) {
            continue;
        }
        let v = hex_digit(b)?;
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
    Some(out)
}

/// Body of a single-character string escape (`\n`, `\t`, `\(` …). Octal
/// escapes, line continuations, and the pass-through case return `None`
/// because they need the caller's cursor.
#[inline]
pub(crate) fn simple_escape(c: u8) -> Option<u8> {
    Some(match c {
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        b'b' => 0x08,
        b'f' => 0x0C,
        b'\\' => b'\\',
        b'(' => b'(',
        b')' => b')',
        _ => return None,
    })
}

/// Advance `pos` past spaces and tabs — never past an end-of-line marker.
pub(crate) fn skip_spaces(bytes: &[u8], pos: &mut usize) {
    while matches!(bytes.get(*pos), Some(b' ' | b'\t')) {
        *pos += 1;
    }
}

/// Advance `pos` past one CR, CRLF, or LF end-of-line marker, if present.
pub(crate) fn skip_line_break(bytes: &[u8], pos: &mut usize) {
    match bytes.get(*pos) {
        Some(b'\r') => {
            *pos += 1;
            if bytes.get(*pos) == Some(&b'\n') {
                *pos += 1;
            }
        }
        Some(b'\n') => *pos += 1,
        _ => {}
    }
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
        for &b in b"\x00\t\n\x0C\r " {
            assert!(is_ws(b), "{b}");
            assert!(is_ws_or_delim(b), "{b}");
        }
        for &b in b"()<>[]{}/%" {
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
    fn decode_hex_strict_rejects_non_hex_bytes() {
        assert_eq!(decode_hex_strict(b"48 69").unwrap(), b"Hi");
        assert_eq!(decode_hex_strict(b"4").unwrap(), vec![0x40]);
        assert_eq!(
            decode_hex_strict(b"deADBeEf").unwrap(),
            decode_hex(b"deAD BeEf")
        );
        assert!(decode_hex_strict(b"48zz").is_none());
    }

    #[test]
    fn simple_escape_covers_the_single_character_forms() {
        for (input, expected) in [
            (b'n', b'\n'),
            (b'r', b'\r'),
            (b't', b'\t'),
            (b'b', 0x08),
            (b'f', 0x0C),
            (b'\\', b'\\'),
            (b'(', b'('),
            (b')', b')'),
        ] {
            assert_eq!(simple_escape(input), Some(expected));
        }
        // Octal digits and unknown escapes are the caller's problem.
        assert_eq!(simple_escape(b'0'), None);
        assert_eq!(simple_escape(b'q'), None);
    }

    #[test]
    fn skip_spaces_and_line_breaks_stop_at_the_right_byte() {
        let mut pos = 0;
        skip_spaces(b" \t \n", &mut pos);
        assert_eq!(pos, 3); // stops at the newline
        for (bytes, expected) in [
            (b"\r\n!".as_slice(), 2usize),
            (b"\r!", 1),
            (b"\n!", 1),
            (b"!", 0),
        ] {
            let mut pos = 0;
            skip_line_break(bytes, &mut pos);
            assert_eq!(pos, expected, "{bytes:?}");
        }
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
