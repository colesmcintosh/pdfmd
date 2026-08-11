//! From-scratch PDF text extractor.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::thread;

use crate::pdf::{Dictionary, Document, Object, ObjectId, PdfError};

mod cmap;
mod content;
mod encoding;
mod font;
mod glyphs;
mod image;
mod parser;

use content::{page_font_refs, ImageFilenames, PageFonts};
use font::PdfFont;
use image::{extract_image, page_xobject_refs};

const MAX_COLLECTED_FORMS: usize = 4_096;
const MAX_COLLECTED_FORM_BYTES: usize = 64 * 1024 * 1024;
const MAX_FORM_RESOURCE_CANDIDATES: usize = 16_384;
const MAX_FORM_IMAGE_CANDIDATES: usize = 16_384;

#[derive(Clone, Copy)]
struct FormCollectionLimits {
    count: usize,
    decoded_bytes: usize,
    candidates: usize,
}

const FORM_COLLECTION_LIMITS: FormCollectionLimits = FormCollectionLimits {
    count: MAX_COLLECTED_FORMS,
    decoded_bytes: MAX_COLLECTED_FORM_BYTES,
    candidates: MAX_FORM_RESOURCE_CANDIDATES,
};

pub use image::ExtractedImage;

/// One unit of per-page extraction work: page id plus font and XObject
/// resource maps. Pre-built once and shipped across the worker pool so the
/// hot loop touches only borrowed references.
type PageJob<'a> = (
    ObjectId,
    &'a HashMap<Vec<u8>, ObjectId>,
    &'a HashMap<Vec<u8>, ObjectId>,
);

/// Decoded Form XObject plus its optional local resource maps. Forms with no
/// `/Resources` entry use the resource context from the `Do` that invokes
/// them, as required for older producer output.
pub(super) struct FormXObject {
    content: Vec<u8>,
    font_refs: Option<HashMap<Vec<u8>, ObjectId>>,
    xobject_refs: Option<HashMap<Vec<u8>, ObjectId>>,
}

struct PreparedForms<'a, 'fonts> {
    xobjects: &'a HashMap<ObjectId, FormXObject>,
    fonts: &'a HashMap<ObjectId, PageFonts<'fonts>>,
    images: &'a ImageFilenames<'a>,
}

/// Extract the textual content of a PDF document. Pages are returned as
/// independent strings so callers don't pay for a join/split round trip.
///
/// When `extract_images` is true, supported image XObjects are collected
/// and the returned text carries inline
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

    let page_xobject_refs_per_page: Vec<HashMap<Vec<u8>, ObjectId>> = resources
        .iter()
        .map(|r| {
            r.as_ref()
                .map(|r| page_xobject_refs(&doc, r))
                .unwrap_or_default()
        })
        .collect();

    // Decode every reachable Form XObject once. Its content is interpreted
    // at each `Do`, but shared decoding and resource discovery stay out of
    // the parallel page hot path.
    let forms = collect_forms(&doc, &page_xobject_refs_per_page);

    // Parse each unique font exactly once, in parallel.
    let unique_ids: Vec<ObjectId> = page_font_refs_per_page
        .iter()
        .flat_map(|m| m.values().copied())
        .chain(
            forms
                .values()
                .filter_map(|form| form.font_refs.as_ref())
                .flat_map(|m| m.values().copied()),
        )
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let font_cache: HashMap<ObjectId, PdfFont> =
        parallel_map(&unique_ids, |&id| (id, PdfFont::from_object(&doc, id)))
            .into_iter()
            .collect();

    let form_fonts: HashMap<ObjectId, PageFonts<'_>> = forms
        .iter()
        .filter_map(|(&id, form)| {
            form.font_refs.as_ref().map(|refs| {
                let fonts = refs
                    .iter()
                    .filter_map(|(name, font_id)| {
                        font_cache.get(font_id).map(|font| (name.clone(), font))
                    })
                    .collect();
                (id, fonts)
            })
        })
        .collect();
    // Image pre-pass. Only runs when the caller asked for images; otherwise
    // the object-to-filename map stays empty and every image `Do` is ignored.
    let (images, image_filenames) = if extract_images {
        collect_images(&doc, &page_xobject_refs_per_page, &forms)
    } else {
        (Vec::new(), HashMap::new())
    };
    let image_names: ImageFilenames<'_> = image_filenames
        .iter()
        .map(|(&id, filename)| (id, filename.as_str()))
        .collect();
    let prepared_forms = PreparedForms {
        xobjects: &forms,
        fonts: &form_fonts,
        images: &image_names,
    };

    // Fan out per-page text extraction across worker threads.
    let inputs: Vec<PageJob<'_>> = pages
        .iter()
        .zip(page_font_refs_per_page.iter())
        .zip(page_xobject_refs_per_page.iter())
        .map(|((page_id, font_refs), xobject_refs)| (*page_id, font_refs, xobject_refs))
        .collect();
    let page_texts: Vec<String> = parallel_map(&inputs, |(page_id, font_refs, xobject_refs)| {
        extract_one_page(
            &doc,
            *page_id,
            font_refs,
            xobject_refs,
            &font_cache,
            &prepared_forms,
        )
        .unwrap_or_default()
    });

    Ok((page_texts, images))
}

