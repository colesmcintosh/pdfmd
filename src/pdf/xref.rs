use std::collections::{BTreeMap, HashMap};

use super::filter::decode_filters;
use super::object::{Dictionary, Object, ObjectId};
use super::parser::Parser;
use super::PdfError;

/// Where an object actually lives. Classic xref entries are `Uncompressed`;
/// PDF 1.5+ object streams produce `Compressed` entries instead.
#[derive(Debug, Clone, Copy)]
pub(super) enum XrefEntry {
    Free,
    Uncompressed { offset: u64 },
    Compressed { stream_obj: u32, index: u32 },
}

pub(super) fn find_startxref(bytes: &[u8]) -> Result<u64, PdfError> {
    // Spec: `startxref` then offset then `%%EOF`, within the last 1024 bytes.
    let tail_start = bytes.len().saturating_sub(2048);
    let tail = &bytes[tail_start..];
    let needle = b"startxref";
    let pos = (0..tail.len().saturating_sub(needle.len()))
        .rev()
        .find(|&i| &tail[i..i + needle.len()] == needle)
        .ok_or_else(|| PdfError::BadXref("missing startxref".into()))?;
    let mut i = pos + needle.len();
    while i < tail.len() && tail[i].is_ascii_whitespace() {
        i += 1;
    }
    let n_start = i;
    while i < tail.len() && tail[i].is_ascii_digit() {
        i += 1;
    }
    // The slice is digit-only by construction, so utf-8 always holds. Parse
    // failures only happen on integer overflow — a ~10^19 offset PDF.
    let s = std::str::from_utf8(&tail[n_start..i]).expect("digit slice is utf-8");
    s.parse::<u64>()
        .map_err(|_| PdfError::BadXref("startxref not numeric".into()))
}

pub(super) fn read_xref_chain(
    bytes: &[u8],
    startxref: u64,
) -> Result<(BTreeMap<ObjectId, XrefEntry>, Dictionary), PdfError> {
    let mut entries: BTreeMap<ObjectId, XrefEntry> = BTreeMap::new();
    let mut final_trailer: Option<Dictionary> = None;
    let mut visited: HashMap<u64, ()> = HashMap::new();
    let mut current = startxref;
    loop {
        if visited.contains_key(&current) {
            break;
        }
        visited.insert(current, ());

        let trailer = if at_keyword(bytes, current as usize, b"xref") {
            read_classic_xref(bytes, current as usize, &mut entries)?
        } else {
            read_xref_stream(bytes, current as usize, &mut entries)?
        };

        // Earliest occurrence wins for incremental updates: the entry from
        // the most-recent xref is already in the map by the time we follow
        // /Prev, so we just don't overwrite it.
        let prev = trailer
            .get(b"Prev")
            .and_then(Object::as_integer)
            .filter(|n| *n > 0);

        if final_trailer.is_none() {
            final_trailer = Some(trailer);
        }
        match prev {
            Some(p) => current = p as u64,
            None => break,
        }
    }
    // The loop always runs at least once (visited is empty on entry) and
    // sets final_trailer before any subsequent iteration. If the first
    // xref read errors we've already returned via `?`.
    Ok((
        entries,
        final_trailer.expect("first iteration sets trailer"),
    ))
}

fn at_keyword(bytes: &[u8], at: usize, kw: &[u8]) -> bool {
    bytes.get(at..at + kw.len()).is_some_and(|w| w == kw)
}

fn read_classic_xref(
    bytes: &[u8],
    at: usize,
    out: &mut BTreeMap<ObjectId, XrefEntry>,
) -> Result<Dictionary, PdfError> {
    let mut p = Parser::with_pos(bytes, at + b"xref".len());
    p.skip_ws_and_comments();
    loop {
        // Section header: `first count`. The `trailer` keyword ends it.
        let pos = p.pos;
        if at_keyword(bytes, pos, b"trailer") {
            p.pos += b"trailer".len();
            break;
        }
        let first = read_uint(bytes, &mut p.pos)?;
        skip_inline(bytes, &mut p.pos);
        let count = read_uint(bytes, &mut p.pos)?;
        skip_eol(bytes, &mut p.pos);
        // Each entry occupies exactly 20 bytes. A malformed file may
        // declare `count` close to u32::MAX; the per-entry truncation
        // check below would still catch it, but only after iterating
        // billions of times. Bound by the remaining bytes up front, and
        // refuse subsection bases that would overflow when added to count.
        let bytes_remaining = (bytes.len() - p.pos) as u64;
        if (count as u64) * 20 > bytes_remaining {
            return Err(PdfError::BadXref(format!(
                "xref subsection count {count} exceeds remaining input"
            )));
        }
        if first.checked_add(count).is_none() {
            return Err(PdfError::BadXref(
                "xref subsection first+count overflows u32".into(),
            ));
        }
        for i in 0..count {
            let row = &bytes[p.pos..p.pos + 20];
            p.pos += 20;
            // Spec mandates 10 ASCII digits + space + 5 ASCII digits, both
            // always valid utf-8 — non-utf8 indicates a malformed PDF that
            // we'd reject elsewhere too.
            let offset_s = std::str::from_utf8(&row[0..10]).expect("ascii digits");
            let gen_s = std::str::from_utf8(&row[11..16]).expect("ascii digits");
            let kind = row[17];
            let n = first + i;
            let g: u16 = gen_s.trim().parse().unwrap_or(0);
            let id = ObjectId(n, g);
            if out.contains_key(&id) {
                continue;
            }
            match kind {
                b'n' => {
                    let offset: u64 = offset_s.trim().parse().unwrap_or(0);
                    out.insert(id, XrefEntry::Uncompressed { offset });
                }
                b'f' => {
                    out.insert(id, XrefEntry::Free);
                }
                _ => {}
            }
        }
        p.skip_ws_and_comments();
    }
    p.skip_ws_and_comments();
    let trailer = p.parse_object()?;
    match trailer {
        Object::Dictionary(d) => Ok(d),
        _ => Err(PdfError::BadXref("trailer is not a dictionary".into())),
    }
}

