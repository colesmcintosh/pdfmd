//! Deterministic PDF fixtures. No dependencies: each page is a content
//! stream of `Tj` / `re` operators, WinAnsi text, and one Identity-H font
//! for the CJK line. `field-note.pdf` is not built here.

use std::fmt::Write as _;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Font {
    Body,
    Bold,
    Italic,
    BoldItalic,
    Mono,
    Cjk,
}

impl Font {
    fn resource(self) -> &'static str {
        match self {
            Self::Body => "F1",
            Self::Bold => "F2",
            Self::Italic => "F3",
            Self::BoldItalic => "F4",
            Self::Mono => "F5",
            Self::Cjk => "F6",
        }
    }
}

struct Op {
    kind: OpKind,
}

enum OpKind {
    Text {
        font: Font,
        size: f32,
        x: f32,
        y: f32,
        text: String,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
}

struct Link {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    url: &'static str,
}

struct Page {
    ops: Vec<Op>,
    links: Vec<Link>,
}

struct Pen {
    y: f32,
    ops: Vec<Op>,
}

impl Pen {
    fn new() -> Self {
        Self {
            y: 740.0,
            ops: Vec::new(),
        }
    }

    fn put(&mut self, font: Font, size: f32, text: &str, leading: f32) {
        self.ops.push(text_op(font, size, 72.0, self.y, text));
        self.y -= leading;
    }

    fn blank(&mut self, dy: f32) {
        self.y -= dy;
    }

    fn rich(&mut self, size: f32, runs: &[(Font, &str)], leading: f32) {
        let mut x = 72.0;
        for &(font, text) in runs {
            self.ops.push(text_op(font, size, x, self.y, text));
            x += advance(font, text, size);
        }
        self.y -= leading;
    }

    fn finish(self) -> Page {
        Page {
            ops: self.ops,
            links: Vec::new(),
        }
    }
}

fn text_op(font: Font, size: f32, x: f32, y: f32, text: &str) -> Op {
    Op {
        kind: OpKind::Text {
            font,
            size,
            x,
            y,
            text: text.to_owned(),
        },
    }
}

fn rect_op(x: f32, y: f32, w: f32, h: f32) -> Op {
    Op {
        kind: OpKind::Rect { x, y, w, h },
    }
}

/// Synthetic fixture ids, in report order. `field-note` is Chrome output.
pub fn synthetic_ids() -> &'static [&'static str] {
    &[
        "headings",
        "paragraphs",
        "emphasis",
        "lists",
        "table-simple",
        "table-merged",
        "columns",
        "margins",
        "page-break",
        "code",
        "links",
        "unicode",
        "hyphen",
    ]
}

pub fn build(id: &str) -> Option<Vec<u8>> {
    let pages = match id {
        "headings" => headings(),
        "paragraphs" => paragraphs(),
        "emphasis" => emphasis(),
        "lists" => lists(),
        "table-simple" => table_simple(),
        "table-merged" => table_merged(),
        "columns" => columns(),
        "margins" => margins(),
        "page-break" => page_break(),
        "code" => code(),
        "links" => links(),
        "unicode" => unicode(),
        "hyphen" => hyphen(),
        _ => return None,
    };
    Some(document(id, &pages))
}

fn headings() -> Vec<Page> {
    let mut pen = Pen::new();
    pen.put(Font::Body, 26.0, "Quality Headings", 36.0);
    pen.put(
        Font::Body,
        12.0,
        "This opening paragraph wraps across a visual line so the",
        15.0,
    );
    pen.put(
        Font::Body,
        12.0,
        "converter can join it back into one block of prose.",
        15.0,
    );
    pen.blank(16.0);
    pen.put(Font::Body, 18.0, "Section Overview", 28.0);
    pen.put(
        Font::Body,
        12.0,
        "The section paragraph stays together across wrapped",
        15.0,
    );
    pen.put(
        Font::Body,
        12.0,
        "lines of ordinary prose in the body size.",
        15.0,
    );
    pen.blank(16.0);
    pen.put(Font::Bold, 14.0, "Detail Notes", 26.0);
    pen.put(
        Font::Body,
        12.0,
        "A short closing paragraph sits under the subsection.",
        15.0,
    );
    vec![pen.finish()]
}

