//! From-scratch PDF reader.
//!
//! Covers what the text extractor needs and nothing more: classic xref
//! tables, xref streams, object streams (PDF 1.5+), the `FlateDecode`
//! filter (with optional PNG predictor), and an in-memory cache keyed by
//! object id. Encryption and incremental updates beyond a single `/Prev`
//! chain are out of scope.

use std::collections::HashMap;
use std::fmt;

mod deflate;
mod filter;
mod object;
mod object_stream;
mod page_tree;
mod parser;
pub(crate) mod syntax;
#[cfg(test)]
pub(crate) mod test_pdf;
mod xref;

pub use object::{Dictionary, Object, ObjectId, Stream};

pub(crate) use filter::collect_filters;
use filter::decode_filters;
use object_stream::{object_stream_candidates, parse_object_stream, ObjectStreamEntry};
use page_tree::collect_pages;
use parser::Parser;
use xref::{find_startxref, read_xref_chain, XrefEntry};

// ---- Errors ----------------------------------------------------------------

#[derive(Debug)]
pub enum PdfError {
    NotPdf,
    BadXref(String),
    BadObject(String),
    BadFilter(String),
    Deflate(String),
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PdfError::NotPdf => f.write_str("input does not look like a PDF"),
            PdfError::BadXref(m) => write!(f, "xref: {m}"),
            PdfError::BadObject(m) => write!(f, "object: {m}"),
            PdfError::BadFilter(m) => write!(f, "filter: {m}"),
            PdfError::Deflate(m) => write!(f, "deflate: {m}"),
        }
    }
}

impl std::error::Error for PdfError {}

// ---- Document --------------------------------------------------------------

pub struct Document<'a> {
    bytes: &'a [u8],
    objects: HashMap<ObjectId, Object>,
    pages: Vec<ObjectId>,
}

