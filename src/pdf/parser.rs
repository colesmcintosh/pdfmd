//! Low-level PDF object syntax parser.
//!
//! Walks raw bytes and produces [`Object`] values. Streams are returned with
//! their raw, undecoded content; the [`super::Document`] layer applies the
//! /Filter chain on demand.

use super::object::{Dictionary, Object, ObjectId, Stream};
use super::PdfError;

pub struct Parser<'a> {
    bytes: &'a [u8],
    pub pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub fn with_pos(bytes: &'a [u8], pos: usize) -> Self {
        Self { bytes, pos }
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
                if self.bytes.get(self.pos) == Some(&b'R') {
                    let after = self.bytes.get(self.pos + 1).copied();
                    if after.is_none() || is_ws_or_delim(after.unwrap()) {
                        let n = parse_u32(num_slice)?;
                        let g = parse_u32(&self.bytes[g_start..g_end])? as u16;
                        self.pos += 1;
                        return Ok(Object::Reference(ObjectId(n, g)));
                    }
                }
            }
            self.pos = saved;
        }

        let s = std::str::from_utf8(num_slice)
            .map_err(|_| PdfError::BadObject("non-utf8 number".into()))?;
        if has_dot {
            Ok(Object::Real(s.parse::<f32>().map_err(|_| {
                PdfError::BadObject(format!("bad real {s}"))
            })?))
        } else {
            Ok(Object::Integer(s.parse::<i64>().map_err(|_| {
                PdfError::BadObject(format!("bad int {s}"))
            })?))
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
        let mut items = Vec::new();
        loop {
            self.skip_ws_and_comments();
            match self.bytes.get(self.pos) {
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Object::Array(items));
                }
                None => return Err(PdfError::BadObject("unterminated array".into())),
                _ => items.push(self.parse_object()?),
            }
        }
    }

    fn parse_dictionary(&mut self) -> Result<Object, PdfError> {
        debug_assert_eq!(&self.bytes[self.pos..self.pos + 2], b"<<");
        self.pos += 2;
        let mut dict = Dictionary::new();
        loop {
            self.skip_ws_and_comments();
            if self.bytes.get(self.pos..self.pos + 2) == Some(b">>") {
                self.pos += 2;
                return Ok(Object::Dictionary(dict));
            }
            if self.bytes.get(self.pos) != Some(&b'/') {
                return Err(PdfError::BadObject(format!(
                    "dict expected name at offset {}",
                    self.pos
                )));
            }
            let key = match self.parse_name()? {
                Object::Name(n) => n,
                _ => unreachable!(),
            };
            let value = self.parse_object()?;
            dict.insert(key, value);
        }
    }

    // ---- Indirect objects + streams --------------------------------------

    /// Parse a complete `N G obj <obj> [stream ... endstream] endobj` block
    /// starting at the current position.
    pub fn parse_indirect_object(&mut self) -> Result<(ObjectId, Object), PdfError> {
        self.skip_ws_and_comments();
        let n_start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        let n = parse_u32(&self.bytes[n_start..self.pos])?;
        self.skip_inline_ws();
        let g_start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        let g = parse_u32(&self.bytes[g_start..self.pos])? as u16;
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
        assert!(matches!(parse(b"42"), Object::Integer(42)));
        assert!(matches!(parse(b"-3.14"), Object::Real(_)));
        if let Object::Real(r) = parse(b".5") {
            assert!((r - 0.5).abs() < 1e-6);
        } else {
            panic!()
        }
    }

    #[test]
    fn references_take_priority() {
        match parse(b"7 0 R") {
            Object::Reference(ObjectId(7, 0)) => {}
            o => panic!("got {o:?}"),
        }
    }

    #[test]
    fn names_decode_hex_escapes() {
        if let Object::Name(n) = parse(b"/A#20B") {
            assert_eq!(n, b"A B");
        } else {
            panic!()
        }
    }

    #[test]
    fn literal_string_with_escapes() {
        if let Object::String(s) = parse(b"(hi\\nthere)") {
            assert_eq!(s, b"hi\nthere");
        } else {
            panic!()
        }
    }

    #[test]
    fn hex_string_decode() {
        if let Object::String(s) = parse(b"<48656c6c6f>") {
            assert_eq!(s, b"Hello");
        } else {
            panic!()
        }
    }

    #[test]
    fn dictionary_with_mixed_entries() {
        let d = parse(b"<< /Type /Foo /Count 5 >>");
        let Object::Dictionary(d) = d else { panic!() };
        assert_eq!(d.get(b"Type").and_then(|o| o.as_name_str()), Some("Foo"));
        assert_eq!(d.get(b"Count").and_then(Object::as_integer), Some(5));
    }

    // ---- Keywords and bools ---------------------------------------------

    #[test]
    fn parses_true_false_null() {
        assert!(matches!(parse(b"true"), Object::Boolean(true)));
        assert!(matches!(parse(b"false"), Object::Boolean(false)));
        assert!(matches!(parse(b"null"), Object::Null));
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
        let mut p = Parser::new(b"+7 0 R");
        assert!(matches!(p.parse_object().unwrap(), Object::Integer(7)));
    }

    #[test]
    fn reference_lookahead_aborts_when_r_is_part_of_keyword() {
        // `7 0 Rx` should NOT be a reference — the R must end at a delim.
        let mut p = Parser::new(b"7 0 Rx");
        assert!(matches!(p.parse_object().unwrap(), Object::Integer(7)));
    }

    #[test]
    fn reference_lookahead_aborts_when_gen_missing() {
        // `7 obj` — second token isn't an integer, so this stays an int.
        let mut p = Parser::new(b"7 obj");
        assert!(matches!(p.parse_object().unwrap(), Object::Integer(7)));
    }

    #[test]
    fn naked_dot_is_a_number() {
        let v = parse(b".75");
        let Object::Real(r) = v else { panic!() };
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
            match parse(input) {
                Object::String(s) => assert_eq!(s, *expected, "input {input:?}"),
                other => panic!("input {input:?} parsed as {other:?}"),
            }
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
        if let Object::String(s) = parse(b"(a(b(c)d)e)") {
            assert_eq!(s, b"a(b(c)d)e");
        } else {
            panic!();
        }
    }

    #[test]
    fn hex_string_with_whitespace_and_odd_nibble() {
        if let Object::String(s) = parse(b"<4 8 6>") {
            assert_eq!(s, vec![0x48, 0x60]);
        } else {
            panic!();
        }
    }

    #[test]
    fn hex_string_unterminated_errors() {
        let mut p = Parser::new(b"<48");
        assert!(p.parse_object().is_err());
    }

    #[test]
    fn hex_string_ignores_non_hex_bytes() {
        // Non-hex characters within the body are dropped (not nibbles).
        if let Object::String(s) = parse(b"<48ZZ69>") {
            assert_eq!(s, b"Hi");
        } else {
            panic!();
        }
    }

    // ---- Names ----------------------------------------------------------

    #[test]
    fn name_without_hash_takes_fast_path() {
        let v = parse(b"/Foo");
        let Object::Name(n) = v else { panic!() };
        assert_eq!(n, b"Foo");
    }

    #[test]
    fn name_with_invalid_hash_escape_passes_through() {
        let v = parse(b"/A#ZZ");
        let Object::Name(n) = v else { panic!() };
        assert_eq!(n, b"A#ZZ");
        let v = parse(b"/B#");
        let Object::Name(n) = v else { panic!() };
        assert_eq!(n, b"B#");
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
        let v = parse(b"[ 1 % comment\n 2 ]");
        let Object::Array(items) = v else { panic!() };
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
        let Object::Stream(s) = obj else { panic!() };
        assert_eq!(s.content, b"hello");
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
        let Object::Stream(s) = obj else { panic!() };
        assert_eq!(s.content, b"the body");
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
        let v = parse(b"% header\n42");
        assert!(matches!(v, Object::Integer(42)));
    }
}
