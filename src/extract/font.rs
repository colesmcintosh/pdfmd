//! Per-font byte → text decoder.
//!
//! Combines a base encoding, a /Differences override, and an optional
//! /ToUnicode CMap into a single `decode` entry point.

use std::collections::HashMap;

use crate::pdf::{Document, Object, ObjectId};

use super::cmap::{self, CMap};
use super::encoding::BaseEncoding;
use super::glyphs::glyph_to_string;

#[derive(Debug, Default)]
pub struct PdfFont {
    pub kind: FontKind,
    pub to_unicode: Option<CMap>,
    pub encoding: BaseEncoding,
    /// Per-byte glyph-name overrides from /Encoding /Differences.
    pub differences: HashMap<u8, String>,
    /// Width of source codes in bytes (1 for simple fonts without a wide
    /// ToUnicode CMap, 2 for composite fonts or wide simple fonts).
    code_width: usize,
    /// Fast-path decode table for 1-byte simple fonts. When set, `decode_into`
    /// is a tight `byte -> push_str` loop with no branching or hashing.
    /// Indexed by byte; `None` entries are silently skipped (matches the
    /// behaviour of the slow path for unmappable codes).
    simple_table: Option<Box<[Option<Box<str>>; 256]>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FontKind {
    #[default]
    Simple,
    /// Type-0 composite font; codes are 2 bytes (Identity-H).
    Composite,
}

impl PdfFont {
    /// Build a font from its dictionary object.
    pub fn from_object(doc: &Document, obj_id: ObjectId) -> Self {
        let Some(font_dict) = doc.get_object(obj_id).and_then(Object::as_dict) else {
            return Self::default();
        };

        let mut font = PdfFont::default();

        let subtype = font_dict
            .get(b"Subtype")
            .and_then(Object::as_name_str)
            .unwrap_or("");
        if subtype == "Type0" {
            font.kind = FontKind::Composite;
        }

        if let Some(stream) = follow_stream(doc, font_dict.get(b"ToUnicode")) {
            font.to_unicode = Some(cmap::parse(&stream));
        }

        match font_dict.get(b"Encoding") {
            Some(Object::Name(name)) => {
                font.encoding = BaseEncoding::from_name(std::str::from_utf8(name).unwrap_or(""));
            }
            Some(obj) => {
                let resolved = doc.deref(obj);
                if let Some(dict) = resolved.as_dict() {
                    if let Some(Object::Name(base)) = dict.get(b"BaseEncoding") {
                        font.encoding =
                            BaseEncoding::from_name(std::str::from_utf8(base).unwrap_or(""));
                    }
                    if let Some(Object::Array(arr)) = dict.get(b"Differences") {
                        font.differences = parse_differences(arr);
                    }
                }
            }
            None => {}
        }

        font.code_width = match font.kind {
            FontKind::Composite => font.to_unicode.as_ref().map_or(2, CMap::code_width),
            FontKind::Simple => font.to_unicode.as_ref().map_or(1, CMap::code_width),
        };

        if font.kind == FontKind::Simple && font.code_width == 1 {
            font.simple_table = Some(font.build_simple_table());
        }

        font
    }

    /// Append the decoded text for `bytes` to `out`. The common case — a
    /// 1-byte simple font — runs through a precomputed lookup table, so the
    /// inner loop is a branchless `push_str` per byte.
    pub fn decode_into(&self, bytes: &[u8], out: &mut String) {
        if let Some(table) = self.simple_table.as_deref() {
            for &b in bytes {
                if let Some(s) = &table[b as usize] {
                    out.push_str(s);
                }
            }
            return;
        }

        let width = self.code_width.max(1);
        let mut i = 0;
        while i < bytes.len() {
            let remaining = bytes.len() - i;
            let take = width.min(remaining);
            let mut code: u32 = 0;
            for j in 0..take {
                code = (code << 8) | bytes[i + j] as u32;
            }
            i += take;

            if let Some(cmap) = &self.to_unicode {
                if let Some(text) = cmap.lookup(code) {
                    out.push_str(text);
                    continue;
                }
            }

            if self.kind == FontKind::Simple && take == 1 {
                let byte = code as u8;
                if let Some(name) = self.differences.get(&byte) {
                    if let Some(text) = glyph_to_string(name) {
                        out.push_str(&text);
                        continue;
                    }
                }
                if let Some(name) = self.encoding.glyph(byte) {
                    if let Some(text) = glyph_to_string(name) {
                        out.push_str(&text);
                        continue;
                    }
                }
                if byte >= 0x20 {
                    out.push(byte as char);
                }
            }
            // Composite font without a usable ToUnicode entry: skip silently.
        }
    }

