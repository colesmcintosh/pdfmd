//! From-scratch PDF text extractor.

use std::collections::{HashMap, HashSet};
use std::thread;

use crate::pdf::{Dictionary, Document, Object, ObjectId, PdfError};

mod cmap;
mod content;
mod encoding;
mod font;
mod glyphs;
mod image;
mod parser;

use content::{page_font_refs, PageFonts};
use font::PdfFont;
use image::{extract_image, page_xobject_refs, PageImages};

pub use image::ExtractedImage;

/// One unit of per-page extraction work: page id, font name → object id map,
/// and image name → output filename map. Pre-built once and shipped across
/// the worker pool so the hot loop touches only borrowed references.
type PageJob<'a> = (
    ObjectId,
    &'a HashMap<Vec<u8>, ObjectId>,
    &'a HashMap<Vec<u8>, String>,
);

/// Extract the textual content of a PDF document. Pages are returned as
/// independent strings so callers don't pay for a join/split round trip.
///
/// When `extract_images` is true, image XObjects in pass-through filters
/// (JPEG, JPEG 2000) are collected and the returned text carries inline
/// markers — `\u{0001}filename\u{0001}` — at the position each image was
/// painted, for the markdown layer to rewrite into `![]()` references.
pub fn extract_text(
    pdf_bytes: &[u8],
    extract_images: bool,
) -> Result<(Vec<String>, Vec<ExtractedImage>), PdfError> {
    let doc = Document::load(pdf_bytes)?;
    let pages: Vec<ObjectId> = doc.pages().to_vec();

    let resources: Vec<Option<Dictionary>> = pages
        .iter()
        .map(|&page_id| page_resources(&doc, page_id))
        .collect();

    // Serial pre-pass: walk each page's /Resources/Font to collect
    // (name → ObjectId) maps. Cheap because no font is parsed yet, and it
    // lets us deduplicate fonts shared across pages.
    let page_font_refs_per_page: Vec<HashMap<Vec<u8>, ObjectId>> = resources
        .iter()
        .map(|r| {
            r.as_ref()
                .map(|r| page_font_refs(&doc, r))
                .unwrap_or_default()
        })
        .collect();

    // Parse each unique font exactly once, in parallel.
    let unique_ids: Vec<ObjectId> = page_font_refs_per_page
        .iter()
        .flat_map(|m| m.values().copied())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let font_cache: HashMap<ObjectId, PdfFont> =
        parallel_map(&unique_ids, |&id| (id, PdfFont::from_object(&doc, id)))
            .into_iter()
            .collect();

    // Image pre-pass. Only runs when the caller asked for images; otherwise
    // we leave the per-page maps empty so the content interpreter never
    // sees any image references.
    let (images, page_image_filenames) = if extract_images {
        collect_images(&doc, &resources)
    } else {
        (Vec::new(), vec![HashMap::new(); pages.len()])
    };

    // Fan out per-page text extraction across worker threads.
    let inputs: Vec<PageJob<'_>> = pages
        .iter()
        .zip(page_font_refs_per_page.iter())
        .zip(page_image_filenames.iter())
        .map(|((page_id, refs), names)| (*page_id, refs, names))
        .collect();
    let page_texts: Vec<String> = parallel_map(&inputs, |(page_id, font_refs, image_names)| {
        extract_one_page(&doc, *page_id, font_refs, &font_cache, image_names).unwrap_or_default()
    });

    Ok((page_texts, images))
}

/// Walk every page's XObject dict, pull out the images we can pass through,
/// and assign each one a stable filename (shared if multiple pages
/// reference the same XObject). Returns the extracted images plus per-page
/// `name → filename` maps for the content interpreter.
fn collect_images(
    doc: &Document,
    resources: &[Option<Dictionary>],
) -> (Vec<ExtractedImage>, Vec<HashMap<Vec<u8>, String>>) {
    let mut images: Vec<ExtractedImage> = Vec::new();
    let mut filename_by_object: HashMap<ObjectId, String> = HashMap::new();
    let mut per_page: Vec<HashMap<Vec<u8>, String>> = Vec::with_capacity(resources.len());

    for r in resources {
        let mut page_map: HashMap<Vec<u8>, String> = HashMap::new();
        let Some(res) = r else {
            per_page.push(page_map);
            continue;
        };

        for (name, obj_id) in page_xobject_refs(doc, res) {
            // Already extracted on an earlier page — just reuse the name.
            if let Some(filename) = filename_by_object.get(&obj_id) {
                page_map.insert(name, filename.clone());
                continue;
            }
            let Some((ext, bytes)) = extract_image(doc, obj_id) else {
                continue;
            };
            let filename = format!("img-{:03}.{}", images.len() + 1, ext);
            filename_by_object.insert(obj_id, filename.clone());
            page_map.insert(name, filename.clone());
            images.push(ExtractedImage { filename, bytes });
        }
        per_page.push(page_map);
    }

    (images, per_page)
}

