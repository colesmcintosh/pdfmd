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
