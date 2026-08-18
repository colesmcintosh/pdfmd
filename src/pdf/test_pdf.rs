//! Shared builders for in-crate PDF fixtures.

use std::collections::BTreeMap;

use super::{Document, ObjectId};

pub(crate) const CATALOG: &str = "<</Type/Catalog/Pages 2 0 R>>";
pub(crate) const PAGES: &str = "<</Type/Pages/Kids[3 0 R]/Count 1>>";

/// Page dictionary body, with `extra` (e.g. `"/Contents 4 0 R"`) appended.
pub(crate) fn page(extra: &str) -> String {
    format!("<</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]{extra}>>")
}

/// Objects 1..=3: catalog, page-tree root, and one page — the prelude nearly
/// every fixture needs. `page_extra` is spliced into the page dictionary.
pub(crate) fn page_tree_objects(page_extra: &str) -> String {
    format!(
        "1 0 obj {CATALOG} endobj\n2 0 obj {PAGES} endobj\n3 0 obj {} endobj\n",
        page(page_extra)
    )
}

pub(crate) fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Append a classic xref covering every `N 0 obj` header in `body`. Missing
/// ids between 1 and the highest found number are marked free.
pub(crate) fn build_xref_pdf(body: &[u8]) -> Vec<u8> {
    build_xref_pdf_with(body, &[], "<</Size {size}/Root 1 0 R>>")
}

/// [`build_xref_pdf`] with hand-written trailing entry rows (each exactly the
/// 19 bytes before the newline) and a trailer template, in which `{size}` and
/// `{xref}` expand to the entry count and the table's byte offset.
pub(crate) fn build_xref_pdf_with(body: &[u8], extra_rows: &[&str], trailer: &str) -> Vec<u8> {
    const MAX_ID: u32 = 256;
    let mut found: Vec<(u32, usize)> = Vec::new();
    for n in 1..=MAX_ID {
        let needle = format!("{n} 0 obj");
        if let Some(off) = find_bytes(body, needle.as_bytes()) {
            found.push((n, off));
        }
    }
    let max = found.iter().map(|(n, _)| *n).max().unwrap_or(0);
    let size = max as usize + 1 + extra_rows.len();
    let mut out = body.to_vec();
    let xref_offset = out.len();
    let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
    for n in 1..=max {
        match found.iter().find(|(m, _)| *m == n) {
            Some((_, off)) => xref.push_str(&format!("{off:010} 00000 n \n")),
            None => xref.push_str("0000000000 00000 f \n"),
        }
    }
    for row in extra_rows {
        xref.push_str(row);
        xref.push('\n');
    }
    let trailer = trailer
        .replace("{size}", &size.to_string())
        .replace("{xref}", &xref_offset.to_string());
    xref.push_str(&format!(
        "trailer {trailer}\nstartxref\n{xref_offset}\n%%EOF\n"
    ));
    out.extend_from_slice(xref.as_bytes());
    out
}

/// Catalog + empty page, plus extra indirect objects. Bytes are leaked so
/// tests can return `Document<'static>`.
pub(crate) fn load_minimal_doc(extra_objs: &[(u32, &[u8])]) -> Document<'static> {
    let mut body = format!("%PDF-1.4\n{}", page_tree_objects("")).into_bytes();
    for (n, payload) in extra_objs {
        body.extend_from_slice(format!("{n} 0 obj ").as_bytes());
        body.extend_from_slice(payload);
        body.extend_from_slice(b" endobj\n");
    }
    let bytes = Box::leak(build_xref_pdf(&body).into_boxed_slice());
    Document::load(bytes).expect("load")
}

/// [`load_minimal_doc`] for object bodies written as text.
pub(crate) fn load_minimal_doc_str(extra_objs: &[(u32, &str)]) -> Document<'static> {
    let bytes: Vec<(u32, &[u8])> = extra_objs.iter().map(|(n, s)| (*n, s.as_bytes())).collect();
    load_minimal_doc(&bytes)
}

/// RFC 1950 zlib wrapper around stored (uncompressed) deflate blocks — just
/// enough to seed `/FlateDecode` payloads without an encoder.
pub(crate) fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78u8, 0x01]; // deflate, 32K window, fastest algorithm
    let mut rest = data;
    loop {
        let take = rest.len().min(65_535);
        let final_block = take == rest.len();
        out.push(u8::from(final_block));
        let len = take as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(&rest[..take]);
        rest = &rest[take..];
        if final_block {
            break;
        }
    }
    // Adler-32 checksum.
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

#[derive(Clone, Copy)]
enum Row {
    Free,
    Uncompressed(u64),
    Compressed { stream_obj: u32, index: u8 },
}

/// Incrementally assemble a PDF 1.5 file whose cross-reference data lives in
/// an xref stream. Objects are appended in call order and their offsets are
/// recorded automatically; [`XrefStreamPdf::finish`] writes the xref stream
/// object itself plus the `startxref` trailer.
pub(crate) struct XrefStreamPdf {
    bytes: Vec<u8>,
    rows: BTreeMap<u32, Row>,
}

impl XrefStreamPdf {
    pub(crate) fn new() -> Self {
        let mut rows = BTreeMap::new();
        rows.insert(0, Row::Free);
        Self {
            bytes: b"%PDF-1.5\n".to_vec(),
            rows,
        }
    }

