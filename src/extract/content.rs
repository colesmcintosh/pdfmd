//! Content-stream interpreter for PDF text extraction.
//!
//! Walks the text-showing operators (`Tj`, `TJ`, `'`, `"`), tracks the text
//! matrix enough to recover line breaks, and applies a simple width-based
//! heuristic to recover inter-word spaces that PDF producers express as
//! horizontal displacements rather than literal ASCII space characters.
//!
//! The tokenizer in [`super::parser`] hands us one operator at a time with
//! its operands borrowed from the input bytes, so this module never sees a
//! heap-allocated `String` operator name or a `Vec<Object>` operand list.

use std::borrow::Cow;
use std::collections::HashMap;

use lopdf::{Dictionary, Object, ObjectId};

use super::font::PdfFont;
use super::image::PageImages;
use super::parser::{Parser, Token};

/// Map from a page's font-resource name (e.g. `b"F1"`) to a borrowed handle
/// on the parsed font in the document-wide cache.
pub type PageFonts<'a> = HashMap<Vec<u8>, &'a PdfFont>;

/// Sentinel that wraps image-reference filenames in the extracted text.
/// The markdown layer rewrites `\u{0001}NAME\u{0001}` into `![](DIR/NAME)`.
/// Chosen because `\u{0001}` never appears in normal PDF text content.
pub const IMAGE_MARK: char = '\u{0001}';

/// Threshold below which a positive `TJ` displacement is treated as kerning
/// rather than a word-break. PDF expresses these values in thousandths of
/// the current text-space unit, so 100 ≈ a tenth of an em.
const TJ_SPACE_THRESHOLD: f32 = 100.0;

#[derive(Debug, Clone, Copy)]
struct Matrix {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Matrix {
    fn identity() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }
    /// Pre-multiply: `self = other * self` (translate-by-other semantics
    /// matches how PDF accumulates `Td` and `Tm` against the line matrix).
    fn translate(&mut self, tx: f32, ty: f32) {
        self.e += tx * self.a + ty * self.c;
        self.f += tx * self.b + ty * self.d;
    }
}

/// Operand stack for a single content-stream operator. PDF operators take at
/// most six numeric operands (`Tm`), and at most one name/string/array
/// operand each, so a fixed-size buffer comfortably holds the worst case
/// without ever touching the heap on the hot path.
#[derive(Default)]
struct Operands<'a> {
    nums: [f32; 6],
    num_count: u8,
    name: Option<&'a [u8]>,
    string: Option<Cow<'a, [u8]>>,
    array: Vec<ArrayItem<'a>>,
    has_array: bool,
}

impl<'a> Operands<'a> {
    fn push_num(&mut self, v: f32) {
        if let Some(slot) = self.nums.get_mut(self.num_count as usize) {
            *slot = v;
        }
        self.num_count = self.num_count.saturating_add(1);
    }
    fn nums(&self) -> &[f32] {
        let n = (self.num_count as usize).min(self.nums.len());
        &self.nums[..n]
    }
    fn reset(&mut self) {
        self.num_count = 0;
        self.name = None;
        self.string = None;
        self.array.clear();
        self.has_array = false;
    }
}

enum ArrayItem<'a> {
    Num(f32),
    Str(Cow<'a, [u8]>),
}

/// Extract the page's text. Newlines mark new lines; pages are returned as
/// independent strings so the caller can splice page breaks between them.
pub fn extract_page_text(
    content_bytes: &[u8],
    fonts: &PageFonts<'_>,
    images: &PageImages<'_>,
) -> String {
    let mut state: TextState<'_> = TextState::default();
    // PDFs almost always emit more bytes than the content stream; preallocate
    // a generous chunk so the inner push loop avoids early growth.
    let mut out = String::with_capacity(content_bytes.len());

    let mut parser = Parser::new(content_bytes);
    let mut ops: Operands<'_> = Operands::default();

    loop {
        match parser.next_token() {
            Token::Eof => break,
            Token::Num(n) => ops.push_num(n),
            Token::Name(n) => ops.name = Some(n),
            Token::Str(s) => ops.string = Some(s),
            Token::ArrayStart => {
                ops.array.clear();
                loop {
                    match parser.next_token() {
                        Token::Num(n) => ops.array.push(ArrayItem::Num(n)),
                        Token::Str(s) => ops.array.push(ArrayItem::Str(s)),
                        Token::ArrayEnd | Token::Eof => break,
                        _ => {}
                    }
                }
                ops.has_array = true;
            }
            // A stray `]` outside an array isn't meaningful; ignore.
            Token::ArrayEnd => {}
            Token::Op(op) => {
                dispatch(op, &ops, &mut state, fonts, images, &mut out);
                if op == b"BI" {
                    // Inline image dictionary follows; consume name/value
                    // pairs until we see `ID`, then skip the raw bytes.
                    skip_inline_image(&mut parser);
                }
                ops.reset();
            }
        }
    }

    out
}

