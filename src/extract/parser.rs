//! Streaming parser for PDF content streams.
//!
//! Replaces `lopdf::content::Content::decode`. The latter materialises every
//! operator and its operands into heap-allocated `String` / `Vec<Object>`
//! values up front, even for operators we ignore (path painting, colour,
//! graphics state). At ~12 000 operators for a typical paper PDF that
//! dominates extraction cost.
//!
//! This parser walks the byte stream once and yields one `Token` at a time.
//! Names and clean literal strings are returned as borrowed slices of the
//! input; only escaped literal strings and hex strings allocate.
//!
//! Out of scope: rendering state, graphics paths, encryption, anything not
//! needed by the text extractor. Tokens for those operators are still
//! produced — the caller discards them on dispatch.

use std::borrow::Cow;

pub struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

#[derive(Debug)]
pub enum Token<'a> {
    /// Numeric operand (integer or real, collapsed to f32).
    Num(f32),
    /// A `/Name`, returned without the leading slash.
    Name(&'a [u8]),
    /// A literal `(...)` or hex `<...>` string, decoded into raw bytes. Most
    /// common-case literals contain no escapes, so they stay borrowed.
    Str(Cow<'a, [u8]>),
    ArrayStart,
    ArrayEnd,
    /// Any keyword the tokenizer doesn't recognise as a literal — this is
    /// where the consumer sees `Tj`, `TJ`, `Tf`, etc.
    Op(&'a [u8]),
    Eof,
}

impl<'a> Parser<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub fn next_token(&mut self) -> Token<'a> {
        self.skip_ws_and_comments();
        let Some(&b) = self.bytes.get(self.pos) else {
            return Token::Eof;
        };
        match b {
            b'(' => {
                self.pos += 1;
                self.read_literal_string()
            }
            b'<' => {
                self.pos += 1;
                if self.bytes.get(self.pos) == Some(&b'<') {
                    // Dict literal `<<` — defensive: shouldn't appear in
                    // content streams, but skip to the matching `>>` so we
                    // don't tokenize its innards as operands.
                    self.pos += 1;
                    self.skip_dict();
                    return self.next_token();
                }
                self.read_hex_string()
            }
            b'>' => {
                // Stray `>>` from a stripped dict literal. Skip and resync.
                self.pos += 1;
                if self.bytes.get(self.pos) == Some(&b'>') {
                    self.pos += 1;
                }
                self.next_token()
            }
            b'[' => {
                self.pos += 1;
                Token::ArrayStart
            }
            b']' => {
                self.pos += 1;
                Token::ArrayEnd
            }
            b'/' => {
                self.pos += 1;
                self.read_name()
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.read_number_or_keyword(),
            _ => self.read_keyword(),
        }
    }

    /// Skip past an inline-image data block. After the caller consumes the
    /// `BI` and `ID` operators, the raw image bytes follow until a closing
    /// `EI` operator at a token boundary. Naively tokenizing those bytes
    /// would emit garbage (and likely misparse parentheses inside the image
    /// data as strings) so the caller asks us to fast-forward.
    pub fn skip_inline_image(&mut self) {
        let bytes = self.bytes;
        let mut i = self.pos;
        while i + 2 <= bytes.len() {
            // Look for ws+"EI"+ws (or EOF) — that's the operator boundary.
            if is_ws(bytes[i]) && bytes[i + 1] == b'E' && bytes.get(i + 2) == Some(&b'I') {
                let after = bytes.get(i + 3);
                if after.is_none() || matches!(after.copied(), Some(c) if is_ws(c) || is_delim(c)) {
                    self.pos = i + 3;
                    return;
                }
            }
            i += 1;
        }
        // Unterminated — bail to end of stream.
        self.pos = bytes.len();
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while let Some(&b) = self.bytes.get(self.pos) {
                if is_ws(b) {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.bytes.get(self.pos) == Some(&b'%') {
                while let Some(&b) = self.bytes.get(self.pos) {
                    if b == b'\n' || b == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                return;
            }
        }
    }

    fn read_name(&mut self) -> Token<'a> {
        let start = self.pos;
        while let Some(&b) = self.bytes.get(self.pos) {
            if is_delim_or_ws(b) {
                break;
            }
            self.pos += 1;
        }
        Token::Name(&self.bytes[start..self.pos])
    }

    fn read_keyword(&mut self) -> Token<'a> {
        let start = self.pos;
        while let Some(&b) = self.bytes.get(self.pos) {
            if is_delim_or_ws(b) {
                break;
            }
            self.pos += 1;
        }
        Token::Op(&self.bytes[start..self.pos])
    }

    fn read_number_or_keyword(&mut self) -> Token<'a> {
        let start = self.pos;
        if matches!(self.bytes[self.pos], b'+' | b'-') {
            self.pos += 1;
        }
        let int_start = self.pos;
        while let Some(&b) = self.bytes.get(self.pos) {
            if !b.is_ascii_digit() {
                break;
            }
            self.pos += 1;
        }
        let mut has_digits = self.pos > int_start;
        if self.bytes.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            let frac_start = self.pos;
            while let Some(&b) = self.bytes.get(self.pos) {
                if !b.is_ascii_digit() {
                    break;
                }
                self.pos += 1;
            }
            has_digits |= self.pos > frac_start;
        }
        // Either the leading char wasn't really a number (e.g. just `+` or
        // `.`) or the run kept going into letters (e.g. `10x`, which would
        // make the whole thing a single bizarre keyword). Restart as keyword.
        let next = self.bytes.get(self.pos).copied();
        let at_boundary = next.is_none() || matches!(next, Some(b) if is_delim_or_ws(b));
        if !has_digits || !at_boundary {
            self.pos = start;
            return self.read_keyword();
        }
        let n = std::str::from_utf8(&self.bytes[start..self.pos])
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.0);
        Token::Num(n)
    }

    fn read_literal_string(&mut self) -> Token<'a> {
        let start = self.pos;
        let mut depth: i32 = 1;
        let mut has_escape = false;
        while let Some(&b) = self.bytes.get(self.pos) {
            match b {
                b'(' => {
                    depth += 1;
                    self.pos += 1;
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        let end = self.pos;
                        self.pos += 1;
                        let raw = &self.bytes[start..end];
                        return if has_escape {
                            Token::Str(Cow::Owned(unescape_literal(raw)))
                        } else {
                            Token::Str(Cow::Borrowed(raw))
                        };
                    }
                    self.pos += 1;
                }
                b'\\' => {
                    has_escape = true;
                    self.pos += 1;
                    if self.pos < self.bytes.len() {
                        self.pos += 1;
                    }
                }
                _ => self.pos += 1,
            }
        }
        // Unterminated string — keep what we have. Better to return partial
        // text than to error out and lose the whole page.
        Token::Str(Cow::Borrowed(&self.bytes[start..self.pos]))
    }

    fn read_hex_string(&mut self) -> Token<'a> {
        let start = self.pos;
        while let Some(&b) = self.bytes.get(self.pos) {
            if b == b'>' {
                break;
            }
            self.pos += 1;
        }
        let raw = &self.bytes[start..self.pos];
        if self.bytes.get(self.pos) == Some(&b'>') {
            self.pos += 1;
        }
        Token::Str(Cow::Owned(decode_hex(raw)))
    }

    fn skip_dict(&mut self) {
        let mut depth: i32 = 1;
        while depth > 0 && self.pos < self.bytes.len() {
            if self.bytes[self.pos..].starts_with(b"<<") {
                depth += 1;
                self.pos += 2;
            } else if self.bytes[self.pos..].starts_with(b">>") {
                depth -= 1;
                self.pos += 2;
            } else {
                self.pos += 1;
            }
        }
    }
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

