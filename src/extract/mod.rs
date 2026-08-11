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

use content::{page_font_refs, PageFonts};
use font::PdfFont;
use image::{extract_image, page_xobject_refs, PageImages};

const MAX_COLLECTED_FORMS: usize = 4_096;
const MAX_COLLECTED_FORM_BYTES: usize = 64 * 1024 * 1024;
const MAX_FORM_RESOURCE_CANDIDATES: usize = 16_384;

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

/// One unit of per-page extraction work: page id, font name → object id map,
/// and image name → output filename map. Pre-built once and shipped across
/// the worker pool so the hot loop touches only borrowed references.
type PageJob<'a> = (
    ObjectId,
    &'a HashMap<Vec<u8>, ObjectId>,
    &'a HashMap<Vec<u8>, ObjectId>,
    &'a HashMap<Vec<u8>, String>,
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
    let prepared_forms = PreparedForms {
        xobjects: &forms,
        fonts: &form_fonts,
    };

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
        .zip(page_xobject_refs_per_page.iter())
        .zip(page_image_filenames.iter())
        .map(|(((page_id, font_refs), xobject_refs), names)| {
            (*page_id, font_refs, xobject_refs, names)
        })
        .collect();
    let page_texts: Vec<String> = parallel_map(
        &inputs,
        |(page_id, font_refs, xobject_refs, image_names)| {
            extract_one_page(
                &doc,
                *page_id,
                font_refs,
                xobject_refs,
                &font_cache,
                image_names,
                &prepared_forms,
            )
            .unwrap_or_default()
        },
    );

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

/// Walk every page's XObject dict, pull out the images we can pass through,
/// and assign each one a stable filename (shared if multiple pages
/// reference the same XObject). Returns the extracted images plus per-page
/// `name → filename` maps for the content interpreter.
fn collect_images(
    doc: &Document<'_>,
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
    doc: &Document<'_>,
    page_id: ObjectId,
    font_refs: &HashMap<Vec<u8>, ObjectId>,
    xobject_refs: &HashMap<Vec<u8>, ObjectId>,
    font_cache: &HashMap<ObjectId, PdfFont>,
    image_names: &HashMap<Vec<u8>, String>,
    forms: &PreparedForms<'_, '_>,
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
    Some(content::extract_page_text_with_forms(
        &content_bytes,
        &fonts,
        xobject_refs,
        &images,
        forms.xobjects,
        forms.fonts,
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
        let image_names: HashMap<Vec<u8>, String> = HashMap::new();
        let form_xobjects = HashMap::new();
        let form_fonts = HashMap::new();
        let forms = PreparedForms {
            xobjects: &form_xobjects,
            fonts: &form_fonts,
        };
        assert!(extract_one_page(
            &doc,
            page,
            &font_refs,
            &xobject_refs,
            &font_cache,
            &image_names,
            &forms,
        )
        .is_none());
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
        let xobject_refs = HashMap::new();
        let font_cache: HashMap<ObjectId, PdfFont> = HashMap::new();
        let form_xobjects = HashMap::new();
        let form_fonts = HashMap::new();
        let forms = PreparedForms {
            xobjects: &form_xobjects,
            fonts: &form_fonts,
        };
        let out = extract_one_page(
            &doc,
            page,
            &font_refs,
            &xobject_refs,
            &font_cache,
            &image_names,
            &forms,
        )
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

    #[test]
    fn collect_images_skips_unsupported_xobjects() {
        let mut res: Dictionary = Dictionary::new();
        let mut xobj = Dictionary::new();
        xobj.insert(b"Im1".to_vec(), Object::Reference(ObjectId(99, 0)));
        res.insert(b"XObject".to_vec(), Object::Dictionary(xobj));
        let resources = vec![Some(res)];
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
        let (images, per_page) = collect_images(&doc, &resources);
        assert!(images.is_empty());
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
