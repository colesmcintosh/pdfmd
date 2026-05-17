//! Low-level PDF object syntax parser.
//!
//! Walks raw bytes and produces [`Object`] values. Streams are returned with
//! their raw, undecoded content; the [`super::Document`] layer applies the
//! /Filter chain on demand.

use super::object::{Dictionary, Object, ObjectId, Stream};
use super::PdfError;

/// Cap on container nesting depth. Real PDFs nest a handful of levels at
/// most; this guards against malformed input like `[[[[...]]]]` that would
/// blow the stack via mutual recursion through parse_object → parse_array
/// / parse_dictionary.
const MAX_PARSE_DEPTH: u32 = 256;

pub struct Parser<'a> {
    bytes: &'a [u8],
    pub pos: usize,
    depth: u32,
}

impl<'a> Parser<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            depth: 0,
        }
    }

    pub fn with_pos(bytes: &'a [u8], pos: usize) -> Self {
        Self {
            bytes,
            pos,
            depth: 0,
        }
    }

    // ---- Whitespace / comments -------------------------------------------

    pub fn skip_ws_and_comments(&mut self) {
        loop {
            while self.pos < self.bytes.len() && is_ws(self.bytes[self.pos]) {
                self.pos += 1;
            }
            if self.pos < self.bytes.len() && self.bytes[self.pos] == b'%' {
                while self.pos < self.bytes.len()
                    && self.bytes[self.pos] != b'\n'
                    && self.bytes[self.pos] != b'\r'
                {
                    self.pos += 1;
                }
            } else {
                return;
            }
        }
    }

    fn skip_inline_ws(&mut self) {
        while self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b' ' | b'\t') {
            self.pos += 1;
        }
    }

    // ---- Object dispatch -------------------------------------------------

    /// Parse a single PDF object starting at the current position.
    pub fn parse_object(&mut self) -> Result<Object, PdfError> {
        self.skip_ws_and_comments();
        let Some(&b) = self.bytes.get(self.pos) else {
            return Err(PdfError::BadObject("unexpected EOF".into()));
        };
        match b {
            b'<' => {
                if self.bytes.get(self.pos + 1) == Some(&b'<') {
                    self.parse_dictionary()
                } else {
                    self.parse_hex_string()
                }
            }
            b'(' => self.parse_literal_string(),
            b'[' => self.parse_array(),
            b'/' => self.parse_name(),
            b't' | b'f' | b'n' => self.parse_keyword_object(),
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.parse_number_or_reference(),
            _ => Err(PdfError::BadObject(format!(
                "unexpected byte 0x{:02X} at offset {}",
                b, self.pos
            ))),
        }
    }

    // ---- Keywords (true / false / null) ----------------------------------

    fn parse_keyword_object(&mut self) -> Result<Object, PdfError> {
        if self.starts_with(b"true") {
            self.pos += 4;
            Ok(Object::Boolean(true))
        } else if self.starts_with(b"false") {
            self.pos += 5;
            Ok(Object::Boolean(false))
        } else if self.starts_with(b"null") {
            self.pos += 4;
            Ok(Object::Null)
        } else {
            Err(PdfError::BadObject(format!(
                "unknown keyword at offset {}",
                self.pos
            )))
        }
    }

    fn starts_with(&self, kw: &[u8]) -> bool {
        self.bytes
            .get(self.pos..self.pos + kw.len())
            .is_some_and(|w| w == kw)
    }

    // ---- Numbers and references ------------------------------------------

    fn parse_number_or_reference(&mut self) -> Result<Object, PdfError> {
        let start = self.pos;
        let mut signed = false;
        if matches!(self.bytes[self.pos], b'+' | b'-') {
            signed = true;
            self.pos += 1;
        }
        let int_start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        let int_end = self.pos;
        let mut has_dot = false;
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'.' {
            has_dot = true;
            self.pos += 1;
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        if int_end == int_start && !has_dot {
            return Err(PdfError::BadObject(format!(
                "expected number at offset {start}"
            )));
        }
        let num_slice = &self.bytes[start..self.pos];

        // Reference lookahead: `N G R` — only valid for positive integers.
        if !signed && !has_dot {
            let saved = self.pos;
            self.skip_inline_ws();
            if self.bytes.get(self.pos).is_some_and(|b| b.is_ascii_digit()) {
                let g_start = self.pos;
                while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
                let g_end = self.pos;
                self.skip_inline_ws();
                // MSRV 1.70 — no `Option::is_none_or` yet; spell it out.
                let after_r = self.bytes.get(self.pos + 1).copied();
                let r_ok = self.bytes.get(self.pos) == Some(&b'R')
                    && after_r.map(is_ws_or_delim).unwrap_or(true);
                if r_ok {
                    // num_slice and the gen slice are both digit-only, so
                    // `parse_u32` only fails on overflow — we clamp.
                    let n = parse_u32(num_slice).unwrap_or(u32::MAX);
                    let g = parse_u32(&self.bytes[g_start..g_end]).unwrap_or(0) as u16;
                    self.pos += 1;
                    return Ok(Object::Reference(ObjectId(n, g)));
                }
            }
            self.pos = saved;
        }

        // num_slice is ASCII digits / sign / decimal — always valid UTF-8.
        let s = std::str::from_utf8(num_slice).expect("digit slice is utf-8");
        if has_dot {
            // f32 parsing of a digit string never fails — at worst it
            // produces +/-infinity for huge magnitudes, which is fine.
            Ok(Object::Real(s.parse::<f32>().unwrap_or(0.0)))
        } else {
            // Saturate on i64 overflow rather than fail the page.
            Ok(Object::Integer(s.parse::<i64>().unwrap_or(i64::MAX)))
        }
    }

    // ---- Strings ---------------------------------------------------------

    fn parse_literal_string(&mut self) -> Result<Object, PdfError> {
        debug_assert_eq!(self.bytes[self.pos], b'(');
        self.pos += 1;
        let mut out = Vec::new();
        let mut depth: i32 = 1;
        while let Some(&b) = self.bytes.get(self.pos) {
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                    self.pos += 1;
                }
                b')' => {
                    depth -= 1;
                    self.pos += 1;
                    if depth == 0 {
                        return Ok(Object::String(out));
                    }
                    out.push(b);
                }
                b'\\' => {
                    self.pos += 1;
                    let Some(&c) = self.bytes.get(self.pos) else {
                        break;
                    };
                    self.pos += 1;
                    match c {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'\\' => out.push(b'\\'),
                        b'(' => out.push(b'('),
                        b')' => out.push(b')'),
                        b'\n' => {} // line continuation
                        b'\r' => {
                            if self.bytes.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'0'..=b'7' => {
                            let mut v: u32 = (c - b'0') as u32;
                            for _ in 0..2 {
                                let Some(&d) = self.bytes.get(self.pos) else {
                                    break;
                                };
                                if !(b'0'..=b'7').contains(&d) {
                                    break;
                                }
                                v = v * 8 + (d - b'0') as u32;
                                self.pos += 1;
                            }
                            out.push((v & 0xFF) as u8);
                        }
                        _ => out.push(c),
                    }
                }
                _ => {
                    out.push(b);
                    self.pos += 1;
                }
            }
        }
        Err(PdfError::BadObject("unterminated literal string".into()))
    }

    fn parse_hex_string(&mut self) -> Result<Object, PdfError> {
        debug_assert_eq!(self.bytes[self.pos], b'<');
        self.pos += 1;
        let mut out = Vec::new();
        let mut nibble: Option<u8> = None;
        while let Some(&b) = self.bytes.get(self.pos) {
            if b == b'>' {
                self.pos += 1;
                if let Some(prev) = nibble {
                    out.push(prev << 4);
                }
                return Ok(Object::String(out));
            }
            self.pos += 1;
            if is_ws(b) {
                continue;
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
        Err(PdfError::BadObject("unterminated hex string".into()))
    }

    // ---- Names -----------------------------------------------------------

    fn parse_name(&mut self) -> Result<Object, PdfError> {
        debug_assert_eq!(self.bytes[self.pos], b'/');
        self.pos += 1;
        let start = self.pos;
        while let Some(&b) = self.bytes.get(self.pos) {
            if is_ws_or_delim(b) {
                break;
            }
            self.pos += 1;
        }
        // Decode `#XX` hex escapes if present. The common case has none so
        // we scan first and only allocate when needed.
        let raw = &self.bytes[start..self.pos];
        if !raw.contains(&b'#') {
            return Ok(Object::Name(raw.to_vec()));
        }
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'#' && i + 2 < raw.len() {
                if let (Some(h), Some(l)) = (hex_digit(raw[i + 1]), hex_digit(raw[i + 2])) {
                    out.push((h << 4) | l);
                    i += 3;
                    continue;
                }
            }
            out.push(raw[i]);
            i += 1;
        }
        Ok(Object::Name(out))
    }

    // ---- Arrays and dicts -----------------------------------------------

    fn parse_array(&mut self) -> Result<Object, PdfError> {
        debug_assert_eq!(self.bytes[self.pos], b'[');
        self.pos += 1;
        if self.depth >= MAX_PARSE_DEPTH {
            return Err(PdfError::BadObject("nested too deep".into()));
        }
        self.depth += 1;
        let mut items = Vec::new();
        let result = loop {
            self.skip_ws_and_comments();
            match self.bytes.get(self.pos) {
                Some(b']') => {
                    self.pos += 1;
                    break Ok(Object::Array(items));
                }
                None => break Err(PdfError::BadObject("unterminated array".into())),
                _ => match self.parse_object() {
                    Ok(item) => items.push(item),
                    Err(e) => break Err(e),
                },
            }
        };
        self.depth -= 1;
        result
    }

    fn parse_dictionary(&mut self) -> Result<Object, PdfError> {
        debug_assert_eq!(&self.bytes[self.pos..self.pos + 2], b"<<");
        self.pos += 2;
        if self.depth >= MAX_PARSE_DEPTH {
            return Err(PdfError::BadObject("nested too deep".into()));
        }
        self.depth += 1;
        let mut dict = Dictionary::new();
        let result = loop {
            self.skip_ws_and_comments();
            if self.bytes.get(self.pos..self.pos + 2) == Some(b">>") {
                self.pos += 2;
                break Ok(Object::Dictionary(dict));
            }
            if self.bytes.get(self.pos) != Some(&b'/') {
                break Err(PdfError::BadObject(format!(
                    "dict expected name at offset {}",
                    self.pos
                )));
            }
            // parse_name always returns Object::Name on success.
            let key = match self.parse_name() {
                Ok(k) => k.as_name().expect("parse_name returns Name").to_vec(),
                Err(e) => break Err(e),
            };
            let value = match self.parse_object() {
                Ok(v) => v,
                Err(e) => break Err(e),
            };
            dict.insert(key, value);
        };
        self.depth -= 1;
        result
    }

    // ---- Indirect objects + streams --------------------------------------

    /// Parse a complete `N G obj <obj> [stream ... endstream] endobj` block
    /// starting at the current position.
    pub fn parse_indirect_object(&mut self) -> Result<(ObjectId, Object), PdfError> {
        self.skip_ws_and_comments();
        // skip_ws_and_comments stops at `bytes.len()`, but `with_pos` lets
        // callers seed pos to something larger — guard the slice arithmetic.
        if self.pos >= self.bytes.len() {
            return Err(PdfError::BadObject("indirect object past EOF".into()));
        }
        let n_start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        // The slice is digit-only, so parse_u32 only fails on overflow
        // (a ~10-billion-object PDF). Saturate rather than fail.
        let n = parse_u32(&self.bytes[n_start..self.pos]).unwrap_or(u32::MAX);
        self.skip_inline_ws();
        let g_start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        let g = parse_u32(&self.bytes[g_start..self.pos]).unwrap_or(0) as u16;
        self.skip_inline_ws();
        if !self.starts_with(b"obj") {
            return Err(PdfError::BadObject(format!(
                "expected `obj` at offset {}",
                self.pos
            )));
        }
        self.pos += 3;
        let object = self.parse_object()?;
        // Stream object: peek for the `stream` keyword.
        self.skip_ws_and_comments();
        let object = if self.starts_with(b"stream") {
            let Object::Dictionary(dict) = object else {
                return Err(PdfError::BadObject(
                    "stream prefix without preceding dictionary".into(),
                ));
            };
            self.pos += b"stream".len();
            // Spec: stream keyword is followed by CRLF or LF (not just CR).
            match self.bytes.get(self.pos) {
                Some(&b'\r') => {
                    self.pos += 1;
                    if self.bytes.get(self.pos) == Some(&b'\n') {
                        self.pos += 1;
                    }
                }
                Some(&b'\n') => self.pos += 1,
                _ => {}
            }
            let length = stream_length(&dict)?;
            let content = if let Some(len) = length {
                let end = self
                    .pos
                    .checked_add(len)
                    .ok_or_else(|| PdfError::BadObject("stream length overflow".into()))?;
                if end > self.bytes.len() {
                    return Err(PdfError::BadObject("stream truncated".into()));
                }
                let bytes = self.bytes[self.pos..end].to_vec();
                self.pos = end;
                bytes
            } else {
                // /Length is an indirect reference we can't resolve at this
                // layer. Scan forward for the matching `endstream`.
                let end = find_endstream(&self.bytes[self.pos..])
                    .ok_or_else(|| PdfError::BadObject("missing endstream".into()))?;
                let bytes = self.bytes[self.pos..self.pos + end].to_vec();
                self.pos += end;
                bytes
            };
            self.skip_ws_and_comments();
            if self.starts_with(b"endstream") {
                self.pos += b"endstream".len();
            }
            Object::Stream(Stream { dict, content })
        } else {
            object
        };
        self.skip_ws_and_comments();
        if self.starts_with(b"endobj") {
            self.pos += b"endobj".len();
        }
        Ok((ObjectId(n, g), object))
    }
}