fn paragraphs() -> Vec<Page> {
    let mut pen = Pen::new();
    pen.put(Font::Body, 26.0, "Wrapped Prose", 36.0);
    for line in [
        "This paragraph is wrapped across several visual lines in",
        "the file and should be read as a single paragraph without",
        "those line breaks.",
    ] {
        pen.put(Font::Body, 12.0, line, 15.0);
    }
    pen.blank(18.0);
    for line in [
        "A second paragraph follows after a wider vertical gap.",
        "It wraps as well, and it remains its own block of prose.",
    ] {
        pen.put(Font::Body, 12.0, line, 15.0);
    }
    vec![pen.finish()]
}

fn emphasis() -> Vec<Page> {
    let mut pen = Pen::new();
    pen.put(Font::Body, 26.0, "Emphasis", 36.0);
    pen.rich(
        12.0,
        &[
            (Font::Body, "A"),
            (Font::Bold, "bold"),
            (Font::Body, "claim, an"),
            (Font::Italic, "italic"),
            (Font::Body, "aside, and a"),
            (Font::BoldItalic, "combined"),
            (Font::Body, "mark."),
        ],
        16.0,
    );
    vec![pen.finish()]
}

fn lists() -> Vec<Page> {
    let mut ops = Vec::new();
    ops.push(text_op(Font::Body, 26.0, 72.0, 740.0, "Lists"));
    for (i, item) in [
        "1. Prepare the sample",
        "2. Run the converter",
        "3. Read the markdown",
    ]
    .iter()
    .enumerate()
    {
        ops.push(text_op(
            Font::Body,
            12.0,
            72.0,
            700.0 - i as f32 * 15.0,
            item,
        ));
    }
    ops.push(text_op(Font::Body, 12.0, 72.0, 640.0, "Shopping"));
    let bullets = [
        (72.0, 600.0, "\u{2022} Fruit"),
        (96.0, 585.0, "\u{2022} Apple"),
        (96.0, 570.0, "\u{2022} Pear"),
        (72.0, 555.0, "\u{2022} Grain"),
    ];
    for (x, y, text) in bullets {
        ops.push(text_op(Font::Body, 12.0, x, y, text));
    }
    vec![Page {
        ops,
        links: Vec::new(),
    }]
}

fn table_simple() -> Vec<Page> {
    let mut ops = Vec::new();
    ops.push(text_op(Font::Body, 26.0, 72.0, 740.0, "Simple Table"));
    ops.push(text_op(
        Font::Body,
        12.0,
        72.0,
        700.0,
        "Two people from the sample.",
    ));
    let rows: &[(&str, &str)] = &[("Name", "Role"), ("Ada", "Engineer"), ("Lin", "Writer")];
    paint_grid(&mut ops, 72.0, 640.0, &[150.0, 150.0], 22.0, rows.len());
    for (r, (left, right)) in rows.iter().enumerate() {
        let y = 646.0 - r as f32 * 22.0;
        let font = if r == 0 { Font::Bold } else { Font::Body };
        ops.push(text_op(font, 11.0, 80.0, y, left));
        ops.push(text_op(font, 11.0, 230.0, y, right));
    }
    vec![Page {
        ops,
        links: Vec::new(),
    }]
}

