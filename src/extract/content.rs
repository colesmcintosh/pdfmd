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

use crate::pdf::{Dictionary, Object, ObjectId};

use super::font::PdfFont;
#[cfg(test)]
use super::image::PageImages;
use super::layout::{font_style, PageLayout, PathRect, Span, SpanKind};
use super::parser::{Parser, Token};
use super::FormXObject;

/// Map from a page's font-resource name (e.g. `b"F1"`) to a borrowed handle
/// on the parsed font in the document-wide cache.
pub type PageFonts<'a> = HashMap<Vec<u8>, &'a PdfFont>;

/// Extracted image filename keyed by the image XObject's object ID. Resource
/// names remain context-local and resolve through `ContentResources` first.
pub(super) type ImageFilenames<'a> = HashMap<ObjectId, &'a str>;

struct ContentResources<'a, 'fonts> {
    fonts: &'a PageFonts<'fonts>,
    xobjects: &'a HashMap<Vec<u8>, ObjectId>,
}

struct FormContext<'a, 'fonts> {
    forms: &'a HashMap<ObjectId, FormXObject>,
    fonts: &'a HashMap<ObjectId, PageFonts<'fonts>>,
    images: &'a ImageFilenames<'a>,
}

/// Sentinel that wraps image-reference filenames in the extracted text.
/// The markdown layer rewrites `\u{0001}NAME\u{0001}` into `![](DIR/NAME)`.
/// Chosen because `\u{0001}` never appears in normal PDF text content.
pub const IMAGE_MARK: char = '\u{0001}';

/// Threshold below which a positive `TJ` displacement is treated as kerning
/// rather than a word-break. PDF expresses these values in thousandths of
/// the current text-space unit, so 100 ≈ a tenth of an em.
const TJ_SPACE_THRESHOLD: f32 = 100.0;

/// Bound recursive Form XObject invocation independently of the resource
/// graph pre-pass. Real documents rarely nest forms more than a few levels;
/// this cap keeps adversarial acyclic chains from exhausting the stack.
const MAX_FORM_DEPTH: usize = 32;
/// Figure drawings can emit tens of thousands of segments. Table detection
/// only needs a handful of axis-aligned rules.
const MAX_PATH_RECTS: usize = 256;

/// Page-local limits keep a branching Form graph from multiplying a small
/// resource set into unbounded work or output.
const MAX_FORM_INVOCATIONS_PER_PAGE: usize = 16_384;
const MAX_FORM_INPUT_BYTES_PER_PAGE: usize = 64 * 1024 * 1024;
const MAX_FORM_OUTPUT_BYTES_PER_PAGE: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
struct FormExecutionLimits {
    invocations: usize,
    input_bytes: usize,
    output_bytes: usize,
}

const FORM_EXECUTION_LIMITS: FormExecutionLimits = FormExecutionLimits {
    invocations: MAX_FORM_INVOCATIONS_PER_PAGE,
    input_bytes: MAX_FORM_INPUT_BYTES_PER_PAGE,
    output_bytes: MAX_FORM_OUTPUT_BYTES_PER_PAGE,
};

struct PageExtractCfg {
    limits: FormExecutionLimits,
    keep_text: bool,
}

struct FormBudget {
    limits: FormExecutionLimits,
    invocations: usize,
    input_bytes: usize,
    output_bytes: usize,
}

impl FormBudget {
    fn new(limits: FormExecutionLimits) -> Self {
        Self {
            limits,
            invocations: 0,
            input_bytes: 0,
            output_bytes: 0,
        }
    }

    fn output_exhausted(&self) -> bool {
        self.output_bytes >= self.limits.output_bytes
    }

    fn output_remaining(&self) -> usize {
        self.limits.output_bytes.saturating_sub(self.output_bytes)
    }

    fn charge_output(&mut self, out: Option<&mut String>, added: usize) {
        let remaining = self.limits.output_bytes.saturating_sub(self.output_bytes);
        if added <= remaining {
            self.output_bytes += added;
            return;
        }

        if let Some(out) = out {
            let mut new_len = out.len().saturating_sub(added - remaining);
            while new_len > 0 && !out.is_char_boundary(new_len) {
                new_len -= 1;
            }
            out.truncate(new_len);
        }
        self.output_bytes = self.limits.output_bytes;
    }
}

struct ActiveForm {
    id: ObjectId,
    /// Bytes below this point belong to the caller and must remain untouched.
    output_floor: usize,
}

struct FormExecution {
    active: Vec<ActiveForm>,
    budget: FormBudget,
}

impl FormExecution {
    fn new(limits: FormExecutionLimits) -> Self {
        Self {
            active: Vec::new(),
            budget: FormBudget::new(limits),
        }
    }

    fn output_floor(&self) -> usize {
        self.active
            .last()
            .map(|form| form.output_floor)
            .unwrap_or(0)
    }

    fn contains(&self, id: ObjectId) -> bool {
        self.active.iter().any(|form| form.id == id)
    }
}

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

fn text_direction_changed(previous: Matrix, next: Matrix) -> bool {
    let previous_length = previous.a.hypot(previous.b);
    let next_length = next.a.hypot(next.b);
    if previous_length <= f32::EPSILON || next_length <= f32::EPSILON {
        return false;
    }
    let dot = previous.a * next.a + previous.b * next.b;
    dot < previous_length * next_length * 0.99
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

impl Operands<'_> {
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
#[cfg(test)]
pub fn extract_page_text(
    content_bytes: &[u8],
    fonts: &PageFonts<'_>,
    images: &PageImages<'_>,
) -> String {
    let mut xobjects = HashMap::new();
    let mut image_filenames = ImageFilenames::new();
    let mut next_object_number = 1u32;
    for (name, &filename) in images {
        let id = ObjectId(next_object_number, 0);
        next_object_number = next_object_number.saturating_add(1);
        xobjects.insert(name.clone(), id);
        image_filenames.insert(id, filename);
    }
    let forms = HashMap::new();
    let form_fonts = HashMap::new();
    extract_page_text_with_forms(
        content_bytes,
        fonts,
        &xobjects,
        &forms,
        &form_fonts,
        &image_filenames,
    )
}

struct PageBuilder {
    out: String,
    layout: PageLayout,
    ctm: Matrix,
    ctm_stack: Vec<Matrix>,
    path_x: f32,
    path_y: f32,
    mcid: Option<u32>,
    mcid_stack: Vec<Option<u32>>,
    scratch: String,
    keep_text: bool,
    last_ws: bool,
    last_alnum: bool,
    emitted: usize,
}

impl PageBuilder {
    #[cfg(test)]
    fn new(cap: usize) -> Self {
        Self::create(cap, true)
    }

    fn create(content_len: usize, keep_text: bool) -> Self {
        let mut layout = PageLayout::default();
        layout.spans.reserve(content_len / 32);
        Self {
            out: if keep_text {
                String::with_capacity(content_len)
            } else {
                String::new()
            },
            layout,
            ctm: Matrix::identity(),
            ctm_stack: Vec::new(),
            path_x: 0.0,
            path_y: 0.0,
            mcid: None,
            mcid_stack: Vec::new(),
            scratch: String::new(),
            keep_text,
            last_ws: true,
            last_alnum: false,
            emitted: 0,
        }
    }

    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let m = self.ctm;
        if m.a == 1.0 && m.b == 0.0 && m.c == 0.0 && m.d == 1.0 && m.e == 0.0 && m.f == 0.0 {
            return (x, y);
        }
        (m.a * x + m.c * y + m.e, m.b * x + m.d * y + m.f)
    }

    fn push_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let (x0, y0) = self.apply(x, y);
        let (x1, y1) = self.apply(x + w, y + h);
        self.record_rect(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs());
    }

    fn push_segment(&mut self, x1: f32, y1: f32, x2: f32, y2: f32) {
        let (ax, ay) = self.apply(x1, y1);
        let (bx, by) = self.apply(x2, y2);
        let dx = (bx - ax).abs();
        let dy = (by - ay).abs();
        // Figures emit dense polylines; only axis-aligned rules feed tables.
        if dx > 1.5 && dy > 1.5 {
            return;
        }
        self.record_rect(ax.min(bx), ay.min(by), dx.max(0.5), dy.max(0.5));
    }

    fn record_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if self.layout.rects.len() >= MAX_PATH_RECTS {
            return;
        }
        let rule = (h < 2.5 && w > 16.0) || (w < 2.5 && h > 16.0);
        let cell = w > 8.0 && h > 8.0 && w < 280.0 && h < 80.0;
        if !rule && !cell {
            return;
        }
        self.layout.rects.push(PathRect { x, y, w, h });
    }
}

