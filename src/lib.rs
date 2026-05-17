//! Convert PDF documents into Markdown.
//!
//! Text is extracted directly from the PDF (see [`extract`]) and then run
//! through a small set of heuristics (see [`heuristics`]) to recover
//! headings, lists, and paragraph boundaries — PDFs carry no semantic
//! structure of their own.

mod extract;
mod heuristics;
mod pdf;

use pdf::PdfError;

pub use extract::ExtractedImage;

use heuristics::format_page;

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

/// Top-level error returned from [`convert_pdf_to_markdown`]. Wraps the
/// from-scratch [`PdfError`] for callers that care to distinguish causes.
pub type Error = PdfError;
/// Convenience alias used throughout the public API.
pub type Result<T> = std::result::Result<T, Error>;

/// Convert the byte contents of a PDF into a Markdown document.
pub fn convert_pdf_to_markdown(pdf_bytes: &[u8], opts: &ConvertOptions) -> Result<ConvertResult> {
    let (raw_pages, images) = extract::extract_text(pdf_bytes, opts.image_dir.is_some())?;

    let pages: Vec<String> = raw_pages
        .iter()
        .map(|p| format_page(p))
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
    ensure_trailing_newline(&mut markdown);
    Ok(ConvertResult { markdown, images })
}

/// Append a `\n` if the string doesn't already end in one. Extracted so the
/// tests can hit the both-already-has-newline and append-needed paths
/// without having to construct PDFs whose extracted text incidentally
/// terminates either way.
fn ensure_trailing_newline(s: &mut String) {
    if !s.ends_with('\n') {
        s.push('\n');
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_image_marks_is_no_op_without_sentinels() {
        let mut s = String::from("nothing to rewrite here\n");
        rewrite_image_marks(&mut s, "figs");
        assert_eq!(s, "nothing to rewrite here\n");
    }

    #[test]
    fn rewrite_image_marks_swaps_each_sentinel_pair() {
        let mut s =
            "before\u{0001}img-001.jpg\u{0001}after\u{0001}img-002.jpg\u{0001}.".to_string();
        rewrite_image_marks(&mut s, "figs/");
        assert_eq!(s, "before![](figs/img-001.jpg)after![](figs/img-002.jpg).");
    }

    #[test]
    fn rewrite_image_marks_tolerates_unterminated_sentinel() {
        let mut s = "before\u{0001}img-001.jpg and never closed".to_string();
        rewrite_image_marks(&mut s, "figs");
        // We keep the partial chunk verbatim rather than crashing.
        assert!(s.contains("img-001.jpg"));
    }

    #[test]
    fn promote_document_title_inserts_h1_for_first_paragraph() {
        let mut s = String::from("Some Title\n\nFirst paragraph.\n");
        promote_document_title(&mut s);
        assert!(s.starts_with("# Some Title"));
    }

    #[test]
    fn promote_document_title_skips_when_already_heading_or_image() {
        let cases = ["# Already a heading\n", "![Img](path)\n", "", "   "];
        for c in cases {
            let mut s = String::from(c);
            promote_document_title(&mut s);
            assert!(!s.starts_with("# ") || c.starts_with("# "));
        }
    }

    #[test]
    fn promote_document_title_skips_multi_line_first_block() {
        // The "first paragraph" contains an internal newline — not a title.
        let mut s = String::from("First line\nSecond line\n\nBody.\n");
        promote_document_title(&mut s);
        assert!(s.starts_with("First line"));
    }

    #[test]
    fn ensure_trailing_newline_appends_only_when_missing() {
        let mut s = String::from("no newline");
        ensure_trailing_newline(&mut s);
        assert_eq!(s, "no newline\n");
        let mut already = String::from("yes\n");
        ensure_trailing_newline(&mut already);
        assert_eq!(already, "yes\n");
    }

    #[test]
    fn convert_appends_trailing_newline_when_missing() {
        // Round-trip via the public API on a real PDF.
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sample.pdf"
        ))
        .expect("read fixture");
        let result = convert_pdf_to_markdown(&bytes, &ConvertOptions::default()).unwrap();
        assert!(result.markdown.ends_with('\n'));
    }
}
