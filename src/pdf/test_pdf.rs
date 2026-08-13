//! Shared builders for in-crate PDF fixtures.

use super::{Document, ObjectId};

pub(crate) fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Append a classic xref covering every `N 0 obj` header in `body`. Missing
/// ids between 1 and the highest found number are marked free.
pub(crate) fn build_xref_pdf(body: &[u8]) -> Vec<u8> {
    const MAX_ID: u32 = 256;
    let mut found: Vec<(u32, usize)> = Vec::new();
    for n in 1..=MAX_ID {
        let needle = format!("{n} 0 obj");
        if let Some(off) = find_bytes(body, needle.as_bytes()) {
            found.push((n, off));
        }
    }
    let max = found.iter().map(|(n, _)| *n).max().unwrap_or(0);
    let mut out = body.to_vec();
    let xref_offset = out.len();
    let mut xref = format!("xref\n0 {}\n0000000000 65535 f \n", max + 1);
    for n in 1..=max {
        match found.iter().find(|(m, _)| *m == n) {
            Some((_, off)) => xref.push_str(&format!("{off:010} 00000 n \n")),
            None => xref.push_str("0000000000 00000 f \n"),
        }
    }
    xref.push_str(&format!(
        "trailer <</Size {}/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n",
        max + 1
    ));
    out.extend_from_slice(xref.as_bytes());
    out
}

/// Catalog + empty page, plus extra indirect objects. Bytes are leaked so
/// tests can return `Document<'static>`.
pub(crate) fn load_minimal_doc(extra_objs: &[(u32, &[u8])]) -> Document<'static> {
    let mut body = b"%PDF-1.4\n\
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj\n"
        .to_vec();
    for (n, payload) in extra_objs {
        body.extend_from_slice(format!("{n} 0 obj ").as_bytes());
        body.extend_from_slice(payload);
        body.extend_from_slice(b" endobj\n");
    }
    let bytes = Box::leak(build_xref_pdf(&body).into_boxed_slice());
    Document::load(bytes).expect("load")
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
        let bytes = build_xref_pdf(
            b"%PDF-1.4\n\
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj\n\
8 0 obj 42 endobj\n",
        );
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
}
