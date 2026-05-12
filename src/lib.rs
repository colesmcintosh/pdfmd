//! Convert PDF documents into Markdown.
//!
//! Text is extracted directly from the PDF (see [`extract`]) and then run
//! through a small set of heuristics (see [`heuristics`]) to recover
//! headings, lists, and paragraph boundaries — PDFs carry no semantic
//! structure of their own.

use anyhow::Result;

mod extract;
mod heuristics;

pub use extract::ExtractedImage;

use heuristics::{format_page, PAGE_BREAK};

/// Options controlling a PDF → Markdown conversion.
#[derive(Default, Clone, Copy)]
pub struct ConvertOptions<'a> {
    /// Insert a `---` horizontal rule between pages.
    pub include_page_breaks: bool,
    /// If `Some`, image XObjects in pass-through formats (JPEG, JPEG 2000)
    /// are extracted and referenced in the markdown as
    /// `![](DIR/img-NNN.ext)`, where `DIR` is the value provided here. If
    /// `None`, images are ignored.
    pub image_dir: Option<&'a str>,
}

/// Result of a PDF → Markdown conversion.
pub struct ConvertResult {
    pub markdown: String,
    /// Images extracted alongside the markdown. Empty when
    /// [`ConvertOptions::image_dir`] is `None`. The caller is responsible
    /// for writing each one to `image_dir/filename`.
    pub images: Vec<ExtractedImage>,
}

/// Convert the byte contents of a PDF into a Markdown document.
pub fn convert_pdf_to_markdown(pdf_bytes: &[u8], opts: &ConvertOptions) -> Result<ConvertResult> {
    let (raw_text, images) = extract::extract_text(pdf_bytes, opts.image_dir.is_some())?;

    let pages: Vec<String> = raw_text
        .split(PAGE_BREAK)
        .map(format_page)
        .filter(|page| !page.trim().is_empty())
        .collect();

    let joiner = if opts.include_page_breaks {
        "\n\n---\n\n"
    } else {
        "\n\n"
    };
    let mut markdown = pages.join(joiner);

    if let Some(dir) = opts.image_dir {
        rewrite_image_marks(&mut markdown, dir);
    }

    promote_document_title(&mut markdown);

    if !markdown.ends_with('\n') {
        markdown.push('\n');
    }
    Ok(ConvertResult { markdown, images })
}

/// Rewrite each `\u{0001}filename\u{0001}` sentinel emitted by the content
/// extractor into a Markdown image reference.
fn rewrite_image_marks(markdown: &mut String, dir: &str) {
    const MARK: char = '\u{0001}';
    if !markdown.contains(MARK) {
        return;
    }
    let trimmed_dir = dir.trim_end_matches('/');
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown.as_str();
    while let Some(start) = rest.find(MARK) {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + MARK.len_utf8()..];
        let Some(end) = after_open.find(MARK) else {
            // Unterminated marker — keep what we have and bail.
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let filename = &after_open[..end];
        out.push_str(&format!("![]({trimmed_dir}/{filename})"));
        rest = &after_open[end + MARK.len_utf8()..];
    }
    out.push_str(rest);
    *markdown = out;
}

/// Promote the first paragraph of the document to an H1 — that's the
/// document title, but our generic heuristics skip long lines as headings
/// because they could be regular prose. The very first paragraph is a
/// safe special case.
fn promote_document_title(markdown: &mut String) {
    let trimmed = markdown.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("![") {
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