/// Extract page text while resolving Form XObjects at each `Do` paint.
#[cfg(test)]
pub(super) fn extract_page_text_with_forms<'fonts>(
    content_bytes: &[u8],
    fonts: &PageFonts<'fonts>,
    xobjects: &HashMap<Vec<u8>, ObjectId>,
    forms: &HashMap<ObjectId, FormXObject>,
    form_fonts: &HashMap<ObjectId, PageFonts<'fonts>>,
    image_filenames: &ImageFilenames<'_>,
) -> String {
    extract_page_layout_with_forms(
        content_bytes,
        fonts,
        xobjects,
        forms,
        form_fonts,
        image_filenames,
    )
    .text
}

pub(super) fn extract_page_layout_with_forms<'fonts>(
    content_bytes: &[u8],
    fonts: &PageFonts<'fonts>,
    xobjects: &HashMap<Vec<u8>, ObjectId>,
    forms: &HashMap<ObjectId, FormXObject>,
    form_fonts: &HashMap<ObjectId, PageFonts<'fonts>>,
    image_filenames: &ImageFilenames<'_>,
) -> PageLayout {
    extract_page_with_form_limits(
        content_bytes,
        fonts,
        xobjects,
        forms,
        form_fonts,
        image_filenames,
        PageExtractCfg {
            limits: FORM_EXECUTION_LIMITS,
            keep_text: true,
        },
    )
}

pub(super) fn extract_page_layout_fast<'fonts>(
    content_bytes: &[u8],
    fonts: &PageFonts<'fonts>,
    xobjects: &HashMap<Vec<u8>, ObjectId>,
    forms: &HashMap<ObjectId, FormXObject>,
    form_fonts: &HashMap<ObjectId, PageFonts<'fonts>>,
    image_filenames: &ImageFilenames<'_>,
) -> PageLayout {
    extract_page_with_form_limits(
        content_bytes,
        fonts,
        xobjects,
        forms,
        form_fonts,
        image_filenames,
        PageExtractCfg {
            limits: FORM_EXECUTION_LIMITS,
            keep_text: false,
        },
    )
}

#[cfg(test)]
fn extract_page_text_with_form_limits<'fonts>(
    content_bytes: &[u8],
    fonts: &PageFonts<'fonts>,
    xobjects: &HashMap<Vec<u8>, ObjectId>,
    forms: &HashMap<ObjectId, FormXObject>,
    form_fonts: &HashMap<ObjectId, PageFonts<'fonts>>,
    image_filenames: &ImageFilenames<'_>,
    limits: FormExecutionLimits,
) -> String {
    extract_page_with_form_limits(
        content_bytes,
        fonts,
        xobjects,
        forms,
        form_fonts,
        image_filenames,
        PageExtractCfg {
            limits,
            keep_text: true,
        },
    )
    .text
}

fn extract_page_with_form_limits<'fonts>(
    content_bytes: &[u8],
    fonts: &PageFonts<'fonts>,
    xobjects: &HashMap<Vec<u8>, ObjectId>,
    forms: &HashMap<ObjectId, FormXObject>,
    form_fonts: &HashMap<ObjectId, PageFonts<'fonts>>,
    image_filenames: &ImageFilenames<'_>,
    cfg: PageExtractCfg,
) -> PageLayout {
    let mut page = PageBuilder::create(content_bytes.len(), cfg.keep_text);
    let resources = ContentResources { fonts, xobjects };
    let form_context = FormContext {
        forms,
        fonts: form_fonts,
        images: image_filenames,
    };
    let mut state = TextState::default();
    let mut form_execution = FormExecution::new(cfg.limits);
    extract_content_into(
        content_bytes,
        &resources,
        &form_context,
        &mut state,
        &mut form_execution,
        &mut page,
    );
    let PageBuilder {
        out,
        mut layout,
        keep_text,
        ..
    } = page;
    if keep_text {
        layout.text = out;
    }
    layout
}

fn extract_content_into<'fonts>(
    content_bytes: &[u8],
    resources: &ContentResources<'_, 'fonts>,
    form_context: &FormContext<'_, 'fonts>,
    state: &mut TextState<'fonts>,
    form_execution: &mut FormExecution,
    page: &mut PageBuilder,
) {
    let mut parser = Parser::new(content_bytes);
    let mut ops: Operands<'_> = Operands::default();

    loop {
        if !form_execution.active.is_empty() && form_execution.budget.output_exhausted() {
            break;
        }
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
            Token::ArrayEnd => {}
            Token::Op(op) => {
                if op == b"BDC" {
                    page.mcid_stack.push(page.mcid);
                    if let Some(id) = parser.pending_mcid.take() {
                        page.mcid = Some(id);
                    }
                } else if op == b"BMC" {
                    page.mcid_stack.push(page.mcid);
                } else if op == b"EMC" {
                    page.mcid = page.mcid_stack.pop().flatten();
                }
                let charge_output = !form_execution.active.is_empty();
                let output_start = if page.keep_text {
                    page.out.len()
                } else {
                    page.emitted
                };
                let charged_start = form_execution.budget.output_bytes;
                dispatch(
                    op,
                    &ops,
                    state,
                    resources,
                    form_context,
                    form_execution,
                    page,
                );
                if charge_output {
                    let added = if page.keep_text {
                        page.out.len().saturating_sub(output_start)
                    } else {
                        page.emitted.saturating_sub(output_start)
                    };
                    let nested_charge = form_execution
                        .budget
                        .output_bytes
                        .saturating_sub(charged_start);
                    let extra = added.saturating_sub(nested_charge);
                    if page.keep_text {
                        form_execution
                            .budget
                            .charge_output(Some(&mut page.out), extra);
                    } else {
                        form_execution.budget.charge_output(None, extra);
                    }
                }
                if op == b"BI" {
                    skip_inline_image(&mut parser);
                }
                ops.reset();
            }
        }
    }
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
    pending_form_word_boundary: bool,
    bold: bool,
    italic: bool,
    mono: bool,
    /// Exponential moving average of the vertical distance between
    /// consecutive lines on this page. Used to tell a normal line wrap
    /// (≈ this value) from a paragraph break (significantly more).
    typical_line_height: Option<f32>,
    /// Cached `|line_matrix|` so `Tj`/`TJ` skip `hypot` per show.
    hx: f32,
    vx: f32,
}

