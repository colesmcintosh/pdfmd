//! End-to-end parsing accuracy and layout quality test suite.
//!
//! Unlike the heuristics unit tests (which construct `Span` structs directly),
//! every test here feeds real byte-level PDF files — with real font `/Widths`,
//! content-stream operators, path rules, CMaps, Form XObjects, and structure
//! trees — through `pdfmd::convert_pdf_to_markdown`, and validates the exact
//! structured Markdown output (tables, reading order, headings, lists, code,
//! emphasis, and false-positive resistance).

use std::fmt::Write as _;
use std::path::PathBuf;

use pdfmd::{convert_pdf_to_markdown, ConvertOptions};

// ---- Structured Markdown inspection helpers --------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    balanced: bool,
}

fn parse_table_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let body = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let body = body.strip_suffix('|').unwrap_or(body);
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            cur.push('|');
            chars.next();
        } else if c == '|' {
            cells.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().to_string());
    (cells.len() >= 2).then_some(cells)
}

fn is_separator_row(line: &str) -> bool {
    let Some(cells) = parse_table_row(line) else {
        return false;
    };
    cells.iter().all(|c| {
        let s = c.trim();
        let s = s.strip_prefix(':').unwrap_or(s);
        let s = s.strip_suffix(':').unwrap_or(s);
        s.len() >= 3 && s.bytes().all(|b| b == b'-')
    })
}

fn parse_tables(md: &str) -> Vec<Table> {
    let lines: Vec<&str> = md.lines().collect();
    let mut tables = Vec::new();
    let mut i = 0;
    let mut in_fence = false;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            i += 1;
            continue;
        }
        if !in_fence && i + 1 < lines.len() && is_separator_row(lines[i + 1]) {
            if let Some(headers) = parse_table_row(lines[i]) {
                let sep_cols = parse_table_row(lines[i + 1]).map_or(0, |v| v.len());
                let mut balanced = sep_cols == headers.len();
                let mut rows = Vec::new();
                i += 2;
                while i < lines.len() && !lines[i].trim().is_empty() {
                    let Some(cells) = parse_table_row(lines[i]) else {
                        break;
                    };
                    if cells.len() != headers.len() {
                        balanced = false;
                    }
                    rows.push(cells);
                    i += 1;
                }
                tables.push(Table {
                    headers,
                    rows,
                    balanced,
                });
                continue;
            }
        }
        i += 1;
    }
    tables
}

fn parse_headings(md: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in md.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let level = trimmed.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&level) {
            if let Some(rest) = trimmed.get(level..).and_then(|s| s.strip_prefix(' ')) {
                out.push((level, rest.trim().to_string()));
            }
        }
    }
    out
}

fn parse_list_items(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in md.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            out.push(rest.trim().to_string());
            continue;
        }
        let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            if let Some(rest) = trimmed[digits..].strip_prefix(". ") {
                out.push(rest.trim().to_string());
            }
        }
    }
    out
}

fn assert_hygiene(md: &str) {
    assert!(
        !md.contains('\u{FFFD}'),
        "output contains replacement char:\n{md}"
    );
    for ch in md.chars() {
        assert!(
            (ch as u32) >= 32 || matches!(ch, '\n' | '\r' | '\t'),
            "output contains unexpected control char U+{:04X}:\n{md}",
            ch as u32
        );
    }
    for (idx, t) in parse_tables(md).iter().enumerate() {
        assert!(
            t.balanced,
            "table #{idx} has unbalanced columns: {t:?}\n{md}"
        );
    }
}

fn assert_order(md: &str, needles: &[&str]) {
    let mut prev = 0usize;
    for &needle in needles {
        let Some(rel) = md[prev..].find(needle) else {
            panic!("expected `{needle}` after byte {prev} in:\n{md}");
        };
        prev += rel + needle.len();
    }
}

fn convert(pdf: &[u8]) -> String {
    let out = convert_pdf_to_markdown(pdf, &ConvertOptions::default()).expect("convert");
    assert_hygiene(&out.markdown);
    out.markdown
}

// ---- Byte-level synthetic PDF builder --------------------------------------

/// Font resource aliases wired into every synthetic page.
#[derive(Clone, Copy)]
enum Font {
    Regular,    // /F1 Helvetica (proportional widths)
    Bold,       // /F2 Helvetica-Bold
    Italic,     // /F3 Helvetica-Oblique
    BoldItalic, // /F4 Helvetica-BoldOblique
    Mono,       // /F5 Courier (fixed 600 width)
}

impl Font {
    fn resource_name(self) -> &'static str {
        match self {
            Font::Regular => "/F1",
            Font::Bold => "/F2",
            Font::Italic => "/F3",
            Font::BoldItalic => "/F4",
            Font::Mono => "/F5",
        }
    }
}

struct PageStream {
    ops: String,
}

impl PageStream {
    fn new() -> Self {
        Self { ops: String::new() }
    }

    fn text(&mut self, font: Font, size: f32, x: f32, y: f32, s: &str) -> &mut Self {
        let esc = pdf_escape(s);
        let _ = writeln!(
            self.ops,
            "BT {} {size:.2} Tf 1 0 0 1 {x:.2} {y:.2} Tm ({esc}) Tj ET",
            font.resource_name()
        );
        self
    }

    /// Emit text using an explicit `TJ` array of strings and kerning numbers.
    fn text_tj(
        &mut self,
        font: Font,
        size: f32,
        x: f32,
        y: f32,
        items: &[TjItem<'_>],
    ) -> &mut Self {
        let _ = write!(
            self.ops,
            "BT {} {size:.2} Tf 1 0 0 1 {x:.2} {y:.2} Tm [",
            font.resource_name()
        );
        for item in items {
            match item {
                TjItem::Str(s) => {
                    let _ = write!(self.ops, "({}) ", pdf_escape(s));
                }
                TjItem::Kern(k) => {
                    let _ = write!(self.ops, "{k:.1} ");
                }
            }
        }
        self.ops.push_str("] TJ ET\n");
        self
    }

    /// Emit text encoded as raw bytes using a custom font resource (e.g. `/F6`).
    fn text_raw_hex(&mut self, font_res: &str, size: f32, x: f32, y: f32, hex: &str) -> &mut Self {
        let _ = writeln!(
            self.ops,
            "BT {font_res} {size:.2} Tf 1 0 0 1 {x:.2} {y:.2} Tm <{hex}> Tj ET"
        );
        self
    }

    /// Emit 90-degree rotated margin text (`0 1 -1 0 x y Tm`).
    fn rotated_text(&mut self, font: Font, size: f32, x: f32, y: f32, s: &str) -> &mut Self {
        let esc = pdf_escape(s);
        let _ = writeln!(
            self.ops,
            "BT {} {size:.2} Tf 0 1 -1 0 {x:.2} {y:.2} Tm ({esc}) Tj ET",
            font.resource_name()
        );
        self
    }

    /// Emit marked content tagged with `/MCID n` for `/StructTreeRoot`.
    fn tagged_text(
        &mut self,
        tag: &str,
        mcid: u32,
        font: Font,
        size: f32,
        pos: (f32, f32),
        s: &str,
    ) -> &mut Self {
        let (x, y) = pos;
        let esc = pdf_escape(s);
        let _ = writeln!(
            self.ops,
            "/{tag} <</MCID {mcid}>> BDC BT {} {size:.2} Tf 1 0 0 1 {x:.2} {y:.2} Tm ({esc}) Tj ET EMC",
            font.resource_name()
        );
        self
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) -> &mut Self {
        let _ = writeln!(self.ops, "{x:.2} {y:.2} {w:.2} {h:.2} re S");
        self
    }

    fn hline(&mut self, x0: f32, x1: f32, y: f32) -> &mut Self {
        let _ = writeln!(self.ops, "{x0:.2} {y:.2} m {x1:.2} {y:.2} l S");
        self
    }

    fn vline(&mut self, x: f32, y0: f32, y1: f32) -> &mut Self {
        let _ = writeln!(self.ops, "{x:.2} {y0:.2} m {x:.2} {y1:.2} l S");
        self
    }

    /// Thin filled rectangle (common PDF encoding for rules and underlines).
    fn thin_hrule(&mut self, x: f32, y: f32, w: f32) -> &mut Self {
        let _ = writeln!(self.ops, "{x:.2} {y:.2} {w:.2} 0.60 re f");
        self
    }

    fn raw(&mut self, ops: &str) -> &mut Self {
        self.ops.push_str(ops);
        if !ops.ends_with('\n') {
            self.ops.push('\n');
        }
        self
    }

    /// Draw a ruled grid using `m`/`l` segments at the given x and y lines.
    fn stroke_grid(&mut self, xs: &[f32], ys: &[f32]) -> &mut Self {
        let (x0, x1) = (*xs.first().unwrap(), *xs.last().unwrap());
        let (y0, y1) = (*ys.first().unwrap(), *ys.last().unwrap());
        for &y in ys {
            self.hline(x0, x1, y);
        }
        for &x in xs {
            self.vline(x, y0, y1);
        }
        self
    }

    /// Draw a ruled grid using individual cell `re` rectangles.
    fn cell_rect_grid(&mut self, xs: &[f32], ys: &[f32]) -> &mut Self {
        for r in 0..ys.len() - 1 {
            for c in 0..xs.len() - 1 {
                self.rect(xs[c], ys[r], xs[c + 1] - xs[c], ys[r + 1] - ys[r]);
            }
        }
        self
    }
}

enum TjItem<'a> {
    Str(&'a str),
    Kern(f32),
}

fn pdf_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            // WinAnsi mappings used by synthetic tests.
            '\u{2022}' => out.push_str("\\225"), // bullet
            '\u{2013}' => out.push_str("\\226"), // en-dash
            '\u{2014}' => out.push_str("\\227"), // em-dash
            '\u{201C}' => out.push_str("\\223"),
            '\u{201D}' => out.push_str("\\224"),
            '\u{2019}' => out.push_str("\\222"),
            '\u{00B7}' => out.push_str("\\267"), // middle dot
            c if (c as u32) < 128 => out.push(c),
            c if (c as u32) <= 255 => {
                let _ = write!(out, "\\{:03o}", c as u32);
            }
            _ => out.push('?'),
        }
    }
    out
}