/// Walk the XObject resource graph and prepare every reachable Form. The
/// visited set prevents malformed resource cycles from making this pre-pass
/// loop forever; invocation cycles are handled separately by the interpreter.
fn collect_forms(
    doc: &Document<'_>,
    page_xobjects: &[HashMap<Vec<u8>, ObjectId>],
) -> HashMap<ObjectId, FormXObject> {
    collect_forms_with_limits(doc, page_xobjects, FORM_COLLECTION_LIMITS)
}

fn collect_forms_with_limits(
    doc: &Document<'_>,
    page_xobjects: &[HashMap<Vec<u8>, ObjectId>],
    limits: FormCollectionLimits,
) -> HashMap<ObjectId, FormXObject> {
    let mut visited = HashSet::new();
    let mut pending = BTreeSet::new();
    for id in page_xobjects.iter().flat_map(|refs| refs.values().copied()) {
        retain_form_candidate(doc, id, limits.candidates, &visited, &mut pending);
    }
    let mut forms = HashMap::new();
    let mut decoded_bytes = 0usize;

    while forms.len() < limits.count && decoded_bytes < limits.decoded_bytes {
        let Some(id) = pending.iter().next().copied() else {
            break;
        };
        pending.remove(&id);
        if !visited.insert(id) {
            continue;
        }
        let Some(stream) = doc.get_object(id).and_then(Object::as_stream) else {
            continue;
        };
        if stream.dict.get(b"Subtype").and_then(Object::as_name) != Some(b"Form".as_slice()) {
            continue;
        }

        let resource_dict = stream
            .dict
            .get(b"Resources")
            .and_then(|obj| doc.deref(obj).as_dict());
        let font_refs = resource_dict.map(|r| page_font_refs(doc, r));
        let xobject_refs = resource_dict.map(|r| page_xobject_refs(doc, r));

        let Ok(content) = doc.decode_stream(stream) else {
            continue;
        };
        if content.len() > limits.decoded_bytes.saturating_sub(decoded_bytes) {
            continue;
        }
        decoded_bytes += content.len();
        if let Some(refs) = &xobject_refs {
            for id in refs.values().copied() {
                retain_form_candidate(doc, id, limits.candidates, &visited, &mut pending);
            }
        }
        forms.insert(
            id,
            FormXObject {
                content,
                font_refs,
                xobject_refs,
            },
        );
    }

    forms
}