/// Look up `/Length` in a stream dict if it's a direct integer.
fn stream_length(dict: &Dictionary) -> Result<Option<usize>, PdfError> {
    match dict.get(b"Length") {
        Some(Object::Integer(n)) if *n >= 0 => Ok(Some(*n as usize)),
        Some(_) => Ok(None), // indirect; caller falls back to scanning
        None => Err(PdfError::BadObject("stream missing /Length".into())),
    }
}

/// Locate the `endstream` keyword in a stream body. The leading newline
/// belongs to the keyword by spec, not to the stream content.
fn find_endstream(bytes: &[u8]) -> Option<usize> {
    let needle = b"endstream";
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            // Walk back over any trailing CR/LF that belongs to the keyword.
            let mut end = i;
            if end > 0 && bytes[end - 1] == b'\n' {
                end -= 1;
                if end > 0 && bytes[end - 1] == b'\r' {
                    end -= 1;
                }
            } else if end > 0 && bytes[end - 1] == b'\r' {
                end -= 1;
            }
            return Some(end);
        }
        i += 1;
    }
    None
}

fn parse_u32(bytes: &[u8]) -> Result<u32, PdfError> {
    if bytes.is_empty() {
        return Err(PdfError::BadObject("empty integer".into()));
    }
    let mut v: u32 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return Err(PdfError::BadObject(format!("bad integer byte 0x{b:02X}")));
        }
        v = v
            .checked_mul(10)
            .and_then(|x| x.checked_add((b - b'0') as u32))
            .ok_or_else(|| PdfError::BadObject("integer overflow".into()))?;
    }
    Ok(v)
}