impl<'a> Document<'a> {
    /// Parse the entire PDF byte slice and resolve every live indirect
    /// object. Returns a self-contained `Document` that can be shared by
    /// reference across threads.
    pub fn load(bytes: &'a [u8]) -> Result<Self, PdfError> {
        if !bytes.starts_with(b"%PDF-") {
            return Err(PdfError::NotPdf);
        }
        let startxref = find_startxref(bytes)?;
        let (xref, trailer) = read_xref_chain(bytes, startxref)?;

        // Materialize every uncompressed object first; object streams need
        // the surrounding objects to already exist when we expand them.
        let mut objects: HashMap<ObjectId, Object> = HashMap::with_capacity(xref.len());
        let mut compressed: Vec<(ObjectId, u32, u32)> = Vec::new();
        let length_cache = std::cell::RefCell::new(HashMap::new());
        for (id, entry) in &xref {
            match *entry {
                XrefEntry::Free => {}
                XrefEntry::Uncompressed { offset } => {
                    let resolve_length = |length_id| {
                        resolve_indirect_length_cached(
                            &mut length_cache.borrow_mut(),
                            bytes,
                            &xref,
                            length_id,
                        )
                    };
                    if let Some(obj) = parse_at(bytes, offset as usize, *id, &resolve_length) {
                        objects.insert(*id, obj);
                    }
                }
                XrefEntry::Compressed { stream_obj, index } => {
                    compressed.push((*id, stream_obj, index));
                }
            }
        }

        // Expand each object stream once, then pull every referenced index
        // out of it. PDF 1.5+ stores most metadata objects this way.
        struct CachedObjectStream {
            decoded: Vec<u8>,
            entries: Vec<Option<ObjectStreamEntry>>,
        }

        let mut objstm_cache: HashMap<u32, CachedObjectStream> = HashMap::new();
        for (id, stream_obj, index) in &compressed {
            let cached = match objstm_cache.get(stream_obj) {
                Some(v) => v,
                None => {
                    let stream_id = ObjectId(*stream_obj, 0);
                    let Some(Object::Stream(s)) = objects.get(&stream_id) else {
                        continue;
                    };
                    let decoded = decode_filters(s, bytes)?;
                    let entries = parse_object_stream(&s.dict, &decoded)?;
                    objstm_cache.insert(*stream_obj, CachedObjectStream { decoded, entries });
                    &objstm_cache[stream_obj]
                }
            };

            let (numbered, indexed_fallback) =
                object_stream_candidates(&cached.entries, id.0, *index as usize);
            let mut inserted = false;
            if let Some(entry) = numbered {
                if let Ok(obj) = Parser::new(entry.content(&cached.decoded)).parse_object() {
                    objects.insert(*id, obj);
                    inserted = true;
                }
            }
            // Index-based lookup variant — some producers don't number entries.
            if !inserted {
                if let Some(entry) = indexed_fallback {
                    if let Ok(obj) = Parser::new(entry.content(&cached.decoded)).parse_object() {
                        objects.insert(*id, obj);
                    }
                }
            }
        }

        // Streams whose /Length integer lives in an object stream could not
        // be bounded exactly during the first pass. Reparse ordinary streams
        // now that compressed objects are materialized. Object and xref
        // streams stay untouched because they bootstrap the object/xref maps
        // above; reparsing one here could create a self/cyclic dependency and
        // leave those maps inconsistent with their source.
        let mut repaired_streams = Vec::new();
        for (id, entry) in &xref {
            let XrefEntry::Uncompressed { offset } = *entry else {
                continue;
            };
            let Some(Object::Stream(stream)) = objects.get(id) else {
                continue;
            };
            let Some(length_id) = stream.dict.get(b"Length").and_then(Object::as_reference) else {
                continue;
            };
            let Some(XrefEntry::Compressed { stream_obj, .. }) = xref.get(&length_id) else {
                continue;
            };
            let stream_type = stream.dict.get(b"Type").and_then(Object::as_name);
            if *stream_obj == id.0 || stream_type == Some(b"ObjStm") || stream_type == Some(b"XRef")
            {
                continue;
            }
            if resolve_materialized_length(&objects, length_id).is_none() {
                continue;
            }
            let resolve_length = |candidate| resolve_materialized_length(&objects, candidate);
            let mut parser = Parser::with_pos(bytes, offset as usize);
            let Ok((parsed_id, obj)) =
                parser.parse_indirect_object_with_length_resolver(&resolve_length)
            else {
                continue;
            };
            if parsed_id == *id {
                repaired_streams.push((*id, obj));
            }
        }
        objects.extend(repaired_streams);

        let pages = collect_pages(&objects, &trailer)?;

        Ok(Document {
            bytes,
            objects,
            pages,
        })
    }

    pub fn get_object(&self, id: ObjectId) -> Option<&Object> {
        self.objects.get(&id)
    }

    /// Follow a chain of indirect references and return the terminal object.
    pub fn deref<'s>(&'s self, obj: &'s Object) -> &'s Object {
        let mut current = obj;
        for _ in 0..32 {
            match current {
                Object::Reference(id) => match self.objects.get(id) {
                    Some(o) => current = o,
                    None => return current,
                },
                _ => return current,
            }
        }
        current
    }

    /// Pages in document order.
    pub fn pages(&self) -> &[ObjectId] {
        &self.pages
    }

    /// Catalog dictionary (`/Type /Catalog`), used to reach `/StructTreeRoot`.
    pub fn catalog(&self) -> Option<&Dictionary> {
        let mut fallback = None;
        for obj in self.objects.values() {
            let Some(dict) = obj.as_dict() else {
                continue;
            };
            if dict.get(b"Type").and_then(Object::as_name) == Some(b"Catalog".as_slice()) {
                return Some(dict);
            }
            if dict.get(b"StructTreeRoot").is_some() {
                fallback = Some(dict);
            }
        }
        fallback
    }