impl TextState<'_> {
    fn inherited_for_form(caller: &Self) -> Self {
        Self {
            font: caller.font,
            font_size: caller.font_size,
            leading: caller.leading,
            bold: caller.bold,
            italic: caller.italic,
            mono: caller.mono,
            hx: caller.hx,
            vx: caller.vx,
            ..Self::default()
        }
    }

    fn hscale(&self) -> f32 {
        if self.hx > 0.0 {
            self.hx
        } else {
            1.0
        }
    }

    fn vscale(&self) -> f32 {
        if self.vx > 0.0 {
            self.vx
        } else {
            1.0
        }
    }

    fn set_line_matrix(&mut self, m: Matrix) {
        self.hx = m.a.hypot(m.b);
        self.vx = m.c.hypot(m.d);
        self.text_matrix = Some(m);
        self.line_matrix = Some(m);
    }
}

fn dispatch<'fonts>(
    op: &[u8],
    ops: &Operands<'_>,
    state: &mut TextState<'fonts>,
    resources: &ContentResources<'_, 'fonts>,
    form_context: &FormContext<'_, 'fonts>,
    form_execution: &mut FormExecution,
    page: &mut PageBuilder,
) {
    match op {
        b"BT" => {
            state.in_text_object = true;
            state.set_line_matrix(Matrix::identity());
        }
        b"ET" => {
            state.in_text_object = false;
        }
        b"Tf" => {
            if let (Some(name), [size, ..]) = (ops.name, ops.nums()) {
                state.font = resources.fonts.get(name).copied();
                state.font_size = *size;
                let (b1, i1, m1) = font_style(name);
                let (b2, i2, m2) = state
                    .font
                    .map(|f| font_style(&f.base_font))
                    .unwrap_or((false, false, false));
                state.bold = b1 || b2;
                state.italic = i1 || i2;
                state.mono = m1 || m2;
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
                let direction_changed = state
                    .line_matrix
                    .map(|previous| text_direction_changed(previous, m))
                    .unwrap_or(false);
                state.set_line_matrix(m);
                position_changed(state, m.e, m.f, form_execution.output_floor(), page);
                if direction_changed {
                    state.pending_space = true;
                }
            }
        }
        b"Td" | b"TD" => {
            if let [tx, ty, ..] = ops.nums() {
                let (tx, ty) = (*tx, *ty);
                if op == b"TD" {
                    state.leading = -ty;
                }
                if let Some(mut line) = state.line_matrix {
                    line.translate(tx, ty);
                    state.set_line_matrix(line);
                    position_changed(state, line.e, line.f, form_execution.output_floor(), page);
                }
            }
        }
        b"T*" => {
            if let Some(mut line) = state.line_matrix {
                line.translate(0.0, -state.leading);
                state.set_line_matrix(line);
                position_changed(state, line.e, line.f, form_execution.output_floor(), page);
            }
        }
        b"Tj" => {
            if let Some(s) = ops.string.as_deref() {
                emit(state, s, page);
            }
        }
        b"'" => {
            if let Some(mut line) = state.line_matrix {
                line.translate(0.0, -state.leading);
                state.set_line_matrix(line);
                position_changed(state, line.e, line.f, form_execution.output_floor(), page);
            }
            if let Some(s) = ops.string.as_deref() {
                emit(state, s, page);
            }
        }
        b"\"" => {
            if let Some(mut line) = state.line_matrix {
                line.translate(0.0, -state.leading);
                state.set_line_matrix(line);
                position_changed(state, line.e, line.f, form_execution.output_floor(), page);
            }
            if let Some(s) = ops.string.as_deref() {
                emit(state, s, page);
            }
        }
        b"Do" => {
            if let Some(name) = ops.name {
                if let Some(id) = resources.xobjects.get(name).copied() {
                    if let Some(filename) = form_context.images.get(&id) {
                        if emit_image_marker(filename, form_execution, page) {
                            state.pending_space = false;
                            state.pending_form_word_boundary = false;
                        }
                        return;
                    }
                    let start = page.out.len();
                    let span_start = page.layout.spans.len();
                    let prev_alnum = page.last_alnum;
                    let prev_ws = page.last_ws;
                    if emit_form(id, state, resources, form_context, form_execution, page) {
                        if page.keep_text {
                            let previous = page.out[..start].chars().next_back();
                            let first = page.out[start..].chars().next();
                            let adjacent_words = previous
                                .zip(first)
                                .map(|(left, right)| {
                                    left.is_alphanumeric() && right.is_alphanumeric()
                                })
                                .unwrap_or(false);
                            let boundary_is_tight =
                                previous.map(|ch| !ch.is_whitespace()).unwrap_or(false)
                                    && first.map(|ch| !ch.is_whitespace()).unwrap_or(false);
                            if boundary_is_tight && (state.pending_space || adjacent_words) {
                                page.out.insert(start, ' ');
                            }
                            state.pending_form_word_boundary = page
                                .out
                                .chars()
                                .next_back()
                                .map(|ch| ch.is_alphanumeric())
                                .unwrap_or(false);
                        } else if let Some(first) = page.layout.spans.get_mut(span_start) {
                            let first_ws = first
                                .text
                                .chars()
                                .next()
                                .map(|ch| ch.is_whitespace())
                                .unwrap_or(true);
                            let first_alnum = first
                                .text
                                .chars()
                                .next()
                                .map(|ch| ch.is_alphanumeric())
                                .unwrap_or(false);
                            if !prev_ws
                                && !first_ws
                                && (state.pending_space || (prev_alnum && first_alnum))
                            {
                                first.space_before = true;
                            }
                            state.pending_form_word_boundary = page.last_alnum;
                        }
                        state.pending_space = false;
                    }
                }
            }
        }
        b"TJ" if ops.has_array => {
            for item in &ops.array {
                match item {
                    ArrayItem::Str(s) => emit(state, s, page),
                    ArrayItem::Num(v) => {
                        if *v <= -TJ_SPACE_THRESHOLD {
                            state.pending_space = true;
                        }
                    }
                }
            }
        }
        b"q" => page.ctm_stack.push(page.ctm),
        b"Q" => {
            if let Some(saved) = page.ctm_stack.pop() {
                page.ctm = saved;
            }
        }
        b"cm" => {
            if let [a, b, c, d, e, f, ..] = ops.nums() {
                let n = Matrix {
                    a: *a,
                    b: *b,
                    c: *c,
                    d: *d,
                    e: *e,
                    f: *f,
                };
                let m = page.ctm;
                page.ctm = Matrix {
                    a: n.a * m.a + n.b * m.c,
                    b: n.a * m.b + n.b * m.d,
                    c: n.c * m.a + n.d * m.c,
                    d: n.c * m.b + n.d * m.d,
                    e: n.e * m.a + n.f * m.c + m.e,
                    f: n.e * m.b + n.f * m.d + m.f,
                };
            }
        }
        b"re" => {
            if let [x, y, w, h, ..] = ops.nums() {
                page.push_rect(*x, *y, *w, *h);
            }
        }
        b"m" => {
            if let [x, y, ..] = ops.nums() {
                page.path_x = *x;
                page.path_y = *y;
            }
        }
        b"l" => {
            if let [x, y, ..] = ops.nums() {
                page.push_segment(page.path_x, page.path_y, *x, *y);
                page.path_x = *x;
                page.path_y = *y;
            }
        }
        _ => {}
    }
}