fn table_merged() -> Vec<Page> {
    // Header and total span the first two columns. The Blue row leaves
    // the middle cell empty. Edges still come from the full rows.
    let mut ops = vec![
        text_op(Font::Body, 26.0, 72.0, 740.0, "Merged Cells"),
        text_op(
            Font::Body,
            12.0,
            72.0,
            700.0,
            "Counts for the merged header.",
        ),
        rect_op(72.0, 640.0, 200.0, 22.0),
        rect_op(272.0, 640.0, 100.0, 22.0),
    ];
    for row in 0..2 {
        let y = 618.0 - row as f32 * 22.0;
        for col in 0..3 {
            let x = 72.0 + col as f32 * 100.0;
            ops.push(rect_op(x, y, 100.0, 22.0));
        }
    }
    ops.push(rect_op(72.0, 574.0, 200.0, 22.0));
    ops.push(rect_op(272.0, 574.0, 100.0, 22.0));
    ops.push(text_op(Font::Bold, 11.0, 80.0, 646.0, "Item"));
    ops.push(text_op(Font::Bold, 11.0, 280.0, 646.0, "Qty"));
    ops.push(text_op(Font::Body, 11.0, 80.0, 624.0, "Red"));
    ops.push(text_op(Font::Body, 11.0, 180.0, 624.0, "Widget"));
    ops.push(text_op(Font::Body, 11.0, 280.0, 624.0, "2"));
    ops.push(text_op(Font::Body, 11.0, 80.0, 602.0, "Blue"));
    ops.push(text_op(Font::Body, 11.0, 280.0, 602.0, "1"));
    ops.push(text_op(Font::Bold, 11.0, 80.0, 580.0, "Total"));
    ops.push(text_op(Font::Body, 11.0, 280.0, 580.0, "3"));
    vec![Page {
        ops,
        links: Vec::new(),
    }]
}

fn paint_grid(ops: &mut Vec<Op>, x0: f32, top: f32, cols: &[f32], row_h: f32, rows: usize) {
    for r in 0..rows {
        let mut x = x0;
        let y = top - r as f32 * row_h;
        for w in cols {
            ops.push(rect_op(x, y, *w, row_h));
            x += *w;
        }
    }
}

fn columns() -> Vec<Page> {
    let mut ops = Vec::new();
    ops.push(text_op(Font::Body, 26.0, 72.0, 740.0, "Two Column Article"));
    let left = [
        "Left alpha begins the left column here and",
        "left bravo continues the same left paragraph",
        "down the left side of the page until the",
        "left column is clearly prose.",
    ];
    let right = [
        "Right one begins the right column here and",
        "right two continues the same right paragraph",
        "down the right side of the page until the",
        "right column is clearly prose.",
    ];
    for (i, (l, r)) in left.iter().zip(right.iter()).enumerate() {
        let y = 690.0 - i as f32 * 15.0;
        ops.push(text_op(Font::Body, 11.0, 72.0, y, l));
        ops.push(text_op(Font::Body, 11.0, 340.0, y, r));
    }
    vec![Page {
        ops,
        links: Vec::new(),
    }]
}

fn margins() -> Vec<Page> {
    let bodies = [
        "First page body stays in the document.",
        "Second page body stays in the document.",
        "Third page body stays in the document.",
    ];
    let mut pages = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        let mut ops = Vec::new();
        ops.push(text_op(Font::Body, 10.0, 72.0, 760.0, "Running Header"));
        if i == 0 {
            ops.push(text_op(Font::Body, 26.0, 72.0, 710.0, "Margin Sample"));
            ops.push(text_op(Font::Body, 12.0, 72.0, 670.0, body));
        } else {
            ops.push(text_op(Font::Body, 12.0, 72.0, 700.0, body));
        }
        let mark = format!("\u{2014} {} \u{2014}", i + 1);
        ops.push(text_op(Font::Body, 11.0, 270.0, 56.0, &mark));
        ops.push(text_op(Font::Body, 10.0, 72.0, 36.0, "Internal Draft"));
        pages.push(Page {
            ops,
            links: Vec::new(),
        });
    }
    pages
}

fn page_break() -> Vec<Page> {
    let first = vec![
        text_op(Font::Body, 26.0, 72.0, 740.0, "Across The Break"),
        text_op(
            Font::Body,
            12.0,
            72.0,
            690.0,
            "The opening sentence stays on the first page.",
        ),
        text_op(
            Font::Body,
            12.0,
            72.0,
            72.0,
            "This sentence starts at the bottom of the first page and",
        ),
    ];
    let second = vec![text_op(
        Font::Body,
        12.0,
        72.0,
        740.0,
        "finishes at the top of the next page as one paragraph.",
    )];
    vec![
        Page {
            ops: first,
            links: Vec::new(),
        },
        Page {
            ops: second,
            links: Vec::new(),
        },
    ]
}