fn is_delim_or_ws(b: u8) -> bool {
    is_ws(b) || is_delim(b)
}

fn unescape_literal(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' && i + 1 < raw.len() {
            let c = raw[i + 1];
            match c {
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                }
                b'r' => {
                    out.push(b'\r');
                    i += 2;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                }
                b'b' => {
                    out.push(0x08);
                    i += 2;
                }
                b'f' => {
                    out.push(0x0C);
                    i += 2;
                }
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                }
                b'(' => {
                    out.push(b'(');
                    i += 2;
                }
                b')' => {
                    out.push(b')');
                    i += 2;
                }
                b'\n' => {
                    // Backslash-newline is a line continuation.
                    i += 2;
                }
                b'\r' => {
                    i += 2;
                    if i < raw.len() && raw[i] == b'\n' {
                        i += 1;
                    }
                }
                b'0'..=b'7' => {
                    let mut v: u32 = 0;
                    let mut n = 0;
                    i += 1;
                    while n < 3 && i < raw.len() && matches!(raw[i], b'0'..=b'7') {
                        v = v * 8 + (raw[i] - b'0') as u32;
                        i += 1;
                        n += 1;
                    }
                    out.push((v & 0xFF) as u8);
                }
                _ => {
                    out.push(c);
                    i += 2;
                }
            }
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    out
}