pub(super) fn read_xref_stream(
    bytes: &[u8],
    at: usize,
    out: &mut BTreeMap<ObjectId, XrefEntry>,
) -> Result<Dictionary, PdfError> {
    // The cross-reference stream object lives at `at`; its first line is
    // `N G obj`, same as any indirect object.
    let mut p = Parser::with_pos(bytes, at);
    let (_, obj) = p.parse_indirect_object()?;
    let Object::Stream(stream) = obj else {
        return Err(PdfError::BadXref("xref stream wasn't a stream".into()));
    };
    let dict = stream.dict.clone();
    let payload = decode_filters(&stream, bytes)?;

    let widths: Vec<usize> = dict
        .get(b"W")
        .and_then(Object::as_array)
        .ok_or_else(|| PdfError::BadXref("xref stream missing /W".into()))?
        .iter()
        .map(|o| o.as_integer().unwrap_or(0).max(0) as usize)
        .collect();
    if widths.len() != 3 {
        return Err(PdfError::BadXref(format!(
            "xref stream /W must have 3 entries, got {}",
            widths.len()
        )));
    }
    let row = widths.iter().sum::<usize>();
    if row == 0 {
        return Err(PdfError::BadXref("xref stream row width is zero".into()));
    }

    let size =
        dict.get(b"Size")
            .and_then(Object::as_integer)
            .ok_or_else(|| PdfError::BadXref("xref stream missing /Size".into()))? as u32;
    let index: Vec<u32> = match dict.get(b"Index").and_then(Object::as_array) {
        Some(arr) => arr
            .iter()
            .map(|o| o.as_integer().unwrap_or(0).max(0) as u32)
            .collect(),
        None => vec![0, size],
    };

    let mut cursor = 0usize;
    for chunk in index.chunks(2) {
        if chunk.len() < 2 {
            break;
        }
        let first = chunk[0];
        let count = chunk[1];
        for i in 0..count {
            if cursor + row > payload.len() {
                return Err(PdfError::BadXref("xref stream truncated".into()));
            }
            let row_bytes = &payload[cursor..cursor + row];
            cursor += row;
            let t = if widths[0] == 0 {
                1 // default per spec
            } else {
                be_uint(&row_bytes[..widths[0]])
            };
            let f1 = be_uint(&row_bytes[widths[0]..widths[0] + widths[1]]);
            let f2 = be_uint(&row_bytes[widths[0] + widths[1]..]);
            // Reject Index entries whose `first + count` would wrap u32 —
            // the wrapped object id would silently alias a different
            // object on read.
            let Some(n) = first.checked_add(i) else {
                return Err(PdfError::BadXref(
                    "xref stream /Index range overflows u32".into(),
                ));
            };
            let id = ObjectId(n, 0);
            if out.contains_key(&id) {
                continue;
            }
            match t {
                0 => {
                    out.insert(id, XrefEntry::Free);
                }
                1 => {
                    out.insert(id, XrefEntry::Uncompressed { offset: f1 });
                }
                2 => {
                    out.insert(
                        id,
                        XrefEntry::Compressed {
                            stream_obj: f1 as u32,
                            index: f2 as u32,
                        },
                    );
                }
                _ => {}
            }
        }
    }
    Ok(dict)
}

pub(super) fn be_uint(bytes: &[u8]) -> u64 {
    let mut v: u64 = 0;
    for &b in bytes {
        v = (v << 8) | b as u64;
    }
    v
}

pub(super) fn read_uint(bytes: &[u8], pos: &mut usize) -> Result<u32, PdfError> {
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t' | b'\r' | b'\n') {
        *pos += 1;
    }
    let start = *pos;
    while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
        *pos += 1;
    }
    if *pos == start {
        return Err(PdfError::BadXref(format!("expected integer at {start}")));
    }
    // The slice is ASCII digits by construction — utf-8 always holds.
    let s = std::str::from_utf8(&bytes[start..*pos]).expect("digit slice is utf-8");
    s.parse::<u32>()
        .map_err(|_| PdfError::BadXref(format!("integer overflow: {s}")))
}

pub(super) fn skip_inline(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t') {
        *pos += 1;
    }
}

pub(super) fn skip_eol(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t') {
        *pos += 1;
    }
    match bytes.get(*pos) {
        Some(&b'\r') => {
            *pos += 1;
            if bytes.get(*pos) == Some(&b'\n') {
                *pos += 1;
            }
        }
        Some(&b'\n') => *pos += 1,
        _ => {}
    }
}