fn retain_form_candidate(
    doc: &Document<'_>,
    id: ObjectId,
    limit: usize,
    visited: &HashSet<ObjectId>,
    pending: &mut BTreeSet<ObjectId>,
) {
    if visited.contains(&id)
        || pending.contains(&id)
        || doc
            .get_object(id)
            .and_then(Object::as_stream)
            .and_then(|stream| stream.dict.get(b"Subtype"))
            .and_then(Object::as_name)
            != Some(b"Form".as_slice())
    {
        return;
    }

    pending.insert(id);
    if visited.len().saturating_add(pending.len()) > limit {
        // HashMap iteration order is unspecified. Keeping the smallest IDs
        // makes a truncated resource walk stable across runs.
        pending.pop_last();
    }
}

/// Gather image XObjects reachable from page and Form resource contexts,
/// extract each object once, and assign deterministic filenames by object ID.
fn collect_images(
    doc: &Document<'_>,
    page_xobjects: &[HashMap<Vec<u8>, ObjectId>],
    forms: &HashMap<ObjectId, FormXObject>,
) -> (Vec<ExtractedImage>, HashMap<ObjectId, String>) {
    collect_images_with_limit(doc, page_xobjects, forms, MAX_FORM_IMAGE_CANDIDATES)
}

fn collect_images_with_limit(
    doc: &Document<'_>,
    page_xobjects: &[HashMap<Vec<u8>, ObjectId>],
    forms: &HashMap<ObjectId, FormXObject>,
    candidate_limit: usize,
) -> (Vec<ExtractedImage>, HashMap<ObjectId, String>) {
    // Page-direct candidates were historically unlimited. Keep all of them,
    // and bound only the newly supported Form-local expansion so a hostile
    // Form graph cannot evict or suppress an image painted directly by a page.
    let mut page_candidates = BTreeSet::new();
    for id in page_xobjects.iter().flat_map(|refs| refs.values().copied()) {
        if is_image_xobject(doc, id) {
            page_candidates.insert(id);
        }
    }
    let mut form_candidates = BTreeSet::new();
    for id in forms
        .values()
        .filter_map(|form| form.xobject_refs.as_ref())
        .flat_map(|refs| refs.values().copied())
    {
        if !page_candidates.contains(&id) {
            retain_image_candidate(doc, id, candidate_limit, &mut form_candidates);
        }
    }
    page_candidates.extend(form_candidates);

    let mut images = Vec::new();
    let mut filename_by_object = HashMap::new();
    for obj_id in page_candidates {
        if let Some((ext, bytes)) = extract_image(doc, obj_id) {
            let filename = format!("img-{:03}.{}", images.len() + 1, ext);
            filename_by_object.insert(obj_id, filename.clone());
            images.push(ExtractedImage { filename, bytes });
        }
    }
    (images, filename_by_object)
}

fn retain_image_candidate(
    doc: &Document<'_>,
    id: ObjectId,
    limit: usize,
    candidates: &mut BTreeSet<ObjectId>,
) {
    if candidates.contains(&id) || !is_image_xobject(doc, id) {
        return;
    }

    candidates.insert(id);
    if candidates.len() > limit {
        candidates.pop_last();
    }
}

fn is_image_xobject(doc: &Document<'_>, id: ObjectId) -> bool {
    doc.get_object(id)
        .and_then(Object::as_stream)
        .and_then(|stream| stream.dict.get(b"Subtype"))
        .and_then(Object::as_name)
        == Some(b"Image".as_slice())
}

fn extract_one_page(
    doc: &Document<'_>,
    page_id: ObjectId,
    font_refs: &HashMap<Vec<u8>, ObjectId>,
    xobject_refs: &HashMap<Vec<u8>, ObjectId>,
    font_cache: &HashMap<ObjectId, PdfFont>,
    forms: &PreparedForms<'_, '_>,
) -> Option<String> {
    let fonts: PageFonts<'_> = font_refs
        .iter()
        .filter_map(|(name, id)| font_cache.get(id).map(|f| (name.clone(), f)))
        .collect();
    let content_bytes = doc.get_page_content(page_id)?;
    Some(content::extract_page_text_with_forms(
        &content_bytes,
        &fonts,
        xobject_refs,
        forms.xobjects,
        forms.fonts,
        forms.images,
    ))
}

