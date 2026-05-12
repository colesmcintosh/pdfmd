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

use content::{page_font_refs, PageFonts};
use font::PdfFont;

/// Extract the textual content of a PDF document. Pages are separated by an
/// ASCII form-feed character, which the markdown layer splits on.
pub fn extract_text(pdf_bytes: &[u8]) -> Result<String> {
    let doc = Document::load_mem(pdf_bytes).context("failed to parse PDF")?;

    // `get_pages` returns a BTreeMap already sorted by page number, so the
    // collected vector preserves document order.
    let pages: Vec<(u32, ObjectId)> = doc.get_pages().into_iter().collect();

    // Serial pre-pass: walk each page's /Resources/Font to collect
    // (name → ObjectId) maps. Cheap because no font is parsed yet, and it
    // lets us deduplicate fonts shared across pages.
    let page_refs: Vec<HashMap<Vec<u8>, ObjectId>> = pages
        .iter()
        .map(|&(_, page_id)| {
            page_resources(&doc, page_id)
                .map(|r| page_font_refs(&doc, &r))
                .unwrap_or_default()
        })
        .collect();

    // Parse each unique font exactly once, in parallel.
    let unique_ids: Vec<ObjectId> = page_refs
        .iter()
        .flat_map(|m| m.values().copied())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let font_cache: HashMap<ObjectId, PdfFont> = unique_ids
        .par_iter()
        .map(|&id| (id, PdfFont::from_object(&doc, id)))
        .collect();

    let page_texts: Vec<String> = pages
        .par_iter()
        .zip(page_refs.par_iter())
        .map(|(&(_, page_id), refs)| {
            extract_one_page(&doc, page_id, refs, &font_cache).unwrap_or_default()
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
    Ok(out)
}

fn extract_one_page(
    doc: &Document,
    page_id: ObjectId,
    refs: &HashMap<Vec<u8>, ObjectId>,
    font_cache: &HashMap<ObjectId, PdfFont>,
) -> Option<String> {
    let fonts: PageFonts<'_> = refs
        .iter()
        .filter_map(|(name, id)| font_cache.get(id).map(|f| (name.clone(), f)))
        .collect();
    let content_bytes = doc.get_page_content(page_id).ok()?;
    Some(content::extract_page_text(&content_bytes, &fonts))
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
