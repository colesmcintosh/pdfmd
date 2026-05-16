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
}