fn emit_image_marker(
    filename: &str,
    form_execution: &FormExecution,
    page: &mut PageBuilder,
) -> bool {
    let marker_len = IMAGE_MARK.len_utf8() * 2 + filename.len() + 2;
    if page.keep_text {
        let out = &mut page.out;
        let output_floor = form_execution.output_floor().min(out.len());
        if !form_execution.active.is_empty() {
            let (removable_breaks, missing_breaks) =
                trailing_break_adjustment(out, 2, output_floor);
            let Some(final_len) = out
                .len()
                .checked_sub(removable_breaks)
                .and_then(|len| len.checked_add(missing_breaks))
                .and_then(|len| len.checked_add(marker_len))
            else {
                return false;
            };
            let added = final_len.saturating_sub(out.len());
            if added > form_execution.budget.output_remaining() {
                return false;
            }
        }

        ensure_trailing_breaks(out, 2, output_floor);
        out.push(IMAGE_MARK);
        out.push_str(filename);
        out.push(IMAGE_MARK);
        out.push_str("\n\n");
    } else if !form_execution.active.is_empty()
        && marker_len > form_execution.budget.output_remaining()
    {
        return false;
    }
    page.emitted += marker_len;
    page.last_ws = true;
    page.last_alnum = false;
    let (x, y) = page.apply(0.0, 0.0);
    page.layout.spans.push(Span {
        text: filename.to_string(),
        x,
        y,
        width: 1.0,
        height: 1.0,
        font_size: 1.0,
        bold: false,
        italic: false,
        mono: false,
        kind: SpanKind::Image,
        mcid: page.mcid,
        space_before: false,
    });
    true
}

fn emit_form<'fonts>(
    id: ObjectId,
    caller_state: &TextState<'fonts>,
    caller_resources: &ContentResources<'_, 'fonts>,
    form_context: &FormContext<'_, 'fonts>,
    form_execution: &mut FormExecution,
    page: &mut PageBuilder,
) -> bool {
    if form_execution.active.len() >= MAX_FORM_DEPTH
        || form_execution.contains(id)
        || form_execution.budget.invocations >= form_execution.budget.limits.invocations
        || form_execution.budget.output_exhausted()
    {
        return false;
    }
    let Some(form) = form_context.forms.get(&id) else {
        return false;
    };
    if form.content.len()
        > form_execution
            .budget
            .limits
            .input_bytes
            .saturating_sub(form_execution.budget.input_bytes)
    {
        return false;
    }

    let start = page.out.len();
    let emitted_start = page.emitted;
    form_execution.budget.invocations += 1;
    form_execution.budget.input_bytes += form.content.len();
    let mut state = TextState::inherited_for_form(caller_state);
    form_execution.active.push(ActiveForm {
        id,
        output_floor: start,
    });
    if let (Some(fonts), Some(xobjects)) = (form_context.fonts.get(&id), form.xobject_refs.as_ref())
    {
        let resources = ContentResources { fonts, xobjects };
        extract_content_into(
            &form.content,
            &resources,
            form_context,
            &mut state,
            form_execution,
            page,
        );
    } else {
        extract_content_into(
            &form.content,
            caller_resources,
            form_context,
            &mut state,
            form_execution,
            page,
        );
    }
    form_execution.active.pop();
    if page.keep_text {
        page.out
            .get(start..)
            .map(|text| text.chars().any(|ch| !ch.is_whitespace()))
            .unwrap_or(false)
    } else {
        page.emitted > emitted_start
    }
}

fn emit(state: &mut TextState<'_>, bytes: &[u8], page: &mut PageBuilder) {
    let Some(font) = state.font else { return };

    page.scratch.clear();
    font.decode_into(bytes, &mut page.scratch);
    if page.scratch.is_empty() {
        return;
    }

    let added_space = if page.keep_text {
        state.pending_space && !ends_with_ascii_whitespace(&page.out) && !page.out.is_empty()
    } else {
        state.pending_space && !page.last_ws && page.emitted > 0
    };
    let form_space = if !added_space && state.pending_form_word_boundary {
        let previous_is_word = if page.keep_text {
            page.out
                .chars()
                .next_back()
                .map(|ch| ch.is_alphanumeric())
                .unwrap_or(false)
        } else {
            page.last_alnum
        };
        let first_is_word = page
            .scratch
            .chars()
            .next()
            .map(|ch| ch.is_alphanumeric())
            .unwrap_or(false);
        previous_is_word && first_is_word
    } else {
        false
    };
    state.pending_space = false;
    state.pending_form_word_boundary = false;

    if page.keep_text {
        if added_space || form_space {
            page.out.push(' ');
        }
        page.out.push_str(&page.scratch);
    }
    page.emitted += page.scratch.len() + usize::from(added_space || form_space);
    page.last_ws = ends_with_ascii_whitespace(&page.scratch);
    page.last_alnum = page
        .scratch
        .chars()
        .next_back()
        .map(|ch| ch.is_alphanumeric())
        .unwrap_or(false);

    let hx = state.hscale();
    let vx = state.vscale();
    let font_size = (state.font_size.abs() * vx).max(0.1);
    let raw_x = state.last_x.unwrap_or(0.0);
    let raw_y = state.last_y.unwrap_or(0.0);
    let (x, y) = page.apply(raw_x, raw_y);
    let extra_w = page.scratch.len() as f32 * font_size * 0.5 * hx.max(0.1);
    let space = added_space || form_space;
    if merge_span(page, x, y, font_size, extra_w, space, state) {
        return;
    }
    page.layout.spans.push(Span {
        text: std::mem::take(&mut page.scratch),
        x,
        y,
        width: extra_w,
        height: font_size,
        font_size,
        bold: state.bold,
        italic: state.italic,
        mono: state.mono,
        kind: SpanKind::Text,
        mcid: page.mcid,
        space_before: space,
    });
}