fn code() -> Vec<Page> {
    let mut pen = Pen::new();
    pen.put(Font::Body, 26.0, "Code Sample", 36.0);
    pen.put(
        Font::Body,
        12.0,
        "The following block is source code.",
        15.0,
    );
    pen.blank(12.0);
    for line in ["fn answer() -> i32 {", "    42", "}"] {
        pen.put(Font::Mono, 12.0, line, 15.0);
    }
    pen.blank(14.0);
    pen.put(Font::Body, 12.0, "A sentence after the block.", 15.0);
    vec![pen.finish()]
}

fn links() -> Vec<Page> {
    let size = 12.0;
    let prefix = "Read the ";
    let label = "project site";
    let x = 72.0 + advance(Font::Body, prefix, size);
    let w = advance(Font::Body, label, size);
    let mut pen = Pen::new();
    pen.put(Font::Body, 26.0, "Links", 36.0);
    // One painted line. The URL lives only in the link annotation.
    pen.put(
        Font::Body,
        size,
        "Read the project site for the converter notes.",
        16.0,
    );
    let mut page = pen.finish();
    page.links.push(Link {
        x,
        y: 700.0 - 2.0,
        w,
        h: size + 4.0,
        url: "https://example.com/pdfmd",
    });
    vec![page]
}

fn unicode() -> Vec<Page> {
    let mut pen = Pen::new();
    pen.put(Font::Body, 26.0, "Unicode Text", 36.0);
    pen.put(
        Font::Body,
        12.0,
        "Caf\u{00E9} na\u{00EF}ve r\u{00E9}sum\u{00E9} \u{2014} accents survive.",
        20.0,
    );
    pen.put(Font::Cjk, 12.0, "\u{4E2D}\u{6587}\u{5B57}", 20.0);
    pen.put(
        Font::Body,
        12.0,
        "The \u{FB01}rst \u{FB02}ow uses ligatures.",
        16.0,
    );
    vec![pen.finish()]
}

fn hyphen() -> Vec<Page> {
    let mut pen = Pen::new();
    pen.put(Font::Body, 26.0, "Hyphenation", 36.0);
    pen.put(Font::Body, 12.0, "The mea-", 15.0);
    pen.put(Font::Body, 12.0, "surement was careful and complete.", 15.0);
    vec![pen.finish()]
}

fn advance(font: Font, text: &str, size: f32) -> f32 {
    match font {
        // `/DW 1000` on the CID font: one em per glyph.
        Font::Cjk => text.chars().count() as f32 * size,
        // No `/Widths`: the extractor estimates half an em per source byte.
        _ => encode_winansi(text).len() as f32 * 0.5 * size,
    }
}

fn encode_winansi(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let Some(byte) = winansi_byte(ch) else {
            panic!("no WinAnsi byte for U+{:04X}", ch as u32);
        };
        out.push(byte);
    }
    out
}

fn winansi_byte(ch: char) -> Option<u8> {
    match ch {
        ' '..='~' => Some(ch as u8),
        // Differences [1 /fi /fl] on the body font.
        '\u{FB01}' => Some(1),
        '\u{FB02}' => Some(2),
        '\u{2022}' => Some(0x95),
        '\u{2013}' => Some(0x96),
        '\u{2014}' => Some(0x97),
        '\u{2018}' => Some(0x91),
        '\u{2019}' => Some(0x92),
        '\u{201C}' => Some(0x93),
        '\u{201D}' => Some(0x94),
        '\u{2026}' => Some(0x85),
        '\u{00A0}'..='\u{00FF}' => Some(ch as u8),
        _ => None,
    }
}

fn encode_cjk(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 2);
    for ch in text.chars() {
        match ch {
            '\u{4E2D}' | '\u{6587}' | '\u{5B57}' => {
                let u = ch as u32;
                out.push((u >> 8) as u8);
                out.push(u as u8);
            }
            _ => panic!("CJK font has no U+{:04X}", ch as u32),
        }
    }
    out
}

