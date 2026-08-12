//! Positioned page content collected while interpreting a content stream.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    Text,
    Image,
}

#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub kind: SpanKind,
    pub mcid: Option<u32>,
    /// Word-break recovered before this span (`TJ` gap / `Td`), not a glyph.
    pub space_before: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct PathRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, Default)]
pub struct PageLayout {
    pub text: String,
    pub spans: Vec<Span>,
    pub rects: Vec<PathRect>,
}

/// Infer bold/italic/mono from a PDF font name (`/BaseFont` or resource name).
pub fn font_style(name: &[u8]) -> (bool, bool, bool) {
    let lower = name.to_ascii_lowercase();
    let n = lower.as_slice();
    let bold = contains(n, b"bold")
        || contains(n, b"black")
        || contains(n, b"heavy")
        || contains(n, b"demibold")
        || contains(n, b"semibold");
    let italic = contains(n, b"italic") || contains(n, b"oblique");
    let mono = contains(n, b"courier")
        || contains(n, b"mono")
        || contains(n, b"consolas")
        || contains(n, b"menlo")
        || contains(n, b"monaco")
        || contains(n, b"lucidaconsole");
    (bold, italic, mono)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_style_detects_common_names() {
        assert_eq!(font_style(b"Times-Bold"), (true, false, false));
        assert_eq!(font_style(b"Helvetica-Oblique"), (false, true, false));
        assert_eq!(font_style(b"Courier"), (false, false, true));
        assert_eq!(font_style(b"Menlo-BoldItalic"), (true, true, true));
        assert_eq!(font_style(b"Helv"), (false, false, false));
    }
}