fn merge_span(
    page: &mut PageBuilder,
    x: f32,
    y: f32,
    font_size: f32,
    extra_w: f32,
    space: bool,
    state: &TextState<'_>,
) -> bool {
    let mcid = page.mcid;
    let Some(last) = page.layout.spans.last_mut() else {
        return false;
    };
    if last.kind != SpanKind::Text
        || last.bold != state.bold
        || last.italic != state.italic
        || last.mono != state.mono
        || last.mcid != mcid
        || (last.y - y).abs() > font_size * 0.35
    {
        return false;
    }
    // Same paint origin (TJ pieces) or a tight continuation on this line.
    let close = (x - last.x).abs() < 1.0 || x <= last.x + last.width + font_size * 0.35;
    if !close {
        return false;
    }
    if space && !last.text.ends_with(' ') {
        last.text.push(' ');
    }
    last.text.push_str(&page.scratch);
    last.width = (x + extra_w - last.x).max(last.width + extra_w);
    true
}

fn ends_with_ascii_whitespace(out: &str) -> bool {
    matches!(out.as_bytes().last(), Some(b' ' | b'\n' | b'\t' | b'\r'))
}

/// Called after `Td`, `Tm`, `T*`, `'`, `"` update the text-line matrix.
/// A vertical change emits a newline (single `\n` for a normal line wrap,
/// `\n\n` for what looks like a paragraph break); a horizontal change
/// defers a space until the next glyph is drawn so trailing position-only
/// operators don't dump stray whitespace.
fn position_changed(
    state: &mut TextState<'_>,
    new_x: f32,
    new_y: f32,
    output_floor: usize,
    page: &mut PageBuilder,
) {
    if !state.in_text_object {
        state.last_x = Some(new_x);
        state.last_y = Some(new_y);
        return;
    }
    let prev_y = state.last_y.unwrap_or(new_y);
    let dy = (new_y - prev_y).abs();
    // Some producers set `Tf` to 1 and put the visible point size in `Tm`.
    // Measure the transformed text basis so those normal advances are not
    // mistaken for paragraph-sized jumps or word breaks.
    let horizontal_scale = state.hscale();
    let vertical_scale = state.vscale();
    let font_size = state.font_size.abs();
    let effective_font_size = (font_size * vertical_scale).max(1.0);
    let line_threshold = effective_font_size * 0.4;
    if dy > line_threshold {
        // Paragraph break: either we've established a typical line height
        // for this page and this jump is much larger, OR the vertical
        // distance is more than two font sizes (e.g. column reset).
        let is_paragraph = match state.typical_line_height {
            Some(typical) => dy > typical * 1.5,
            None => dy > effective_font_size * 2.0,
        };
        if page.keep_text && !page.out.is_empty() {
            ensure_trailing_breaks(
                &mut page.out,
                if is_paragraph { 2 } else { 1 },
                output_floor,
            );
        }
        if is_paragraph {
            page.last_ws = true;
        }
        state.pending_space = false;
        state.pending_form_word_boundary = false;
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
        if dx > (font_size * horizontal_scale).max(1.0) * 0.2 {
            state.pending_space = true;
        }
    }
    state.last_x = Some(new_x);
    state.last_y = Some(new_y);
}

/// Normalize Form-owned trailing newlines, then ensure the suffix has at
/// least `count` without modifying caller-owned bytes below `output_floor`.
fn ensure_trailing_breaks(out: &mut String, count: usize, output_floor: usize) {
    let (removable, missing) = trailing_break_adjustment(out, count, output_floor);
    out.truncate(out.len() - removable);
    for _ in 0..missing {
        out.push('\n');
    }
}

fn trailing_break_adjustment(out: &str, count: usize, output_floor: usize) -> (usize, usize) {
    let output_floor = output_floor.min(out.len());
    let removable = out.as_bytes()[output_floor..]
        .iter()
        .rev()
        .take_while(|&&byte| byte == b'\n')
        .count();
    let normalized_len = out.len() - removable;
    let existing = out.as_bytes()[..normalized_len]
        .iter()
        .rev()
        .take(count)
        .take_while(|&&byte| byte == b'\n')
        .count();
    (removable, count - existing)
}