fn extract_one_page(
    doc: &Document,
    page_id: ObjectId,
    font_refs: &HashMap<Vec<u8>, ObjectId>,
    font_cache: &HashMap<ObjectId, PdfFont>,
    image_names: &HashMap<Vec<u8>, String>,
) -> Option<String> {
    let fonts: PageFonts<'_> = font_refs
        .iter()
        .filter_map(|(name, id)| font_cache.get(id).map(|f| (name.clone(), f)))
        .collect();
    let images: PageImages<'_> = image_names
        .iter()
        .map(|(name, filename)| (name.clone(), filename.as_str()))
        .collect();
    let content_bytes = doc.get_page_content(page_id)?;
    Some(content::extract_page_text(&content_bytes, &fonts, &images))
}

/// Walk up the page tree until we find a `/Resources` dictionary.
fn page_resources(doc: &Document, page_id: ObjectId) -> Option<Dictionary> {
    let mut current = page_id;
    for _ in 0..64 {
        let dict = doc.get_object(current)?.as_dict()?;
        if let Some(res) = dict.get(b"Resources") {
            return match res {
                Object::Reference(id) => doc.get_object(*id)?.as_dict().cloned(),
                Object::Dictionary(d) => Some(d.clone()),
                _ => None,
            };
        }
        let parent = dict.get(b"Parent")?;
        let Object::Reference(parent_id) = parent else {
            return None;
        };
        current = *parent_id;
    }
    None
}

