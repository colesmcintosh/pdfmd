//! From-scratch PDF text extractor built on `lopdf` for object parsing.

use anyhow::{Context, Result};
use lopdf::{Document, Object};

mod cmap;
mod content;
mod encoding;
mod font;
mod glyphs;

use content::collect_page_fonts;

/// Extract the textual content of a PDF document. Pages are separated by an
/// ASCII form-feed character, which the markdown layer splits on.
pub fn extract_text(pdf_bytes: &[u8]) -> Result<String> {
    let doc = Document::load_mem(pdf_bytes).context("failed to parse PDF")?;
    let mut out = String::new();

    for (page_num, page_id) in doc.get_pages() {
        let page_text = extract_one_page(&doc, page_id).unwrap_or_default();
        if page_num > 1 {
            out.push('\u{000C}');
        }
        out.push_str(&page_text);
    }
    Ok(out)
}

fn extract_one_page(doc: &Document, page_id: lopdf::ObjectId) -> Option<String> {
    let resources = page_resources(doc, page_id)?;
    let fonts = collect_page_fonts(doc, &resources);
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
