//! Parser for the subset of CMap syntax used in PDF `ToUnicode` streams.
//!
//! Reference: Adobe Tech Note #5411 ("ToUnicode Mapping File Tutorial").
//! We only need `beginbfchar` / `beginbfrange` plus enough of
//! `begincodespacerange` to learn whether source codes are 1- or 2-byte.

use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct CMap {
    /// Width of source codes in bytes (1 for simple fonts, 2 for CID fonts).
    pub code_width: usize,
    map: HashMap<u32, String>,
}

impl CMap {
    pub fn lookup(&self, code: u32) -> Option<&str> {
        self.map.get(&code).map(String::as_str)
    }

    pub fn code_width(&self) -> usize {
        self.code_width.max(1)
    }
}

/// Parse the body of a `ToUnicode` stream.
pub fn parse(data: &[u8]) -> CMap {
    let mut cmap = CMap {
        code_width: 1,
        map: HashMap::new(),
    };
    let tokens = tokenize(data);
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Keyword(kw) if kw == "begincodespacerange" => {
                i += 1;
                while i < tokens.len() {
                    if matches!(&tokens[i], Token::Keyword(k) if k == "endcodespacerange") {
                        i += 1;
                        break;
                    }
                    let Some((lo, _)) = take_hex_pair(&tokens, &mut i) else { break };
                    cmap.code_width = cmap.code_width.max(lo.len());
                }
            }
            Token::Keyword(kw) if kw == "beginbfchar" => {
                i += 1;
                while i < tokens.len() {
                    if matches!(&tokens[i], Token::Keyword(k) if k == "endbfchar") {
                        i += 1;
                        break;
                    }
                    let Some((src, dst)) = take_hex_pair(&tokens, &mut i) else { break };
                    if let (Some(code), Some(text)) = (bytes_to_u32(&src), utf16be_to_string(&dst))
                    {
                        cmap.map.insert(code, text);
                    }
                }
            }
            Token::Keyword(kw) if kw == "beginbfrange" => {
                i += 1;
                while i < tokens.len() {
                    if matches!(&tokens[i], Token::Keyword(k) if k == "endbfrange") {
                        i += 1;
                        break;
                    }
                    let Some(lo) = take_hex(&tokens, &mut i) else { break };
                    let Some(hi) = take_hex(&tokens, &mut i) else { break };
                    let (Some(lo_code), Some(hi_code)) = (bytes_to_u32(&lo), bytes_to_u32(&hi))
                    else {
                        continue;
                    };
                    match tokens.get(i) {
                        Some(Token::Hex(dst)) => {
                            let dst = dst.clone();
                            i += 1;
                            // Sequential range: increment the final Unicode
                            // code point for each source code in [lo, hi].
                            for (offset, code) in (lo_code..=hi_code).enumerate() {
                                let mut shifted = dst.clone();
                                add_to_last_u16(&mut shifted, offset as u32);
                                if let Some(text) = utf16be_to_string(&shifted) {
                                    cmap.map.insert(code, text);
                                }
                            }
                        }
                        Some(Token::ArrayStart) => {
                            i += 1;
                            let mut entries: Vec<Vec<u8>> = Vec::new();
                            while let Some(tok) = tokens.get(i) {
                                match tok {
                                    Token::Hex(h) => {
                                        entries.push(h.clone());
                                        i += 1;
                                    }
                                    Token::ArrayEnd => {
                                        i += 1;
                                        break;
                                    }
                                    _ => {
                                        i += 1;
                                    }
                                }
                            }
                            for (offset, code) in (lo_code..=hi_code).enumerate() {
                                if let Some(bytes) = entries.get(offset) {
                                    if let Some(text) = utf16be_to_string(bytes) {
                                        cmap.map.insert(code, text);
                                    }
                                }
                            }
                        }
                        _ => {
                            i += 1;
                        }
                    }
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    cmap
}

/// Convenience: consume the next hex string from the token stream.
fn take_hex(tokens: &[Token], i: &mut usize) -> Option<Vec<u8>> {
    while let Some(tok) = tokens.get(*i) {
        match tok {
            Token::Hex(bytes) => {
                let out = bytes.clone();
                *i += 1;
                return Some(out);
            }
            Token::Keyword(_) => return None, // hit a block end
            _ => *i += 1,
        }
    }
    None
}

/// Convenience: consume two consecutive hex strings (a `bfchar` entry).
fn take_hex_pair(tokens: &[Token], i: &mut usize) -> Option<(Vec<u8>, Vec<u8>)> {
    let a = take_hex(tokens, i)?;
    let b = take_hex(tokens, i)?;
    Some((a, b))
}

fn bytes_to_u32(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || bytes.len() > 4 {
        return None;
    }
    let mut v: u32 = 0;
    for b in bytes {
        v = (v << 8) | *b as u32;
    }
    Some(v)
}

fn utf16be_to_string(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    // Some PDF producers emit a single byte as the destination of a bfchar.
    // Treat that as Latin-1.
    if bytes.len() == 1 {
        return Some((bytes[0] as char).to_string());
    }
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

fn add_to_last_u16(bytes: &mut Vec<u8>, offset: u32) {
    if bytes.len() < 2 {
        return;
    }
    let n = bytes.len();
    let last = u16::from_be_bytes([bytes[n - 2], bytes[n - 1]]) as u32 + offset;
    let last = (last & 0xFFFF) as u16;
    let new_bytes = last.to_be_bytes();
    bytes[n - 2] = new_bytes[0];
    bytes[n - 1] = new_bytes[1];
}

#[derive(Debug, Clone)]
enum Token {
    Keyword(String),
    Hex(Vec<u8>),
    ArrayStart,
    ArrayEnd,
    /// Anything else we don't care about (numbers, names, strings).
    Other,
}

fn tokenize(data: &[u8]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' | b'\x0C' => i += 1,
            b'%' => {
                // PostScript line comment.
                while i < data.len() && data[i] != b'\n' && data[i] != b'\r' {
                    i += 1;
                }
            }
            b'<' => {
                i += 1;
                // Skip dict literal `<<`.
                if i < data.len() && data[i] == b'<' {
                    i += 1;
                    out.push(Token::Other);
                    continue;
                }
                let start = i;
                while i < data.len() && data[i] != b'>' {
                    i += 1;
                }
                let hex = &data[start..i];
                if i < data.len() {
                    i += 1; // skip '>'
                }
                if let Some(bytes) = decode_hex(hex) {
                    out.push(Token::Hex(bytes));
                } else {
                    out.push(Token::Other);
                }
            }
            b'>' => {
                i += 1;
                if i < data.len() && data[i] == b'>' {
                    i += 1; // dict literal `>>`
                    out.push(Token::Other);
                }
            }
            b'[' => {
                out.push(Token::ArrayStart);
                i += 1;
            }
            b']' => {
                out.push(Token::ArrayEnd);
                i += 1;
            }
            b'(' => {
                // Literal string: skip with paren balance.
                let mut depth = 1;
                i += 1;
                while i < data.len() && depth > 0 {
                    match data[i] {
                        b'\\' => i += 2,
                        b'(' => {
                            depth += 1;
                            i += 1;
                        }
                        b')' => {
                            depth -= 1;
                            i += 1;
                        }
                        _ => i += 1,
                    }
                }
                out.push(Token::Other);
            }
            b'/' => {
                // Name: `/Foo`.
                i += 1;
                while i < data.len() && !is_delim(data[i]) {
                    i += 1;
                }
                out.push(Token::Other);
            }
            _ => {
                let start = i;
                while i < data.len() && !is_delim(data[i]) {
                    i += 1;
                }
                let word = std::str::from_utf8(&data[start..i])
                    .unwrap_or("")
                    .to_string();
                if is_relevant_keyword(&word) {
                    out.push(Token::Keyword(word));
                } else {
                    out.push(Token::Other);
                }
            }
        }
    }
    out
}

fn is_delim(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0C' | b'<' | b'>' | b'[' | b']' | b'(' | b')' | b'/' | b'%')
}

fn is_relevant_keyword(s: &str) -> bool {
    matches!(
        s,
        "begincodespacerange"
            | "endcodespacerange"
            | "beginbfchar"
            | "endbfchar"
            | "beginbfrange"
            | "endbfrange"
    )
}

fn decode_hex(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut nibble: Option<u8> = None;
    for &b in s {
        if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
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

    #[test]
    fn parses_bfchar() {
        let src = b"\
            2 beginbfchar
            <01> <0041>
            <02> <0042>
            endbfchar
        ";
        let cmap = parse(src);
        assert_eq!(cmap.lookup(0x01), Some("A"));
        assert_eq!(cmap.lookup(0x02), Some("B"));
    }

    #[test]
    fn parses_bfrange_sequential() {
        let src = b"\
            1 beginbfrange
            <10> <12> <0061>
            endbfrange
        ";
        let cmap = parse(src);
        assert_eq!(cmap.lookup(0x10), Some("a"));
        assert_eq!(cmap.lookup(0x11), Some("b"));
        assert_eq!(cmap.lookup(0x12), Some("c"));
    }

    #[test]
    fn parses_bfrange_array() {
        let src = b"\
            1 beginbfrange
            <20> <22> [<0058> <0059> <005A>]
            endbfrange
        ";
        let cmap = parse(src);
        assert_eq!(cmap.lookup(0x20), Some("X"));
        assert_eq!(cmap.lookup(0x21), Some("Y"));
        assert_eq!(cmap.lookup(0x22), Some("Z"));
    }

    #[test]
    fn picks_up_codespace_width() {
        let src = b"\
            1 begincodespacerange
            <0000> <FFFF>
            endcodespacerange
        ";
        let cmap = parse(src);
        assert_eq!(cmap.code_width, 2);
    }

    #[test]
    fn ligature_mapping() {
        // bfchar value with multiple UTF-16 units (the 'fi' ligature).
        let src = b"\
            1 beginbfchar
            <01> <00660069>
            endbfchar
        ";
        let cmap = parse(src);
        assert_eq!(cmap.lookup(0x01), Some("fi"));
    }
}