    /// `N 0 obj <body> endobj`.
    pub(crate) fn obj(&mut self, n: u32, body: &str) -> &mut Self {
        self.rows
            .insert(n, Row::Uncompressed(self.bytes.len() as u64));
        self.bytes
            .extend_from_slice(format!("{n} 0 obj {body} endobj\n").as_bytes());
        self
    }

    /// Objects 1..=3: catalog, page-tree root, and one page whose dictionary
    /// carries `page_extra` (e.g. `"/Contents 4 0 R"`).
    pub(crate) fn page_tree(&mut self, page_extra: &str) -> &mut Self {
        self.obj(1, CATALOG).obj(2, PAGES).obj(3, &page(page_extra))
    }

    /// `N 0 obj <dict> stream … endstream endobj`. `dict` is written verbatim
    /// so the caller controls `/Length`.
    pub(crate) fn stream(&mut self, n: u32, dict: &str, payload: &[u8]) -> &mut Self {
        self.rows
            .insert(n, Row::Uncompressed(self.bytes.len() as u64));
        self.bytes
            .extend_from_slice(format!("{n} 0 obj {dict}\nstream\n").as_bytes());
        self.bytes.extend_from_slice(payload);
        self.bytes.extend_from_slice(b"\nendstream endobj\n");
        self
    }

    /// Object stream `n` holding `objects`, with header offsets computed.
    pub(crate) fn objstm(&mut self, n: u32, objects: &[(u32, &str)]) -> &mut Self {
        let mut header = String::new();
        let mut offset = 0usize;
        for (num, body) in objects {
            header.push_str(&format!("{num} {offset} "));
            offset += body.len();
        }
        let mut payload = header.clone().into_bytes();
        for (_, body) in objects {
            payload.extend_from_slice(body.as_bytes());
        }
        let dict = format!(
            "<</Type/ObjStm/N {}/First {}/Length {}>>",
            objects.len(),
            header.len(),
            payload.len()
        );
        self.stream(n, &dict, &payload)
    }

    /// Record object `n` as living at `index` inside object stream `stream_obj`.
    pub(crate) fn compressed(&mut self, n: u32, stream_obj: u32, index: u8) -> &mut Self {
        self.rows.insert(n, Row::Compressed { stream_obj, index });
        self
    }

    /// Append the xref stream as object `n`, then `startxref`.
    pub(crate) fn finish(mut self, n: u32) -> Vec<u8> {
        let xref_offset = self.bytes.len();
        self.rows.insert(n, Row::Uncompressed(xref_offset as u64));
        let size = self.rows.keys().copied().max().unwrap_or(0) + 1;
        let mut payload = Vec::with_capacity(size as usize * 4);
        for id in 0..size {
            let (kind, f1, f2) = match self.rows.get(&id) {
                Some(Row::Uncompressed(off)) => (1u8, *off, 0u8),
                Some(Row::Compressed { stream_obj, index }) => (2, *stream_obj as u64, *index),
                _ => (0, 0, 0),
            };
            payload.push(kind);
            payload.extend_from_slice(&(f1 as u16).to_be_bytes());
            payload.push(f2);
        }
        self.bytes.extend_from_slice(
            format!(
                "{n} 0 obj <</Type/XRef/Size {size}/Root 1 0 R/W [1 2 1]/Length {}>>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        self.bytes.extend_from_slice(&payload);
        self.bytes.extend_from_slice(b"\nendstream endobj\n");
        self.bytes
            .extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_bytes_locates_and_misses() {
        assert_eq!(find_bytes(b"abcde", b"cd"), Some(2));
        assert_eq!(find_bytes(b"abcde", b"xyz"), None);
        assert_eq!(find_bytes(b"", b"a"), None);
    }

    #[test]
    fn build_xref_pdf_pads_missing_ids() {
        let body = format!("%PDF-1.4\n{}8 0 obj 42 endobj\n", page_tree_objects(""));
        let bytes = build_xref_pdf(body.as_bytes());
        let doc = Document::load(&bytes).expect("load");
        assert_eq!(doc.pages().len(), 1);
        assert!(doc.get_object(ObjectId(8, 0)).is_some());
        assert!(doc.get_object(ObjectId(4, 0)).is_none());
    }

    #[test]
    fn load_minimal_doc_materializes_extra_objects() {
        let doc = load_minimal_doc(&[(5, b"99")]);
        assert_eq!(doc.pages().len(), 1);
        assert_eq!(
            doc.get_object(ObjectId(5, 0)).and_then(|o| o.as_integer()),
            Some(99)
        );
    }

    #[test]
    fn xref_stream_builder_round_trips_compressed_objects() {
        let mut pdf = XrefStreamPdf::new();
        let leaf = page("");
        pdf.objstm(4, &[(1, CATALOG), (2, PAGES), (3, &leaf)]);
        for index in 0..3u8 {
            pdf.compressed(u32::from(index) + 1, 4, index);
        }
        let bytes = pdf.finish(5);
        assert_eq!(Document::load(&bytes).expect("load").pages().len(), 1);
    }
}