fn skip_inline_image(parser: &mut Parser<'_>) {
    loop {
        match parser.next_token() {
            Token::Op(op) if op == b"ID" => {
                parser.skip_inline_image();
                return;
            }
            Token::Eof => return,
            _ => {}
        }
    }
}

#[derive(Default)]
struct TextState<'a> {
    in_text_object: bool,
    text_matrix: Option<Matrix>,
    line_matrix: Option<Matrix>,
    /// Currently selected font; resolved once at each `Tf` so the per-glyph
    /// hot path avoids hashing the page's font name on every text-show.
    font: Option<&'a PdfFont>,
    font_size: f32,
    leading: f32,
    last_y: Option<f32>,
    last_x: Option<f32>,
    pending_space: bool,
    /// Exponential moving average of the vertical distance between
    /// consecutive lines on this page. Used to tell a normal line wrap
    /// (≈ this value) from a paragraph break (significantly more).
    typical_line_height: Option<f32>,
}

fn dispatch<'a>(
    op: &[u8],
    ops: &Operands<'a>,
    state: &mut TextState<'a>,
    fonts: &PageFonts<'a>,
    images: &PageImages<'_>,
    out: &mut String,
) {
    match op {
        b"BT" => {
            state.in_text_object = true;
            state.text_matrix = Some(Matrix::identity());
            state.line_matrix = Some(Matrix::identity());
        }
        b"ET" => {
            state.in_text_object = false;
        }
        b"Tf" => {
            if let (Some(name), [size, ..]) = (ops.name, ops.nums()) {
                state.font = fonts.get(name).copied();
                state.font_size = *size;
            }
        }
        b"TL" => {
            if let [v, ..] = ops.nums() {
                state.leading = *v;
            }
        }
        b"Tm" => {
            if let [a, b, c, d, e, f, ..] = ops.nums() {
                let m = Matrix {
                    a: *a,
                    b: *b,
                    c: *c,
                    d: *d,
                    e: *e,
                    f: *f,
                };
                state.text_matrix = Some(m);
                state.line_matrix = Some(m);
                position_changed(state, m.e, m.f, out);
            }
        }
        b"Td" | b"TD" => {
            if let [tx, ty, ..] = ops.nums() {
                let (tx, ty) = (*tx, *ty);
                if op == b"TD" {
                    state.leading = -ty;
                }
                if let Some(line) = state.line_matrix.as_mut() {
                    line.translate(tx, ty);
                    let new_line = *line;
                    state.text_matrix = Some(new_line);
                    position_changed(state, new_line.e, new_line.f, out);
                }
            }
        }
        b"T*" => {
            if let Some(line) = state.line_matrix.as_mut() {
                line.translate(0.0, -state.leading);
                let new_line = *line;
                state.text_matrix = Some(new_line);
                position_changed(state, new_line.e, new_line.f, out);
            }
        }
        b"Tj" => {
            if let Some(s) = ops.string.as_deref() {
                emit(state, s, out);
            }
        }
        b"'" => {
            if let Some(line) = state.line_matrix.as_mut() {
                line.translate(0.0, -state.leading);
                let new_line = *line;
                state.text_matrix = Some(new_line);
                position_changed(state, new_line.e, new_line.f, out);
            }
            if let Some(s) = ops.string.as_deref() {
                emit(state, s, out);
            }
        }
        b"\"" => {
            if let Some(line) = state.line_matrix.as_mut() {
                line.translate(0.0, -state.leading);
                let new_line = *line;
                state.text_matrix = Some(new_line);
                position_changed(state, new_line.e, new_line.f, out);
            }
            if let Some(s) = ops.string.as_deref() {
                emit(state, s, out);
            }
        }
        b"Do" => {
            // Paint an XObject by its resource name. We only care about
            // image XObjects we previously chose to extract; everything
            // else (Form XObjects, unsupported filters) is invisible here.
            if let Some(name) = ops.name {
                if let Some(filename) = images.get(name) {
                    state.pending_space = false;
                    ensure_trailing_breaks(out, 2);
                    out.push(IMAGE_MARK);
                    out.push_str(filename);
                    out.push(IMAGE_MARK);
                    out.push_str("\n\n");
                }
            }
        }
        b"TJ" if ops.has_array => {
            for item in &ops.array {
                match item {
                    ArrayItem::Str(s) => emit(state, s, out),
                    ArrayItem::Num(v) => {
                        // PDF spec 9.4.3: positive values move the next
                        // glyph LEFT (kerning that closes a gap),
                        // negative values move it RIGHT — that is the
                        // shape of an inter-word break when the PDF
                        // author omits a literal space character.
                        if *v <= -TJ_SPACE_THRESHOLD {
                            state.pending_space = true;
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn emit(state: &mut TextState<'_>, bytes: &[u8], out: &mut String) {
    let Some(font) = state.font else { return };

    // Optimistically flush a deferred word-break before decoding so the
    // common case (decode produces ≥1 char) avoids an O(n) string insert.
    // If the decode produces nothing we pop the space back off below.
    let added_space =
        state.pending_space && !ends_with_ascii_whitespace(out) && !out.is_empty() && {
            out.push(' ');
            true
        };
    let was_pending = state.pending_space;
    state.pending_space = false;
    let start = out.len();
    font.decode_into(bytes, out);
    if out.len() == start {
        if added_space {
            out.pop();
        }
        state.pending_space = was_pending;
    }
}

fn ends_with_ascii_whitespace(out: &str) -> bool {
    matches!(out.as_bytes().last(), Some(b' ' | b'\n' | b'\t' | b'\r'))
}

/// Called after `Td`, `Tm`, `T*`, `'`, `"` update the text-line matrix.
/// A vertical change emits a newline (single `\n` for a normal line wrap,
/// `\n\n` for what looks like a paragraph break); a horizontal change
/// defers a space until the next glyph is drawn so trailing position-only
/// operators don't dump stray whitespace.
fn position_changed(state: &mut TextState<'_>, new_x: f32, new_y: f32, out: &mut String) {
    if !state.in_text_object {
        state.last_x = Some(new_x);
        state.last_y = Some(new_y);
        return;
    }
    let prev_y = state.last_y.unwrap_or(new_y);
    let dy = (new_y - prev_y).abs();
    let line_threshold = state.font_size.max(1.0) * 0.4;
    if dy > line_threshold {
        // Paragraph break: either we've established a typical line height
        // for this page and this jump is much larger, OR the vertical
        // distance is more than two font sizes (e.g. column reset).
        let is_paragraph = match state.typical_line_height {
            Some(typical) => dy > typical * 1.5,
            None => dy > state.font_size.max(1.0) * 2.0,
        };
        if !out.is_empty() {
            ensure_trailing_breaks(out, if is_paragraph { 2 } else { 1 });
        }
        state.pending_space = false;
        // Train the EMA on line-height-sized jumps only; column/section
        // resets would otherwise blow the running average.
        if !is_paragraph {
            let new_ema = match state.typical_line_height {
                Some(t) => t * 0.7 + dy * 0.3,
                None => dy,
            };
            state.typical_line_height = Some(new_ema);
        }
    } else if let Some(prev_x) = state.last_x {
        let dx = new_x - prev_x;
        // A forward horizontal jump of more than ~20% of an em is too wide
        // to be intra-glyph kerning; treat it as a deferred word break.
        if dx > state.font_size.max(1.0) * 0.2 {
            state.pending_space = true;
        }
    }
    state.last_x = Some(new_x);
    state.last_y = Some(new_y);
}

/// Ensure `out` ends with exactly `count` newline characters (collapsing
/// any existing trailing newlines first).
fn ensure_trailing_breaks(out: &mut String, count: usize) {
    while out.ends_with('\n') {
        out.pop();
    }
    for _ in 0..count {
        out.push('\n');
    }
}

/// Map a page's `/Resources/Font` entries to their font object IDs without
/// parsing the fonts themselves — the caller looks the parsed fonts up in
/// a document-wide cache to avoid re-parsing the same font across pages.
pub fn page_font_refs(doc: &lopdf::Document, resources: &Dictionary) -> HashMap<Vec<u8>, ObjectId> {
    let mut out = HashMap::new();
    let Ok(font_dict_obj) = resources.get(b"Font") else {
        return out;
    };
    let font_dict = match font_dict_obj {
        Object::Reference(id) => doc.get_object(*id).and_then(Object::as_dict).ok(),
        Object::Dictionary(d) => Some(d),
        _ => None,
    };
    let Some(font_dict) = font_dict else {
        return out;
    };
    for (name, obj) in font_dict.iter() {
        if let Object::Reference(id) = obj {
            out.insert(name.clone(), *id);
        }
    }
    out
}