fn render(ops: &[Op]) -> Vec<u8> {
    let mut s = String::new();
    for op in ops {
        match &op.kind {
            OpKind::Text {
                font,
                size,
                x,
                y,
                text,
            } => {
                let name = font.resource();
                if *font == Font::Cjk {
                    let hex = hex_encode(&encode_cjk(text));
                    let _ = writeln!(
                        s,
                        "BT /{name} {size:.2} Tf 1 0 0 1 {x:.2} {y:.2} Tm <{hex}> Tj ET"
                    );
                } else {
                    let lit = pdf_literal(&encode_winansi(text));
                    let _ = writeln!(
                        s,
                        "BT /{name} {size:.2} Tf 1 0 0 1 {x:.2} {y:.2} Tm {lit} Tj ET"
                    );
                }
            }
            OpKind::Rect { x, y, w, h } => {
                let _ = writeln!(s, "{x:.2} {y:.2} {w:.2} {h:.2} re S");
            }
        }
    }
    s.into_bytes()
}

fn pdf_literal(bytes: &[u8]) -> String {
    let mut s = String::from("(");
    for &b in bytes {
        match b {
            b'\\' | b'(' | b')' => {
                s.push('\\');
                s.push(b as char);
            }
            0x20..=0x7E => s.push(b as char),
            _ => {
                let _ = write!(s, "\\{b:03o}");
            }
        }
    }
    s.push(')');
    s
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0F) as usize] as char);
    }
    s
}

struct Pdf {
    objs: Vec<Vec<u8>>,
}

impl Pdf {
    fn new() -> Self {
        Self {
            objs: vec![Vec::new()],
        }
    }

    fn alloc(&mut self) -> usize {
        self.objs.push(Vec::new());
        self.objs.len() - 1
    }

    fn set(&mut self, id: usize, body: Vec<u8>) {
        self.objs[id] = body;
    }

    fn push(&mut self, body: Vec<u8>) -> usize {
        let id = self.alloc();
        self.set(id, body);
        id
    }

    fn finish(&self, root: usize, info: usize) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = vec![0u64; self.objs.len()];
        for (id, body) in self.objs.iter().enumerate().skip(1) {
            offsets[id] = out.len() as u64;
            out.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            if !body.ends_with(b"\n") {
                out.push(b'\n');
            }
            out.extend_from_slice(b"endobj\n");
        }
        let xref_at = out.len();
        let size = self.objs.len();
        let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            let row = format!("{:010} 00000 n \n", offset);
            debug_assert_eq!(row.len(), 20);
            xref.push_str(&row);
        }
        let _ = write!(
            xref,
            "trailer << /Size {size} /Root {root} 0 R /Info {info} 0 R >>\nstartxref\n{xref_at}\n%%EOF\n"
        );
        out.extend_from_slice(xref.as_bytes());
        out
    }
}

struct FontIds {
    body: usize,
    bold: usize,
    italic: usize,
    both: usize,
    mono: usize,
    cjk: usize,
}

fn document(title: &str, pages: &[Page]) -> Vec<u8> {
    let mut pdf = Pdf::new();
    let catalog = pdf.alloc();
    let page_tree = pdf.alloc();
    let info = pdf.push(
        format!(
            "<< /Title {} /Producer (pdfmd tests/quality fixture generator) >>",
            pdf_literal(title.as_bytes())
        )
        .into_bytes(),
    );
    let fonts = push_fonts(&mut pdf);
    let mut kids = Vec::new();
    for page in pages {
        let content = pdf.push(stream_obj(&render(&page.ops)));
        let mut annots = Vec::new();
        for link in &page.links {
            annots.push(pdf.push(annot_body(link)));
        }
        kids.push(pdf.push(page_body(page_tree, content, &annots, &fonts)));
    }
    pdf.set(
        catalog,
        format!("<< /Type /Catalog /Pages {page_tree} 0 R >>").into_bytes(),
    );
    pdf.set(page_tree, pages_body(&kids));
    pdf.finish(catalog, info)
}

