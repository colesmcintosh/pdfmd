use std::collections::{BTreeMap, HashMap};

use super::filter::decode_filters;
use super::object::{Dictionary, Object, ObjectId};
use super::parser::Parser;
use super::syntax::{skip_line_break, skip_spaces};
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
        skip_spaces(bytes, &mut p.pos);
        let count = read_uint(bytes, &mut p.pos)?;
        skip_spaces(bytes, &mut p.pos);
        skip_line_break(bytes, &mut p.pos);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::test_pdf::{build_xref_pdf_with, page_tree_objects};
    use crate::pdf::Document;

    /// A 1-object PDF body whose only indirect object is an xref stream with
    /// the given `(type, field1, field2)` rows. There is no `/Filter`, so the
    /// rows are written raw into the stream payload.
    fn xref_stream_pdf(entries: &[(u8, u64, u32)], extra_dict_entries: &str) -> Vec<u8> {
        let mut payload: Vec<u8> = Vec::new();
        for (kind, f1, f2) in entries {
            payload.push(*kind);
            payload.extend_from_slice(&(*f1 as u16).to_be_bytes());
            payload.push(*f2 as u8);
        }
        let mut bytes = format!(
            "1 0 obj <</Type/XRef/Size {}/W [1 2 1]/Length {}{}>>\nstream\n",
            entries.len(),
            payload.len(),
            extra_dict_entries,
        )
        .into_bytes();
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\nendstream endobj\n");
        bytes
    }

    fn classic_pdf(extra_rows: &[&str], trailer: &str) -> Vec<u8> {
        let body = format!("%PDF-1.4\n{}", page_tree_objects(""));
        build_xref_pdf_with(body.as_bytes(), extra_rows, trailer)
    }

    // ---- startxref ------------------------------------------------------

    #[test]
    fn find_startxref_locates_offset_or_errors() {
        assert_eq!(
            find_startxref(b"trash\nstartxref\n1234\n%%EOF").unwrap(),
            1234
        );
        assert!(find_startxref(b"no marker at all").is_err());
        assert!(find_startxref(b"startxref\n\n%%EOF").is_err());
    }

    // ---- Byte helpers ---------------------------------------------------

    #[test]
    fn be_uint_collapses_byte_run() {
        assert_eq!(be_uint(&[0x12, 0x34, 0x56]), 0x123456);
        assert_eq!(be_uint(&[]), 0);
    }

    #[test]
    fn read_uint_handles_leading_whitespace_and_errors() {
        let mut pos = 0;
        assert_eq!(read_uint(b"  \t  42 next", &mut pos).unwrap(), 42);
        // No digits: error.
        let mut pos = 0;
        assert!(read_uint(b"   ", &mut pos).is_err());
        // Overflows u32.
        let mut pos = 0;
        assert!(read_uint(b"9999999999999", &mut pos).is_err());
    }

    // ---- Classic xref ---------------------------------------------------

    #[test]
    fn classic_xref_with_unknown_entry_kind_is_skipped() {
        // The extra entry uses kind 'x' instead of 'n' or 'f' — the loader
        // should silently skip it without erroring.
        let bytes = classic_pdf(&["0000099999 00000 x "], "<</Size {size}/Root 1 0 R>>");
        assert_eq!(Document::load(&bytes).expect("load").pages().len(), 1);
    }

    #[test]
    fn classic_xref_skips_already_known_entries_on_prev_chain() {
        // /Prev points back at the same table, so every entry in the second
        // pass is already known and hits the `continue`.
        let bytes = classic_pdf(&[], "<</Size {size}/Root 1 0 R/Prev {xref}>>");
        assert_eq!(Document::load(&bytes).expect("load").pages().len(), 1);
    }

    #[test]
    fn classic_xref_errors_on_malformed_headers_and_trailers() {
        for tail in [
            "xref\n0 BAD\ntrailer <</Size 0/Root 1 0 R>>\nstartxref\n9\n%%EOF\n",
            "xref\nBAD 1\ntrailer <</Size 0/Root 1 0 R>>\nstartxref\n9\n%%EOF\n",
            "xref\n0 1\n0000000000 65535 f \ntrailer @@@\nstartxref\n9\n%%EOF\n",
            "xref\n0 1\n0000000000 65535 f \ntrailer 42\nstartxref\n9\n%%EOF\n",
            // Entry rows cut short of the mandated 20 bytes each.
            "xref\n0 5\n0000000000 65535 f \n0000000010 00000 n\nstartxref\n9\n%%EOF\n",
            // first + count overflows u32.
            "xref\n4294967295 1\n0000000000 65535 f \ntrailer <</Size 1>>\nstartxref\n9\n%%EOF\n",
        ] {
            let bytes = format!("%PDF-1.4\n{tail}");
            assert!(Document::load(bytes.as_bytes()).is_err(), "{tail}");
        }
    }

    #[test]
    fn load_errors_on_pdf_with_no_root() {
        let bytes = classic_pdf(&[], "<</Size {size}>>");
        assert!(Document::load(&bytes).is_err());
    }

    // ---- Xref streams ---------------------------------------------------

    #[test]
    fn read_xref_stream_recognises_every_entry_kind() {
        let entries = &[
            (0u8, 0u64, 0u32),   // free
            (1u8, 100u64, 0u32), // uncompressed at offset 100
            (2u8, 99u64, 3u32),  // compressed: lives in objstm 99, idx 3
            (5u8, 0u64, 0u32),   // unknown kind — silently skipped
        ];
        let bytes = xref_stream_pdf(entries, "");
        let mut out = BTreeMap::new();
        let dict = read_xref_stream(&bytes, 0, &mut out).unwrap();
        assert!(dict.get(b"Type").is_some());
        // Debug-format comparisons sidestep the dead arms a `matches!`
        // expansion would introduce.
        assert_eq!(format!("{:?}", out[&ObjectId(0, 0)]), "Free");
        assert_eq!(
            format!("{:?}", out[&ObjectId(1, 0)]),
            "Uncompressed { offset: 100 }",
        );
        assert_eq!(
            format!("{:?}", out[&ObjectId(2, 0)]),
            "Compressed { stream_obj: 99, index: 3 }",
        );
        assert!(!out.contains_key(&ObjectId(3, 0)));
    }

    #[test]
    fn read_xref_stream_honours_index_chunks() {
        // Two-entry stream describing IDs starting at 10.
        let bytes = xref_stream_pdf(&[(1, 10, 0), (1, 20, 0)], "/Index [10 2]");
        let mut out = BTreeMap::new();
        read_xref_stream(&bytes, 0, &mut out).unwrap();
        assert!(out.contains_key(&ObjectId(10, 0)));
        assert!(out.contains_key(&ObjectId(11, 0)));
    }

    #[test]
    fn read_xref_stream_existing_entry_wins() {
        let bytes = xref_stream_pdf(&[(1, 200, 0)], "");
        let mut out = BTreeMap::new();
        out.insert(ObjectId(0, 0), XrefEntry::Uncompressed { offset: 999 });
        read_xref_stream(&bytes, 0, &mut out).unwrap();
        assert_eq!(
            format!("{:?}", out[&ObjectId(0, 0)]),
            "Uncompressed { offset: 999 }",
        );
    }

    #[test]
    fn read_xref_stream_rejects_malformed_streams() {
        for body in [
            // /W must have exactly three widths, summing to a non-zero row.
            b"1 0 obj <</Type/XRef/Size 1/W [1 2]/Length 0>>\nstream\n\nendstream endobj\n".as_slice(),
            b"1 0 obj <</Type/XRef/Size 1/W [0 0 0]/Length 0>>\nstream\n\nendstream endobj\n",
            b"1 0 obj <</Type/XRef/Size 1/Length 0>>\nstream\n\nendstream endobj\n",
            // /Size is required.
            b"1 0 obj <</Type/XRef/W [1 2 1]/Length 4>>\nstream\n\x01\x00\x10\x00\nendstream endobj\n",
            // /W = 4 bytes per row and /Size = 2 needs 8 payload bytes.
            b"1 0 obj <</Type/XRef/Size 2/W [1 2 1]/Length 4>>\nstream\n\x01\x00\x10\x00\nendstream endobj\n",
            // /Index range wraps u32.
            b"1 0 obj <</Type/XRef/Size 1/W [1 2 1]/Index [4294967295 2]/Length 8>>\nstream\n\x00\x00\x00\x00\x00\x00\x00\x00\nendstream endobj\n",
            // Not a stream at all.
            b"1 0 obj 42 endobj\n",
            // FlateDecode body that isn't valid zlib.
            b"1 0 obj <</Type/XRef/Size 1/W [1 2 1]/Filter/FlateDecode/Length 4>>\nstream\nJUNK\nendstream endobj\n",
        ] {
            let mut out = BTreeMap::new();
            assert!(read_xref_stream(body, 0, &mut out).is_err());
        }
    }

    #[test]
    fn xref_stream_with_zero_type_width_defaults_to_one() {
        // /W [0 2 1] omits the type field — spec says it defaults to 1.
        let bytes = b"1 0 obj <</Type/XRef/Size 1/W [0 2 1]/Length 3>>\nstream\n\x00\x10\x00\nendstream endobj\n";
        let mut out = BTreeMap::new();
        read_xref_stream(bytes, 0, &mut out).unwrap();
        assert_eq!(
            format!("{:?}", out[&ObjectId(0, 0)]),
            "Uncompressed { offset: 16 }",
        );
    }

    #[test]
    fn xref_stream_with_odd_index_chunk_breaks_loop() {
        // /Index has 3 entries (not a multiple of 2) — the trailing single
        // entry should break the chunk loop.
        let bytes = b"1 0 obj <</Type/XRef/Size 1/W [1 2 1]/Index [0 1 5]/Length 4>>\nstream\n\x01\x00\x10\x00\nendstream endobj\n";
        let mut out = BTreeMap::new();
        read_xref_stream(bytes, 0, &mut out).unwrap();
        assert!(out.contains_key(&ObjectId(0, 0)));
        assert!(!out.contains_key(&ObjectId(5, 0)));
    }
}