fn decode_hex(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() / 2);
    let mut nibble: Option<u8> = None;
    for &b in raw {
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => continue,
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
        // Per PDF spec a trailing single nibble pads with 0.
        out.push(prev << 4);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(data: &[u8]) -> Vec<String> {
        let mut p = Parser::new(data);
        let mut out = Vec::new();
        loop {
            match p.next_token() {
                Token::Eof => break,
                Token::Op(b) => out.push(format!("op:{}", std::str::from_utf8(b).unwrap())),
                Token::Num(n) => out.push(format!("num:{n}")),
                Token::Name(n) => out.push(format!("name:{}", std::str::from_utf8(n).unwrap())),
                Token::Str(s) => {
                    out.push(format!("str:{}", std::str::from_utf8(&s).unwrap_or("?")))
                }
                Token::ArrayStart => out.push("[".into()),
                Token::ArrayEnd => out.push("]".into()),
            }
        }
        out
    }

    #[test]
    fn tokenizes_a_text_show_sequence() {
        assert_eq!(
            ops(b"BT /F1 12 Tf 100 200 Td (Hello) Tj ET"),
            [
                "op:BT",
                "name:F1",
                "num:12",
                "op:Tf",
                "num:100",
                "num:200",
                "op:Td",
                "str:Hello",
                "op:Tj",
                "op:ET"
            ]
        );
    }

    #[test]
    fn handles_kerned_tj_array() {
        let got = ops(b"[(He) -50 (llo)] TJ");
        assert_eq!(got, ["[", "str:He", "num:-50", "str:llo", "]", "op:TJ"]);
    }

    #[test]
    fn hex_strings_decode_to_bytes() {
        // <48656C6C6F> is "Hello" in ASCII hex.
        assert_eq!(ops(b"<48656C6C6F> Tj"), ["str:Hello", "op:Tj"]);
    }

    #[test]
    fn comments_are_ignored() {
        assert_eq!(ops(b"% header\n42 % trailing\nTj"), ["num:42", "op:Tj"]);
    }

    #[test]
    fn escape_sequences_in_literal_string() {
        assert_eq!(ops(b"(line1\\nline2) Tj"), ["str:line1\nline2", "op:Tj"]);
    }

    #[test]
    fn balanced_parens_inside_literal() {
        assert_eq!(ops(b"((nested)) Tj"), ["str:(nested)", "op:Tj"]);
    }

    #[test]
    fn signed_and_real_numbers() {
        assert_eq!(
            ops(b"-1.5 +2 .25 100 Tm"),
            ["num:-1.5", "num:2", "num:0.25", "num:100", "op:Tm"]
        );
    }

    #[test]
    fn dict_literal_inside_content_stream_is_skipped() {
        // Real PDFs sometimes embed inline dicts (`<< ... >>`) inside
        // marked-content operators. Our tokenizer should skip past them
        // and resume on the next operator.
        let toks = ops(b"<</K -1>> 42 Tj");
        assert_eq!(toks, ["num:42", "op:Tj"]);
    }

    #[test]
    fn stray_close_dict_does_not_crash() {
        // `>>` outside of a dict literal — we just resync.
        let toks = ops(b">> 7 Tj");
        assert_eq!(toks, ["num:7", "op:Tj"]);
    }

    #[test]
    fn arrays_emit_start_end_tokens() {
        let toks = ops(b"[ 1 2 3 ] TJ");
        assert_eq!(toks, ["[", "num:1", "num:2", "num:3", "]", "op:TJ"]);
    }

    #[test]
    fn unterminated_literal_string_returns_partial_payload() {
        let toks = ops(b"(unclosed forever");
        assert_eq!(toks.len(), 1);
        assert!(toks[0].starts_with("str:"));
    }

    #[test]
    fn literal_string_round_trips_every_escape() {
        // Every match arm in `unescape_literal`.
        let raw = b"(\\n\\r\\t\\b\\f\\\\\\(\\)\\\n\\\r\n\\\rZ\\101\\q)";
        let toks = ops(raw);
        assert_eq!(toks.len(), 1);
        let s = toks[0].trim_start_matches("str:");
        assert!(s.starts_with("\n\r\t"));
    }

    #[test]
    fn hex_string_handles_odd_nibble() {
        // Odd hex string padded with zero — also covers the `if get == >` branch.
        // ops() formats the byte through utf8-or-`?`, so 0x40 ('@') round-trips.
        assert_eq!(ops(b"<4> Tj"), ["str:@", "op:Tj"]);
    }

    #[test]
    fn hex_string_skips_non_hex_bytes() {
        assert_eq!(ops(b"<48 6 9>"), ["str:Hi"]);
    }

    #[test]
    fn hex_string_without_closing_angle() {
        // Tokenizer doesn't fail — it returns what it has and EOFs next.
        assert_eq!(ops(b"<48"), ["str:H"]);
    }

    #[test]
    fn lone_dot_is_a_keyword_not_a_number() {
        // `.` alone isn't a valid number per our tokenizer — falls through.
        let toks = ops(b". Tj");
        assert_eq!(toks, ["op:.", "op:Tj"]);
    }

    #[test]
    fn number_followed_by_letters_is_a_keyword() {
        let toks = ops(b"10x");
        assert_eq!(toks, ["op:10x"]);
    }

    #[test]
    fn decode_hex_pads_odd_trailing_nibble() {
        assert_eq!(decode_hex(b"4"), vec![0x40]);
        assert_eq!(decode_hex(b"48 69"), b"Hi");
        // Non-hex bytes get skipped.
        assert_eq!(decode_hex(b"!!"), Vec::<u8>::new());
    }

    #[test]
    fn unescape_literal_handles_each_escape_arm() {
        let raw = b"\\n\\r\\t\\b\\f\\\\\\(\\)\\\nA\\\r\nB\\\rC\\101D\\?";
        let out = unescape_literal(raw);
        assert!(out.starts_with(b"\n\r\t\x08\x0C\\()"));
    }

    #[test]
    fn unescape_literal_octal_at_eof() {
        // Trailing octal sequence with no following digit must not run past
        // the buffer.
        let out = unescape_literal(b"\\7");
        assert_eq!(out, vec![0x07]);
    }

    #[test]
    fn unescape_literal_trailing_backslash_passes_through() {
        // No following byte to escape — the lone backslash falls into the
        // catch-all `else` and is emitted literally.
        let out = unescape_literal(b"a\\");
        assert_eq!(out, b"a\\");
    }

    #[test]
    fn skip_inline_image_jumps_past_data() {
        // BI dict ID <raw...> EI body — drain tokens up to the `ID`
        // operator using ops(), then exercise skip_inline_image on what's
        // left.
        let stream = b"BI /W 1 /H 1 ID \x00\x01\x02\nEI 99 Tj";
        let mut p = Parser::new(stream);
        // Drain through the `ID` op without leaving an unreachable match arm.
        let mut hit_id = false;
        for _ in 0..16 {
            let tok = p.next_token();
            let tag = describe(&tok);
            if tag == "op:ID" {
                hit_id = true;
                break;
            }
            assert_ne!(tag, "eof", "ran off the end before reaching ID");
        }
        assert!(hit_id);
        p.skip_inline_image();
        // Post-EI we should see `99 Tj` from the rest of the stream.
        let remaining: Vec<String> = (0..2).map(|_| describe(&p.next_token())).collect();
        assert_eq!(remaining, ["num:99", "op:Tj"]);
    }

    #[test]
    fn skip_inline_image_bails_at_eof_when_unterminated() {
        // No `EI` sequence in the body → skip_inline_image runs off the end.
        let mut p = Parser::new(b"image-bytes-without-the-end-marker");
        p.skip_inline_image();
        assert_eq!(describe(&p.next_token()), "eof");
    }

    fn describe(tok: &Token<'_>) -> String {
        match tok {
            Token::Eof => "eof".into(),
            Token::Op(b) => format!("op:{}", std::str::from_utf8(b).unwrap_or("?")),
            Token::Name(n) => format!("name:{}", std::str::from_utf8(n).unwrap_or("?")),
            Token::Num(n) => format!("num:{n}"),
            Token::Str(s) => format!("str:{}", std::str::from_utf8(s).unwrap_or("?")),
            Token::ArrayStart => "[".into(),
            Token::ArrayEnd => "]".into(),
        }
    }
}