/// Tiny work-stealing-free parallel map: split into one chunk per worker
/// thread and `Vec::extend` the partial results in place. Stays
/// dependency-free and is fast enough that the per-page cost dominates.
fn parallel_map<T, R, F>(input: &[T], f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync + Send,
{
    let len = input.len();
    if len == 0 {
        return Vec::new();
    }
    // Available_parallelism returns 0 on error; clamp to 1.
    let workers = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .max(1)
        .min(len);
    if workers == 1 {
        return input.iter().map(&f).collect();
    }
    let chunk = (len + workers - 1) / workers;
    // Pre-size the output so each worker can write into its own slice.
    let mut out: Vec<Option<R>> = (0..len).map(|_| None).collect();
    thread::scope(|s| {
        let f = &f;
        for (in_chunk, out_chunk) in input.chunks(chunk).zip(out.chunks_mut(chunk)) {
            s.spawn(move || {
                for (slot, item) in out_chunk.iter_mut().zip(in_chunk) {
                    *slot = Some(f(item));
                }
            });
        }
    });
    out.into_iter().map(Option::unwrap).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_map_handles_empty_input() {
        let out: Vec<i32> = parallel_map::<i32, i32, _>(&[], |x| *x);
        assert!(out.is_empty());
    }

    #[test]
    fn parallel_map_single_worker_path_runs_serially() {
        // A 1-element input forces the workers.min(len) clamp to 1, which
        // takes the serial fast path.
        let out = parallel_map(&[42], |x| x * 2);
        assert_eq!(out, vec![84]);
    }

    #[test]
    fn parallel_map_distributes_work_across_workers() {
        let input: Vec<i32> = (0..32).collect();
        let out = parallel_map(&input, |x| x * x);
        let expected: Vec<i32> = input.iter().map(|x| x * x).collect();
        assert_eq!(out, expected);
    }

    #[test]
    fn page_resources_inherits_from_parent_pages_node() {
        // Page leaf carries no /Resources but its parent /Pages does.
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1/Resources<</Font<</F1 4 0 R>>>>>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 1 1]>> endobj
4 0 obj <</Type/Font/Subtype/Type1/BaseFont/Helvetica>> endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let page = doc.pages()[0];
        let res = page_resources(&doc, page).expect("inherited resources");
        assert!(res.get(b"Font").is_some());
    }

    #[test]
    fn page_resources_returns_none_when_root_loop_exhausts() {
        // A self-referential /Parent chain — the 64-iteration cap kicks in.
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 3 0 R/MediaBox[0 0 1 1]>> endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let page = doc.pages()[0];
        // No /Resources anywhere along the chain → None (or recursion cap).
        assert!(page_resources(&doc, page).is_none());
    }

    #[test]
    fn page_resources_follows_resources_reference() {
        // /Resources is itself an indirect reference.
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources 4 0 R/MediaBox[0 0 1 1]>> endobj
4 0 obj <</Font<</F1 5 0 R>>>> endobj
5 0 obj <</Type/Font/Subtype/Type1/BaseFont/Helvetica>> endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let page = doc.pages()[0];
        let res = page_resources(&doc, page).unwrap();
        assert!(res.get(b"Font").is_some());
    }

    #[test]
    fn page_resources_returns_none_for_unsupported_resources_object() {
        // /Resources points at an Integer — neither Reference nor Dictionary.
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources 42/MediaBox[0 0 1 1]>> endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let page = doc.pages()[0];
        assert!(page_resources(&doc, page).is_none());
    }

    #[test]
    fn page_resources_returns_none_when_resources_reference_misses() {
        // /Resources is a Reference to a non-existent object.
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources 99 0 R/MediaBox[0 0 1 1]>> endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let page = doc.pages()[0];
        assert!(page_resources(&doc, page).is_none());
    }

    #[test]
    fn page_resources_returns_none_when_parent_is_not_a_reference() {
        // The page has /Parent set to an Integer rather than a Reference.
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 42/MediaBox[0 0 1 1]>> endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let page = doc.pages()[0];
        assert!(page_resources(&doc, page).is_none());
    }

    #[test]
    fn extract_one_page_returns_none_when_page_has_no_content() {
        // Page dict without /Contents — get_page_content returns None.
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let page = doc.pages()[0];
        let font_refs = HashMap::new();
        let font_cache: HashMap<ObjectId, PdfFont> = HashMap::new();
        let image_names: HashMap<Vec<u8>, String> = HashMap::new();
        assert!(extract_one_page(&doc, page, &font_refs, &font_cache, &image_names).is_none());
    }

    #[test]
    fn extract_one_page_populates_image_names_for_caller() {
        // The closure that builds PageImages from image_names runs only
        // when image_names has entries. Drive it directly so the .map()
        // closure region gets covered.
        let mut image_names = HashMap::new();
        image_names.insert(b"Im1".to_vec(), "figs/x.jpg".to_string());

        // Minimal PDF with a single empty page so doc.get_page_content
        // gives us back at least a `Do` operator that references the
        // image name above.
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]/Contents 4 0 R>> endobj
4 0 obj <</Length 7>>
stream
/Im1 Do
endstream
endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let page = doc.pages()[0];
        let font_refs = HashMap::new();
        let font_cache: HashMap<ObjectId, PdfFont> = HashMap::new();
        let out = extract_one_page(&doc, page, &font_refs, &font_cache, &image_names)
            .expect("extract one page");
        // The `Do` operator emits the rewritten filename through the
        // marker; checking for the substring is enough.
        assert!(out.contains("figs/x.jpg"));
    }

    #[test]
    fn collect_images_dedupes_across_pages() {
        // Two pages reference the same image XObject; only one entry should
        // end up in the extracted image list and both per-page maps should
        // point at the same filename.
        let mut res: Dictionary = Dictionary::new();
        let mut xobj = Dictionary::new();
        xobj.insert(b"Im1".to_vec(), Object::Reference(ObjectId(99, 0)));
        res.insert(b"XObject".to_vec(), Object::Dictionary(xobj));
        let resources = vec![Some(res.clone()), Some(res)];
        // We need a doc that has obj 99 as a JPEG image.
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
99 0 obj <</Subtype/Image/Filter/DCTDecode/Length 3>>
stream
JPG
endstream
endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let (images, per_page) = collect_images(&doc, &resources);
        assert_eq!(images.len(), 1);
        assert_eq!(per_page.len(), 2);
        // Both pages map Im1 → the same filename.
        assert_eq!(
            per_page[0].get(b"Im1".as_slice()),
            per_page[1].get(b"Im1".as_slice())
        );
    }

    #[test]
    fn collect_images_handles_none_resources_entries() {
        // A page with no Resources dict at all (None) must not crash.
        let resources: Vec<Option<Dictionary>> = vec![None];
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let (images, per_page) = collect_images(&doc, &resources);
        assert!(images.is_empty());
        assert_eq!(per_page.len(), 1);
        assert!(per_page[0].is_empty());
    }

    /// Builder for in-test PDFs with a classic xref table. Scans the body
    /// for `N 0 obj` markers and emits offsets for every contiguous id
    /// it finds, padding the gap with `f` entries.
    fn build_xref_pdf(body: &[u8]) -> Vec<u8> {
        let mut out = body.to_vec();
        let xref_offset = out.len();
        let mut found: Vec<(u32, usize)> = Vec::new();
        for n in 1u32..200 {
            let needle = format!("{n} 0 obj");
            if let Some(off) = (0..=out.len().saturating_sub(needle.len()))
                .find(|&i| out[i..i + needle.len()] == *needle.as_bytes())
            {
                found.push((n, off));
            }
        }
        let max = found.iter().map(|(n, _)| *n).max().unwrap_or(0);
        let mut xref = String::from("xref\n");
        xref.push_str(&format!("0 {}\n", max + 1));
        xref.push_str("0000000000 65535 f \n");
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
}
