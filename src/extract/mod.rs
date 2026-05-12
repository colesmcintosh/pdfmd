//! From-scratch PDF text extractor built on `lopdf` for object parsing.

use std::collections::HashMap;

use anyhow::{Context, Result};
use lopdf::{Document, Object, ObjectId};
use rayon::prelude::*;

mod cmap;
mod content;
mod encoding;
mod font;
mod glyphs;
mod image;

use content::{page_font_refs, PageFonts};
use font::PdfFont;
use image::{extract_image, page_xobject_refs, PageImages};

pub use image::ExtractedImage;

/// Extract the textual content of a PDF document. Pages are separated by an
/// ASCII form-feed character, which the markdown layer splits on.
///
/// When `extract_images` is true, image XObjects in pass-through filters
/// (JPEG, JPEG 2000) are collected and the returned text carries inline
/// markers — `\u{0001}filename\u{0001}` — at the position each image was
/// painted, for the markdown layer to rewrite into `![]()` references.
pub fn extract_text(pdf_bytes: &[u8], extract_images: bool) -> Result<(String, Vec<ExtractedImage>)> {
    let doc = Document::load_mem(pdf_bytes).context("failed to parse PDF")?;

    // `get_pages` returns a BTreeMap already sorted by page number, so the
    // collected vector preserves document order.
    let pages: Vec<(u32, ObjectId)> = doc.get_pages().into_iter().collect();

    let resources: Vec<Option<lopdf::Dictionary>> = pages
        .iter()
        .map(|&(_, page_id)| page_resources(&doc, page_id))
        .collect();

    // Serial pre-pass: walk each page's /Resources/Font to collect
    // (name → ObjectId) maps. Cheap because no font is parsed yet, and it
    // lets us deduplicate fonts shared across pages.
    let page_font_refs_per_page: Vec<HashMap<Vec<u8>, ObjectId>> = resources
        .iter()
        .map(|r| r.as_ref().map(|r| page_font_refs(&doc, r)).unwrap_or_default())
        .collect();

    // Parse each unique font exactly once, in parallel.
    let unique_ids: Vec<ObjectId> = page_font_refs_per_page
        .iter()
        .flat_map(|m| m.values().copied())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let font_cache: HashMap<ObjectId, PdfFont> = unique_ids
        .par_iter()
        .map(|&id| (id, PdfFont::from_object(&doc, id)))
        .collect();

    // Image pre-pass. Only runs when the caller asked for images; otherwise
    // we leave the per-page maps empty so the content interpreter never
    // sees any image references.
    let (images, page_image_filenames) = if extract_images {
        collect_images(&doc, &resources)
    } else {
        (Vec::new(), vec![HashMap::new(); pages.len()])
    };

    let page_texts: Vec<String> = pages
        .par_iter()
        .zip(page_font_refs_per_page.par_iter())
        .zip(page_image_filenames.par_iter())
        .map(|((&(_, page_id), font_refs), image_names)| {
            extract_one_page(&doc, page_id, font_refs, &font_cache, image_names).unwrap_or_default()
        })
        .collect();

    let total: usize = page_texts.iter().map(String::len).sum::<usize>() + page_texts.len();
    let mut out = String::with_capacity(total);
    for (i, text) in page_texts.iter().enumerate() {
        if i > 0 {
            out.push('\u{000C}');
        }
        out.push_str(text);
    }
    Ok((out, images))
}

/// Walk every page's XObject dict, pull out the images we can pass through,
/// and assign each one a stable filename (shared if multiple pages
/// reference the same XObject). Returns the extracted images plus per-page
/// `name → filename` maps for the content interpreter.
fn collect_images(
    doc: &Document,
    resources: &[Option<lopdf::Dictionary>],
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
    let content_bytes = doc.get_page_content(page_id).ok()?;
    Some(content::extract_page_text(&content_bytes, &fonts, &images))
}

/// Walk up the page tree until we find a `/Resources` dictionary.
fn page_resources(doc: &Document, page_id: lopdf::ObjectId) -> Option<lopdf::Dictionary> {
    let mut current = page_id;
    loop {
        let dict = doc.get_object(current).ok()?.as_dict().ok()?;
        if let Ok(res) = dict.get(b"Resources") {
            return match res {
                Object::Reference(id) => doc.get_object(*id).ok()?.as_dict().ok().cloned(),
                Object::Dictionary(d) => Some(d.clone()),
                _ => None,
            };
        }
        let Ok(parent) = dict.get(b"Parent") else {
            return None;
        };
        let Object::Reference(parent_id) = parent else {
            return None;
        };
        current = *parent_id;
    }
}