/// Map a page's `/Resources/Font` entries to their font object IDs without
/// parsing the fonts themselves — the caller looks the parsed fonts up in
/// a document-wide cache to avoid re-parsing the same font across pages.
pub fn page_font_refs(
    doc: &crate::pdf::Document<'_>,
    resources: &Dictionary,
) -> HashMap<Vec<u8>, ObjectId> {
    let mut out = HashMap::new();
    let Some(font_dict_obj) = resources.get(b"Font") else {
        return out;
    };
    let font_dict = match font_dict_obj {
        Object::Reference(id) => doc.get_object(*id).and_then(Object::as_dict),
        Object::Dictionary(d) => Some(d),
        _ => None,
    };
    let Some(font_dict) = font_dict else {
        return out;
    };
    for (name, obj) in font_dict.iter() {
        if let Some(id) = obj.as_reference() {
            out.insert(name.to_vec(), id);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font_map(font: &PdfFont) -> HashMap<Vec<u8>, &PdfFont> {
        let mut m = HashMap::new();
        m.insert(b"F1".to_vec(), font);
        m
    }

    fn form(content: &[u8]) -> FormXObject {
        FormXObject {
            content: content.to_vec(),
            font_refs: None,
            xobject_refs: None,
        }
    }

    #[test]
    fn extract_handles_every_text_show_variant() {
        // Default font decodes ASCII bytes through StandardEncoding so we
        // can read back the output as the literal characters we wrote.
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        let stream = b"\
BT
/F1 12 Tf
14 TL
1 0 0 1 50 700 Tm
(line1) Tj
T*
(line2) '
0 -14 Td
(line3) \"
0 -14 TD
[(He) -200 (llo)] TJ
ET
";
        let out = extract_page_text(stream, &fonts, &images);
        // Every literal we emitted ought to appear.
        for needle in ["line1", "line2", "line3", "He", "llo"] {
            assert!(out.contains(needle), "missing {needle}:\n{out}");
        }
    }

    #[test]
    fn extract_drops_position_change_outside_text_object() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        // `Tm` outside a text object should be a no-op (updates last_x/y
        // but doesn't emit content).
        let stream = b"1 0 0 1 0 0 Tm";
        let out = extract_page_text(stream, &fonts, &images);
        assert!(out.is_empty());
    }

    #[test]
    fn extract_paints_image_xobjects_via_do_op() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let mut images = PageImages::new();
        images.insert(b"Im1".to_vec(), "img-001.jpg");
        // `Do` reads the most recent /Name operand and emits an image marker.
        let stream = b"/Im1 Do";
        let out = extract_page_text(stream, &fonts, &images);
        assert!(out.contains("img-001.jpg"));
        // Sentinel character must appear too.
        assert!(out.contains(IMAGE_MARK));
    }

    #[test]
    fn unknown_xobject_names_are_ignored() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        let stream = b"/Unknown Do";
        let out = extract_page_text(stream, &fonts, &images);
        assert!(out.is_empty());
    }

    #[test]
    fn stray_array_end_outside_array_is_ignored() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        let stream = b"BT /F1 12 Tf 100 700 Td (ok) Tj ] ET";
        let out = extract_page_text(stream, &fonts, &images);
        assert!(out.contains("ok"));
    }

    #[test]
    fn tj_array_ignores_non_text_operands() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        let stream = b"BT /F1 12 Tf [/Ignored (ok)] TJ ET";
        let out = extract_page_text(stream, &fonts, &images);
        assert_eq!(out, "ok");
    }

    #[test]
    fn pending_space_is_restored_when_decode_emits_nothing() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        let stream = b"BT /F1 12 Tf [(A) -200 (\x01) (B)] TJ ET";
        let out = extract_page_text(stream, &fonts, &images);
        assert_eq!(out, "A B");
    }

    #[test]
    fn form_word_boundary_waits_for_the_next_decoded_character() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let id = ObjectId(1, 0);
        let xobjects = HashMap::from([(b"Fm".to_vec(), id)]);
        let forms = HashMap::from([(id, form(b"BT /F1 12 Tf (word) Tj ET"))]);
        let form_fonts = HashMap::new();
        let image_filenames = ImageFilenames::new();

        let punctuation = extract_page_text_with_forms(
            b"/Fm Do BT /F1 12 Tf (!) Tj ET",
            &fonts,
            &xobjects,
            &forms,
            &form_fonts,
            &image_filenames,
        );
        assert_eq!(punctuation, "word!");

        let alphanumeric = extract_page_text_with_forms(
            b"/Fm Do BT /F1 12 Tf (\x01) Tj (next) Tj ET",
            &fonts,
            &xobjects,
            &forms,
            &form_fonts,
            &image_filenames,
        );
        assert_eq!(alphanumeric, "word next");
    }

    #[test]
    fn form_execution_budgets_bound_branching_and_output() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let root = ObjectId(1, 0);
        let branch = ObjectId(2, 0);
        let leaf = ObjectId(3, 0);
        let xobjects = HashMap::from([
            (b"Root".to_vec(), root),
            (b"Branch".to_vec(), branch),
            (b"Leaf".to_vec(), leaf),
        ]);
        let forms = HashMap::from([
            (root, form(b"/Branch Do /Branch Do")),
            (branch, form(b"/Leaf Do /Leaf Do")),
            (leaf, form(b"BT /F1 12 Tf (x) Tj ET")),
        ]);
        let form_fonts = HashMap::new();
        let image_filenames = ImageFilenames::new();

        let invocation_limited = extract_page_text_with_form_limits(
            b"/Root Do",
            &fonts,
            &xobjects,
            &forms,
            &form_fonts,
            &image_filenames,
            FormExecutionLimits {
                invocations: 4,
                input_bytes: usize::MAX,
                output_bytes: usize::MAX,
            },
        );
        assert_eq!(invocation_limited, "x x");

        let output_forms = HashMap::from([(leaf, form(b"BT /F1 12 Tf (abcdef) Tj ET"))]);
        let output_limited = extract_page_text_with_form_limits(
            b"/Leaf Do /Leaf Do",
            &fonts,
            &xobjects,
            &output_forms,
            &form_fonts,
            &image_filenames,
            FormExecutionLimits {
                invocations: usize::MAX,
                input_bytes: usize::MAX,
                output_bytes: 3,
            },
        );
        assert_eq!(output_limited, "abc");

        let silent = ObjectId(4, 0);
        let marker = ObjectId(5, 0);
        let input_xobjects = HashMap::from([
            (b"Root".to_vec(), root),
            (b"Silent".to_vec(), silent),
            (b"Marker".to_vec(), marker),
        ]);
        let root_content = b"/Silent Do /Silent Do /Marker Do";
        let silent_content = b"q Q";
        let input_forms = HashMap::from([
            (root, form(root_content)),
            (silent, form(silent_content)),
            (marker, form(b"BT /F1 12 Tf (x) Tj ET")),
        ]);
        let input_limited = extract_page_text_with_form_limits(
            b"/Root Do",
            &fonts,
            &input_xobjects,
            &input_forms,
            &form_fonts,
            &image_filenames,
            FormExecutionLimits {
                invocations: usize::MAX,
                input_bytes: root_content.len() + silent_content.len() * 2,
                output_bytes: usize::MAX,
            },
        );
        assert!(input_limited.is_empty());
    }

    #[test]
    fn form_image_preserves_caller_breaks_at_its_output_floor() {
        let form_id = ObjectId(1, 0);
        let image_id = ObjectId(2, 0);
        let fonts: PageFonts<'_> = HashMap::new();
        let xobjects = HashMap::from([(b"Fm".to_vec(), form_id)]);
        let local_xobjects = HashMap::from([(b"Im".to_vec(), image_id)]);
        let forms = HashMap::from([(
            form_id,
            FormXObject {
                content: b"/Im Do".to_vec(),
                font_refs: Some(HashMap::new()),
                xobject_refs: Some(local_xobjects),
            },
        )]);
        let form_fonts = HashMap::from([(form_id, PageFonts::new())]);
        let image_filenames = HashMap::from([(image_id, "img-001.jpg")]);
        let resources = ContentResources {
            fonts: &fonts,
            xobjects: &xobjects,
        };
        let form_context = FormContext {
            forms: &forms,
            fonts: &form_fonts,
            images: &image_filenames,
        };
        let mut state = TextState {
            pending_space: true,
            ..TextState::default()
        };
        let marker = format!("{mark}img-001.jpg{mark}\n\n", mark = IMAGE_MARK);
        let mut form_execution = FormExecution::new(FormExecutionLimits {
            invocations: usize::MAX,
            input_bytes: usize::MAX,
            output_bytes: marker.len(),
        });
        let mut page = PageBuilder::new(0);
        page.out = String::from("caller\n\n\n");

        extract_content_into(
            b"/Fm Do",
            &resources,
            &form_context,
            &mut state,
            &mut form_execution,
            &mut page,
        );

        assert_eq!(page.out, format!("caller\n\n\n{marker}"));
        assert!(!state.pending_space);
        assert!(!state.pending_form_word_boundary);
    }

    #[test]
    fn form_image_marker_is_atomic_at_the_output_budget() {
        let form_id = ObjectId(1, 0);
        let image_id = ObjectId(2, 0);
        let fonts: PageFonts<'_> = HashMap::new();
        let xobjects = HashMap::from([(b"Fm".to_vec(), form_id)]);
        let forms = HashMap::from([(
            form_id,
            FormXObject {
                content: b"/Im Do".to_vec(),
                font_refs: Some(HashMap::new()),
                xobject_refs: Some(HashMap::from([(b"Im".to_vec(), image_id)])),
            },
        )]);
        let form_fonts = HashMap::from([(form_id, PageFonts::new())]);
        let image_filenames = HashMap::from([(image_id, "img-001.jpg")]);
        let complete = format!("\n\n{mark}img-001.jpg{mark}\n\n", mark = IMAGE_MARK);

        let too_small = extract_page_text_with_form_limits(
            b"/Fm Do",
            &fonts,
            &xobjects,
            &forms,
            &form_fonts,
            &image_filenames,
            FormExecutionLimits {
                invocations: usize::MAX,
                input_bytes: usize::MAX,
                output_bytes: complete.len() - 1,
            },
        );
        assert!(too_small.is_empty());

        let exact = extract_page_text_with_form_limits(
            b"/Fm Do",
            &fonts,
            &xobjects,
            &forms,
            &form_fonts,
            &image_filenames,
            FormExecutionLimits {
                invocations: usize::MAX,
                input_bytes: usize::MAX,
                output_bytes: complete.len(),
            },
        );
        assert_eq!(exact, complete);

        let image_xobjects = HashMap::from([(b"Im".to_vec(), image_id)]);
        let resources = ContentResources {
            fonts: &fonts,
            xobjects: &image_xobjects,
        };
        let form_context = FormContext {
            forms: &forms,
            fonts: &form_fonts,
            images: &image_filenames,
        };
        let mut state = TextState {
            pending_space: true,
            pending_form_word_boundary: true,
            ..TextState::default()
        };
        let mut form_execution = FormExecution::new(FormExecutionLimits {
            invocations: usize::MAX,
            input_bytes: usize::MAX,
            output_bytes: complete.len() - 1,
        });
        form_execution.active.push(ActiveForm {
            id: form_id,
            output_floor: 0,
        });
        let mut page = PageBuilder::new(0);
        extract_content_into(
            b"/Im Do",
            &resources,
            &form_context,
            &mut state,
            &mut form_execution,
            &mut page,
        );
        assert!(page.out.is_empty());
        assert!(state.pending_space);
        assert!(state.pending_form_word_boundary);
    }

    #[test]
    fn ensure_trailing_breaks_collapses_existing_newlines() {
        let mut s = String::from("abc\n\n\n");
        ensure_trailing_breaks(&mut s, 1, 0);
        assert_eq!(s, "abc\n");
        // No prior newlines: append the requested number.
        let mut s = String::from("abc");
        ensure_trailing_breaks(&mut s, 2, 0);
        assert_eq!(s, "abc\n\n");

        let mut s = String::from("caller\n\n\n");
        let output_floor = s.len();
        ensure_trailing_breaks(&mut s, 2, output_floor);
        assert_eq!(s, "caller\n\n\n");

        let mut s = String::from("caller\n");
        let output_floor = s.len();
        ensure_trailing_breaks(&mut s, 2, output_floor);
        assert_eq!(s, "caller\n\n");

        let mut s = String::from("caller");
        let output_floor = s.len();
        ensure_trailing_breaks(&mut s, 2, output_floor);
        assert_eq!(s, "caller\n\n");
    }

    #[test]
    fn skip_inline_image_exits_on_eof_without_id() {
        // No `ID` keyword in the stream → skip_inline_image must terminate
        // when the outer tokenizer hits EOF.
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        let stream = b"BI /W 1";
        // Just exercising the path — should not loop forever.
        let _ = extract_page_text(stream, &fonts, &images);
    }

    #[test]
    fn operands_buffer_caps_at_capacity() {
        // More than 6 numeric operands shouldn't panic; the extras are
        // silently dropped by `push_num`.
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        let stream = b"1 2 3 4 5 6 7 8 9 Tm";
        let _ = extract_page_text(stream, &fonts, &images);
    }

    #[test]
    fn position_change_records_paragraph_break_on_big_dy() {
        // First text-show establishes a baseline; the next text-show is
        // shifted vertically by a large dy, triggering ensure_trailing_breaks
        // for a paragraph break.
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        let stream = b"\
BT
/F1 12 Tf
1 0 0 1 0 700 Tm
(top) Tj
1 0 0 1 0 100 Tm
(bottom) Tj
ET
";
        let out = extract_page_text(stream, &fonts, &images);
        assert!(out.contains("top"));
        assert!(out.contains("bottom"));
        assert!(out.contains("\n"));
    }

    #[test]
    fn position_change_records_normal_line_break_with_text() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        let stream = b"\
BT
/F1 12 Tf
1 0 0 1 0 100 Tm
(top) Tj
1 0 0 1 0 88 Tm
(bottom) Tj
ET
";
        let out = extract_page_text(stream, &fonts, &images);
        assert!(out.contains("top\nbottom"), "{out:?}");
    }

    #[test]
    fn line_break_uses_font_size_scaled_by_text_matrix() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        // Word-generated PDFs commonly select a 1-unit font and scale the
        // text matrix to the visible 12-point size. A one-em line advance is
        // a normal wrap, not the 12x paragraph jump implied by raw `Tf`.
        let stream = b"\
BT
/F1 1 Tf
12 0 0 12 0 100 Tm
(top) Tj
0 -1 Td
(bottom) Tj
ET
";
        let out = extract_page_text(stream, &fonts, &images);
        assert_eq!(out, "top\nbottom");
    }

    #[test]
    fn word_gap_uses_font_size_scaled_by_text_matrix() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        // Same Tf=1 / Tm-scale-12 setup as the line-break test. A 0.1-em
        // Td is kerning; a 0.3-em Td is a word break. Raw `Tf` would treat
        // both as spaces because 0.1*12 already exceeds 0.2.
        let kerning = b"\
BT
/F1 1 Tf
12 0 0 12 0 100 Tm
(Hello) Tj
0.1 0 Td
(World) Tj
ET
";
        assert_eq!(extract_page_text(kerning, &fonts, &images), "HelloWorld");
        let word_break = b"\
BT
/F1 1 Tf
12 0 0 12 0 100 Tm
(Hello) Tj
0.3 0 Td
(World) Tj
ET
";
        assert_eq!(
            extract_page_text(word_break, &fonts, &images),
            "Hello World"
        );
    }

    #[test]
    fn text_direction_change_preserves_word_boundary() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        // A rotated production tag follows an upright footer with a small
        // page-space delta. It is a distinct text run even though the scaled
        // vertical threshold correctly does not classify it as a new line.
        let stream = b"\
BT
/F1 1 Tf
6.5 0 0 6.5 25 20 Tm
(932) Tj
0 5 -5 0 22 18 Tm
(cprice) Tj
ET
";
        let out = extract_page_text(stream, &fonts, &images);
        assert_eq!(out, "932 cprice");
    }

    #[test]
    fn position_change_with_empty_output_records_state_only() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        let stream = b"BT /F1 12 Tf 1 0 0 1 0 100 Tm 1 0 0 1 0 88 Tm ET";
        let out = extract_page_text(stream, &fonts, &images);
        assert!(out.is_empty());
    }

    #[test]
    fn operators_with_missing_operands_are_no_ops() {
        // Every text operator's body is gated on having the right operands.
        // Feeding bare operators without operands exercises the
        // pattern-doesn't-match arm of each `if let` in the dispatch.
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        let stream = b"BT Tf TL Tm Td TD T* Tj ' \" TJ Do ET";
        let out = extract_page_text(stream, &fonts, &images);
        assert!(out.is_empty());
    }

    #[test]
    fn emit_without_font_is_a_no_op() {
        // `Tj` runs before any `Tf` — emit returns early because state.font
        // is None.
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        let stream = b"BT (no font yet) Tj ET";
        let out = extract_page_text(stream, &fonts, &images);
        assert!(out.is_empty());
    }

    #[test]
    fn inline_image_block_is_skipped_via_id() {
        // BI / ID / EI sequence — the content interpreter should swallow
        // the inline image bytes and resume with the trailing operator.
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let images = PageImages::new();
        let stream = b"BT /F1 12 Tf BI /W 1 /H 1 ID \x00\x01\x02\nEI (after) Tj ET";
        let out = extract_page_text(stream, &fonts, &images);
        assert!(out.contains("after"));
    }

    #[test]
    fn page_font_refs_returns_empty_for_non_dict_font_entry() {
        // /Font set to an integer — neither a Reference nor a Dictionary →
        // page_font_refs returns empty.
        let mut res = crate::pdf::Dictionary::new();
        res.insert(b"Font".to_vec(), Object::Integer(0));
        let bytes = build_pdf_with_xref(
            b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
",
        );
        let doc = crate::pdf::Document::load(&bytes).unwrap();
        assert!(page_font_refs(&doc, &res).is_empty());
    }

    #[test]
    fn page_font_refs_returns_empty_when_font_entry_missing() {
        let res = crate::pdf::Dictionary::new();
        let bytes = build_pdf_with_xref(
            b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
",
        );
        let doc = crate::pdf::Document::load(&bytes).unwrap();
        assert!(page_font_refs(&doc, &res).is_empty());
    }

    #[test]
    fn page_font_refs_returns_empty_when_font_reference_resolves_to_non_dict() {
        // /Font is a Reference pointing at a non-dict object → empty.
        let mut res = crate::pdf::Dictionary::new();
        res.insert(b"Font".to_vec(), Object::Reference(ObjectId(4, 0)));
        let bytes = build_pdf_with_xref(
            b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
4 0 obj 42 endobj
",
        );
        let doc = crate::pdf::Document::load(&bytes).unwrap();
        assert!(page_font_refs(&doc, &res).is_empty());
    }

    #[test]
    fn page_font_refs_handles_each_dict_shape() {
        use crate::pdf::Document;
        // Build a doc with a Font dict referenced indirectly and a direct
        // font ref inside a resources dict.
        let pdf = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources 4 0 R/MediaBox[0 0 1 1]>> endobj
4 0 obj <</Font 5 0 R>> endobj
5 0 obj <</F1 6 0 R>> endobj
6 0 obj <</Type/Font/Subtype/Type1/BaseFont/Helvetica>> endobj
";
        let bytes = build_pdf_with_xref(pdf);
        let doc = Document::load(&bytes).unwrap();
        let page_id = doc.pages()[0];
        let res = super::super::page_resources(&doc, page_id).unwrap();
        let refs = page_font_refs(&doc, &res);
        assert!(refs.contains_key(b"F1".as_slice()));
    }

    #[test]
    fn emit_records_positioned_text_and_path_rects() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let layout = extract_page_layout_with_forms(
            b"BT /F1 18 Tf 1 0 0 1 50 700 Tm (Hello) Tj ET 10 20 30 40 re",
            &fonts,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(layout.text, "Hello");
        assert_eq!(layout.spans.len(), 1);
        assert!((layout.spans[0].x - 50.0).abs() < 0.1);
        assert!((layout.spans[0].y - 700.0).abs() < 0.1);
        assert!(layout.spans[0].font_size > 10.0);
        assert_eq!(layout.rects.len(), 1);
        assert!((layout.rects[0].w - 30.0).abs() < 0.1);
    }

    #[test]
    fn graphics_state_and_path_ops_are_recorded() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let layout = extract_page_layout_with_forms(
            b"q 2 0 0 2 0 0 cm 0 0 m 40 0 l Q 10 20 30 40 re /Span BMC BT /F1 12 Tf (X) Tj ET EMC",
            &fonts,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(layout.rects.len() >= 2);
        assert_eq!(layout.spans[0].text, "X");
        assert_eq!(layout.spans[0].mcid, None);
        // `Q` restores identity so the unscaled `re` is recorded as 30×40.
        assert!(layout.rects.iter().any(|r| (r.w - 30.0).abs() < 0.1));
    }

    #[test]
    fn bdc_attaches_mcid_to_span() {
        let font = PdfFont::default();
        let fonts = font_map(&font);
        let layout = extract_page_layout_with_forms(
            b"BT /F1 12 Tf /P << /MCID 3 >> BDC (Hi) Tj EMC ET",
            &fonts,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(layout.spans[0].text, "Hi");
        assert_eq!(layout.spans[0].mcid, Some(3));
    }

    /// Helper for in-test PDF construction. Mirrors the minimal_pdf in
    /// pdf::tests but parameterised on the body bytes.
    fn build_pdf_with_xref(body: &[u8]) -> Vec<u8> {
        let mut out = body.to_vec();
        let xref_offset = out.len();
        // Scan for `N 0 obj` headers in document order.
        let mut offsets = Vec::new();
        let mut n = 1;
        loop {
            let needle = format!("{n} 0 obj");
            let Some(p) = (0..=out.len().saturating_sub(needle.len()))
                .find(|&i| &out[i..i + needle.len()] == needle.as_bytes())
            else {
                break;
            };
            offsets.push(p);
            n += 1;
        }
        let count = offsets.len();
        let mut xref = String::from("xref\n");
        xref.push_str(&format!("0 {}\n", count + 1));
        xref.push_str("0000000000 65535 f \n");
        for off in &offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        xref.push_str(&format!(
            "trailer <</Size {}/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n",
            count + 1
        ));
        out.extend_from_slice(xref.as_bytes());
        out
    }
}
