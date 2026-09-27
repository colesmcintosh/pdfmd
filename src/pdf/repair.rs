//! Xref reconstruction for damaged files.
//!
//! When `startxref` is missing, points at garbage, or the table it names
//! doesn't lead to a page tree, rebuild the object map by scanning the
//! whole file for `N G obj` headers — the same recovery Acrobat, pdf.js,
//! and MuPDF perform. Later definitions win, matching incremental updates.

use std::collections::{BTreeMap, HashMap};

use super::object::{Dictionary, Object, ObjectId};
use super::parser::Parser;
use super::syntax::{is_delim, is_ws};
use super::xref::XrefEntry;

/// Trailer keys worth carrying over from `trailer` dictionaries and xref
/// streams found during the scan.
const TRAILER_KEYS: [&[u8]; 3] = [b"Root", b"Encrypt", b"Info"];

pub(super) fn scan_xref(bytes: &[u8]) -> (BTreeMap<ObjectId, XrefEntry>, Dictionary) {
    let mut latest: HashMap<u32, (u16, usize)> = HashMap::new();
    let mut trailer = Dictionary::new();
    let mut i = 0;
    while let Some(hit) = find(bytes, i, b"obj") {
        i = hit + 3;
        if bytes.get(i).is_some_and(|&b| !is_ws(b) && !is_delim(b)) {
            continue;
        }
        if let Some((num, gen, start)) = object_header_before(bytes, hit) {
            latest.insert(num, (gen, start));
            // Xref streams stand in for the trailer in PDF 1.5+ files.
            let head = &bytes[i..bytes.len().min(i + 512)];
            if find(head, 0, b"/XRef").is_some() {
                if let Ok((_, Object::Stream(s))) =
                    Parser::with_pos(bytes, start).parse_indirect_object()
                {
                    merge_trailer(&mut trailer, &s.dict);
                }
            }
        }
    }
    let mut i = 0;
    while let Some(hit) = find(bytes, i, b"trailer") {
        i = hit + b"trailer".len();
        if let Ok(Object::Dictionary(d)) = Parser::with_pos(bytes, i).parse_object() {
            merge_trailer(&mut trailer, &d);
        }
    }
    let entries = latest
        .into_iter()
        .map(|(num, (gen, offset))| {
            let offset = offset as u64;
            (ObjectId(num, gen), XrefEntry::Uncompressed { offset })
        })
        .collect();
    (entries, trailer)
}

fn merge_trailer(trailer: &mut Dictionary, from: &Dictionary) {
    for key in TRAILER_KEYS {
        if let Some(v) = from.get(key) {
            trailer.insert(key.to_vec(), v.clone());
        }
    }
}

/// Given the index of an `obj` keyword, walk back over `<num> <gen> ` and
/// return the object id plus the offset of `<num>`.
fn object_header_before(bytes: &[u8], obj_at: usize) -> Option<(u32, u16, usize)> {
    let mut p = obj_at;
    let gen_end = skip_ws_back(bytes, p)?;
    if gen_end == p {
        return None;
    }
    p = digits_back(bytes, gen_end)?;
    let gen: u16 = parse_digits(&bytes[p..gen_end])?;
    let num_end = skip_ws_back(bytes, p)?;
    if num_end == p {
        return None;
    }
    let num_start = digits_back(bytes, num_end)?;
    if num_start > 0 && !is_ws(bytes[num_start - 1]) && !is_delim(bytes[num_start - 1]) {
        return None;
    }
    let num: u32 = parse_digits(&bytes[num_start..num_end])?;
    Some((num, gen, num_start))
}

fn skip_ws_back(bytes: &[u8], mut p: usize) -> Option<usize> {
    while p > 0 && is_ws(bytes[p - 1]) {
        p -= 1;
    }
    (p > 0).then_some(p)
}

fn digits_back(bytes: &[u8], end: usize) -> Option<usize> {
    let mut p = end;
    while p > 0 && bytes[p - 1].is_ascii_digit() {
        p -= 1;
    }
    (p < end).then_some(p)
}

fn parse_digits<T: std::str::FromStr>(digits: &[u8]) -> Option<T> {
    std::str::from_utf8(digits).ok()?.parse().ok()
}

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offset_of(entries: &BTreeMap<ObjectId, XrefEntry>, id: ObjectId) -> Option<u64> {
        match entries.get(&id)? {
            XrefEntry::Uncompressed { offset } => Some(*offset),
            _ => None,
        }
    }

    #[test]
    fn scan_finds_objects_and_trailer() {
        let pdf = b"%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\n 12 3 obj 5 endobj\ntrailer<</Root 1 0 R/Size 13>>";
        let (entries, trailer) = scan_xref(pdf);
        assert_eq!(offset_of(&entries, ObjectId(1, 0)), Some(9));
        assert_eq!(offset_of(&entries, ObjectId(12, 3)), Some(41));
        assert_eq!(
            trailer.get(b"Root").and_then(Object::as_reference),
            Some(ObjectId(1, 0))
        );
        assert!(trailer.get(b"Size").is_none());
    }

    #[test]
    fn later_definitions_win() {
        let pdf = b"1 0 obj 1 endobj\n1 0 obj 2 endobj\n";
        let (entries, _) = scan_xref(pdf);
        assert_eq!(entries.len(), 1);
        assert_eq!(offset_of(&entries, ObjectId(1, 0)), Some(17));
    }

    #[test]
    fn rejects_lookalike_headers() {
        for pdf in [
            &b"objective"[..],
            b"x1 0 obj",
            b"1 0obj",
            b"1 obj",
            b"obj",
            b" 0 obj",
            b"99999999999 0 obj",
            b"1 99999 obj",
        ] {
            let (entries, _) = scan_xref(pdf);
            assert!(entries.is_empty(), "{:?}", std::str::from_utf8(pdf));
        }
    }

    #[test]
    fn xref_stream_dict_supplies_trailer_keys() {
        let pdf =
            b"7 0 obj<</Type/XRef/Root 1 0 R/Encrypt 9 0 R/Length 0>>stream\n\nendstream endobj";
        let (_, trailer) = scan_xref(pdf);
        assert_eq!(
            trailer.get(b"Root").and_then(Object::as_reference),
            Some(ObjectId(1, 0))
        );
        assert!(trailer.get(b"Encrypt").is_some());
    }

    #[test]
    fn unparseable_trailer_is_ignored() {
        let (_, trailer) = scan_xref(b"trailer garbage");
        assert!(trailer.get(b"Root").is_none());
    }
}