/// Walk up the page tree until we find a `/Resources` dictionary.
fn page_resources(doc: &Document<'_>, page_id: ObjectId) -> Option<Dictionary> {
    let mut current = page_id;
    for _ in 0..64 {
        let dict = doc.get_object(current).and_then(Object::as_dict)?;
        if let Some(res) = dict.get(b"Resources") {
            return match res {
                Object::Reference(id) => doc.get_object(*id).and_then(Object::as_dict).cloned(),
                Object::Dictionary(d) => Some(d.clone()),
                _ => None,
            };
        }
        let parent = dict.get(b"Parent").and_then(Object::as_reference)?;
        current = parent;
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
    fn extract_text_propagates_document_load_error() {
        assert!(extract_text(b"not a pdf", false).is_err());
    }

    #[test]
    fn extract_text_collects_images_only_when_requested() {
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<</XObject<</Im1 4 0 R>>>>/MediaBox[0 0 1 1]>> endobj
4 0 obj <</Subtype/Image/Filter/DCTDecode/Length 3>>
stream
JPG
endstream
endobj
";
        let bytes = build_xref_pdf(pdf);

        let (pages, images) = extract_text(&bytes, false).unwrap();
        assert_eq!(pages, vec![String::new()]);
        assert!(images.is_empty());

        let (pages, images) = extract_text(&bytes, true).unwrap();
        assert_eq!(pages, vec![String::new()]);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].filename, "img-001.jpg");
    }

    #[test]
    fn forms_preserve_order_resources_reuse_and_cycles() {
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<</Font<</F1 6 0 R>>/XObject<</Fm1 8 0 R>>>>/MediaBox[0 0 100 100]/Contents 4 0 R>> endobj
4 0 obj <</Length 20 0 R>>
stream
BT /F1 12 Tf (A) Tj ET /Fm1 Do BT /F1 12 Tf (A) Tj ET /Fm1 Do
endstream
endobj
6 0 obj <</Type/Font/Subtype/Type1/Encoding<</Differences[65/X]>>>> endobj
7 0 obj <</Type/Font/Subtype/Type1/Encoding<</Differences[65/Y]>>>> endobj
8 0 obj <</Type/XObject/Subtype/Form/BBox[0 0 10 10]/Resources<</Font<</F1 7 0 R>>/XObject<</Nested 10 0 R/Self 8 0 R/Fallback 11 0 R>>>>/Length 20 0 R>>
stream
BT /F1 12 Tf (A) Tj ET /Nested Do /Self Do /Fallback Do
endstream
endobj
9 0 obj <</Type/Font/Subtype/Type1/Encoding<</Differences[65/Z]>>>> endobj
10 0 obj <</Type/XObject/Subtype/Form/BBox[0 0 10 10]/Resources<</Font<</F1 9 0 R>>/XObject<</Back 8 0 R>>>>/Length 20 0 R>>
stream
BT /F1 12 Tf (A) Tj ET /Back Do
endstream
endobj
11 0 obj <</Type/XObject/Subtype/Form/BBox[0 0 10 10]/Length 20 0 R>>
stream
BT /F1 12 Tf (A) Tj ET
endstream
endobj
";
        let bytes = build_xref_pdf(pdf);
        let (pages, images) = extract_text(&bytes, false).unwrap();

        // Page, local form, nested form, and inherited-resource form fonts
        // decode A as X/Y/Z/Y. The active-stack guard drops Self and Back,
        // while popping after each paint lets the second Fm1 run normally.
        assert_eq!(pages, vec!["X Y Z Y X Y Z Y"]);
        assert!(images.is_empty());
    }

    #[test]
    fn resource_less_form_inherits_and_isolates_text_state() {
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<</Font<</F1 6 0 R/F2 7 0 R>>/XObject<</Fm 8 0 R>>>>/MediaBox[0 0 100 100]/Contents 4 0 R>> endobj
4 0 obj <</Length 20 0 R>>
stream
BT /F1 12 Tf 14 TL ET /Fm Do BT 1 0 0 1 0 100 Tm (A) Tj T* (A) Tj ET
endstream
endobj
6 0 obj <</Type/Font/Subtype/Type1/Encoding<</Differences[65/X]>>>> endobj
7 0 obj <</Type/Font/Subtype/Type1/Encoding<</Differences[65/Y]>>>> endobj
8 0 obj <</Type/XObject/Subtype/Form/BBox[0 0 10 10]/Length 20 0 R>>
stream
BT 1 0 0 1 0 100 Tm (A) Tj T* (A) Tj /F2 24 Tf 99 TL ET
endstream
endobj
";
        let bytes = build_xref_pdf(pdf);
        let (pages, _) = extract_text(&bytes, false).unwrap();

        // The Form starts with the page's selected F1/12/14 state, then its
        // F2/24/99 changes stay local. Both inherited 14-unit advances remain
        // normal line breaks and the outer text continues in F1.
        assert_eq!(pages, vec!["X\nX X\nX"]);
    }

    #[test]
    fn forms_extract_local_images_in_paint_order_with_resource_inheritance() {
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<</XObject<</Outer 8 0 R/Im 9 0 R/Empty 14 0 R>>>>/MediaBox[0 0 100 100]/Contents 4 0 R>> endobj
4 0 obj <</Length 20 0 R>>
stream
/Im Do /Outer Do /Empty Do /Outer Do /Im Do
endstream
endobj
8 0 obj <</Type/XObject/Subtype/Form/BBox[0 0 10 10]/Resources<</XObject<</Im 10 0 R/Inherited 11 0 R/Nested 12 0 R>>>>/Length 20 0 R>>
stream
/Im Do /Inherited Do /Nested Do
endstream
endobj
9 0 obj <</Type/XObject/Subtype/Image/Filter/DCTDecode/Length 3>>
stream
PAG
endstream
endobj
10 0 obj <</Type/XObject/Subtype/Image/Filter/DCTDecode/Length 3>>
stream
LOC
endstream
endobj
11 0 obj <</Type/XObject/Subtype/Form/BBox[0 0 10 10]/Length 20 0 R>>
stream
/Im Do
endstream
endobj
12 0 obj <</Type/XObject/Subtype/Form/BBox[0 0 10 10]/Resources<</XObject<</Im 13 0 R>>>>/Length 20 0 R>>
stream
/Im Do
endstream
endobj
13 0 obj <</Type/XObject/Subtype/Image/Filter/DCTDecode/Length 3>>
stream
NST
endstream
endobj
14 0 obj <</Type/XObject/Subtype/Form/BBox[0 0 10 10]/Resources<<>>/Length 20 0 R>>
stream
/Im Do
endstream
endobj
";
        let bytes = build_xref_pdf(pdf);

        let (pages, images) = extract_text(&bytes, false).unwrap();
        assert_eq!(pages, vec![String::new()]);
        assert!(images.is_empty());

        let (pages, images) = extract_text(&bytes, true).unwrap();
        let painted: Vec<&str> = pages[0]
            .split(content::IMAGE_MARK)
            .enumerate()
            .filter_map(|(index, part)| (index % 2 == 1).then_some(part))
            .collect();
        assert_eq!(
            painted,
            vec![
                "img-001.jpg",
                "img-002.jpg",
                "img-002.jpg",
                "img-003.jpg",
                "img-002.jpg",
                "img-002.jpg",
                "img-003.jpg",
                "img-001.jpg",
            ]
        );
        assert_eq!(images.len(), 3);
        assert_eq!(images[0].bytes, b"PAG");
        assert_eq!(images[1].bytes, b"LOC");
        assert_eq!(images[2].bytes, b"NST");
    }

    #[test]
    fn collect_forms_respects_deterministic_count_and_byte_limits() {
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
8 0 obj <</Subtype/Form/Length 4>>
stream
ABCD
endstream
endobj
9 0 obj <</Subtype/Form/Length 3>>
stream
EFG
endstream
endobj
10 0 obj <</Subtype/Form/Length 1>>
stream
H
endstream
endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let page_xobjects = vec![HashMap::from([
            (b"High".to_vec(), ObjectId(10, 0)),
            (b"Low".to_vec(), ObjectId(8, 0)),
            (b"Middle".to_vec(), ObjectId(9, 0)),
        ])];

        let count_limited = collect_forms_with_limits(
            &doc,
            &page_xobjects,
            FormCollectionLimits {
                count: 2,
                decoded_bytes: usize::MAX,
                candidates: usize::MAX,
            },
        );
        let mut count_ids: Vec<ObjectId> = count_limited.keys().copied().collect();
        count_ids.sort_unstable();
        assert_eq!(count_ids, vec![ObjectId(8, 0), ObjectId(9, 0)]);

        let byte_limited = collect_forms_with_limits(
            &doc,
            &page_xobjects,
            FormCollectionLimits {
                count: usize::MAX,
                decoded_bytes: 5,
                candidates: usize::MAX,
            },
        );
        let mut byte_ids: Vec<ObjectId> = byte_limited.keys().copied().collect();
        byte_ids.sort_unstable();
        assert_eq!(byte_ids, vec![ObjectId(8, 0), ObjectId(10, 0)]);
        assert_eq!(
            byte_limited
                .values()
                .map(|form| form.content.len())
                .sum::<usize>(),
            5
        );
    }

    #[test]
    fn collect_forms_bounds_initial_and_nested_candidate_frontiers() {
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
6 0 obj <</Subtype/Image/Length 0>>
stream

endstream
endobj
7 0 obj <</Subtype/Form/Length 0>>
stream

endstream
endobj
8 0 obj <</Subtype/Form/Resources<</XObject<</Nested 7 0 R/NotForm 6 0 R>>>>/Length 0>>
stream

endstream
endobj
9 0 obj <</Subtype/Form/Length 0>>
stream

endstream
endobj
10 0 obj <</Subtype/Form/Length 0>>
stream

endstream
endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();

        // A lower-numbered image must not consume the single Form slot, and
        // unordered resource-map iteration must still retain the lowest Form.
        let initial = vec![HashMap::from([
            (b"High".to_vec(), ObjectId(10, 0)),
            (b"Low".to_vec(), ObjectId(9, 0)),
            (b"Image".to_vec(), ObjectId(6, 0)),
        ])];
        let initial_limited = collect_forms_with_limits(
            &doc,
            &initial,
            FormCollectionLimits {
                count: usize::MAX,
                decoded_bytes: usize::MAX,
                candidates: 1,
            },
        );
        let mut initial_ids: Vec<ObjectId> = initial_limited.keys().copied().collect();
        initial_ids.sort_unstable();
        assert_eq!(initial_ids, vec![ObjectId(9, 0)]);

        // The nested lower ID replaces the pending sibling without allowing
        // the combined visited + pending frontier to exceed two candidates.
        let nested = vec![HashMap::from([
            (b"Root".to_vec(), ObjectId(8, 0)),
            (b"Sibling".to_vec(), ObjectId(9, 0)),
        ])];
        let nested_limited = collect_forms_with_limits(
            &doc,
            &nested,
            FormCollectionLimits {
                count: usize::MAX,
                decoded_bytes: usize::MAX,
                candidates: 2,
            },
        );
        let mut nested_ids: Vec<ObjectId> = nested_limited.keys().copied().collect();
        nested_ids.sort_unstable();
        assert_eq!(nested_ids, vec![ObjectId(7, 0), ObjectId(8, 0)]);
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
    fn page_resources_returns_none_when_page_object_is_missing() {
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[]/Count 0>> endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        assert!(page_resources(&doc, ObjectId(99, 0)).is_none());
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
        let xobject_refs = HashMap::new();
        let font_cache: HashMap<ObjectId, PdfFont> = HashMap::new();
        let form_xobjects = HashMap::new();
        let form_fonts = HashMap::new();
        let image_names = ImageFilenames::new();
        let forms = PreparedForms {
            xobjects: &form_xobjects,
            fonts: &form_fonts,
            images: &image_names,
        };
        assert!(
            extract_one_page(&doc, page, &font_refs, &xobject_refs, &font_cache, &forms,).is_none()
        );
    }

    #[test]
    fn extract_one_page_resolves_image_object_filename() {
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
        let image_id = ObjectId(99, 0);
        let xobject_refs = HashMap::from([(b"Im1".to_vec(), image_id)]);
        let font_cache: HashMap<ObjectId, PdfFont> = HashMap::new();
        let form_xobjects = HashMap::new();
        let form_fonts = HashMap::new();
        let image_names = HashMap::from([(image_id, "figs/x.jpg")]);
        let forms = PreparedForms {
            xobjects: &form_xobjects,
            fonts: &form_fonts,
            images: &image_names,
        };
        let out = extract_one_page(&doc, page, &font_refs, &xobject_refs, &font_cache, &forms)
            .expect("extract one page");
        // The `Do` operator emits the rewritten filename through the
        // marker; checking for the substring is enough.
        assert!(out.contains("figs/x.jpg"));
    }

    #[test]
    fn collect_images_dedupes_across_pages() {
        // Two pages reference the same image XObject through different local
        // names; object identity still produces one payload and filename.
        let page_xobjects = vec![
            HashMap::from([(b"Im1".to_vec(), ObjectId(99, 0))]),
            HashMap::from([(b"Alias".to_vec(), ObjectId(99, 0))]),
        ];
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
        let forms = HashMap::new();
        let (images, filenames) = collect_images(&doc, &page_xobjects, &forms);
        assert_eq!(images.len(), 1);
        assert_eq!(
            filenames.get(&ObjectId(99, 0)).map(String::as_str),
            Some("img-001.jpg")
        );
    }

    #[test]
    fn collect_images_handles_none_resources_entries() {
        // A page with no XObject resources must not crash.
        let page_xobjects = vec![HashMap::new()];
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let forms = HashMap::new();
        let (images, filenames) = collect_images(&doc, &page_xobjects, &forms);
        assert!(images.is_empty());
        assert!(filenames.is_empty());
    }

    #[test]
    fn collect_images_skips_unsupported_xobjects() {
        let page_xobjects = vec![HashMap::from([(b"Im1".to_vec(), ObjectId(99, 0))])];
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
99 0 obj <</Subtype/Image/Filter/LZWDecode/Length 3>>
stream
BAD
endstream
endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let forms = HashMap::new();
        let (images, filenames) = collect_images(&doc, &page_xobjects, &forms);
        assert!(images.is_empty());
        assert!(filenames.is_empty());
    }

    #[test]
    fn collect_images_bounds_form_candidates_without_dropping_page_images() {
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
90 0 obj <</Subtype/Image/Filter/DCTDecode/Length 3>>
stream
LOW
endstream
endobj
91 0 obj <</Subtype/Image/Filter/DCTDecode/Length 3>>
stream
MID
endstream
endobj
92 0 obj <</Subtype/Image/Filter/DCTDecode/Length 3>>
stream
HIG
endstream
endobj
";
        let bytes = build_xref_pdf(pdf);
        let doc = Document::load(&bytes).unwrap();
        let page_xobjects = vec![HashMap::from([(b"Page".to_vec(), ObjectId(92, 0))])];
        let form_id = ObjectId(8, 0);
        let forms = HashMap::from([(
            form_id,
            FormXObject {
                content: Vec::new(),
                font_refs: Some(HashMap::new()),
                xobject_refs: Some(HashMap::from([
                    (b"Low".to_vec(), ObjectId(90, 0)),
                    (b"Middle".to_vec(), ObjectId(91, 0)),
                ])),
            },
        )]);

        let (images, filenames) = collect_images_with_limit(&doc, &page_xobjects, &forms, 1);
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].bytes, b"LOW");
        assert_eq!(images[1].bytes, b"HIG");
        let mut ids: Vec<ObjectId> = filenames.keys().copied().collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![ObjectId(90, 0), ObjectId(92, 0)]);
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