/// Realistic Helvetica-like glyph widths (1/1000 em) for ASCII 32..=126 so
/// `pdfmd`'s real pen-advance math mirrors real-world PDFs.
fn helvetica_widths_array() -> String {
    let mut out = String::from("[");
    for code in 32u8..=126 {
        let w: u16 = match code {
            b' ' => 278,
            b'!' | b'i' | b'l' | b'|' | b'.' | b',' | b':' | b';' | b'\'' => 278,
            b'(' | b')' | b'[' | b']' | b'f' | b't' | b'r' | b'-' | b'/' => 333,
            b'I' | b'j' => 278,
            b'W' | b'M' | b'm' => 833,
            b'w' | b'G' | b'O' | b'Q' | b'%' => 778,
            b'A'..=b'Z' => 667,
            b'0'..=b'9' => 556,
            _ => 520,
        };
        let _ = write!(out, "{w} ");
    }
    out.push(']');
    out
}

fn courier_widths_array() -> String {
    let mut out = String::from("[");
    for _ in 32u8..=126 {
        out.push_str("600 ");
    }
    out.push(']');
    out
}

struct PdfBuilder {
    pages: Vec<PageStream>,
    extra_objs: Vec<(u32, Vec<u8>)>,
    extra_font_res: Vec<(String, u32)>,
    extra_xobj_res: Vec<(String, u32)>,
    struct_entries: Vec<(usize, u32, String)>, // (page_idx, mcid, role_name)
    next_id: u32,
}

impl PdfBuilder {
    fn new() -> Self {
        // Reserve 1=Catalog, 2=Pages, 3..=7=5 standard fonts.
        Self {
            pages: Vec::new(),
            extra_objs: Vec::new(),
            extra_font_res: Vec::new(),
            extra_xobj_res: Vec::new(),
            struct_entries: Vec::new(),
            next_id: 20,
        }
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn add_page(&mut self, page: PageStream) -> usize {
        let idx = self.pages.len();
        self.pages.push(page);
        idx
    }

    fn add_struct_role(&mut self, page_idx: usize, mcid: u32, role: &str) {
        self.struct_entries.push((page_idx, mcid, role.to_string()));
    }

    /// Add a custom font with a `/ToUnicode` CMap under resource name `res` (e.g. `F6`).
    fn add_cmap_font(&mut self, res: &str, base_font: &str, cmap_body: &str) {
        let cmap_id = self.alloc_id();
        let font_id = self.alloc_id();
        let cmap_obj = format!(
            "<</Length {}>>\nstream\n{cmap_body}\nendstream",
            cmap_body.len()
        );
        let widths = helvetica_widths_array();
        let font_obj = format!(
            "<</Type/Font/Subtype/Type1/BaseFont/{base_font}/FirstChar 32/LastChar 126/Widths {widths}/ToUnicode {cmap_id} 0 R>>"
        );
        self.extra_objs.push((cmap_id, cmap_obj.into_bytes()));
        self.extra_objs.push((font_id, font_obj.into_bytes()));
        self.extra_font_res.push((res.to_string(), font_id));
    }

    /// Add a Form XObject under resource name `res` (e.g. `Fm1`).
    fn add_form_xobject(&mut self, res: &str, content: &str) {
        let form_id = self.alloc_id();
        let obj = format!(
            "<</Type/XObject/Subtype/Form/BBox[0 0 612 792]/Length {}>>\nstream\n{content}\nendstream",
            content.len()
        );
        self.extra_objs.push((form_id, obj.into_bytes()));
        self.extra_xobj_res.push((res.to_string(), form_id));
    }

    fn finish(mut self) -> Vec<u8> {
        let mut objs: Vec<(u32, Vec<u8>)> = Vec::new();
        let prop_w = helvetica_widths_array();
        let mono_w = courier_widths_array();

        // Standard fonts 3..=7.
        let font_defs = [
            (3, "Helvetica", &prop_w),
            (4, "Helvetica-Bold", &prop_w),
            (5, "Helvetica-Oblique", &prop_w),
            (6, "Helvetica-BoldOblique", &prop_w),
            (7, "Courier", &mono_w),
        ];
        for (id, name, w) in font_defs {
            let dict = format!(
                "<</Type/Font/Subtype/Type1/BaseFont/{name}/Encoding/WinAnsiEncoding/FirstChar 32/LastChar 126/Widths {w}>>"
            );
            objs.push((id, dict.into_bytes()));
        }

        let mut font_res = String::from("/F1 3 0 R /F2 4 0 R /F3 5 0 R /F4 6 0 R /F5 7 0 R");
        for (res, id) in &self.extra_font_res {
            let _ = write!(font_res, " /{res} {id} 0 R");
        }
        let xobj_res = if self.extra_xobj_res.is_empty() {
            String::new()
        } else {
            let mut s = String::from("/XObject<<");
            for (res, id) in &self.extra_xobj_res {
                let _ = write!(s, "/{res} {id} 0 R ");
            }
            s.push_str(">>");
            s
        };

        let mut page_ids = Vec::new();
        for page in std::mem::take(&mut self.pages) {
            let content_id = self.alloc_id();
            let page_id = self.alloc_id();
            let stream_obj = format!(
                "<</Length {}>>\nstream\n{}\nendstream",
                page.ops.len(),
                page.ops
            );
            let page_obj = format!(
                "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Resources<</Font<<{font_res}>>{xobj_res}>>/Contents {content_id} 0 R>>"
            );
            objs.push((content_id, stream_obj.into_bytes()));
            objs.push((page_id, page_obj.into_bytes()));
            page_ids.push(page_id);
        }

        let struct_root_entry = if self.struct_entries.is_empty() {
            String::new()
        } else {
            let root_id = self.alloc_id();
            let mut elem_refs = String::new();
            for (page_idx, mcid, role) in std::mem::take(&mut self.struct_entries) {
                let elem_id = self.alloc_id();
                let pg_id = page_ids[page_idx];
                let elem = format!("<</Type/StructElem/S/{role}/Pg {pg_id} 0 R/K {mcid}>>");
                objs.push((elem_id, elem.into_bytes()));
                let _ = write!(elem_refs, "{elem_id} 0 R ");
            }
            let root = format!("<</Type/StructTreeRoot/K[{elem_refs}]>>");
            objs.push((root_id, root.into_bytes()));
            format!("/StructTreeRoot {root_id} 0 R")
        };

        let mut kids = String::new();
        for id in &page_ids {
            let _ = write!(kids, "{id} 0 R ");
        }
        objs.push((
            1,
            format!("<</Type/Catalog/Pages 2 0 R{struct_root_entry}>>").into_bytes(),
        ));
        objs.push((
            2,
            format!("<</Type/Pages/Kids[{kids}]/Count {}>>", page_ids.len()).into_bytes(),
        ));
        objs.extend(self.extra_objs);
        objs.sort_by_key(|(id, _)| *id);

        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets: Vec<(u32, usize)> = Vec::new();
        for (id, body) in &objs {
            offsets.push((*id, out.len()));
            out.extend_from_slice(format!("{id} 0 obj ").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b" endobj\n");
        }
        let max_id = offsets.iter().map(|(n, _)| *n).max().unwrap_or(0);
        let xref_off = out.len();
        let mut xref = format!("xref\n0 {}\n0000000000 65535 f \n", max_id + 1);
        for n in 1..=max_id {
            match offsets.iter().find(|(id, _)| *id == n) {
                Some((_, off)) => {
                    let _ = writeln!(xref, "{off:010} 00000 n ");
                }
                None => xref.push_str("0000000000 00000 f \n"),
            }
        }
        let _ = write!(
            xref,
            "trailer <</Size {}/Root 1 0 R>>\nstartxref\n{xref_off}\n%%EOF\n",
            max_id + 1
        );
        out.extend_from_slice(xref.as_bytes());
        out
    }
}

fn single_page(build: impl FnOnce(&mut PageStream)) -> Vec<u8> {
    let mut doc = PdfBuilder::new();
    let mut page = PageStream::new();
    build(&mut page);
    doc.add_page(page);
    doc.finish()
}

// ============================================================================
// 1. RULED TABLES (POSITIVE CASES)
// ============================================================================

#[test]
fn ruled_table_from_cell_rectangles() {
    let pdf = single_page(|p| {
        p.text(Font::Bold, 18.0, 72.0, 740.0, "Quarterly Summary");
        let xs = [72.0, 192.0, 312.0, 432.0];
        let ys = [600.0, 624.0, 648.0, 672.0];
        p.cell_rect_grid(&xs, &ys);
        let grid = [
            ["Region", "Q1", "Q2"],
            ["North", "120", "145"],
            ["South", "98", "112"],
        ];
        for (r, row) in grid.iter().enumerate() {
            let y = 656.0 - r as f32 * 24.0;
            for (c, val) in row.iter().enumerate() {
                p.text(Font::Regular, 10.0, xs[c] + 8.0, y, val);
            }
        }
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].headers, vec!["Region", "Q1", "Q2"]);
    assert_eq!(
        tables[0].rows,
        vec![vec!["North", "120", "145"], vec!["South", "98", "112"]]
    );
}

#[test]
fn ruled_table_from_stroke_lines() {
    let pdf = single_page(|p| {
        let xs = [72.0, 200.0, 320.0, 440.0];
        let ys = [560.0, 585.0, 610.0, 635.0];
        p.stroke_grid(&xs, &ys);
        let grid = [
            ["Stage", "Time", "Share"],
            ["Inflate", "0.9 ms", "21%"],
            ["Extract", "2.1 ms", "48%"],
        ];
        for (r, row) in grid.iter().enumerate() {
            let y = 618.0 - r as f32 * 25.0;
            for (c, val) in row.iter().enumerate() {
                p.text(Font::Regular, 10.0, xs[c] + 6.0, y, val);
            }
        }
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].headers, vec!["Stage", "Time", "Share"]);
    assert_eq!(
        tables[0].rows,
        vec![
            vec!["Inflate", "0.9 ms", "21%"],
            vec!["Extract", "2.1 ms", "48%"]
        ]
    );
}

#[test]
fn ruled_table_under_cm_transform() {
    let pdf = single_page(|p| {
        p.raw("q 1 0 0 1 72 500 cm");
        p.stroke_grid(&[0.0, 120.0, 240.0], &[0.0, 24.0, 48.0, 72.0]);
        p.text(Font::Regular, 10.0, 10.0, 56.0, "Key");
        p.text(Font::Regular, 10.0, 130.0, 56.0, "Value");
        p.text(Font::Regular, 10.0, 10.0, 32.0, "Alpha");
        p.text(Font::Regular, 10.0, 130.0, 32.0, "42");
        p.text(Font::Regular, 10.0, 10.0, 8.0, "Beta");
        p.text(Font::Regular, 10.0, 130.0, 8.0, "99");
        p.raw("Q");
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].headers, vec!["Key", "Value"]);
    assert_eq!(
        tables[0].rows,
        vec![vec!["Alpha", "42"], vec!["Beta", "99"]]
    );
}

#[test]
fn ruled_table_with_wrapped_cell_text_and_empty_cells() {
    let pdf = single_page(|p| {
        let xs = [72.0, 200.0, 360.0];
        let ys = [560.0, 600.0, 640.0, 668.0];
        p.stroke_grid(&xs, &ys);
        p.text(Font::Bold, 10.0, 80.0, 650.0, "Feature");
        p.text(Font::Bold, 10.0, 208.0, 650.0, "Notes");
        // Row 1: multi-line wrapped prose inside second cell.
        p.text(Font::Regular, 10.0, 80.0, 622.0, "Tables");
        p.text(Font::Regular, 10.0, 208.0, 624.0, "Supports ruled");
        p.text(Font::Regular, 10.0, 208.0, 610.0, "and borderless");
        // Row 2: empty second cell.
        p.text(Font::Regular, 10.0, 80.0, 578.0, "Images");
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].headers, vec!["Feature", "Notes"]);
    assert_eq!(
        tables[0].rows,
        vec![
            vec!["Tables", "Supports ruled and borderless"],
            vec!["Images", ""]
        ]
    );
}

#[test]
fn ruled_table_escapes_literal_pipe_characters() {
    let pdf = single_page(|p| {
        p.cell_rect_grid(&[72.0, 200.0, 340.0], &[600.0, 624.0, 648.0]);
        p.text(Font::Regular, 10.0, 80.0, 632.0, "Op");
        p.text(Font::Regular, 10.0, 208.0, 632.0, "Syntax");
        p.text(Font::Regular, 10.0, 80.0, 608.0, "Or");
        p.text(Font::Regular, 10.0, 208.0, 608.0, "a | b");
    });
    let md = convert(&pdf);
    assert!(md.contains("a \\| b"), "pipe should be escaped:\n{md}");
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].rows, vec![vec!["Or", "a | b"]]);
}

