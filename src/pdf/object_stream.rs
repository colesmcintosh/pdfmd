use super::object::{Dictionary, Object};
use super::parser::Parser;
use super::xref::read_uint;
use super::PdfError;

#[derive(Debug)]
pub(super) struct ObjectStreamEntry {
    number: u32,
    start: usize,
    end: usize,
}

impl ObjectStreamEntry {
    pub(super) fn number(&self) -> u32 {
        self.number
    }

    pub(super) fn content<'a>(&self, decoded: &'a [u8]) -> &'a [u8] {
        &decoded[self.start..self.end]
    }
}

pub(super) fn parse_object_stream(
    dict: &Dictionary,
    decoded: &[u8],
) -> Result<Vec<Option<ObjectStreamEntry>>, PdfError> {
    let n_raw = dict
        .get(b"N")
        .and_then(Object::as_integer)
        .ok_or_else(|| PdfError::BadObject("objstm missing /N".into()))?;
    let first_raw = dict
        .get(b"First")
        .and_then(Object::as_integer)
        .ok_or_else(|| PdfError::BadObject("objstm missing /First".into()))?;
    // /N is an entry count; a negative or absurd value cast through
    // `as usize` would otherwise become ~1.8×10^19 and abort the allocator
    // on the with_capacity below. Each header is at least two bytes
    // ("0 0"), so cap by `decoded.len() / 2`.
    if n_raw < 0 || (n_raw as u64) > decoded.len() as u64 / 2 {
        return Err(PdfError::BadObject(format!(
            "objstm /N out of range: {n_raw}"
        )));
    }
    if first_raw < 0 || (first_raw as u64) > decoded.len() as u64 {
        return Err(PdfError::BadObject(format!(
            "objstm /First out of range: {first_raw}"
        )));
    }
    let n = n_raw as usize;
    let first = first_raw as usize;

    // Header: N pairs of "obj_num offset" pointing into the body at byte
    // /First. The Nth object ends at the next offset (or end of stream).
    let mut p = Parser::with_pos(decoded, 0);
    let mut headers: Vec<(u32, usize)> = Vec::with_capacity(n);
    for _ in 0..n {
        p.skip_ws_and_comments();
        let num = read_uint(decoded, &mut p.pos)?;
        p.skip_ws_and_comments();
        let off = read_uint(decoded, &mut p.pos)? as usize;
        headers.push((num, off));
    }
    // One slot per /N header so xref object-stream indices stay aligned
    // even when an offset is unusable. Callers treat `None` as a hole.
    let mut out: Vec<Option<ObjectStreamEntry>> = Vec::with_capacity(n);
    for (i, &(num, off)) in headers.iter().enumerate() {
        let entry = first.checked_add(off).and_then(|start| {
            let end = headers
                .get(i + 1)
                .map(|(_, next_off)| first.checked_add(*next_off))
                .unwrap_or(Some(decoded.len()))?;
            (start <= end && end <= decoded.len()).then_some(ObjectStreamEntry {
                number: num,
                start,
                end,
            })
        });
        out.push(entry);
    }
    Ok(out)
}