fn push_fonts(pdf: &mut Pdf) -> FontIds {
    let body = pdf.push(font_type1("Helvetica", true));
    let bold = pdf.push(font_type1("Helvetica-Bold", false));
    let italic = pdf.push(font_type1("Helvetica-Oblique", false));
    let both = pdf.push(font_type1("Helvetica-BoldOblique", false));
    let mono = pdf.push(font_type1("Courier", false));
    let cmap = pdf.push(stream_obj(CJK_CMAP.as_bytes()));
    let desc = pdf.push(
        b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /STSong-Light \
          /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
          /DW 1000 >>"
            .to_vec(),
    );
    let cjk = pdf.push(
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /STSong-Light /Encoding /Identity-H \
             /DescendantFonts [{desc} 0 R] /ToUnicode {cmap} 0 R >>"
        )
        .into_bytes(),
    );
    FontIds {
        body,
        bold,
        italic,
        both,
        mono,
        cjk,
    }
}

fn font_type1(name: &str, ligatures: bool) -> Vec<u8> {
    if ligatures {
        format!(
            "<< /Type /Font /Subtype /Type1 /BaseFont /{name} /Encoding << \
             /Type /Encoding /BaseEncoding /WinAnsiEncoding /Differences [1 /fi /fl] >> >>"
        )
        .into_bytes()
    } else {
        format!("<< /Type /Font /Subtype /Type1 /BaseFont /{name} /Encoding /WinAnsiEncoding >>")
            .into_bytes()
    }
}

const CJK_CMAP: &str = "\
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Adobe-Identity-UCS def
/CMapType 2 def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
3 beginbfchar
<4E2D> <4E2D>
<6587> <6587>
<5B57> <5B57>
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end
";

fn stream_obj(data: &[u8]) -> Vec<u8> {
    let mut body = format!("<< /Length {} >>\nstream\n", data.len()).into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(b"\nendstream");
    body
}

fn pages_body(kids: &[usize]) -> Vec<u8> {
    let mut s = String::from("<< /Type /Pages /Kids [");
    for (i, id) in kids.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        let _ = write!(s, "{id} 0 R");
    }
    let _ = write!(s, "] /Count {} >>", kids.len());
    s.into_bytes()
}

fn page_body(parent: usize, content: usize, annots: &[usize], fonts: &FontIds) -> Vec<u8> {
    let mut s = format!(
        "<< /Type /Page /Parent {parent} 0 R /MediaBox [0 0 612 792] /Resources << /Font << \
         /F1 {body} 0 R /F2 {bold} 0 R /F3 {italic} 0 R /F4 {both} 0 R /F5 {mono} 0 R \
         /F6 {cjk} 0 R >> >> /Contents {content} 0 R",
        body = fonts.body,
        bold = fonts.bold,
        italic = fonts.italic,
        both = fonts.both,
        mono = fonts.mono,
        cjk = fonts.cjk,
    );
    if !annots.is_empty() {
        s.push_str(" /Annots [");
        for (i, id) in annots.iter().enumerate() {
            if i > 0 {
                s.push(' ');
            }
            let _ = write!(s, "{id} 0 R");
        }
        s.push(']');
    }
    s.push_str(" >>");
    s.into_bytes()
}

fn annot_body(link: &Link) -> Vec<u8> {
    format!(
        "<< /Type /Annot /Subtype /Link /Rect [{x:.2} {y:.2} {x2:.2} {y2:.2}] \
         /Border [0 0 0] /A << /Type /Action /S /URI /URI {url} >> >>",
        x = link.x,
        y = link.y,
        x2 = link.x + link.w,
        y2 = link.y + link.h,
        url = pdf_literal(link.url.as_bytes()),
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_synthetic_fixture_builds_a_pdf() {
        for id in synthetic_ids() {
            let bytes = build(id).expect(id);
            assert!(bytes.starts_with(b"%PDF-1.4\n"), "{id}");
            assert_eq!(build(id).expect(id), bytes, "{id} is deterministic");
        }
        assert!(build("field-note").is_none());
    }
}