#[test]
fn ruled_table_preserves_reading_order_above_and_below() {
    let pdf = single_page(|p| {
        p.text(
            Font::Regular,
            11.0,
            72.0,
            730.0,
            "Paragraph ABOVE the ruled table comes first.",
        );
        p.stroke_grid(&[72.0, 200.0, 340.0], &[620.0, 644.0, 668.0, 692.0]);
        p.text(Font::Regular, 10.0, 80.0, 676.0, "Metric");
        p.text(Font::Regular, 10.0, 208.0, 676.0, "Score");
        p.text(Font::Regular, 10.0, 80.0, 652.0, "Recall");
        p.text(Font::Regular, 10.0, 208.0, 652.0, "98.1%");
        p.text(Font::Regular, 10.0, 80.0, 628.0, "Precision");
        p.text(Font::Regular, 10.0, 208.0, 628.0, "97.4%");
        p.text(
            Font::Regular,
            11.0,
            72.0,
            580.0,
            "Paragraph BELOW the ruled table must follow it.",
        );
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_order(
        &md,
        &[
            "Paragraph ABOVE the ruled table",
            "| Metric | Score |",
            "| Precision | 97.4% |",
            "Paragraph BELOW the ruled table",
        ],
    );
}

#[test]
fn multiple_ruled_tables_on_same_page_stay_separate() {
    let pdf = single_page(|p| {
        p.text(Font::Bold, 14.0, 72.0, 740.0, "Two Tables Report");
        // Table 1 at top.
        p.stroke_grid(&[72.0, 180.0, 290.0], &[650.0, 674.0, 698.0, 722.0]);
        p.text(Font::Regular, 10.0, 80.0, 706.0, "Fruit");
        p.text(Font::Regular, 10.0, 188.0, 706.0, "Count");
        p.text(Font::Regular, 10.0, 80.0, 682.0, "Apple");
        p.text(Font::Regular, 10.0, 188.0, 682.0, "10");
        p.text(Font::Regular, 10.0, 80.0, 658.0, "Pear");
        p.text(Font::Regular, 10.0, 188.0, 658.0, "20");

        // Prose between the two tables must NOT be swallowed.
        p.text(
            Font::Regular,
            11.0,
            72.0,
            610.0,
            "Middle commentary separates the first and second tables cleanly.",
        );

        // Table 2 below.
        p.stroke_grid(&[72.0, 180.0, 290.0], &[500.0, 524.0, 548.0, 572.0]);
        p.text(Font::Regular, 10.0, 80.0, 556.0, "City");
        p.text(Font::Regular, 10.0, 188.0, 556.0, "Temp");
        p.text(Font::Regular, 10.0, 80.0, 532.0, "Oslo");
        p.text(Font::Regular, 10.0, 188.0, 532.0, "4C");
        p.text(Font::Regular, 10.0, 80.0, 508.0, "Rome");
        p.text(Font::Regular, 10.0, 188.0, 508.0, "19C");
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 2, "expected 2 distinct tables:\n{md}");
    assert_eq!(tables[0].headers, vec!["Fruit", "Count"]);
    assert_eq!(tables[1].headers, vec!["City", "Temp"]);
    assert_order(
        &md,
        &[
            "| Fruit | Count |",
            "Middle commentary separates",
            "| City | Temp |",
        ],
    );
}

#[test]
fn ruled_table_ignores_unrelated_header_and_footer_rules() {
    let pdf = single_page(|p| {
        // Page header rule at top.
        p.thin_hrule(72.0, 755.0, 468.0);
        p.text(Font::Regular, 9.0, 72.0, 760.0, "Technical Specification");
        p.text(
            Font::Regular,
            11.0,
            72.0,
            720.0,
            "Introductory sentence above the ruled table.",
        );
        // Real ruled table in middle.
        p.stroke_grid(&[72.0, 200.0, 340.0], &[580.0, 604.0, 628.0, 652.0]);
        p.text(Font::Regular, 10.0, 80.0, 636.0, "Param");
        p.text(Font::Regular, 10.0, 208.0, 636.0, "Default");
        p.text(Font::Regular, 10.0, 80.0, 612.0, "Timeout");
        p.text(Font::Regular, 10.0, 208.0, 612.0, "30s");
        p.text(Font::Regular, 10.0, 80.0, 588.0, "Retries");
        p.text(Font::Regular, 10.0, 208.0, 588.0, "3");
        // Prose below + footer rule.
        p.text(
            Font::Regular,
            11.0,
            72.0,
            530.0,
            "Closing explanation sits beneath the table.",
        );
        p.thin_hrule(72.0, 60.0, 468.0);
        p.text(Font::Regular, 9.0, 280.0, 44.0, "Page 1");
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].headers, vec!["Param", "Default"]);
    assert_eq!(
        tables[0].rows,
        vec![vec!["Timeout", "30s"], vec!["Retries", "3"]]
    );
    assert_order(
        &md,
        &[
            "Introductory sentence",
            "| Param | Default |",
            "Closing explanation",
        ],
    );
}

#[test]
fn ruled_table_with_stacked_numeric_records_falls_back_to_rows() {
    let pdf = single_page(|p| {
        // Horizontal band rules + center divider framing 2 numeric records in one band.
        p.thin_hrule(72.0, 680.0, 220.0);
        p.thin_hrule(72.0, 656.0, 220.0);
        p.thin_hrule(72.0, 616.0, 220.0);
        p.vline(182.0, 616.0, 680.0);
        p.text(Font::Bold, 10.0, 82.0, 664.0, "Model");
        p.text(Font::Bold, 10.0, 192.0, 664.0, "BLEU");
        p.text(Font::Regular, 10.0, 82.0, 642.0, "Base");
        p.text(Font::Regular, 10.0, 192.0, 642.0, "27.3");
        p.text(Font::Regular, 10.0, 82.0, 626.0, "Big");
        p.text(Font::Regular, 10.0, 192.0, 626.0, "28.4");
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].headers, vec!["Model", "BLEU"]);
    assert_eq!(
        tables[0].rows,
        vec![vec!["Base", "27.3"], vec!["Big", "28.4"]]
    );
}

// ============================================================================
// 2. BORDERLESS & BOOKTABS TABLES (POSITIVE CASES)
// ============================================================================

#[test]
fn borderless_table_aligned_columns() {
    let pdf = single_page(|p| {
        let rows = [
            ["Model", "Params", "Accuracy", "Latency"],
            ["Small", "7B", "81.2%", "12 ms"],
            ["Medium", "14B", "85.6%", "21 ms"],
            ["Large", "70B", "89.4%", "64 ms"],
        ];
        let xs = [72.0, 180.0, 270.0, 360.0];
        for (r, row) in rows.iter().enumerate() {
            let y = 700.0 - r as f32 * 16.0;
            for (c, val) in row.iter().enumerate() {
                let font = if r == 0 { Font::Bold } else { Font::Regular };
                p.text(font, 10.0, xs[c], y, val);
            }
        }
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(
        tables[0].headers,
        vec!["Model", "Params", "Accuracy", "Latency"]
    );
    assert_eq!(tables[0].rows.len(), 3);
    assert_eq!(tables[0].rows[2], vec!["Large", "70B", "89.4%", "64 ms"]);
}

#[test]
fn booktabs_table_with_horizontal_rules_and_cmidrules() {
    // Academic LaTeX booktabs style: toprule, cmidrules, midrule, bottomrule,
    // ZERO vertical rules. Must NOT be mangled by `ruled_table`.
    let pdf = single_page(|p| {
        p.text(
            Font::Regular,
            10.0,
            72.0,
            730.0,
            "Table 2: Translation quality across architectures.",
        );
        p.thin_hrule(72.0, 712.0, 420.0); // \toprule
        p.text(Font::Bold, 10.0, 240.0, 698.0, "BLEU");
        p.text(Font::Bold, 10.0, 375.0, 698.0, "FLOPs");
        p.thin_hrule(210.0, 693.0, 110.0); // \cmidrule(lr){2-3}
        p.thin_hrule(345.0, 693.0, 125.0); // \cmidrule(lr){4-5}
        p.text(Font::Bold, 10.0, 80.0, 680.0, "Model");
        p.text(Font::Bold, 10.0, 215.0, 680.0, "EN-DE");
        p.text(Font::Bold, 10.0, 280.0, 680.0, "EN-FR");
        p.text(Font::Bold, 10.0, 350.0, 680.0, "EN-DE");
        p.text(Font::Bold, 10.0, 420.0, 680.0, "EN-FR");
        p.thin_hrule(72.0, 674.0, 420.0); // \midrule
        let rows = [
            ["GNMT", "24.6", "39.9", "2.3e19", "1.4e20"],
            ["ConvS2S", "25.2", "40.5", "9.6e18", "1.5e20"],
            ["Transformer", "28.4", "41.8", "2.3e19", "2.3e19"],
        ];
        for (r, row) in rows.iter().enumerate() {
            let y = 660.0 - r as f32 * 14.0;
            let xs = [80.0, 215.0, 280.0, 350.0, 420.0];
            for (c, val) in row.iter().enumerate() {
                p.text(Font::Regular, 10.0, xs[c], y, val);
            }
        }
        p.thin_hrule(72.0, 622.0, 420.0); // \bottomrule
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].headers.len(), 5, "{:?}", tables[0]);
    assert!(
        tables[0]
            .rows
            .iter()
            .any(|r| r == &vec!["Transformer", "28.4", "41.8", "2.3e19", "2.3e19"]),
        "missing Transformer row: {:?}",
        tables[0]
    );
}

#[test]
fn borderless_table_narrow_numeric_columns_stay_split() {
    // Closely spaced numeric columns like BERT Table 6 (`#L`, `#H`, `#A`, `LM`, `MNLI`, `MRPC`).
    let pdf = single_page(|p| {
        let xs = [72.0, 96.0, 128.0, 154.0, 190.0, 228.0];
        let rows = [
            ["L", "H", "A", "LM", "MNLI", "MRPC"],
            ["3", "768", "12", "5.84", "77.9", "79.8"],
            ["6", "768", "12", "4.68", "81.9", "84.8"],
            ["12", "1024", "16", "3.54", "85.7", "86.9"],
            ["24", "1024", "16", "3.23", "86.6", "87.8"],
        ];
        for (r, row) in rows.iter().enumerate() {
            let y = 700.0 - r as f32 * 14.0;
            for (c, val) in row.iter().enumerate() {
                p.text(Font::Regular, 9.0, xs[c], y, val);
            }
        }
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].headers, vec!["L", "H", "A", "LM", "MNLI", "MRPC"]);
    assert_eq!(tables[0].rows.len(), 4, "{:?}", tables[0]);
    assert_eq!(
        tables[0].rows[2],
        vec!["12", "1024", "16", "3.54", "85.7", "86.9"]
    );
    assert_eq!(
        tables[0].rows[3],
        vec!["24", "1024", "16", "3.23", "86.6", "87.8"]
    );
}

#[test]
fn borderless_table_with_dash_placeholders_and_empty_cells() {
    // Mirrors BERT Table 4 where a narrow `-` sits next to a numeric value.
    let pdf = single_page(|p| {
        let xs = [72.0, 180.0, 212.0];
        let rows = [
            ["System", "Dev", "Test"],
            ["ESIM+GloVe", "51.9", "52.7"],
            ["OpenAI GPT", "-", "78.0"],
            ["BERT-Base", "81.6", "-"],
            ["BERT-Large", "86.6", "86.3"],
        ];
        for (r, row) in rows.iter().enumerate() {
            let y = 700.0 - r as f32 * 14.0;
            for (c, val) in row.iter().enumerate() {
                p.text(Font::Regular, 10.0, xs[c], y, val);
            }
        }
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].headers, vec!["System", "Dev", "Test"]);
    assert_eq!(tables[0].rows[1], vec!["OpenAI GPT", "-", "78.0"]);
    assert_eq!(tables[0].rows[2], vec!["BERT-Base", "81.6", "-"]);
}

#[test]
fn borderless_table_folds_centered_multirow_labels() {
    let pdf = single_page(|p| {
        p.text(Font::Bold, 10.0, 72.0, 720.0, "Dataset");
        p.text(Font::Bold, 10.0, 170.0, 720.0, "Method");
        p.text(Font::Bold, 10.0, 280.0, 720.0, "Score");

        // Group 1: two rows with label centered between them (nearer row 1).
        p.text(Font::Regular, 10.0, 170.0, 704.0, "Baseline");
        p.text(Font::Regular, 10.0, 280.0, 704.0, "81.0");
        p.text(Font::Regular, 10.0, 72.0, 698.0, "AQuA");
        p.text(Font::Regular, 10.0, 170.0, 690.0, "VecCISC");
        p.text(Font::Regular, 10.0, 280.0, 690.0, "84.3");

        // Group 2: two rows with label centered between them.
        p.text(Font::Regular, 10.0, 170.0, 676.0, "Baseline");
        p.text(Font::Regular, 10.0, 280.0, 676.0, "39.9");
        p.text(Font::Regular, 10.0, 72.0, 670.0, "GPQA");
        p.text(Font::Regular, 10.0, 170.0, 662.0, "VecCISC");
        p.text(Font::Regular, 10.0, 280.0, 662.0, "41.4");
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].rows.len(), 4, "{:?}", tables[0]);
    assert!(tables[0].rows.iter().any(|r| r[0] == "AQuA"));
    assert!(tables[0].rows.iter().any(|r| r[0] == "GPQA"));
}

#[test]
fn borderless_table_keeps_superscripts_on_same_row() {
    let pdf = single_page(|p| {
        p.text(Font::Bold, 10.0, 72.0, 700.0, "Model");
        p.text(Font::Bold, 10.0, 220.0, 700.0, "FLOPs");
        p.text(Font::Regular, 10.0, 72.0, 684.0, "Base");
        p.text(Font::Regular, 10.0, 220.0, 684.0, "3.3 \u{00B7} 10");
        p.text(Font::Regular, 7.0, 250.0, 687.5, "18");
        p.text(Font::Regular, 10.0, 72.0, 668.0, "Big");
        p.text(Font::Regular, 10.0, 220.0, 668.0, "2.3 \u{00B7} 10");
        p.text(Font::Regular, 7.0, 250.0, 671.5, "19");
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].rows.len(), 2, "{:?}", tables[0]);
    assert!(tables[0].rows[0][1].contains("1018"), "{:?}", tables[0]);
    assert!(tables[0].rows[1][1].contains("1019"), "{:?}", tables[0]);
}

#[test]
fn borderless_table_does_not_swallow_caption_below() {
    // Mirrors BERT Table 7 where a caption directly underneath has a wide gap.
    let pdf = single_page(|p| {
        let rows = [
            ["System", "Dev F1", "Test F1"],
            ["ELMo", "95.7", "92.2"],
            ["BERT-Base", "96.4", "92.4"],
            ["BERT-Large", "96.6", "92.8"],
        ];
        let xs = [72.0, 190.0, 250.0];
        for (r, row) in rows.iter().enumerate() {
            let y = 700.0 - r as f32 * 14.0;
            for (c, val) in row.iter().enumerate() {
                p.text(Font::Regular, 10.0, xs[c], y, val);
            }
        }
        // Caption sits tightly below the last row with justified word spacing.
        p.text(Font::Regular, 9.5, 72.0, 642.0, "Table 7:");
        p.text(Font::Regular, 9.5, 118.0, 642.0, "CoNLL-2003 NER results.");
        p.text(
            Font::Regular,
            9.5,
            72.0,
            629.0,
            "Scores averaged over five runs.",
        );
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].rows.len(), 3, "caption leaked into table:\n{md}");
    assert!(
        !tables[0]
            .rows
            .iter()
            .any(|r| r.iter().any(|c| c.contains("Table 7"))),
        "{md}"
    );
    assert!(md.contains("Table 7: CoNLL-2003 NER results."), "{md}");
}

#[test]
fn borderless_table_staggered_multiline_header_stays_attached() {
    // Mirrors Attention Is All You Need Table 3: single-line header labels sit
    // vertically centered between two-line wrapped labels on the right columns.
    let pdf = single_page(|p| {
        // Top half of 2-line headers on right columns.
        p.text(Font::Regular, 9.0, 280.0, 712.0, "train");
        p.text(Font::Regular, 9.0, 330.0, 712.0, "PPL");
        p.text(Font::Regular, 9.0, 380.0, 712.0, "BLEU");
        // Centered single-line headers on left columns (6pt below top half!).
        p.text(Font::Regular, 9.0, 110.0, 706.0, "N");
        p.text(Font::Regular, 9.0, 150.0, 706.0, "dmodel");
        p.text(Font::Regular, 9.0, 210.0, 706.0, "h");
        // Bottom half of 2-line headers on right columns (6pt below middle!).
        p.text(Font::Regular, 9.0, 280.0, 700.0, "steps");
        p.text(Font::Regular, 9.0, 330.0, 700.0, "(dev)");
        p.text(Font::Regular, 9.0, 380.0, 700.0, "(dev)");
        p.thin_hrule(72.0, 694.0, 350.0);

        let rows = [
            ["base", "6", "512", "8", "100K", "4.92", "25.8"],
            ["big", "6", "1024", "16", "300K", "4.33", "26.4"],
        ];
        let xs = [72.0, 110.0, 150.0, 210.0, 280.0, 330.0, 380.0];
        for (r, row) in rows.iter().enumerate() {
            let y = 682.0 - r as f32 * 14.0;
            for (c, val) in row.iter().enumerate() {
                p.text(Font::Regular, 9.0, xs[c], y, val);
            }
        }
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    // Header text (`dmodel`, `train`, `PPL`, `BLEU`) must be INSIDE the table,
    // not orphaned as prose above it.
    let table_text: String = tables[0]
        .headers
        .iter()
        .chain(tables[0].rows.iter().flatten())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    for label in ["dmodel", "train", "PPL", "BLEU", "steps", "base", "big"] {
        assert!(
            table_text.contains(label),
            "missing header/row token `{label}` inside table:\n{md}"
        );
    }
}

#[test]
fn borderless_table_right_aligned_numbers_and_text_labels() {
    let pdf = single_page(|p| {
        p.text(Font::Bold, 10.0, 72.0, 700.0, "Category");
        p.text(Font::Bold, 10.0, 260.0, 700.0, "Revenue");
        p.text(Font::Bold, 10.0, 360.0, 700.0, "Growth");
        // Right-aligned numbers have varying x0 but overlap each column's range.
        p.text(Font::Regular, 10.0, 72.0, 684.0, "Cloud");
        p.text(Font::Regular, 10.0, 268.0, 684.0, "$12.4M");
        p.text(Font::Regular, 10.0, 368.0, 684.0, "+18%");

        p.text(Font::Regular, 10.0, 72.0, 668.0, "Enterprise");
        p.text(Font::Regular, 10.0, 262.0, 668.0, "$104.2M");
        p.text(Font::Regular, 10.0, 372.0, 668.0, "+9%");

        p.text(Font::Regular, 10.0, 72.0, 652.0, "Edge");
        p.text(Font::Regular, 10.0, 274.0, 652.0, "$4.1M");
        p.text(Font::Regular, 10.0, 368.0, 652.0, "+31%");
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(tables[0].headers, vec!["Category", "Revenue", "Growth"]);
    assert_eq!(tables[0].rows.len(), 3);
    assert_eq!(tables[0].rows[1], vec!["Enterprise", "$104.2M", "+9%"]);
}

// ============================================================================
// 3. TABLE FALSE-POSITIVE TRAPS (NEGATIVE CASES — MUST PRODUCE 0 TABLES)
// ============================================================================

#[test]
fn trap_hyperlink_underlines_do_not_create_tables() {
    // Mirrors NIST CSF 2.0: prose paragraphs containing multiple underlined
    // hyperlinks at staggered x-positions plus page header/footer rules.
    let pdf = single_page(|p| {
        p.thin_hrule(72.0, 756.0, 468.0);
        let lines = [
            (
                720.0,
                "Organizations consult NIST SP 800-53 for detailed controls.",
                195.0,
                110.0,
            ),
            (
                704.0,
                "Templates live on the NIST Cybersecurity Framework portal.",
                170.0,
                190.0,
            ),
            (
                688.0,
                "Feedback goes directly to cyberframework@nist.gov anytime.",
                205.0,
                125.0,
            ),
            (
                672.0,
                "Implementation Examples map outcomes to concrete actions.",
                72.0,
                130.0,
            ),
            (
                656.0,
                "Informative References connect standards across sectors.",
                72.0,
                120.0,
            ),
        ];
        for (y, text, ux, uw) in lines {
            p.text(Font::Regular, 11.0, 72.0, y, text);
            p.thin_hrule(ux, y - 2.0, uw);
        }
        p.thin_hrule(72.0, 60.0, 468.0);
    });
    let md = convert(&pdf);
    assert!(
        parse_tables(&md).is_empty(),
        "hyperlink underlines fabricated a table:\n{md}"
    );
    assert!(md.contains("cyberframework@nist.gov"), "{md}");
}

#[test]
fn trap_header_footer_rules_and_callout_box_do_not_create_tables() {
    // Mirrors RFC 9110 / NIST CSF: header rule, callout box around a note,
    // regular body prose, and footer rule. Must produce ZERO tables.
    let pdf = single_page(|p| {
        p.thin_hrule(72.0, 756.0, 468.0);
        p.text(Font::Bold, 13.0, 72.0, 725.0, "3.5 User Agents");
        p.text(
            Font::Regular,
            11.0,
            72.0,
            705.0,
            "The term user agent refers to any of the various client programs that initiate",
        );
        p.text(
            Font::Regular,
            11.0,
            72.0,
            691.0,
            "a request on behalf of an end user across the HTTP protocol.",
        );
        // Single callout box enclosing a note.
        p.rect(90.0, 610.0, 400.0, 56.0);
        p.text(
            Font::Regular,
            10.0,
            102.0,
            644.0,
            "Note: Clients should validate response framing before caching content.",
        );
        p.text(
            Font::Regular,
            10.0,
            102.0,
            628.0,
            "See Section 5 for additional requirements on intermediaries.",
        );
        p.text(
            Font::Regular,
            11.0,
            72.0,
            580.0,
            "Normal prose continues underneath the callout box without disruption.",
        );
        p.thin_hrule(72.0, 60.0, 468.0);
    });
    let md = convert(&pdf);
    assert!(
        parse_tables(&md).is_empty(),
        "header/footer/callout box fabricated a table:\n{md}"
    );
    assert_order(
        &md,
        &[
            "User Agents",
            "The term user agent",
            "Note: Clients should validate",
            "Normal prose continues",
        ],
    );
}

#[test]
fn trap_two_column_prose_is_not_a_table() {
    let pdf = single_page(|p| {
        let left = [
            "We investigate how language models scale",
            "when trained across diverse public text",
            "corpora without proprietary filtering.",
            "Our experiments span five reasoning",
            "benchmarks across science and math.",
            "Results confirm consistent gains.",
        ];
        let right = [
            "In addition to accuracy, we evaluate",
            "inference latency and memory overhead",
            "on commodity workstation hardware.",
            "Clustering redundant traces reduces",
            "total token usage by nearly half while",
            "preserving calibration accuracy.",
        ];
        for i in 0..left.len() {
            let y = 700.0 - i as f32 * 14.0;
            p.text(Font::Regular, 10.0, 72.0, y, left[i]);
            p.text(Font::Regular, 10.0, 320.0, y, right[i]);
        }
    });
    let md = convert(&pdf);
    assert!(
        parse_tables(&md).is_empty(),
        "two-column prose became a table:\n{md}"
    );
    assert_order(
        &md,
        &[
            "We investigate how language models scale",
            "Results confirm consistent gains.",
            "In addition to accuracy",
            "preserving calibration accuracy.",
        ],
    );
}

#[test]
fn trap_justified_two_sentence_lines_are_not_a_table() {
    // Mirrors BERT p.12: two justified prose lines with wide inter-sentence space.
    let pdf = single_page(|p| {
        p.text(
            Font::Regular,
            10.0,
            72.0,
            700.0,
            "For fine-tuning, most hyperparameters stay fixed except for train-",
        );
        p.text(Font::Regular, 10.0, 72.0, 686.0, "ing epochs.");
        p.text(
            Font::Regular,
            10.0,
            142.0,
            686.0,
            "The dropout probability was always",
        );
        p.text(Font::Regular, 10.0, 72.0, 672.0, "kept at 0.1.");
        p.text(
            Font::Regular,
            10.0,
            142.0,
            672.0,
            "The optimal hyperparameter values",
        );
        p.text(
            Font::Regular,
            10.0,
            72.0,
            658.0,
            "are task-specific across all downstream benchmarks.",
        );
    });
    let md = convert(&pdf);
    assert!(
        parse_tables(&md).is_empty(),
        "justified prose sentences became a table:\n{md}"
    );
    assert!(md.contains("training epochs."), "{md}");
    assert!(md.contains("The dropout probability was always"), "{md}");
}

#[test]
fn trap_bullet_list_with_tab_gap_is_not_a_table() {
    // Mirrors NIST CSF 2.0 sub-bullets (`o` followed by tab gap and text).
    let pdf = single_page(|p| {
        let items = [
            ("o", "RS.MI-01: Incidents are contained promptly"),
            ("o", "RS.MI-02: Incidents are eradicated safely"),
            ("o", "RS.MI-03: Newly identified risks are mitigated"),
        ];
        for (i, (marker, text)) in items.iter().enumerate() {
            let y = 700.0 - i as f32 * 15.0;
            p.text(Font::Regular, 10.0, 90.0, y, marker);
            p.text(Font::Regular, 10.0, 120.0, y, text);
        }
    });
    let md = convert(&pdf);
    assert!(
        parse_tables(&md).is_empty(),
        "tabbed bullet list became a 2-col table:\n{md}"
    );
    assert!(md.contains("RS.MI-01"), "{md}");
    assert!(md.contains("RS.MI-02"), "{md}");
}

#[test]
fn trap_numbered_headings_and_equations_are_not_tables() {
    let pdf = single_page(|p| {
        p.text(Font::Bold, 11.0, 72.0, 720.0, "3");
        p.text(Font::Bold, 11.0, 102.0, 720.0, "Experiments");
        p.text(Font::Bold, 11.0, 72.0, 704.0, "3.1");
        p.text(Font::Bold, 11.0, 102.0, 704.0, "Datasets");

        p.text(Font::Regular, 10.0, 160.0, 660.0, "f(x) = Wx + b");
        p.text(Font::Regular, 10.0, 480.0, 660.0, "(1)");
        p.text(Font::Regular, 10.0, 160.0, 644.0, "g(y) = softmax(y)");
        p.text(Font::Regular, 10.0, 480.0, 644.0, "(2)");
    });
    let md = convert(&pdf);
    assert!(
        parse_tables(&md).is_empty(),
        "section numbers or equations became a table:\n{md}"
    );
}

#[test]
fn trap_prose_straddling_background_rects_is_not_a_table() {
    // Decorative shading rects behind full-width prose lines must NOT split prose into cells.
    let pdf = single_page(|p| {
        p.cell_rect_grid(&[72.0, 220.0, 380.0], &[640.0, 670.0, 700.0]);
        p.text(
            Font::Regular,
            11.0,
            80.0,
            680.0,
            "This continuous sentence runs straight across the decorative vertical border.",
        );
        p.text(
            Font::Regular,
            11.0,
            80.0,
            650.0,
            "Another continuous sentence also crosses the decorative box boundary freely.",
        );
    });
    let md = convert(&pdf);
    assert!(
        parse_tables(&md).is_empty(),
        "prose crossing decorative rects became a table:\n{md}"
    );
}

// ============================================================================
// 4. MULTI-COLUMN BANDING & READING ORDER
// ============================================================================

#[test]
fn mixed_bands_title_two_col_full_table_two_col() {
    let pdf = single_page(|p| {
        // Band 1: full-width title.
        p.text(
            Font::Bold,
            18.0,
            72.0,
            745.0,
            "Fast Layout Reconstruction for Academic Papers",
        );

        // Band 2: two-column prose above table (kept comfortably left of x=320 gutter).
        for i in 0..5 {
            let y = 710.0 - i as f32 * 14.0;
            p.text(
                Font::Regular,
                10.0,
                72.0,
                y,
                &format!("Top-left column sentence {i} flows cleanly."),
            );
            p.text(
                Font::Regular,
                10.0,
                320.0,
                y,
                &format!("Top-right column sentence {i} flows cleanly."),
            );
        }

        // Band 3: full-width table + caption.
        p.text(
            Font::Regular,
            10.0,
            72.0,
            620.0,
            "Table 1: Benchmark throughput spanning the entire page width.",
        );
        let xs = [72.0, 220.0, 350.0, 460.0];
        let rows = [
            ["Engine", "Pages/s", "Binary", "Deps"],
            ["pdfmd", "3900", "700 KB", "0"],
            ["heavy", "120", "45 MB", "82"],
        ];
        for (r, row) in rows.iter().enumerate() {
            let y = 600.0 - r as f32 * 14.0;
            for (c, val) in row.iter().enumerate() {
                p.text(Font::Regular, 10.0, xs[c], y, val);
            }
        }

        // Band 4: two-column prose below table.
        for i in 0..5 {
            let y = 540.0 - i as f32 * 14.0;
            p.text(
                Font::Regular,
                10.0,
                72.0,
                y,
                &format!("Bottom-left column line {i} discusses gains."),
            );
            p.text(
                Font::Regular,
                10.0,
                320.0,
                y,
                &format!("Bottom-right column line {i} wraps up work."),
            );
        }
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 1, "{md}");
    assert_eq!(
        tables[0].headers,
        vec!["Engine", "Pages/s", "Binary", "Deps"]
    );
    assert_eq!(tables[0].rows[0], vec!["pdfmd", "3900", "700 KB", "0"]);
    assert_order(
        &md,
        &[
            "# Fast Layout Reconstruction",
            "Top-left column sentence 0",
            "Top-left column sentence 4",
            "Top-right column sentence 0",
            "Top-right column sentence 4",
            "Table 1: Benchmark throughput",
            "| Engine | Pages/s | Binary | Deps |",
            "Bottom-left column line 0",
            "Bottom-left column line 4",
            "Bottom-right column line 0",
            "Bottom-right column line 4",
        ],
    );
}

#[test]
fn two_column_side_by_side_tables_stay_separate() {
    // Mirrors BERT p.7: two independent single-column tables sitting across
    // the gutter on a two-column page must NOT merge horizontally.
    let pdf = single_page(|p| {
        for i in 0..4 {
            let y = 730.0 - i as f32 * 14.0;
            p.text(
                Font::Regular,
                10.0,
                50.0,
                y,
                &format!("Left prose line {i} discusses SQuAD results."),
            );
            p.text(
                Font::Regular,
                10.0,
                320.0,
                y,
                &format!("Right prose line {i} discusses SWAG scores."),
            );
        }
        // Left-column table.
        let left_rows = [
            ["System", "EM", "F1"],
            ["Human", "82.3", "91.2"],
            ["BERT", "87.4", "93.2"],
        ];
        for (r, row) in left_rows.iter().enumerate() {
            let y = 650.0 - r as f32 * 14.0;
            p.text(Font::Regular, 9.5, 50.0, y, row[0]);
            p.text(Font::Regular, 9.5, 160.0, y, row[1]);
            p.text(Font::Regular, 9.5, 210.0, y, row[2]);
        }
        // Right-column table at the exact same y coordinates.
        let right_rows = [
            ["Model", "Dev", "Test"],
            ["GPT", "-", "78.0"],
            ["BERT", "86.6", "86.3"],
        ];
        for (r, row) in right_rows.iter().enumerate() {
            let y = 650.0 - r as f32 * 14.0;
            p.text(Font::Regular, 9.5, 320.0, y, row[0]);
            p.text(Font::Regular, 9.5, 430.0, y, row[1]);
            p.text(Font::Regular, 9.5, 480.0, y, row[2]);
        }
    });
    let md = convert(&pdf);
    let tables = parse_tables(&md);
    assert_eq!(tables.len(), 2, "expected 2 separate column tables:\n{md}");
    assert_eq!(tables[0].headers, vec!["System", "EM", "F1"]);
    assert_eq!(tables[1].headers, vec!["Model", "Dev", "Test"]);
    assert_order(
        &md,
        &[
            "Left prose line 0",
            "| System | EM | F1 |",
            "Right prose line 0",
            "| Model | Dev | Test |",
        ],
    );
}

#[test]
fn three_column_layout_reads_left_middle_right() {
    let pdf = single_page(|p| {
        for i in 0..6 {
            let y = 700.0 - i as f32 * 14.0;
            p.text(
                Font::Regular,
                9.5,
                40.0,
                y,
                &format!("ColOne text line {i} flows here."),
            );
            p.text(
                Font::Regular,
                9.5,
                220.0,
                y,
                &format!("ColTwo text line {i} flows here."),
            );
            p.text(
                Font::Regular,
                9.5,
                400.0,
                y,
                &format!("ColThree text line {i} flows here."),
            );
        }
    });
    let md = convert(&pdf);
    assert!(parse_tables(&md).is_empty(), "{md}");
    assert_order(
        &md,
        &[
            "ColOne text line 0",
            "ColOne text line 5",
            "ColTwo text line 0",
            "ColTwo text line 5",
            "ColThree text line 0",
            "ColThree text line 5",
        ],
    );
}

#[test]
fn rotated_margin_stamp_moves_to_page_end() {
    let pdf = single_page(|p| {
        p.rotated_text(Font::Regular, 16.0, 24.0, 680.0, "arXiv:2605.08070v1");
        p.text(Font::Bold, 16.0, 72.0, 720.0, "Paper Title");
        p.text(
            Font::Regular,
            11.0,
            72.0,
            680.0,
            "First sentence of the abstract stays completely intact.",
        );
        p.text(
            Font::Regular,
            11.0,
            72.0,
            666.0,
            "Second sentence follows immediately without interruption.",
        );
    });
    let md = convert(&pdf);
    assert_order(
        &md,
        &[
            "# Paper Title",
            "First sentence of the abstract stays completely intact. Second sentence follows immediately without interruption.",
            "arXiv:2605.08070v1",
        ],
    );
}

// ============================================================================
// 5. HEADINGS, TAGGED PDF, RUNNING MARGINS & TRAPS
// ============================================================================

#[test]
fn heading_hierarchy_and_wrapped_multiline_bold_headings() {
    let pdf = single_page(|p| {
        p.text(Font::Bold, 20.0, 72.0, 740.0, "Document Title");
        p.text(Font::Bold, 14.0, 72.0, 705.0, "1 Introduction");
        p.text(
            Font::Regular,
            10.0,
            72.0,
            688.0,
            "Opening body copy at standard ten-point size.",
        );
        // Wrapped 2-line bold numbered subsection heading (like VecCISC 5.1 & A.2).
        p.text(
            Font::Bold,
            10.5,
            72.0,
            660.0,
            "5.1 Embeddings and Clustering for Scaling",
        );
        p.text(Font::Bold, 10.5, 72.0, 646.0, "Inference Time Reasoning");
        p.text(
            Font::Regular,
            10.0,
            72.0,
            630.0,
            "Body paragraph under the wrapped subsection heading.",
        );
        // Wrapped 2-line appendix heading (`A.2 ...`).
        p.text(
            Font::Bold,
            10.5,
            72.0,
            602.0,
            "A.2 Cluster Hyperparameters and",
        );
        p.text(Font::Bold, 10.5, 72.0, 588.0, "Implementation Details");
        p.text(
            Font::Regular,
            10.0,
            72.0,
            572.0,
            "Details about hyperparameters follow here.",
        );
    });
    let md = convert(&pdf);
    let headings = parse_headings(&md);
    assert!(
        headings.contains(&(1, "Document Title".to_string())),
        "headings={headings:?}\n{md}"
    );
    assert!(
        headings.iter().any(|(lvl, t)| *lvl == 2
            && t == "Embeddings and Clustering for Scaling Inference Time Reasoning"),
        "wrapped 5.1 heading missed: {headings:?}\n{md}"
    );
    assert!(
        headings
            .iter()
            .any(|(lvl, t)| *lvl == 2 && t == "Cluster Hyperparameters and Implementation Details"),
        "wrapped A.2 heading missed: {headings:?}\n{md}"
    );
}

#[test]
fn tagged_pdf_struct_tree_headings_override_visual_size() {
    let mut doc = PdfBuilder::new();
    let mut page = PageStream::new();
    page.tagged_text(
        "H1",
        0,
        Font::Regular,
        11.0,
        (72.0, 720.0),
        "Tagged Top Heading",
    );
    page.tagged_text(
        "H3",
        1,
        Font::Regular,
        11.0,
        (72.0, 695.0),
        "Tagged Sub Heading",
    );
    page.text(
        Font::Regular,
        11.0,
        72.0,
        670.0,
        "Untagged body sentence at the exact same font size.",
    );
    let p0 = doc.add_page(page);
    doc.add_struct_role(p0, 0, "H1");
    doc.add_struct_role(p0, 1, "H3");
    let md = convert(&doc.finish());
    let headings = parse_headings(&md);
    assert_eq!(
        headings,
        vec![
            (1, "Tagged Top Heading".to_string()),
            (3, "Tagged Sub Heading".to_string())
        ],
        "{md}"
    );
}

#[test]
fn heading_traps_labels_figures_and_escaped_hash() {
    let pdf = single_page(|p| {
        p.text(Font::Bold, 18.0, 72.0, 740.0, "Traps Test");
        p.text(Font::Bold, 10.0, 72.0, 710.0, "Cluster ID: 6");
        p.text(Font::Regular, 10.0, 72.0, 690.0, "# of calls in pipeline");
        p.text(
            Font::Regular,
            10.0,
            72.0,
            670.0,
            "11086.2 10932.4 10629.9 7691.9",
        );
        p.text(
            Font::Bold,
            10.0,
            72.0,
            650.0,
            "This bold sentence ends with a period so it is not a heading.",
        );
    });
    let md = convert(&pdf);
    let headings = parse_headings(&md);
    assert_eq!(headings, vec![(1, "Traps Test".to_string())], "{md}");
    assert!(md.contains("**Cluster ID: 6**"), "{md}");
    assert!(md.contains("\\# of calls in pipeline"), "{md}");
    assert!(md.contains("11086.2 10932.4 10629.9 7691.9"), "{md}");
}

#[test]
fn running_headers_and_footers_stripped_across_multipage_doc() {
    let mut doc = PdfBuilder::new();
    for i in 1..=4 {
        let mut p = PageStream::new();
        p.text(Font::Regular, 9.0, 72.0, 760.0, "ACME TECHNICAL REPORT");
        p.text(
            Font::Regular,
            11.0,
            72.0,
            700.0,
            &format!("Body content unique to page {i}."),
        );
        p.text(Font::Regular, 9.0, 260.0, 40.0, "Confidential Draft");
        doc.add_page(p);
    }
    let pdf = doc.finish();
    let res = convert_pdf_to_markdown(
        &pdf,
        &ConvertOptions {
            include_page_breaks: true,
            image_dir: None,
        },
    )
    .expect("convert");
    assert_hygiene(&res.markdown);
    assert!(
        !res.markdown.contains("ACME TECHNICAL REPORT"),
        "{}",
        res.markdown
    );
    assert!(
        !res.markdown.contains("Confidential Draft"),
        "{}",
        res.markdown
    );
    assert_eq!(res.markdown.matches("\n\n---\n\n").count(), 3);
    for i in 1..=4 {
        assert!(res
            .markdown
            .contains(&format!("Body content unique to page {i}.")));
    }
}

// ============================================================================
// 6. LISTS, CODE, EMPHASIS, HYPHENATION, CMAPS & FORM XOBJECTS
// ============================================================================

#[test]
fn wrapped_unicode_bullet_list_becomes_markdown_list() {
    let pdf = single_page(|p| {
        p.text(Font::Bold, 16.0, 72.0, 740.0, "Contributions");
        // Three multi-line wrapped bullets spaced tightly together (like Attention p.4 / VecCISC p.1).
        p.text(
            Font::Regular,
            10.0,
            72.0,
            700.0,
            "\u{2022} First contribution wraps onto a second line",
        );
        p.text(
            Font::Regular,
            10.0,
            82.0,
            686.0,
            "with continuation text explaining the method.",
        );
        p.text(
            Font::Regular,
            10.0,
            72.0,
            672.0,
            "\u{2022} Second contribution also wraps across",
        );
        p.text(
            Font::Regular,
            10.0,
            82.0,
            658.0,
            "multiple lines and includes hyphen-",
        );
        p.text(Font::Regular, 10.0, 82.0, 644.0, "ated words cleanly.");
        p.text(
            Font::Regular,
            10.0,
            72.0,
            630.0,
            "\u{2022} Third contribution is a single line.",
        );
    });
    let md = convert(&pdf);
    let items = parse_list_items(&md);
    assert_eq!(items.len(), 3, "expected 3 markdown list items:\n{md}");
    assert_eq!(
        items[0],
        "First contribution wraps onto a second line with continuation text explaining the method."
    );
    assert_eq!(
        items[1],
        "Second contribution also wraps across multiple lines and includes hyphenated words cleanly."
    );
    assert_eq!(items[2], "Third contribution is a single line.");
    assert!(!md.contains('\u{2022}'), "raw bullet leaked:\n{md}");
}

#[test]
fn wrapped_ordered_list_preserves_all_items() {
    let pdf = single_page(|p| {
        p.text(Font::Bold, 16.0, 72.0, 740.0, "Procedure");
        p.text(
            Font::Regular,
            10.0,
            72.0,
            700.0,
            "1. Parse the object table and locate",
        );
        p.text(Font::Regular, 10.0, 86.0, 686.0, "every page dictionary.");
        p.text(
            Font::Regular,
            10.0,
            72.0,
            672.0,
            "2. Interpret content streams and collect",
        );
        p.text(Font::Regular, 10.0, 86.0, 658.0, "positioned glyph spans.");
        p.text(Font::Regular, 10.0, 72.0, 644.0, "3. Emit clean Markdown.");
    });
    let md = convert(&pdf);
    let items = parse_list_items(&md);
    assert_eq!(items.len(), 3, "expected 3 ordered list items:\n{md}");
    assert_eq!(
        items[0],
        "Parse the object table and locate every page dictionary."
    );
    assert_eq!(
        items[1],
        "Interpret content streams and collect positioned glyph spans."
    );
    assert_eq!(items[2], "Emit clean Markdown.");
}

#[test]
fn inline_emphasis_and_hyphenation_across_lines() {
    let pdf = single_page(|p| {
        p.text(Font::Bold, 16.0, 72.0, 740.0, "Formatting");
        p.text(Font::Regular, 10.0, 72.0, 700.0, "We use ");
        p.text(Font::Bold, 10.0, 106.0, 700.0, "bold text");
        p.text(Font::Regular, 10.0, 145.0, 700.0, " and ");
        p.text(Font::Italic, 10.0, 167.0, 700.0, "Pro-");
        p.text(Font::Italic, 10.0, 72.0, 686.0, "ceedings of ACL");
        p.text(Font::Regular, 10.0, 146.0, 686.0, " plus ");
        p.text(Font::BoldItalic, 10.0, 170.0, 686.0, "both.");
    });
    let md = convert(&pdf);
    assert!(md.contains("**bold text**"), "{md}");
    assert!(md.contains("*Proceedings of ACL*"), "{md}");
    assert!(md.contains("***both.***"), "{md}");
}

#[test]
fn monospace_courier_block_becomes_fenced_code() {
    let pdf = single_page(|p| {
        p.text(Font::Bold, 16.0, 72.0, 740.0, "Example");
        p.text(
            Font::Mono,
            10.0,
            72.0,
            700.0,
            "fn add(a: i32, b: i32) -> i32 {",
        );
        p.text(Font::Mono, 10.0, 72.0, 686.0, "    a + b");
        p.text(Font::Mono, 10.0, 72.0, 672.0, "}");
    });
    let md = convert(&pdf);
    assert!(md.contains("```\nfn add(a: i32, b: i32) -> i32 {"), "{md}");
    assert!(md.contains("a + b\n}\n```"), "{md}");
}

#[test]
fn tounicode_cmap_ligatures_and_tj_word_spacing() {
    let mut doc = PdfBuilder::new();
    doc.add_cmap_font(
        "F6",
        "CustomSerif",
        "\
/CIDInit /ProcSet findresource begin
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
3 beginbfchar
<01> <00660069>
<02> <0066006C>
<03> <2014>
endbfchar
1 beginbfrange
<20> <7E> <0020>
endbfrange
endcmap
end",
    );
    let mut page = PageStream::new();
    // "ef" + <01>("fi") + "cient " + <02>("fl") + "ow " + <03>("—") + " ok"
    page.text_raw_hex(
        "/F6",
        11.0,
        72.0,
        720.0,
        "6566016369656E7420026F772003206F6B",
    );
    // TJ kerning vs word-break gap (-250 > threshold => space; -20 => kerning).
    page.text_tj(
        Font::Regular,
        11.0,
        72.0,
        690.0,
        &[
            TjItem::Str("Ker"),
            TjItem::Kern(-20.0),
            TjItem::Str("ning"),
            TjItem::Kern(-250.0),
            TjItem::Str("works"),
        ],
    );
    doc.add_page(page);
    let md = convert(&doc.finish());
    assert!(md.contains("efficient flow \u{2014} ok"), "{md}");
    assert!(md.contains("Kerning works"), "{md}");
}

#[test]
fn form_xobject_text_integrates_into_reading_order() {
    let mut doc = PdfBuilder::new();
    doc.add_form_xobject(
        "Fm1",
        "BT /F1 11 Tf 1 0 0 1 72 680 Tm (Text rendered inside a Form XObject.) Tj ET",
    );
    let mut page = PageStream::new();
    page.text(Font::Bold, 16.0, 72.0, 730.0, "Form Integration");
    page.raw("/Fm1 Do");
    page.text(
        Font::Regular,
        11.0,
        72.0,
        640.0,
        "Following paragraph after the form.",
    );
    doc.add_page(page);
    let md = convert(&doc.finish());
    assert_order(
        &md,
        &[
            "# Form Integration",
            "Text rendered inside a Form XObject.",
            "Following paragraph after the form.",
        ],
    );
}

// ============================================================================
// 7. DEEP REGRESSION SUITE ON BUNDLED REAL PDF (`tests/fixtures/sample.pdf`)
// ============================================================================

fn convert_sample_pdf() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdf");
    let bytes = std::fs::read(path).expect("read sample.pdf");
    convert(&bytes)
}

#[test]
fn sample_pdf_hygiene_and_document_title() {
    let md = convert_sample_pdf();
    assert!(
        md.starts_with(
            "# VecCISC: Improving Confidence-Informed Self-Consistency with Reasoning Trace Clustering and Candidate Answer Selection\n"
        ),
        "unexpected title:\n{}",
        md.lines().next().unwrap_or("")
    );
    assert!(md.ends_with('\n'));
}

#[test]
fn sample_pdf_extracts_all_five_tables_accurately() {
    let md = convert_sample_pdf();
    let tables = parse_tables(&md);
    assert_eq!(
        tables.len(),
        5,
        "expected exactly 5 tables in sample.pdf, found {}:\n{:?}",
        tables.len(),
        tables
            .iter()
            .map(|t| (&t.headers, t.rows.len()))
            .collect::<Vec<_>>()
    );

    // Table 1: VecCISC + KMeans call budget reductions (7 cols x 5 data rows).
    let t1 = &tables[0];
    assert_eq!(
        t1.headers,
        vec![
            "Dataset",
            "Budget",
            "GPT-4o mini",
            "Llama 3.1 8B",
            "Qwen2.5 7B",
            "Mistral 7B",
            "Llama3.3 70B"
        ]
    );
    assert_eq!(t1.rows.len(), 5, "{t1:?}");
    assert_eq!(
        t1.rows[0],
        vec![
            "AQuA-RAT",
            "20",
            "5.660(-71.72%)",
            "13.45(-32.77%)",
            "14.78(-26.09%)",
            "17.05(-14.74%)",
            "2.600(-86.99%)"
        ]
    );
    assert_eq!(t1.rows[4][0], "GPQA");
    assert_eq!(t1.rows[4][6], "17.27(-13.63%)");

    // Table 2: VecCISC + HAC call budget reductions (7 cols x 5 data rows).
    let t2 = &tables[1];
    assert_eq!(t2.headers, t1.headers);
    assert_eq!(t2.rows.len(), 5, "{t2:?}");
    assert_eq!(t2.rows[0][0], "AQuA-RAT");
    assert_eq!(t2.rows[0][2], "13.67(-31.64%)");
    assert_eq!(t2.rows[4][6], "17.95(-10.24%)");

    // Table 3: VecCISC + KMeans token reductions (7 cols x 15 data rows, 5 datasets x 3 methods).
    let t3 = &tables[2];
    assert_eq!(
        t3.headers,
        vec![
            "Dataset",
            "Method",
            "GPT 4o-mini",
            "Llama3.1 8B",
            "Qwen2.5 7B",
            "Mistral 7B",
            "Llama3.3 70B"
        ]
    );
    assert_eq!(t3.rows.len(), 15, "{t3:?}");
    for ds in [
        "AQuA-RAT",
        "CommonsenseQA",
        "ARC-Challenging",
        "MMLU-Pro",
        "GPQA",
    ] {
        assert!(
            t3.rows.iter().any(|r| r[0] == ds),
            "Table 3 missing multirow dataset label {ds}"
        );
    }
    assert_eq!(t3.rows[0][1], "CISC");
    assert_eq!(t3.rows[0][2], "13042.0");
    assert_eq!(t3.rows[14][1], "VecCISC (min-centroid)");
    assert_eq!(t3.rows[14][6], "22389.4");

    // Table 4: VecCISC + HAC token reductions (7 cols x 15 data rows).
    let t4 = &tables[3];
    assert_eq!(t4.headers, t3.headers);
    assert_eq!(t4.rows.len(), 15, "{t4:?}");
    assert_eq!(t4.rows[1][2], "8914.0");
    assert_eq!(t4.rows[13][6], "22382.6");

    // Table 5: Accuracy comparison (7 cols x 26 rows: subheader + 5 datasets x 5 methods).
    let t5 = &tables[4];
    assert_eq!(t5.headers, t3.headers);
    assert_eq!(t5.rows.len(), 26, "{t5:?}");
    for ds in [
        "AQuA-RAT",
        "CommonsenseQA",
        "ARC-Challenging",
        "MMLU-Pro",
        "GPQA",
    ] {
        assert!(
            t5.rows.iter().any(|r| r[0] == ds),
            "Table 5 missing multirow dataset label {ds}"
        );
    }
    assert_eq!(t5.rows[1][1], "SC Baseline");
    assert_eq!(t5.rows[1][2], "83.5 / 83.5");
    assert_eq!(t5.rows[25][1], "VecCISC + HAC");
    assert_eq!(t5.rows[25][6], "61.7 / 61.7");
}

#[test]
fn sample_pdf_preserves_multicolumn_reading_order_and_margin_stamp() {
    let md = convert_sample_pdf();
    // Abstract precedes Introduction; page 1 left column finishes before right
    // column; rotated arXiv stamp does not split prose mid-sentence.
    assert_order(
        &md,
        &[
            "### Abstract",
            "reduces the total token usage by 47%",
            "# Introduction",
            "will lead to a greater",
            "number of potential answers",
            "arXiv:2605.08070v1",
            "# Methodology",
            "# Experiments",
            "# Results and Discussion",
            "Table 1:",
            "Table 2:",
            "Table 3:",
            "# Related Work",
            "# Conclusion",
            "# Limitations",
            "Table 4:",
            "Table 5:",
            "### References",
        ],
    );
}

#[test]
fn sample_pdf_recovers_section_and_subsection_headings() {
    let md = convert_sample_pdf();
    let headings = parse_headings(&md);
    let has = |lvl: usize, needle: &str| {
        headings
            .iter()
            .any(|(l, t)| *l == lvl && t.contains(needle))
    };
    assert!(has(3, "Abstract"), "{headings:?}");
    assert!(has(1, "Introduction"), "{headings:?}");
    assert!(has(1, "Methodology"), "{headings:?}");
    assert!(has(2, "Sampling"), "{headings:?}");
    assert!(has(2, "Embedding Generation"), "{headings:?}");
    assert!(has(2, "Cluster Representative Selection"), "{headings:?}");
    assert!(has(1, "Experiments"), "{headings:?}");
    assert!(has(2, "Datasets"), "{headings:?}");
    assert!(has(1, "Results and Discussion"), "{headings:?}");
    assert!(has(1, "Related Work"), "{headings:?}");
    // Wrapped two-line numbered bold subsections must also be recovered as H2.
    assert!(
        has(
            2,
            "Embeddings and Clustering for Scaling Inference Time Reasoning"
        ),
        "wrapped 5.1 heading missing: {headings:?}"
    );
    assert!(
        has(2, "Cluster Hyperparameters and Implementation Details"),
        "wrapped A.2 heading missing: {headings:?}"
    );
    assert!(has(1, "Conclusion"), "{headings:?}");
    assert!(has(1, "Limitations"), "{headings:?}");
    assert!(has(3, "References"), "{headings:?}");
}

#[test]
fn sample_pdf_converts_bullet_lists_and_escapes_prompts() {
    let md = convert_sample_pdf();
    let items = parse_list_items(&md);
    // 4 contribution bullets in Introduction + 5 dataset bullets in Section 3.1.
    assert!(
        items.iter().any(|i| i.starts_with("We introduce VecCISC")),
        "missing Introduction bullet 1; items={items:?}"
    );
    assert!(
        items
            .iter()
            .any(|i| i.starts_with("We will release our code publicly")),
        "missing Introduction bullet 4; items={items:?}"
    );
    assert!(
        items.iter().any(|i| i.contains("AQuA_Rat")),
        "missing Datasets bullet AQuA_Rat; items={items:?}"
    );
    assert!(
        items.iter().any(|i| i.contains("GPQA")),
        "missing Datasets bullet GPQA; items={items:?}"
    );
    // Quoted prompt headings (`## Instructions`) must stay escaped, not become headings.
    assert!(md.contains("\\## Instructions"), "{md}");
    assert!(
        !parse_headings(&md)
            .iter()
            .any(|(_, t)| t == "Instructions" || t == "Output"),
        "prompt markdown leaked as real headings"
    );
}