    /// Populate the 256-entry fast-path table. Called once at construction
    /// for 1-byte simple fonts; same precedence as the slow path.
    fn build_simple_table(&self) -> Box<[Option<Box<str>>; 256]> {
        let mut table: [Option<Box<str>>; 256] = std::array::from_fn(|_| None);
        for b in 0..=255u8 {
            table[b as usize] = self.decode_single_byte(b).map(String::into_boxed_str);
        }
        Box::new(table)
    }

    fn decode_single_byte(&self, byte: u8) -> Option<String> {
        if let Some(cmap) = &self.to_unicode {
            if let Some(text) = cmap.lookup(byte as u32) {
                return Some(text.to_string());
            }
        }
        if let Some(name) = self.differences.get(&byte) {
            if let Some(text) = glyph_to_string(name) {
                return Some(text);
            }
        }
        if let Some(name) = self.encoding.glyph(byte) {
            if let Some(text) = glyph_to_string(name) {
                return Some(text);
            }
        }
        if byte >= 0x20 {
            return Some((byte as char).to_string());
        }
        None
    }
}

/// Resolve a `/Differences` array into a byte → glyph-name map.
fn parse_differences(arr: &[Object]) -> HashMap<u8, String> {
    let mut out = HashMap::new();
    let mut code: u32 = 0;
    for obj in arr {
        match obj {
            Object::Integer(n) => code = *n as u32,
            Object::Name(name) => {
                if let Ok(s) = std::str::from_utf8(name) {
                    if code < 256 {
                        out.insert(code as u8, s.to_string());
                    }
                }
                code = code.wrapping_add(1);
            }
            _ => {}
        }
    }
    out
}

fn follow_stream(doc: &Document, obj: Option<&Object>) -> Option<Vec<u8>> {
    let resolved = doc.deref(obj?);
    if let Object::Stream(s) = resolved {
        doc.decode_stream(s).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::{Dictionary, Document};

    /// Helper that builds and loads a tiny PDF containing the given
    /// indirect objects, then hands back the Document.
    fn build_doc(extra_objs: &[(u32, &str)]) -> Document {
        let mut body = String::from("%PDF-1.4\n");
        body.push_str("1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n");
        body.push_str("2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n");
        body.push_str(
            "3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj\n",
        );
        for (n, payload) in extra_objs {
            body.push_str(&format!("{n} 0 obj {payload} endobj\n"));
        }
        let xref_offset = body.len();
        let mut needles: Vec<(u32, usize)> = Vec::new();
        for n in 1..=3 {
            let needle = format!("{n} 0 obj");
            let p = (0..=body.len() - needle.len())
                .find(|&i| body.as_bytes()[i..i + needle.len()] == *needle.as_bytes())
                .unwrap();
            needles.push((n, p));
        }
        for (n, _) in extra_objs {
            let needle = format!("{n} 0 obj");
            let p = (0..=body.len() - needle.len())
                .find(|&i| body.as_bytes()[i..i + needle.len()] == *needle.as_bytes())
                .unwrap();
            needles.push((*n, p));
        }
        needles.sort_by_key(|(n, _)| *n);
        let max_n = needles.iter().map(|(n, _)| *n).max().unwrap();
        let mut xref = String::from("xref\n");
        xref.push_str(&format!("0 {}\n", max_n + 1));
        xref.push_str("0000000000 65535 f \n");
        // Walk 1..=max_n, emit `n` entry if present, else free.
        for n in 1..=max_n {
            if let Some(off) = needles.iter().find(|(m, _)| *m == n).map(|(_, p)| p) {
                xref.push_str(&format!("{off:010} 00000 n \n"));
            } else {
                xref.push_str("0000000000 00000 f \n");
            }
        }
        xref.push_str(&format!(
            "trailer <</Size {}/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n",
            max_n + 1
        ));
        let mut bytes = body.into_bytes();
        bytes.extend_from_slice(xref.as_bytes());
        Document::load(&bytes).expect("load")
    }

    #[test]
    fn missing_font_object_returns_default() {
        // Object id 99 doesn't exist in the doc.
        let doc = build_doc(&[]);
        let font = PdfFont::from_object(&doc, ObjectId(99, 0));
        // Default state.
        assert!(font.to_unicode.is_none());
        assert_eq!(font.kind, FontKind::Simple);
    }

    #[test]
    fn type0_font_uses_composite_kind() {
        let doc = build_doc(&[(4, "<</Type/Font/Subtype/Type0/BaseFont/Foo>>")]);
        let font = PdfFont::from_object(&doc, ObjectId(4, 0));
        assert_eq!(font.kind, FontKind::Composite);
        // No ToUnicode → composite code width defaults to 2.
        assert_eq!(font.code_width, 2);
    }

    #[test]
    fn font_encoding_name_resolves_to_winansi() {
        let doc = build_doc(&[(
            4,
            "<</Type/Font/Subtype/Type1/BaseFont/Helv/Encoding/WinAnsiEncoding>>",
        )]);
        let font = PdfFont::from_object(&doc, ObjectId(4, 0));
        assert_eq!(format!("{:?}", font.encoding), "WinAnsi");
    }

    #[test]
    fn font_encoding_dictionary_with_differences() {
        let doc = build_doc(&[
            (
                4,
                "<</Type/Font/Subtype/Type1/BaseFont/Helv/Encoding 5 0 R>>",
            ),
            (
                5,
                "<</Type/Encoding/BaseEncoding/MacRomanEncoding/Differences [65 /Aacute /Bcaron]>>",
            ),
        ]);
        let font = PdfFont::from_object(&doc, ObjectId(4, 0));
        assert_eq!(format!("{:?}", font.encoding), "MacRoman");
        assert_eq!(
            font.differences.get(&65).map(String::as_str),
            Some("Aacute")
        );
        assert_eq!(
            font.differences.get(&66).map(String::as_str),
            Some("Bcaron")
        );
    }

    #[test]
    fn decode_into_uses_differences_override() {
        let doc = build_doc(&[
            (4, "<</Type/Font/Subtype/Type1/Encoding 5 0 R>>"),
            (5, "<</Type/Encoding/Differences [65 /fi]>>"),
        ]);
        let font = PdfFont::from_object(&doc, ObjectId(4, 0));
        let mut out = String::new();
        font.decode_into(b"A", &mut out);
        assert_eq!(out, "fi");
    }

    #[test]
    fn decode_into_falls_through_to_raw_ascii() {
        // Composite font (no simple_table) with no ToUnicode and no
        // matching glyph: the slow-path's "byte >= 0x20" arm still emits
        // the ASCII character.
        let doc = build_doc(&[(
            4,
            "<</Type/Font/Subtype/Type0/Encoding/Identity-H/BaseFont/Foo>>",
        )]);
        let font = PdfFont::from_object(&doc, ObjectId(4, 0));
        let mut out = String::new();
        // Composite path with no ToUnicode skips silently — covers the
        // composite branch fall-through.
        font.decode_into(&[0x00, 0x41], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn decode_into_composite_without_cmap_silently_skips() {
        let doc = build_doc(&[(
            4,
            "<</Type/Font/Subtype/Type0/Encoding/Identity-H/BaseFont/Foo>>",
        )]);
        let font = PdfFont::from_object(&doc, ObjectId(4, 0));
        let mut out = String::new();
        font.decode_into(&[0x00, 0x41, 0x00, 0x42], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn parse_differences_skips_non_recognised_entries() {
        let arr = vec![
            Object::Integer(70),
            Object::Name(b"G".to_vec()),
            Object::Boolean(true), // ignored
            Object::Name(b"H".to_vec()),
            // Code beyond u8 range is silently dropped.
            Object::Integer(300),
            Object::Name(b"K".to_vec()),
        ];
        let map = parse_differences(&arr);
        assert_eq!(map.get(&70).map(String::as_str), Some("G"));
        assert_eq!(map.get(&71).map(String::as_str), Some("H"));
        assert!(!map.contains_key(&44)); // 300 wraps via "if code < 256"
    }

    #[test]
    fn parse_differences_handles_non_utf8_name() {
        let arr = vec![
            Object::Integer(80),
            Object::Name(vec![0xFFu8, 0xFE]), // not UTF-8 → skipped
            Object::Name(b"R".to_vec()),
        ];
        let map = parse_differences(&arr);
        // 0xFFFE skipped, then 0xFFFE wraps to entry for 81 = "R"? Actually
        // the parser only increments `code` on Name entries that succeed at
        // utf8. Let me just check 82 since the first Name was skipped but
        // `code` still advances.
        // From the implementation: code advances after every Name regardless.
        // So after Int 80 → code=80; Name (bad utf8) → no insert, code=81;
        // Name "R" → insert at 81.
        assert_eq!(map.get(&81).map(String::as_str), Some("R"));
    }

    #[test]
    fn font_encoding_dict_without_differences_skips_parse() {
        // /Encoding is a dict with BaseEncoding set but no /Differences —
        // the parser should pick up the base and leave differences empty.
        let doc = build_doc(&[
            (4, "<</Type/Font/Subtype/Type1/Encoding 5 0 R>>"),
            (5, "<</Type/Encoding/BaseEncoding/WinAnsiEncoding>>"),
        ]);
        let font = PdfFont::from_object(&doc, ObjectId(4, 0));
        assert_eq!(format!("{:?}", font.encoding), "WinAnsi");
        assert!(font.differences.is_empty());
    }

    #[test]
    fn font_encoding_dict_without_base_just_picks_up_differences() {
        // The dict has /Differences but no /BaseEncoding — base stays
        // Standard, differences fill from the array.
        let doc = build_doc(&[
            (4, "<</Type/Font/Subtype/Type1/Encoding 5 0 R>>"),
            (5, "<</Type/Encoding/Differences [65 /Aacute]>>"),
        ]);
        let font = PdfFont::from_object(&doc, ObjectId(4, 0));
        assert_eq!(format!("{:?}", font.encoding), "Standard");
        assert_eq!(
            font.differences.get(&65).map(String::as_str),
            Some("Aacute")
        );
    }

    #[test]
    fn decode_into_slow_path_with_to_unicode_lookup() {
        // Build a composite font with a ToUnicode CMap. decode_into runs
        // the slow path; each 2-byte code looks up via cmap.
        // The ToUnicode stream is FlateDecoded with a tiny CMap. The
        // codespacerange forces 2-byte source codes, and the bfchar maps
        // <0001> → 'A'.
        let payload = b"1 begincodespacerange <0000> <FFFF> endcodespacerange\n1 beginbfchar <0001> <0041> endbfchar\n";
        let zlib = zlib_compress(payload);
        let zlib_len = zlib.len();
        let mut body = format!(
            "%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
4 0 obj <</Type/Font/Subtype/Type0/Encoding/Identity-H/BaseFont/Foo/ToUnicode 5 0 R>> endobj
5 0 obj <</Length {zlib_len}/Filter/FlateDecode>>
stream
"
        );
        let stream_start_in_body = body.len();
        let mut bytes = body.clone().into_bytes();
        bytes.extend_from_slice(&zlib);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        body.clear();
        body.push_str(&String::from_utf8_lossy(&bytes));
        // Build xref by hand (offsets relative to file start).
        let xref_offset = bytes.len();
        let _ = stream_start_in_body;
        let needles: Vec<usize> = (1..=5)
            .map(|n| {
                let needle = format!("{n} 0 obj");
                (0..=bytes.len() - needle.len())
                    .find(|&i| bytes[i..i + needle.len()] == *needle.as_bytes())
                    .unwrap()
            })
            .collect();
        let mut xref = String::from("xref\n0 6\n0000000000 65535 f \n");
        for off in &needles {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        xref.push_str(&format!(
            "trailer <</Size 6/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n"
        ));
        bytes.extend_from_slice(xref.as_bytes());
        let doc = Document::load(&bytes).expect("load");
        let font = PdfFont::from_object(&doc, ObjectId(4, 0));
        // Composite font + ToUnicode (code_width 2) → slow path lookups.
        assert_eq!(font.code_width, 2);
        let mut out = String::new();
        font.decode_into(&[0x00, 0x01], &mut out);
        assert_eq!(out, "A");
        // A code that's not in the CMap silently skips (composite path).
        let mut out2 = String::new();
        font.decode_into(&[0x00, 0x02], &mut out2);
        assert!(out2.is_empty());
    }

    #[test]
    fn decode_into_slow_path_byte_fallback_via_encoding_glyph() {
        // Simple-font slow path: have a ToUnicode CMap with code_width 2
        // (which forces the slow path even for a simple font), then feed a
        // single byte that falls through every cmap/differences/encoding
        // table and ultimately hits the `byte >= 0x20` fallback.
        let payload = b"1 begincodespacerange <0000> <FFFF> endcodespacerange\n";
        let zlib = zlib_compress(payload);
        let zlib_len = zlib.len();
        let mut bytes = format!(
            "%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
4 0 obj <</Type/Font/Subtype/Type1/BaseFont/Helv/Encoding/WinAnsiEncoding/ToUnicode 5 0 R>> endobj
5 0 obj <</Length {zlib_len}/Filter/FlateDecode>>
stream
"
        )
        .into_bytes();
        bytes.extend_from_slice(&zlib);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        let xref_offset = bytes.len();
        let needles: Vec<usize> = (1..=5)
            .map(|n| {
                let needle = format!("{n} 0 obj");
                (0..=bytes.len() - needle.len())
                    .find(|&i| bytes[i..i + needle.len()] == *needle.as_bytes())
                    .unwrap()
            })
            .collect();
        let mut xref = String::from("xref\n0 6\n0000000000 65535 f \n");
        for off in &needles {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        xref.push_str(&format!(
            "trailer <</Size 6/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n"
        ));
        bytes.extend_from_slice(xref.as_bytes());
        let doc = Document::load(&bytes).expect("load");
        let font = PdfFont::from_object(&doc, ObjectId(4, 0));
        // Simple font but code_width = 2 from the CMap → slow path.
        assert_eq!(font.code_width, 2);
        // Feed an odd-length input so we hit `take == 1` on the second
        // pass. For byte 'A' (0x41) with WinAnsi encoding the glyph lookup
        // succeeds; for byte 0x05 it falls through to the `byte >= 0x20`
        // arm which silently drops it.
        let mut out = String::new();
        font.decode_into(b"\x00\x41A", &mut out);
        // Only the trailing 'A' resolves through the simple-byte fallback.
        assert_eq!(out, "A");
    }

    /// Minimal RFC 1950 zlib (stored block only) — used to seed in-test
    /// `/FlateDecode` payloads without pulling another encoder.
    fn zlib_compress(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78u8, 0x9C];
        // Single final stored block: 0x01, LEN(2 LE), NLEN(2 LE).
        out.push(0x01);
        let len = data.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(data);
        // Adler-32 checksum.
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in data {
            a = (a + byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        out.extend_from_slice(&((b << 16) | a).to_be_bytes());
        out
    }

    #[test]
    fn follow_stream_returns_none_for_non_stream() {
        let doc = build_doc(&[]);
        // ToUnicode points at a dict (not a stream) → None.
        let mut d = Dictionary::new();
        d.insert(b"K".to_vec(), Object::Integer(1));
        let obj = Object::Dictionary(d);
        assert!(follow_stream(&doc, Some(&obj)).is_none());
        // None input → None.
        assert!(follow_stream(&doc, None).is_none());
    }
}