    /// Concatenated, filter-decoded bytes of the page's content stream(s).
    pub fn get_page_content(&self, page_id: ObjectId) -> Option<Vec<u8>> {
        let page = self.get_object(page_id)?.as_dict()?;
        let contents = page.get(b"Contents")?;
        match self.deref(contents) {
            Object::Stream(s) => decode_filters(s, self.bytes).ok(),
            Object::Array(items) => {
                let mut out = Vec::new();
                for item in items {
                    if let Object::Stream(s) = self.deref(item) {
                        if let Ok(mut bytes) = decode_filters(s, self.bytes) {
                            // PDF content streams may abut without trailing
                            // whitespace; the spec wants us to insert one.
                            if !out.is_empty()
                                && !out.last().is_some_and(|b: &u8| b.is_ascii_whitespace())
                            {
                                out.push(b'\n');
                            }
                            out.append(&mut bytes);
                        }
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// Decode the stream's `/Filter` chain and return the resulting bytes.
    pub fn decode_stream(&self, stream: &Stream) -> Result<Vec<u8>, PdfError> {
        decode_filters(stream, self.bytes)
    }

    pub fn stream_content<'s>(&'s self, stream: &'s Stream) -> &'s [u8] {
        stream.content(self.bytes)
    }
}

// ---- Helpers ---------------------------------------------------------------

fn parse_at(
    bytes: &[u8],
    at: usize,
    expected: ObjectId,
    resolve_length: &dyn Fn(ObjectId) -> Option<usize>,
) -> Option<Object> {
    let mut p = Parser::with_pos(bytes, at);
    let (id, obj) = p
        .parse_indirect_object_with_length_resolver(resolve_length)
        .ok()?;
    if id.0 != expected.0 {
        return None;
    }
    Some(obj)
}

fn resolve_indirect_length(
    bytes: &[u8],
    xref: &std::collections::BTreeMap<ObjectId, XrefEntry>,
    id: ObjectId,
) -> Option<usize> {
    let XrefEntry::Uncompressed { offset } = xref.get(&id)? else {
        return None;
    };
    let mut p = Parser::with_pos(bytes, *offset as usize);
    let (parsed_id, value) = p.parse_indirect_object().ok()?;
    if parsed_id != id {
        return None;
    }
    let Object::Integer(value) = value else {
        return None;
    };
    usize::try_from(value).ok()
}

fn resolve_indirect_length_cached(
    cache: &mut HashMap<ObjectId, Option<usize>>,
    bytes: &[u8],
    xref: &std::collections::BTreeMap<ObjectId, XrefEntry>,
    id: ObjectId,
) -> Option<usize> {
    if let Some(value) = cache.get(&id) {
        return *value;
    }
    let value = resolve_indirect_length(bytes, xref, id);
    cache.insert(id, value);
    value
}

fn resolve_materialized_length(objects: &HashMap<ObjectId, Object>, id: ObjectId) -> Option<usize> {
    let Object::Integer(value) = objects.get(&id)? else {
        return None;
    };
    usize::try_from(*value).ok()
}

#[cfg(test)]
mod tests {
    use super::test_pdf::{build_xref_pdf, page, page_tree_objects, XrefStreamPdf, CATALOG, PAGES};
    use super::*;

    /// Classic-xref PDF whose page draws object 4, defined by `contents`.
    fn classic_pdf(contents: &str) -> Vec<u8> {
        let body = format!(
            "%PDF-1.4\n{}{contents}",
            page_tree_objects("/Contents 4 0 R")
        );
        build_xref_pdf(body.as_bytes())
    }

    fn dict(entries: &[(&[u8], Object)]) -> Dictionary {
        let mut d = Dictionary::new();
        for (key, value) in entries {
            d.insert(key.to_vec(), value.clone());
        }
        d
    }

    /// In-memory `Document` over hand-built objects; `bytes` stays empty, so
    /// only owned streams are readable.
    fn doc_from(objects: &[(u32, Object)]) -> Document<'static> {
        Document {
            bytes: b"",
            objects: objects
                .iter()
                .map(|(n, obj)| (ObjectId(*n, 0), obj.clone()))
                .collect(),
            pages: Vec::new(),
        }
    }

    fn page_with_contents(contents: Object) -> Object {
        Object::Dictionary(dict(&[(b"Contents", contents)]))
    }

    fn page_count(bytes: &[u8]) -> usize {
        Document::load(bytes).expect("load").pages().len()
    }

    fn first_page_content(bytes: &[u8]) -> Vec<u8> {
        let doc = Document::load(bytes).expect("load");
        doc.get_page_content(doc.pages()[0]).expect("page content")
    }

    // ---- Loading --------------------------------------------------------

    #[test]
    fn loads_minimal_pdf_and_walks_pages() {
        let bytes = classic_pdf(
            "4 0 obj <</Length 24>>\nstream\nBT /F1 12 Tf (Hi) Tj ET\nendstream\nendobj\n",
        );
        let content = first_page_content(&bytes);
        assert!(content.windows(2).any(|w| w == b"Hi"));
    }

    #[test]
    fn rejects_non_pdf_header() {
        // Comparing on the Display string sidesteps a `match` whose unused
        // arms would otherwise show up as uncovered branches.
        let err = Document::load(b"\x00not a pdf")
            .err()
            .expect("expected Err for non-PDF input");
        assert_eq!(err.to_string(), "input does not look like a PDF");
    }

    #[test]
    fn pdf_error_display_lines() {
        // Exercise every match arm in the Display impl.
        let cases: Vec<(PdfError, &str)> = vec![
            (PdfError::NotPdf, "does not look like a PDF"),
            (PdfError::BadXref("x".into()), "xref: x"),
            (PdfError::BadObject("o".into()), "object: o"),
            (PdfError::BadFilter("f".into()), "filter: f"),
            (PdfError::Deflate("d".into()), "deflate: d"),
        ];
        for (err, expected) in cases {
            let s = format!("{err}");
            assert!(s.contains(expected), "{s} did not contain {expected}");
            // Also touch the Debug impl so it isn't dead-coded.
            let _ = format!("{err:?}");
        }
        // std::error::Error trait should be implemented.
        let _: Box<dyn std::error::Error> = Box::new(PdfError::NotPdf);
    }

    #[test]
    fn document_load_propagates_xref_failures() {
        // Missing `startxref`, then a `startxref` past the end of the file.
        assert!(Document::load(b"%PDF-1.4\nnot a real pdf").is_err());
        assert!(Document::load(b"%PDF-1.4\nstartxref\n9999\n%%EOF").is_err());
    }

    #[test]
    fn document_load_skips_uncompressed_entries_that_fail_to_parse() {
        // Entry 4 points at offset 5 — the middle of `%PDF-1.4`, where no
        // indirect-object header starts. parse_at returns None and the
        // entry is silently dropped.
        let body = format!("%PDF-1.4\n{}", page_tree_objects(""));
        let bytes = test_pdf::build_xref_pdf_with(
            body.as_bytes(),
            &["0000000005 00000 n "],
            "<</Size {size}/Root 1 0 R>>",
        );
        let doc = Document::load(&bytes).expect("load");
        assert_eq!(doc.pages().len(), 1);
        assert!(doc.get_object(ObjectId(4, 0)).is_none());
    }

    // ---- Indirect stream /Length ----------------------------------------

    #[test]
    fn indirect_stream_length_preserves_terminal_cr_data_byte() {
        // Object 5 is the exact length, so the trailing CR is data, not the
        // `endstream` delimiter.
        let bytes = classic_pdf(
            "4 0 obj <</Length 5 0 R>>\nstream\nABC\r\nendstream\nendobj\n5 0 obj 4 endobj\n",
        );
        assert_eq!(first_page_content(&bytes), b"ABC\r");
    }

    #[test]
    fn stale_indirect_stream_length_falls_back_to_endstream_scan() {
        // Object 5 claims the content stream is empty. Trusting it would
        // silently drop the page.
        let bytes = classic_pdf(
            "4 0 obj <</Length 5 0 R>>\nstream\nABC\nendstream\nendobj\n5 0 obj 0 endobj\n",
        );
        assert_eq!(first_page_content(&bytes), b"ABC");
    }

    #[test]
    fn compressed_indirect_stream_length_preserves_terminal_cr_data_byte() {
        // Same as above, but /Length lives inside an object stream, so it is
        // only resolvable in the repair pass.
        let mut pdf = XrefStreamPdf::new();
        pdf.page_tree("/Contents 4 0 R")
            .stream(4, "<</Length 5 0 R>>", b"ABC\r")
            .objstm(6, &[(5, "4")])
            .compressed(5, 6, 0);
        assert_eq!(first_page_content(&pdf.finish(7)), b"ABC\r");
    }

    #[test]
    fn stale_compressed_stream_length_keeps_scanned_stream() {
        // The repair pass reparses object 4; it must not replace the body
        // the first pass recovered by scanning.
        let mut pdf = XrefStreamPdf::new();
        pdf.page_tree("/Contents 4 0 R")
            .stream(4, "<</Length 5 0 R>>", b"ABC")
            .objstm(6, &[(5, "0")])
            .compressed(5, 6, 0);
        assert_eq!(first_page_content(&pdf.finish(7)), b"ABC");
    }

    // ---- Object streams --------------------------------------------------

    #[test]
    fn loads_pdf_with_object_stream() {
        // PDF 1.5+ layout: catalog/pages/page packed into an object stream,
        // referenced from an xref stream with type-2 entries.
        let leaf = page("");
        let mut pdf = XrefStreamPdf::new();
        pdf.objstm(4, &[(1, CATALOG), (2, PAGES), (3, &leaf)]);
        for index in 0..3u8 {
            pdf.compressed(u32::from(index) + 1, 4, index);
        }
        assert_eq!(page_count(&pdf.finish(5)), 1);
    }

    #[test]
    fn document_load_reuses_objstm_cache_across_compressed_entries() {
        // Identical to the test above from the loader's perspective, except
        // that the second and third lookups hit the object-stream cache.
        let leaf = page("");
        let mut pdf = XrefStreamPdf::new();
        pdf.objstm(4, &[(1, CATALOG), (2, PAGES), (3, &leaf)]);
        for index in 0..3u8 {
            pdf.compressed(u32::from(index) + 1, 4, index);
        }
        assert_eq!(page_count(&pdf.finish(5)), 1);
    }

    #[test]
    fn document_load_falls_back_to_index_based_objstm_lookup() {
        // Some producers don't put the actual object id in the objstm
        // header. The find-by-number misses, and the index-based fallback
        // should still pick up the payload at slot 0.
        let mut pdf = XrefStreamPdf::new();
        let leaf = page("");
        pdf.obj(2, PAGES)
            .obj(3, &leaf)
            .objstm(4, &[(99, CATALOG)])
            .compressed(1, 4, 0);
        assert_eq!(page_count(&pdf.finish(5)), 1);
    }

    #[test]
    fn compressed_object_with_out_of_range_index_is_skipped() {
        let mut pdf = XrefStreamPdf::new();
        pdf.page_tree("")
            .objstm(4, &[(8, "<</Ignored true>>")])
            .compressed(5, 4, 9);
        let bytes = pdf.finish(6);
        let doc = Document::load(&bytes).expect("load");
        assert_eq!(doc.pages().len(), 1);
        assert!(doc.get_object(ObjectId(5, 0)).is_none());
    }

    #[test]
    fn document_load_skips_compressed_objects_with_missing_objstm() {
        // Object 4 claims to live in object stream 99, which doesn't exist.
        let mut pdf = XrefStreamPdf::new();
        pdf.page_tree("").compressed(4, 99, 0);
        assert_eq!(page_count(&pdf.finish(5)), 1);
    }

    #[test]
    fn document_load_propagates_objstm_errors() {
        // A /FlateDecode objstm whose body is garbage, then one whose header
        // has a non-numeric object id. Both propagate out of Document::load.
        for (dict, payload) in [
            (
                "<</Type/ObjStm/N 1/First 4/Filter/FlateDecode/Length 4>>",
                b"JUNK".as_slice(),
            ),
            ("<</Type/ObjStm/N 1/First 6/Length 12>>", b"BAD 0 (oops)"),
        ] {
            let mut pdf = XrefStreamPdf::new();
            pdf.page_tree("")
                .stream(4, dict, payload)
                .compressed(5, 4, 0);
            assert!(Document::load(&pdf.finish(6)).is_err(), "{dict}");
        }
    }

    #[test]
    fn loads_pdf_with_xref_stream_root() {
        let mut pdf = XrefStreamPdf::new();
        pdf.page_tree("");
        assert_eq!(page_count(&pdf.finish(4)), 1);
    }

    // ---- deref / catalog -------------------------------------------------

    #[test]
    fn deref_resolves_chains_and_handles_dead_refs() {
        // id 1 points at id 2, which is an int.
        let doc = doc_from(&[
            (1, Object::Reference(ObjectId(2, 0))),
            (2, Object::Integer(7)),
        ]);
        let obj = doc.get_object(ObjectId(1, 0)).unwrap();
        assert_eq!(doc.deref(obj).as_integer(), Some(7));
        assert!(doc.catalog().is_none());
        // Dangling reference: deref returns the unresolved reference itself.
        let dangling = Object::Reference(ObjectId(999, 0));
        assert!(doc.deref(&dangling).as_reference().is_some());
    }

    #[test]
    fn deref_terminates_on_cycle() {
        let doc = doc_from(&[
            (1, Object::Reference(ObjectId(2, 0))),
            (2, Object::Reference(ObjectId(1, 0))),
        ]);
        // We don't care which id we land on — just that we terminate at a
        // Reference (the cycle never reaches a concrete value).
        let start = Object::Reference(ObjectId(1, 0));
        assert!(doc.deref(&start).as_reference().is_some());
    }

    #[test]
    fn catalog_prefers_type_catalog_over_struct_tree_fallback() {
        let catalog = Object::Dictionary(dict(&[(b"Type", Object::Name(b"Catalog".to_vec()))]));
        let other = Object::Dictionary(dict(&[(b"StructTreeRoot", Object::Integer(0))]));
        let doc = doc_from(&[(1, catalog), (2, other.clone())]);
        assert_eq!(
            doc.catalog()
                .unwrap()
                .get(b"Type")
                .and_then(Object::as_name),
            Some(b"Catalog".as_slice())
        );
        // Without a /Type /Catalog dict, any /StructTreeRoot holder wins.
        let doc = doc_from(&[(1, other)]);
        assert!(doc.catalog().unwrap().get(b"StructTreeRoot").is_some());
    }

    // ---- Page content ----------------------------------------------------

    #[test]
    fn page_content_supports_array_of_streams() {
        let stream = |body: &[u8]| Object::Stream(Stream::owned(Dictionary::new(), body.to_vec()));
        let doc = doc_from(&[
            (10, stream(b"first")),
            (11, stream(b"second")),
            (
                20,
                page_with_contents(Object::Array(vec![
                    Object::Reference(ObjectId(10, 0)),
                    Object::Reference(ObjectId(11, 0)),
                ])),
            ),
        ]);
        // Two stream bodies joined by a newline (since neither ends in
        // whitespace).
        assert_eq!(
            doc.get_page_content(ObjectId(20, 0)).unwrap(),
            b"first\nsecond"
        );
    }

    #[test]
    fn page_content_array_skips_streams_that_fail_to_decode() {
        // The first stream has a corrupt FlateDecode body, so only the
        // second shows up in the joined output.
        let bad = dict(&[(b"Filter", Object::Name(b"FlateDecode".to_vec()))]);
        let doc = doc_from(&[
            (
                10,
                Object::Stream(Stream::owned(bad, b"NOT-VALID-ZLIB".to_vec())),
            ),
            (
                11,
                Object::Stream(Stream::owned(Dictionary::new(), b"GOOD".to_vec())),
            ),
            (
                20,
                page_with_contents(Object::Array(vec![
                    Object::Reference(ObjectId(10, 0)),
                    Object::Reference(ObjectId(11, 0)),
                ])),
            ),
        ]);
        assert_eq!(doc.get_page_content(ObjectId(20, 0)).unwrap(), b"GOOD");
    }

    #[test]
    fn page_content_returns_empty_for_array_with_no_streams() {
        // A /Contents array pointing only at non-streams yields an empty
        // body rather than failing.
        let doc = doc_from(&[
            (2, Object::Integer(7)),
            (
                1,
                page_with_contents(Object::Array(vec![Object::Reference(ObjectId(2, 0))])),
            ),
        ]);
        assert_eq!(doc.get_page_content(ObjectId(1, 0)), Some(Vec::new()));
    }

    #[test]
    fn page_content_returns_none_for_unusable_pages() {
        // Unknown page id, page without /Contents, and an unsupported
        // /Contents object all decline rather than panic.
        assert!(doc_from(&[]).get_page_content(ObjectId(99, 0)).is_none());
        let doc = doc_from(&[(1, Object::Dictionary(Dictionary::new()))]);
        assert!(doc.get_page_content(ObjectId(1, 0)).is_none());
        let doc = doc_from(&[(1, page_with_contents(Object::Integer(42)))]);
        assert!(doc.get_page_content(ObjectId(1, 0)).is_none());
    }

    // ---- /Length resolution helpers --------------------------------------

    #[test]
    fn parse_at_returns_none_for_mismatched_id_or_bad_offset() {
        let bytes = b"5 0 obj 42 endobj";
        // Asking for id 7 at offset 0 returns None because id 5 lives there.
        assert!(parse_at(bytes, 0, ObjectId(7, 0), &|_| None).is_none());
        // A wildly out-of-bounds offset also returns None (parse_indirect
        // bails on an empty slice).
        assert!(parse_at(bytes, bytes.len() + 100, ObjectId(5, 0), &|_| None).is_none());
    }

    #[test]
    fn indirect_length_resolvers_reject_non_integer_values() {
        let id = ObjectId(5, 0);
        let mut xref = std::collections::BTreeMap::new();
        xref.insert(id, XrefEntry::Uncompressed { offset: 0 });
        assert_eq!(
            resolve_indirect_length(b"5 0 obj 4.5 endobj", &xref, id),
            None
        );

        let objects = [(id, Object::Real(4.5))].into_iter().collect();
        assert_eq!(resolve_materialized_length(&objects, id), None);
    }

    #[test]
    fn indirect_length_cache_memoizes_hits_and_misses_by_exact_id() {
        let mut cache = HashMap::new();
        let mut xref = std::collections::BTreeMap::new();
        let mut resolve = |bytes: &[u8], xref: &_, id| {
            resolve_indirect_length_cached(&mut cache, bytes, xref, id)
        };

        let hit = ObjectId(5, 0);
        xref.insert(hit, XrefEntry::Uncompressed { offset: 0 });
        assert_eq!(resolve(b"5 0 obj 4 endobj", &xref, hit), Some(4));
        // Changing the backing bytes makes a repeated parse observable: the
        // cached result must still win.
        assert_eq!(resolve(b"5 0 obj 9 endobj", &xref, hit), Some(4));

        let miss = ObjectId(6, 0);
        xref.insert(miss, XrefEntry::Uncompressed { offset: 0 });
        assert_eq!(resolve(b"6 0 obj [1 2 3] endobj", &xref, miss), None);
        assert_eq!(resolve(b"6 0 obj 8 endobj", &xref, miss), None);

        let next_generation = ObjectId(5, 1);
        xref.insert(next_generation, XrefEntry::Uncompressed { offset: 0 });
        assert_eq!(
            resolve(b"5 1 obj 9 endobj", &xref, next_generation),
            Some(9)
        );
        assert_eq!(cache.len(), 3);
    }
}
