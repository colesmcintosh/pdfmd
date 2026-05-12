//! Content-stream interpreter for PDF text extraction.
//!
//! Walks the text-showing operators (`Tj`, `TJ`, `'`, `"`), tracks the text
//! matrix enough to recover line breaks, and applies a simple width-based
//! heuristic to recover inter-word spaces that PDF producers express as
//! horizontal displacements rather than literal ASCII space characters.

use std::collections::HashMap;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Object, ObjectId};

use super::font::PdfFont;

/// Map from a page's font-resource name (e.g. `b"F1"`) to a borrowed handle
/// on the parsed font in the document-wide cache.
pub type PageFonts<'a> = HashMap<Vec<u8>, &'a PdfFont>;

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

/// Extract the page's text. Newlines mark new lines; pages are returned as
/// independent strings so the caller can splice page breaks between them.
pub fn extract_page_text(content_bytes: &[u8], fonts: &PageFonts<'_>) -> String {
    let Ok(content) = Content::decode(content_bytes) else {
        return String::new();
    };

    let mut state = TextState::default();
    let mut out = String::new();

    for op in &content.operations {
        execute(op, fonts, &mut state, &mut out);
    }
    out
}

#[derive(Default)]
struct TextState {
    in_text_object: bool,
    text_matrix: Option<Matrix>,
    line_matrix: Option<Matrix>,
    font: Option<Vec<u8>>, // font name in the page's font dict
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

fn execute(op: &Operation, fonts: &PageFonts<'_>, state: &mut TextState, out: &mut String) {
    match op.operator.as_str() {
        "BT" => {
            state.in_text_object = true;
            state.text_matrix = Some(Matrix::identity());
            state.line_matrix = Some(Matrix::identity());
        }
        "ET" => {
            state.in_text_object = false;
        }
        "Tf" if op.operands.len() >= 2 => {
            if let Object::Name(name) = &op.operands[0] {
                state.font = Some(name.clone());
            }
            state.font_size = number(&op.operands[1]);
        }
        "TL" => {
            if let Some(v) = op.operands.first() {
                state.leading = number(v);
            }
        }
        "Tm" if op.operands.len() == 6 => {
            let m = Matrix {
                a: number(&op.operands[0]),
                b: number(&op.operands[1]),
                c: number(&op.operands[2]),
                d: number(&op.operands[3]),
                e: number(&op.operands[4]),
                f: number(&op.operands[5]),
            };
            state.text_matrix = Some(m);
            state.line_matrix = Some(m);
            position_changed(state, m.e, m.f, out);
        }
        "Td" | "TD" if op.operands.len() >= 2 => {
            let tx = number(&op.operands[0]);
            let ty = number(&op.operands[1]);
            if op.operator == "TD" {
                state.leading = -ty;
            }
            if let Some(line) = state.line_matrix.as_mut() {
                line.translate(tx, ty);
                let new_line = *line;
                state.text_matrix = Some(new_line);
                position_changed(state, new_line.e, new_line.f, out);
            }
        }
        "T*" => {
            if let Some(line) = state.line_matrix.as_mut() {
                line.translate(0.0, -state.leading);
                let new_line = *line;
                state.text_matrix = Some(new_line);
                position_changed(state, new_line.e, new_line.f, out);
            }
        }
        "Tj" => {
            if let Some(Object::String(bytes, _)) = op.operands.first() {
                emit(state, fonts, bytes, out);
            }
        }
        "'" => {
            // Move to next line, then show string.
            if let Some(line) = state.line_matrix.as_mut() {
                line.translate(0.0, -state.leading);
                let new_line = *line;
                state.text_matrix = Some(new_line);
                position_changed(state, new_line.e, new_line.f, out);
            }
            if let Some(Object::String(bytes, _)) = op.operands.first() {
                emit(state, fonts, bytes, out);
            }
        }
        "\"" => {
            if let Some(line) = state.line_matrix.as_mut() {
                line.translate(0.0, -state.leading);
                let new_line = *line;
                state.text_matrix = Some(new_line);
                position_changed(state, new_line.e, new_line.f, out);
            }
            if let Some(Object::String(bytes, _)) = op.operands.get(2) {
                emit(state, fonts, bytes, out);
            }
        }
        "TJ" => {
            if let Some(Object::Array(arr)) = op.operands.first() {
                for elem in arr {
                    match elem {
                        Object::String(bytes, _) => emit(state, fonts, bytes, out),
                        Object::Integer(_) | Object::Real(_) => {
                            // PDF spec 9.4.3: positive values move the next
                            // glyph LEFT (kerning that closes a gap),
                            // negative values move it RIGHT — that is the
                            // shape of an inter-word break when the PDF
                            // author omits a literal space character.
                            let v = number(elem);
                            if v <= -TJ_SPACE_THRESHOLD {
                                state.pending_space = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
}

fn emit(state: &mut TextState, fonts: &PageFonts<'_>, bytes: &[u8], out: &mut String) {
    let Some(name) = &state.font else { return };
    let Some(font) = fonts.get(name) else { return };
    let decoded = font.decode(bytes);
    if decoded.is_empty() {
        return;
    }
    flush_pending_space(state, out);
    out.push_str(&decoded);
}

/// Called after `Td`, `Tm`, `T*`, `'`, `"` update the text-line matrix.
/// A vertical change emits a newline (single `\n` for a normal line wrap,
/// `\n\n` for what looks like a paragraph break); a horizontal change
/// defers a space until the next glyph is drawn so trailing position-only
/// operators don't dump stray whitespace.
fn position_changed(state: &mut TextState, new_x: f32, new_y: f32, out: &mut String) {
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

fn flush_pending_space(state: &mut TextState, out: &mut String) {
    if state.pending_space {
        if !out.is_empty() && !out.ends_with(|c: char| c.is_whitespace()) {
            out.push(' ');
        }
        state.pending_space = false;
    }
}

fn number(obj: &Object) -> f32 {
    match obj {
        Object::Integer(i) => *i as f32,
        Object::Real(r) => *r,
        _ => 0.0,
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
