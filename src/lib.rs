//! Convert PDF documents into Markdown.
//!
//! Text is extracted directly from the PDF (see [`extract`]) and then run
//! through a small set of heuristics (see [`heuristics`]) to recover
//! headings, lists, and paragraph boundaries — PDFs carry no semantic
//! structure of their own.

use anyhow::Result;

mod extract;
mod heuristics;

use heuristics::{format_page, PAGE_BREAK};

/// Convert the byte contents of a PDF into a Markdown string.
///
/// When `include_page_breaks` is true, page boundaries from the original
/// PDF are preserved as horizontal rules (`---`).
pub fn convert_pdf_to_markdown(pdf_bytes: &[u8], include_page_breaks: bool) -> Result<String> {
    let raw_text = extract::extract_text(pdf_bytes)?;

    let pages: Vec<String> = raw_text
        .split(PAGE_BREAK)
        .map(format_page)
        .filter(|page| !page.trim().is_empty())
        .collect();

    let joiner = if include_page_breaks { "\n\n---\n\n" } else { "\n\n" };
    let mut markdown = pages.join(joiner);

    promote_document_title(&mut markdown);

    if !markdown.ends_with('\n') {
        markdown.push('\n');
    }
    Ok(markdown)
}

/// Promote the first paragraph of the document to an H1 — that's the
/// document title, but our generic heuristics skip long lines as headings
/// because they could be regular prose. The very first paragraph is a
/// safe special case.
fn promote_document_title(markdown: &mut String) {
    let trimmed = markdown.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return;
    }
    let leading_ws = markdown.len() - trimmed.len();
    let end = trimmed.find("\n\n").unwrap_or(trimmed.len());
    let title = trimmed[..end].trim();
    if title.is_empty() || title.contains('\n') {
        return;
    }
    let replacement = format!("# {title}");
    markdown.replace_range(leading_ws..leading_ws + end, &replacement);
}