fn is_ws(b: u8) -> bool {
    matches!(b, 0x00 | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_ws_or_delim(b: u8) -> bool {
    is_ws(b) || is_delim(b)
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &[u8]) -> Object {
        Parser::new(input).parse_object().unwrap()
    }

    #[test]
    fn integers_and_reals() {
        assert_eq!(parse(b"42").as_integer(), Some(42));
        // Some non-pi negative real, round-tripped through f32.
        let r = parse(b"-2.5").as_real().unwrap();
        assert!((r + 2.5).abs() < 1e-5);
        let r = parse(b".5").as_real().unwrap();
        assert!((r - 0.5).abs() < 1e-6);
    }

    #[test]
    fn references_take_priority() {
        assert_eq!(parse(b"7 0 R").as_reference(), Some(ObjectId(7, 0)));
    }

    #[test]
    fn names_decode_hex_escapes() {
        assert_eq!(parse(b"/A#20B").as_name(), Some(b"A B".as_slice()));
    }

    #[test]
    fn literal_string_with_escapes() {
        assert_eq!(
            parse(b"(hi\\nthere)").as_string(),
            Some(b"hi\nthere".as_slice())
        );
    }

    #[test]
    fn hex_string_decode() {
        assert_eq!(
            parse(b"<48656c6c6f>").as_string(),
            Some(b"Hello".as_slice())
        );
        // Uppercase A-F variant exercises the second arm of hex_digit.
        assert_eq!(
            parse(b"<DEADBEEF>").as_string(),
            Some(&[0xDE, 0xAD, 0xBE, 0xEF][..]),
        );
    }

    #[test]
    fn dictionary_with_mixed_entries() {
        let parsed = parse(b"<< /Type /Foo /Count 5 >>");
        let d = parsed.as_dict().unwrap();
        assert_eq!(d.get(b"Type").and_then(|o| o.as_name_str()), Some("Foo"));
        assert_eq!(d.get(b"Count").and_then(Object::as_integer), Some(5));
    }

    // ---- Keywords and bools ---------------------------------------------

    #[test]
    fn parses_true_false_null() {
        assert_eq!(parse(b"true").as_boolean(), Some(true));
        assert_eq!(parse(b"false").as_boolean(), Some(false));
        assert!(parse(b"null").is_null());
    }

    #[test]
    fn rejects_unknown_keyword() {
        let mut p = Parser::new(b"truth");
        assert!(p.parse_object().is_err());
    }

    #[test]
    fn rejects_eof_object() {
        let mut p = Parser::new(b"   ");
        assert!(p.parse_object().is_err());
    }

    #[test]
    fn rejects_unexpected_lead_byte() {
        let mut p = Parser::new(b"@");
        assert!(p.parse_object().is_err());
    }

    // ---- Numbers --------------------------------------------------------

    #[test]
    fn number_after_sign_is_not_a_reference() {
        // `+7 0 R` shouldn't be parsed as a reference (signed lookahead path).
        assert_eq!(parse(b"+7 0 R").as_integer(), Some(7));
    }

    #[test]
    fn reference_lookahead_aborts_when_r_is_part_of_keyword() {
        // `7 0 Rx` should NOT be a reference — the R must end at a delim.
        assert_eq!(parse(b"7 0 Rx").as_integer(), Some(7));
    }

    #[test]
    fn reference_lookahead_aborts_when_gen_missing() {
        // `7 obj` — second token isn't an integer, so this stays an int.
        assert_eq!(parse(b"7 obj").as_integer(), Some(7));
    }

    #[test]
    fn naked_dot_is_a_number() {
        let r = parse(b".75").as_real().unwrap();
        assert!((r - 0.75).abs() < 1e-6);
    }

    #[test]
    fn rejects_pure_sign_with_no_digits() {
        let mut p = Parser::new(b"+");
        assert!(p.parse_object().is_err());
    }

    // ---- Strings --------------------------------------------------------

    #[test]
    fn literal_string_supports_every_escape() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"(\\n)", b"\n"),
            (b"(\\r)", b"\r"),
            (b"(\\t)", b"\t"),
            (b"(\\b)", b"\x08"),
            (b"(\\f)", b"\x0C"),
            (b"(\\\\)", b"\\"),
            (b"(\\()", b"("),
            (b"(\\))", b")"),
            (b"(a\\\nb)", b"ab"),   // line continuation
            (b"(a\\\r\nb)", b"ab"), // CRLF continuation
            (b"(a\\\rb)", b"ab"),   // CR continuation
            (b"(\\101)", b"A"),     // octal escape
            (b"(\\7)", b"\x07"),    // single-digit octal
            (b"(\\z)", b"z"),       // unknown escape echoes the char
        ];
        for (input, expected) in cases {
            assert_eq!(parse(input).as_string(), Some(*expected), "input {input:?}");
        }
    }

    #[test]
    fn literal_string_unterminated_errors() {
        let mut p = Parser::new(b"(no close");
        assert!(p.parse_object().is_err());
    }

    #[test]
    fn literal_string_unterminated_after_escape_errors() {
        let mut p = Parser::new(b"(escape\\");
        assert!(p.parse_object().is_err());
    }

    #[test]
    fn literal_string_with_balanced_nested_parens() {
        assert_eq!(
            parse(b"(a(b(c)d)e)").as_string(),
            Some(b"a(b(c)d)e".as_slice()),
        );
    }

    #[test]
    fn hex_string_with_whitespace_and_odd_nibble() {
        assert_eq!(parse(b"<4 8 6>").as_string(), Some(&[0x48, 0x60][..]));
    }

    #[test]
    fn hex_string_unterminated_errors() {
        let mut p = Parser::new(b"<48");
        assert!(p.parse_object().is_err());
    }

    #[test]
    fn hex_string_ignores_non_hex_bytes() {
        // Non-hex characters within the body are dropped (not nibbles).
        assert_eq!(parse(b"<48ZZ69>").as_string(), Some(b"Hi".as_slice()));
    }

    // ---- Names ----------------------------------------------------------

    #[test]
    fn name_without_hash_takes_fast_path() {
        assert_eq!(parse(b"/Foo").as_name(), Some(b"Foo".as_slice()));
    }

    #[test]
    fn name_with_invalid_hash_escape_passes_through() {
        assert_eq!(parse(b"/A#ZZ").as_name(), Some(b"A#ZZ".as_slice()));
        assert_eq!(parse(b"/B#").as_name(), Some(b"B#".as_slice()));
    }

    // ---- Arrays & dicts -------------------------------------------------

    #[test]
    fn unterminated_array_errors() {
        let mut p = Parser::new(b"[ 1 2");
        assert!(p.parse_object().is_err());
    }

    #[test]
    fn dictionary_expects_name_key() {
        let mut p = Parser::new(b"<< 1 2 >>");
        assert!(p.parse_object().is_err());
    }

    #[test]
    fn comments_inside_array_are_skipped() {
        let parsed = parse(b"[ 1 % comment\n 2 ]");
        let items = parsed.as_array().unwrap();
        assert_eq!(items.len(), 2);
    }

    // ---- Indirect objects + streams -------------------------------------

    #[test]
    fn parses_indirect_object_with_inline_stream() {
        let bytes = b"\
1 0 obj
<< /Length 5 >>
stream
hello
endstream
endobj
";
        let mut p = Parser::new(bytes);
        let (id, obj) = p.parse_indirect_object().unwrap();
        assert_eq!(id, ObjectId(1, 0));
        assert_eq!(obj.as_stream().unwrap().content, b"hello");
    }

    #[test]
    fn indirect_object_with_indirect_length_scans_for_endstream() {
        let bytes = b"\
2 0 obj
<< /Length 99 0 R >>
stream
the body
endstream
endobj
";
        let mut p = Parser::new(bytes);
        let (id, obj) = p.parse_indirect_object().unwrap();
        assert_eq!(id, ObjectId(2, 0));
        assert_eq!(obj.as_stream().unwrap().content, b"the body");
    }

    #[test]
    fn stream_prefix_without_dict_errors() {
        let bytes = b"1 0 obj\n123\nstream\n...endstream endobj";
        let mut p = Parser::new(bytes);
        assert!(p.parse_indirect_object().is_err());
    }

    #[test]
    fn stream_missing_length_errors() {
        let bytes = b"1 0 obj\n<< >>\nstream\nbody\nendstream endobj";
        let mut p = Parser::new(bytes);
        assert!(p.parse_indirect_object().is_err());
    }

    #[test]
    fn stream_truncated_by_length_errors() {
        let bytes = b"1 0 obj\n<< /Length 100 >>\nstream\nbody\nendstream endobj";
        let mut p = Parser::new(bytes);
        assert!(p.parse_indirect_object().is_err());
    }

    #[test]
    fn stream_indirect_length_missing_endstream_errors() {
        let bytes = b"1 0 obj\n<< /Length 99 0 R >>\nstream\nlost forever endobj";
        let mut p = Parser::new(bytes);
        assert!(p.parse_indirect_object().is_err());
    }

    #[test]
    fn indirect_object_requires_obj_keyword() {
        let bytes = b"1 0 NOPE\n123\nendobj";
        let mut p = Parser::new(bytes);
        assert!(p.parse_indirect_object().is_err());
    }

    #[test]
    fn find_endstream_handles_each_terminator() {
        assert_eq!(find_endstream(b"abc\nendstream"), Some(3));
        assert_eq!(find_endstream(b"abc\r\nendstream"), Some(3));
        assert_eq!(find_endstream(b"abc\rendstream"), Some(3));
        assert_eq!(find_endstream(b"endstream"), Some(0));
        assert_eq!(find_endstream(b"no marker"), None);
    }

    #[test]
    fn parse_u32_rejects_empty_and_overflow_and_non_digit() {
        assert!(parse_u32(b"").is_err());
        assert!(parse_u32(b"9999999999").is_err());
        assert!(parse_u32(b"12x3").is_err());
        assert_eq!(parse_u32(b"42").unwrap(), 42);
    }

    #[test]
    fn comments_at_top_level_are_ignored() {
        assert_eq!(parse(b"% header\n42").as_integer(), Some(42));
    }

    #[test]
    fn parse_array_propagates_inner_object_error() {
        // `@` isn't a valid object lead byte — the inner parse_object errors.
        let mut p = Parser::new(b"[ @ ]");
        assert!(p.parse_object().is_err());
    }

    #[test]
    fn parse_dictionary_propagates_value_error() {
        // /Key has no parseable value, so parse_object errors.
        let mut p = Parser::new(b"<< /K @ >>");
        assert!(p.parse_object().is_err());
    }

    #[test]
    fn parse_indirect_object_propagates_inner_object_error() {
        // The body between `obj` and `endobj` isn't a valid PDF object.
        let mut p = Parser::new(b"1 0 obj @ endobj");
        assert!(p.parse_indirect_object().is_err());
    }

    #[test]
    fn parse_indirect_object_past_eof_errors() {
        // with_pos lets a caller seed pos past the end — guard kicks in.
        let mut p = Parser::with_pos(b"1 0 obj 42 endobj", 999);
        assert!(p.parse_indirect_object().is_err());
    }

    #[test]
    fn stream_keyword_followed_by_crlf_is_skipped() {
        // CRLF after `stream` — both bytes are consumed before body starts.
        let bytes = b"1 0 obj <</Length 3>>\nstream\r\nABC\nendstream endobj";
        let mut p = Parser::new(bytes);
        let (_, obj) = p.parse_indirect_object().unwrap();
        assert_eq!(obj.as_stream().unwrap().content, b"ABC");
    }

    #[test]
    fn stream_keyword_followed_by_lone_cr_is_tolerated() {
        // Single CR — we consume it and read the body that follows.
        let bytes = b"1 0 obj <</Length 3>>\nstream\rABC\nendstream endobj";
        let mut p = Parser::new(bytes);
        let (_, obj) = p.parse_indirect_object().unwrap();
        assert_eq!(obj.as_stream().unwrap().content, b"ABC");
    }

    #[test]
    fn stream_keyword_with_no_eol_uses_remaining_bytes() {
        // No EOL at all — the body starts immediately.
        let bytes = b"1 0 obj <</Length 3>>\nstreamABC\nendstream endobj";
        let mut p = Parser::new(bytes);
        let (_, obj) = p.parse_indirect_object().unwrap();
        assert_eq!(obj.as_stream().unwrap().content, b"ABC");
    }

    #[test]
    fn stream_with_length_overflowing_usize_errors() {
        let bytes = format!(
            "1 0 obj <</Length {}>>\nstream\nbody\nendstream endobj",
            usize::MAX
        );
        let mut p = Parser::new(bytes.as_bytes());
        assert!(p.parse_indirect_object().is_err());
    }

    #[test]
    fn deeply_nested_array_errors_instead_of_overflowing_stack() {
        // 10_000 levels of `[` would smash the stack without the depth cap.
        let input = vec![b'['; 10_000];
        let mut p = Parser::new(&input);
        let err = p.parse_object().unwrap_err();
        assert!(err.to_string().contains("nested too deep"));
    }

    #[test]
    fn deeply_nested_dictionary_errors_instead_of_overflowing_stack() {
        let mut input = Vec::new();
        for _ in 0..2_000 {
            input.extend_from_slice(b"<</K ");
        }
        let mut p = Parser::new(&input);
        let err = p.parse_object().unwrap_err();
        assert!(err.to_string().contains("nested too deep"));
    }

    #[test]
    fn unescape_octal_at_end_with_no_following_digits_breaks_loop() {
        // The `(\7)` form is already covered; here the octal escape ends
        // at the closing paren without consuming a 2nd or 3rd digit and
        // the inner `break` arm runs.
        assert_eq!(parse(b"(\\7)").as_string(), Some(b"\x07".as_slice()));
    }
}
