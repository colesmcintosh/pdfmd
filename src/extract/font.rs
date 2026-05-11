//! Per-font byte → text decoder.
//!
//! Combines a base encoding, a /Differences override, and an optional
//! /ToUnicode CMap into a single `decode` entry point.

use std::collections::HashMap;

use lopdf::{Document, Object, ObjectId};

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
        let Ok(font_dict) = doc.get_object(obj_id).and_then(Object::as_dict) else {
            return Self::default();
        };

        let mut font = PdfFont::default();

        let subtype = font_dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name_str().ok())
            .unwrap_or("");
        if subtype == "Type0" {
            font.kind = FontKind::Composite;
        }

        // /ToUnicode is an indirect stream.
        if let Some(stream) = follow_stream(doc, font_dict.get(b"ToUnicode").ok()) {
            font.to_unicode = Some(cmap::parse(&stream));
        }

        match font_dict.get(b"Encoding").ok() {
            Some(Object::Name(name)) => {
                font.encoding = BaseEncoding::from_name(std::str::from_utf8(name).unwrap_or(""));
            }
            Some(obj) => {
                let resolved = resolve(doc, obj);
                if let Some(dict) = resolved.as_dict().ok() {
                    if let Ok(Object::Name(base)) = dict.get(b"BaseEncoding") {
                        font.encoding =
                            BaseEncoding::from_name(std::str::from_utf8(base).unwrap_or(""));
                    }
                    if let Ok(Object::Array(arr)) = dict.get(b"Differences") {
                        font.differences = parse_differences(arr);
                    }
                }
            }
            None => {}
        }

        font
    }

    pub fn decode(&self, bytes: &[u8]) -> String {
        let width = match self.kind {
            FontKind::Composite => self.to_unicode.as_ref().map_or(2, CMap::code_width),
            FontKind::Simple => self.to_unicode.as_ref().map_or(1, CMap::code_width),
        };
        let mut out = String::with_capacity(bytes.len());
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
        out
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
    let obj = obj?;
    let resolved = resolve(doc, obj);
    match resolved {
        Object::Stream(stream) => {
            let mut stream = stream.clone();
            stream.decompress();
            Some(stream.content)
        }
        _ => None,
    }
}

fn resolve<'a>(doc: &'a Document, obj: &'a Object) -> Object {
    match obj {
        Object::Reference(id) => doc.get_object(*id).cloned().unwrap_or(Object::Null),
        other => other.clone(),
    }
}