/// Pick the entry for object `expected_number`, plus a fallback.
///
/// The xref index is authoritative and correct for normal PDFs, so consult it
/// first. Keep the number-first malformed-producer behaviour: if the indexed
/// header names a different object, prefer an entry whose header names the
/// requested id, and offer the indexed payload as a fallback.
pub(super) fn object_stream_candidates(
    entries: &[Option<ObjectStreamEntry>],
    expected_number: u32,
    index: usize,
) -> (Option<&ObjectStreamEntry>, Option<&ObjectStreamEntry>) {
    let indexed = entries.get(index).and_then(Option::as_ref);
    let numbered = match indexed {
        Some(entry) if entry.number() == expected_number => Some(entry),
        _ => entries
            .iter()
            .filter_map(Option::as_ref)
            .find(|entry| entry.number() == expected_number),
    };
    let indexed_fallback = indexed.filter(|entry| {
        numbered
            .map(|numbered| !std::ptr::eq(*entry, numbered))
            .unwrap_or(true)
    });
    (numbered, indexed_fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(n: i64, first: i64) -> Dictionary {
        let mut d = Dictionary::new();
        d.insert(b"N".to_vec(), Object::Integer(n));
        d.insert(b"First".to_vec(), Object::Integer(first));
        d
    }

    #[test]
    fn parse_object_stream_returns_each_entry() {
        // Header: "10 0 11 4" — obj #10 at offset 0, obj #11 at offset 4.
        let body = b"10 0 11 4 (hi)(by)";
        let entries = parse_object_stream(&dict(2, 10), body).unwrap();
        assert_eq!(entries.len(), 2);
        let first = entries[0].as_ref().unwrap();
        let second = entries[1].as_ref().unwrap();
        assert_eq!(first.number(), 10);
        assert_eq!(first.content(body), b"(hi)");
        assert_eq!(second.number(), 11);
        assert_eq!(second.content(body), b"(by)");
    }

    #[test]
    fn parse_object_stream_errors_without_required_keys() {
        assert!(parse_object_stream(&Dictionary::new(), b"").is_err());
        let mut d = Dictionary::new();
        d.insert(b"N".to_vec(), Object::Integer(0));
        assert!(parse_object_stream(&d, b"").is_err());
        // Header token isn't an integer.
        assert!(parse_object_stream(&dict(1, 5), b"ABC 0 (hi)").is_err());
    }

    #[test]
    fn parse_object_stream_rejects_out_of_range_n_and_first() {
        // Negative counts cast through `as usize` become ~1.8×10^19 and
        // would abort the allocator; oversized ones can't fit the payload.
        for (n, first, key) in [
            (-1, 0, "/N"),
            (i64::MAX, 0, "/N"),
            (0, -1, "/First"),
            (0, i64::MAX, "/First"),
        ] {
            let err = parse_object_stream(&dict(n, first), b"short").unwrap_err();
            assert!(
                err.to_string().contains(&format!("{key} out of range")),
                "{err}"
            );
        }
    }

    #[test]
    fn parse_object_stream_skips_entries_with_bad_offsets() {
        // Header points obj 2 way past the end of the body. Both slots stay
        // aligned with /N even though neither offset is usable.
        let entries = parse_object_stream(&dict(2, 10), b"1 0 2 999 hi").unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(Option::is_none));
    }

    #[test]
    fn parse_object_stream_keeps_index_slots_for_bad_offsets() {
        // A bad first offset must not slide the second object into index 0.
        let body = b"10 999 11 0 (hi)";
        let entries = parse_object_stream(&dict(2, 12), body).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_none());
        assert_eq!(entries[1].as_ref().unwrap().number(), 11);
        assert_eq!(entries[1].as_ref().unwrap().content(body), b"(hi)");

        let (primary, fallback) = object_stream_candidates(&entries, 10, 0);
        assert!(primary.is_none());
        assert!(fallback.is_none());

        let (primary, fallback) = object_stream_candidates(&entries, 11, 1);
        assert_eq!(primary.unwrap().content(body), b"(hi)");
        assert!(fallback.is_none());
    }

    #[test]
    fn object_stream_lookup_keeps_malformed_producer_fallback_order() {
        let body = b"10 0 11 4(aa)(bb)";
        let entries = parse_object_stream(&dict(2, 9), body).unwrap();

        // A valid xref index is the common O(1) path and needs no fallback.
        let (primary, fallback) = object_stream_candidates(&entries, 11, 1);
        assert_eq!(primary.unwrap().content(body), b"(bb)");
        assert!(fallback.is_none());

        // If the index and header disagree, preserve the old behaviour: try
        // the matching object number first, then the indexed payload.
        let (primary, fallback) = object_stream_candidates(&entries, 10, 1);
        assert_eq!(primary.unwrap().content(body), b"(aa)");
        assert_eq!(fallback.unwrap().content(body), b"(bb)");

        // Some malformed producers do not put the requested id in the header;
        // their indexed payload remains available as the sole fallback.
        let (primary, fallback) = object_stream_candidates(&entries, 99, 0);
        assert!(primary.is_none());
        assert_eq!(fallback.unwrap().content(body), b"(aa)");
    }
}
