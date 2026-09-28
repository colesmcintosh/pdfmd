//! Heuristics that turn extracted PDF content into structured Markdown.
//!
//! `format_page` keeps the original string-only path (used by unit tests).
//! `format_pages` consumes positioned spans so convert can recover columns,
//! font-size headings, tables, and running headers.

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::extract::layout::{PageLayout, PathRect, Span, SpanKind};
use crate::extract::structure::{Role, RoleMap};
use crate::extract::IMAGE_MARK;

use lines::{
    format_list_item, heading_level, is_all_caps_heading, is_list_item, is_numbered_heading,
    named_section, short_title_case, strip_heading_prefix,
};

mod columns;
mod lines;
mod tables;

/// Format a single page of raw text into a Markdown fragment.
#[cfg(test)]
pub fn format_page(raw: &str) -> String {
    format_page_layout(&layout_from_raw(raw), 0, &HashMap::new(), &[], &[])
}

/// Format each page from positioned spans. Empty pages stay empty strings.
pub fn format_pages(pages: &[PageLayout], roles: &RoleMap) -> Vec<String> {
    let (headers, footers) = running_margins(pages);
    let n = pages.len();
    if n <= 4 {
        return pages
            .iter()
            .enumerate()
            .map(|(i, page)| format_page_layout(page, i, roles, &headers, &footers))
            .collect();
    }
    crate::util::parallel_map(pages, |i, page| {
        format_page_layout(page, i, roles, &headers, &footers)
    })
}

#[cfg(test)]
fn layout_from_raw(raw: &str) -> PageLayout {
    let mut y = 1000.0;
    let mut spans = Vec::new();
    for line in raw.lines() {
        if line.trim().is_empty() {
            y -= 24.0;
            continue;
        }
        let text = line.trim().to_string();
        let width = text.len() as f32 * 6.0;
        spans.push(Span {
            text,
            x: 0.0,
            y,
            width,
            height: 12.0,
            font_size: 12.0,
            bold: false,
            italic: false,
            mono: false,
            kind: SpanKind::Text,
            mcid: None,
            space_before: false,
        });
        y -= 14.0;
    }
    PageLayout {
        text: raw.to_string(),
        spans,
        rects: Vec::new(),
    }
}

fn format_page_layout(
    page: &PageLayout,
    page_idx: usize,
    roles: &RoleMap,
    headers: &[String],
    footers: &[String],
) -> String {
    if page.spans.is_empty() {
        return String::new();
    }
    let skip: std::collections::HashSet<&str> = headers
        .iter()
        .chain(footers.iter())
        .map(String::as_str)
        .collect();
    let mut spans: Vec<&Span> = page.spans.iter().collect();
    if !skip.is_empty() {
        let cols = vec![0usize; spans.len()];
        let lines = visual_lines(&spans, &cols);
        let drop_y: Vec<f32> = lines
            .iter()
            .filter(|l| skip.contains(plain_line(l).as_str()))
            .map(|l| l.y)
            .collect();
        if !drop_y.is_empty() {
            spans.retain(|s| {
                s.kind == SpanKind::Image || drop_y.iter().all(|y| (s.y - y).abs() > 2.0)
            });
        }
    }
    if spans.is_empty() {
        return String::new();
    }
    // Text set along the page edge (an arXiv stamp) has no horizontal
    // extent; left in, it lands on whatever prose line shares its baseline.
    // A page that is mostly rotated is read as it is.
    let (rotated, upright): (Vec<&Span>, Vec<&Span>) =
        spans.iter().copied().partition(|s| is_rotated(s));
    let margin_notes = if rotated.len() * 2 < spans.len() {
        spans = upright;
        rotated
    } else {
        Vec::new()
    };

    let median = median_size(&spans);
    let (gaps, bands) = columns::page_bands(&spans);

    let mut parts = Vec::new();
    for (i, band) in bands.iter().enumerate() {
        // Only a banded page clips rects, so a single-band page keeps every rule.
        let rects: Vec<PathRect> = if bands.len() == 1 {
            page.rects.clone()
        } else {
            let top = if i == 0 {
                f32::MAX
            } else {
                bands[i - 1].bottom
            };
            let bottom = bands.get(i + 1).map_or(f32::MIN, |b| b.top);
            let (top, bottom) = ((top + band.top) / 2.0, (bottom + band.bottom) / 2.0);
            page.rects
                .iter()
                .copied()
                .filter(|r| r.y < top && r.y + r.h > bottom)
                .collect()
        };
        let band_gaps: &[f32] = if band.split { &gaps } else { &[] };
        for col_spans in columns::split_columns(&band.spans, band_gaps) {
            let md = format_column(&col_spans, &rects, page_idx, roles, median);
            if !md.is_empty() {
                parts.push(md);
            }
        }
    }
    let notes: Vec<&str> = margin_notes.iter().map(|s| s.text.trim()).collect();
    parts.push(escape_leading_hash(notes.join(" ")));
    join_blocks(parts)
}

/// Text whose pen moved along y rather than x: several glyphs, no width.
fn is_rotated(s: &Span) -> bool {
    s.kind == SpanKind::Text && s.width < s.font_size * 0.25 && s.text.trim().chars().count() >= 2
}

/// Join markdown blocks with a blank line between them, dropping empties.
fn join_blocks(parts: impl IntoIterator<Item = String>) -> String {
    parts
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn format_column(
    spans: &[&Span],
    rects: &[crate::extract::layout::PathRect],
    page_idx: usize,
    roles: &RoleMap,
    median: f32,
) -> String {
    let x0 = spans.iter().map(|s| s.x).fold(f32::MAX, f32::min);
    let x1 = spans.iter().map(|s| s.x + s.width).fold(f32::MIN, f32::max);
    if rects.len() >= 3 {
        let col_rects: Vec<_> = rects
            .iter()
            .copied()
            .filter(|r| r.x < x1 && r.x + r.w > x0)
            .collect();
        if col_rects.len() >= 3 {
            if let Some((table, bbox)) = tables::ruled_table(spans, &col_rects) {
                let rest: Vec<&Span> = spans
                    .iter()
                    .copied()
                    .filter(|s| {
                        s.kind == SpanKind::Image
                            || s.x + s.width < bbox[0] - 1.0
                            || s.x > bbox[2] + 1.0
                            || s.y + s.height < bbox[1] - 1.0
                            || s.y > bbox[3] + 1.0
                    })
                    .collect();
                if rest.is_empty() {
                    return table;
                }
                let rest_md = format_column_text(&rest, page_idx, roles, median);
                // Text above the table's top edge reads first.
                return if rest.iter().any(|s| s.y > bbox[3]) {
                    join_blocks([rest_md, table])
                } else {
                    join_blocks([table, rest_md])
                };
            }
        }
    }
    format_column_text(spans, page_idx, roles, median)
}

fn format_column_text(spans: &[&Span], page_idx: usize, roles: &RoleMap, median: f32) -> String {
    let cols = vec![0usize; spans.len()];
    let lines = visual_lines(spans, &cols);
    if lines.is_empty() {
        return String::new();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some((n, table)) = tables::table_at(&lines, i) {
            out.push(table);
            i += n;
            continue;
        }
        let start = i;
        i += 1;
        while i < lines.len() {
            let dy = (lines[i - 1].y - lines[i].y).abs();
            let size = lines[i]
                .spans
                .iter()
                .map(|s| s.font_size)
                .fold(12.0f32, f32::max);
            if dy > size * 1.5 {
                break;
            }
            // A heading set larger than the text under it is its own block.
            let (above, here) = (text_size(&lines[i - 1]), text_size(&lines[i]));
            if above > here * 1.15 || here > above * 1.15 {
                break;
            }
            if is_bold_line(&lines[i - 1]) && !is_bold_line(&lines[i]) {
                break;
            }
            // A quoted Markdown heading line stays on its own.
            if opens_with_hash(&lines[i - 1]) || opens_with_hash(&lines[i]) {
                break;
            }
            if tables::table_at(&lines, i).is_some() {
                break;
            }
            i += 1;
        }
        let block = format_line_block(&lines[start..i], page_idx, roles, median);
        if !block.is_empty() {
            out.push(block);
        }
    }
    join_blocks(out)
}

fn format_line_block(lines: &[VLine<'_>], page_idx: usize, roles: &RoleMap, median: f32) -> String {
    if lines.is_empty() {
        return String::new();
    }
    if lines.iter().all(line_is_blank) {
        return image_block(lines);
    }
    if lines.iter().all(|l| is_list_item(&plain_line(l))) {
        return lines
            .iter()
            .map(|l| format_list_item(&plain_line(l)))
            .collect::<Vec<_>>()
            .join("\n");
    }
    if lines.len() >= 2 && lines.iter().all(is_mono_line) {
        let mut body = String::new();
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                body.push('\n');
            }
            body.push_str(&plain_line(line));
        }
        return format!("```\n{body}\n```");
    }
    if lines.len() == 1 {
        let line = plain_line(&lines[0]);
        if let Some(level) = heading_for_line(&lines[0], &line, page_idx, roles, median) {
            return format!("{} {}", "#".repeat(level), strip_heading_prefix(&line));
        }
        return escape_leading_hash(style_line(&lines[0]));
    }
    escape_leading_hash(join_paragraph(lines))
}

/// Body text that happens to open with `#` (`# of calls`, a quoted
/// Markdown prompt) must not render as a heading.
fn escape_leading_hash(text: String) -> String {
    if text.starts_with('#') {
        format!("\\{text}")
    } else {
        text
    }
}

/// Largest text size on a line; 0 for a line of images.
fn text_size(line: &VLine<'_>) -> f32 {
    line.spans
        .iter()
        .filter(|s| s.kind == SpanKind::Text)
        .map(|s| s.font_size)
        .fold(0.0f32, f32::max)
}

fn opens_with_hash(line: &VLine<'_>) -> bool {
    line.spans
        .iter()
        .find(|s| s.kind == SpanKind::Text && !s.text.trim().is_empty())
        .is_some_and(|s| s.text.trim_start().starts_with('#'))
}

fn is_bold_line(line: &VLine<'_>) -> bool {
    let mut text = line
        .spans
        .iter()
        .filter(|s| s.kind == SpanKind::Text && !s.text.trim().is_empty())
        .peekable();
    text.peek().is_some() && text.all(|s| s.bold)
}

fn line_is_blank(line: &VLine<'_>) -> bool {
    line.spans
        .iter()
        .all(|s| s.kind == SpanKind::Image || s.text.trim().is_empty())
}

fn heading_for_line(
    vline: &VLine<'_>,
    line: &str,
    page_idx: usize,
    roles: &RoleMap,
    median: f32,
) -> Option<usize> {
    if let Some(Role::Heading(n)) = line_role(vline, page_idx, roles) {
        return Some((n as usize).clamp(1, 6));
    }
    let size = vline
        .spans
        .iter()
        .filter(|s| s.kind == SpanKind::Text)
        .map(|s| s.font_size)
        .fold(0.0f32, f32::max);
    if median > 0.1 && size >= median * 1.8 && line.len() <= 120 {
        return Some(1);
    }
    if median > 0.1 && size >= median * 1.4 && line.len() <= 120 {
        return Some(2);
    }
    let bold = vline.spans.iter().any(|s| s.bold);
    if median > 0.1 && (size >= median * 1.15 || bold) && !is_bold_label(line) {
        if let Some(level) = heading_level(line) {
            return Some(level);
        }
        if bold && short_title_case(line) {
            return Some(3);
        }
    }
    if is_numbered_heading(line) {
        return heading_level(line);
    }
    if let Some(level) = named_section(line) {
        return Some(level);
    }
    // Body-size Title Case lines are ordinary prose; all-caps stays a heading.
    if is_all_caps_heading(line) {
        return Some(2);
    }
    None
}

/// Emphasised text that is a label rather than a title: `Cluster ID: 6`,
/// `Given answer: {answer}`, or a bare `{placeholder}`.
fn is_bold_label(line: &str) -> bool {
    if !line.starts_with(|c: char| c.is_uppercase() || c.is_ascii_digit()) {
        return true;
    }
    line.rsplit_once(": ")
        .is_some_and(|(_, value)| !value.starts_with(char::is_uppercase))
}

fn line_role(line: &VLine<'_>, page_idx: usize, roles: &RoleMap) -> Option<Role> {
    for s in &line.spans {
        if let Some(mcid) = s.mcid {
            if let Some(r) = roles.get(&(page_idx, mcid)) {
                return Some(*r);
            }
        }
    }
    None
}

fn join_paragraph(lines: &[VLine<'_>]) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let t = style_line(line);
        if i == 0 {
            out.push_str(&t);
            continue;
        }
        // Carry an emphasis run across the line break before joining, so a
        // word hyphenated inside italics still rejoins.
        let wrap = ["***", "**", "*"]
            .into_iter()
            .find(|w| t.starts_with(w) && !t[w.len()..].starts_with('*'));
        let rest = match wrap {
            Some(w) if continue_emphasis(&mut out, w) => &t[w.len()..],
            _ => t.as_str(),
        };
        if out.ends_with('-') && rest.starts_with(char::is_lowercase) {
            out.pop();
            out.push_str(rest);
            continue;
        }
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(rest);
    }
    out
}

/// Reopen the emphasis run `out` just closed with `wrap`, so neighbouring
/// runs in one style read `**a b**` rather than `**a** **b**`.
fn continue_emphasis(out: &mut String, wrap: &str) -> bool {
    if wrap.is_empty() {
        return false;
    }
    let body = out.trim_end_matches(' ');
    let Some(before) = body.strip_suffix(wrap) else {
        return false;
    };
    if before.is_empty() || before.ends_with('*') {
        return false;
    }
    let cut = before.len();
    out.replace_range(cut..cut + wrap.len(), "");
    true
}

fn style_line(line: &VLine<'_>) -> String {
    let mut out = String::new();
    for s in &line.spans {
        if s.kind == SpanKind::Image {
            push_image_mark(&mut out, &s.text);
            continue;
        }
        push_span(&mut out, s, true);
    }
    out
}

/// Append one span, restoring the word break the extractor recorded but did
/// not emit as a glyph. Emphasis markers are optional so the plain-text
/// callers (line matching, running-margin detection) share the same joiner.
fn push_span(out: &mut String, s: &Span, styled: bool) {
    if s.space_before && !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
    let t = s.text.as_str();
    let wrap = match (styled && !t.trim().is_empty(), s.bold, s.italic) {
        (true, true, true) => "***",
        (true, true, false) => "**",
        (true, false, true) => "*",
        _ => "",
    };
    if wrap.is_empty() {
        out.push_str(t);
        return;
    }
    // Markers hug the text: `** Bold**` does not render as bold.
    let core = t.trim();
    let lead = &t[..t.len() - t.trim_start().len()];
    let trail = &t[t.trim_end().len()..];
    let reopened = continue_emphasis(out, wrap);
    out.push_str(lead);
    if !reopened {
        out.push_str(wrap);
    }
    out.push_str(core);
    out.push_str(wrap);
    out.push_str(trail);
}

fn image_block(lines: &[VLine<'_>]) -> String {
    let mut out = String::new();
    for line in lines {
        for s in &line.spans {
            if s.kind == SpanKind::Image {
                push_image_mark(&mut out, &s.text);
            }
        }
    }
    out
}

fn push_image_mark(out: &mut String, filename: &str) {
    out.push(IMAGE_MARK);
    out.push_str(filename);
    out.push(IMAGE_MARK);
}

fn is_mono_line(line: &VLine<'_>) -> bool {
    let text: Vec<_> = line
        .spans
        .iter()
        .filter(|s| s.kind == SpanKind::Text)
        .collect();
    !text.is_empty() && text.iter().all(|s| s.mono)
}

fn median_size(spans: &[&Span]) -> f32 {
    let mut items: Vec<(f32, usize)> = spans
        .iter()
        .filter(|s| s.kind == SpanKind::Text && s.font_size > 0.1)
        .map(|s| (s.font_size, s.text.len().max(1)))
        .collect();
    if items.is_empty() {
        return 12.0;
    }
    items.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
    let total: usize = items.iter().map(|(_, n)| n).sum();
    let mut acc = 0usize;
    for (size, n) in items {
        acc += n;
        if acc * 2 >= total {
            return size;
        }
    }
    12.0
}

fn running_margins(pages: &[PageLayout]) -> (Vec<String>, Vec<String>) {
    if pages.len() < 3 {
        return (Vec::new(), Vec::new());
    }
    let mut heads: HashMap<String, usize> = HashMap::new();
    let mut foots: HashMap<String, usize> = HashMap::new();
    for page in pages {
        let (first, last) = first_last(page);
        if let Some(t) = first {
            if !t.is_empty() && t.len() < 80 {
                *heads.entry(t).or_insert(0) += 1;
            }
        }
        if let Some(t) = last {
            if !t.is_empty() && t.len() < 80 {
                *foots.entry(t).or_insert(0) += 1;
            }
        }
    }
    let keep = |freq: HashMap<String, usize>| {
        freq.into_iter()
            .filter(|(_, n)| *n >= 3)
            .map(|(t, _)| t)
            .collect()
    };
    (keep(heads), keep(foots))
}

fn first_last(page: &PageLayout) -> (Option<String>, Option<String>) {
    let mut max_y = f32::MIN;
    let mut min_y = f32::MAX;
    let mut n = 0usize;
    for s in &page.spans {
        if s.kind != SpanKind::Text {
            continue;
        }
        n += 1;
        max_y = max_y.max(s.y);
        min_y = min_y.min(s.y);
    }
    if n == 0 {
        return (None, None);
    }
    (
        Some(band_text(&page.spans, max_y)),
        Some(band_text(&page.spans, min_y)),
    )
}

fn band_text(spans: &[Span], y: f32) -> String {
    let mut band: Vec<&Span> = spans
        .iter()
        .filter(|s| s.kind == SpanKind::Text && (s.y - y).abs() < 6.0)
        .collect();
    band.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal));
    let mut out = String::new();
    for s in band {
        push_span(&mut out, s, false);
    }
    out.trim().to_string()
}

pub(super) struct VLine<'a> {
    y: f32,
    col: usize,
    spans: Vec<&'a Span>,
}

fn visual_lines<'a>(spans: &[&'a Span], cols: &[usize]) -> Vec<VLine<'a>> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&i, &j| {
        cols[i]
            .cmp(&cols[j])
            .then_with(|| {
                spans[j]
                    .y
                    .partial_cmp(&spans[i].y)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| {
                spans[i]
                    .x
                    .partial_cmp(&spans[j].x)
                    .unwrap_or(Ordering::Equal)
            })
    });
    let mut lines: Vec<VLine<'a>> = Vec::new();
    let mut line_size = 0.0f32;
    for i in order {
        let s = spans[i];
        let col = cols[i];
        if let Some(last) = lines.last_mut() {
            // Measure against the larger text so a superscript joins its line.
            let thresh = (s.font_size.max(line_size) * 0.45).max(2.0);
            if last.col == col && (last.y - s.y).abs() < thresh {
                last.spans.push(s);
                // The baseline is the main text's, not a raised exponent's.
                if s.font_size > line_size {
                    last.y = s.y;
                    line_size = s.font_size;
                }
                continue;
            }
        }
        line_size = s.font_size;
        lines.push(VLine {
            y: s.y,
            col,
            spans: vec![s],
        });
    }
    for line in &mut lines {
        line.spans
            .sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal));
    }
    lines
}

pub(super) fn plain_line(line: &VLine<'_>) -> String {
    let mut out = String::new();
    for s in &line.spans {
        if s.kind != SpanKind::Image {
            push_span(&mut out, s, false);
        }
    }
    let trimmed = out.trim();
    if trimmed.len() == out.len() {
        out
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_section_titles_are_headings() {
        assert_eq!(
            format_page("Abstract\n\nBody copy goes here."),
            "### Abstract\n\nBody copy goes here."
        );
        assert_eq!(
            format_page("Introduction\n\nBody copy goes here."),
            "# Introduction\n\nBody copy goes here."
        );
    }
    use crate::extract::layout::PathRect;

    fn sp(text: &str, x: f32, y: f32, size: f32) -> Span {
        Span {
            text: text.into(),
            x,
            y,
            width: text.len() as f32 * size * 0.5,
            height: size,
            font_size: size,
            bold: false,
            italic: false,
            mono: false,
            kind: SpanKind::Text,
            mcid: None,
            space_before: false,
        }
    }

    #[test]
    fn paragraph_lines_are_rejoined() {
        let raw = "This is a paragraph\nthat wraps across\ntwo lines.";
        assert_eq!(
            format_page(raw),
            "This is a paragraph that wraps across two lines."
        );
    }

    #[test]
    fn all_caps_short_line_becomes_h2() {
        let raw = "INTRODUCTION\n\nBody copy goes here.";
        assert_eq!(format_page(raw), "## INTRODUCTION\n\nBody copy goes here.");
    }

    #[test]
    fn numbered_heading_levels() {
        assert_eq!(heading_level("1. Overview"), Some(1));
        assert_eq!(heading_level("1.2 Details"), Some(2));
        assert_eq!(heading_level("1.2.3 Sub-detail"), Some(3));
    }

    #[test]
    fn bullets_become_markdown_list() {
        let raw = "- apples\n- oranges\n- pears";
        assert_eq!(format_page(raw), "- apples\n- oranges\n- pears");
    }

    #[test]
    fn unicode_bullets_become_markdown_list() {
        let raw = "\u{2022} alpha\n\u{2022} beta";
        assert_eq!(format_page(raw), "- alpha\n- beta");
    }

    #[test]
    fn ordered_list_is_preserved() {
        let raw = "1. first\n2. second\n3. third";
        assert_eq!(format_page(raw), "1. first\n2. second\n3. third");
    }

    #[test]
    fn empty_blocks_produce_no_markdown() {
        assert_eq!(format_page("\n\n"), "");
    }

    #[test]
    fn font_size_promotes_title() {
        let page = PageLayout {
            text: String::new(),
            spans: vec![
                sp("Big Title", 50.0, 700.0, 24.0),
                sp(
                    "Body text that is long enough to stay a paragraph.",
                    50.0,
                    660.0,
                    12.0,
                ),
            ],
            rects: Vec::new(),
        };
        let md = format_pages(&[page], &HashMap::new());
        assert!(md[0].starts_with("# Big Title"), "{}", md[0]);
        assert!(md[0].contains("Body text"));
    }

    #[test]
    fn hyphenation_joins_wrapped_words() {
        let page = PageLayout {
            text: String::new(),
            spans: vec![
                sp("hyphen-", 50.0, 700.0, 12.0),
                sp("ation works", 50.0, 686.0, 12.0),
            ],
            rects: Vec::new(),
        };
        let md = format_pages(&[page], &HashMap::new());
        assert_eq!(md[0], "hyphenation works");
    }

    #[test]
    fn columns_read_left_then_right() {
        let mut spans = Vec::new();
        for i in 0..4 {
            spans.push(sp("L", 20.0, 700.0 - i as f32 * 14.0, 12.0));
            spans.push(sp("R", 320.0, 700.0 - i as f32 * 14.0, 12.0));
        }
        let page = PageLayout {
            text: String::new(),
            spans,
            rects: Vec::new(),
        };
        let md = format_pages(&[page], &HashMap::new());
        let left_at = md[0].find('L').unwrap();
        let right_at = md[0].rfind('R').unwrap();
        assert!(left_at < right_at, "{}", md[0]);
    }

    #[test]
    fn two_column_prose_is_not_a_table() {
        let left = [
            "of tokens, and show that it is possible to train",
            "state-of-the-art models using publicly available",
            "datasets exclusively, without resorting to closed",
            "sources that would prevent a full release.",
        ];
        let right = [
            "that the performance of a 7B model continues to",
            "improve even after 1T tokens of extra training.",
            "The focus of this work is to train a series of",
            "language models that achieve strong results.",
        ];
        let mut spans = Vec::new();
        for i in 0..4 {
            spans.push(sp(left[i], 20.0, 700.0 - i as f32 * 14.0, 12.0));
            spans.push(sp(right[i], 320.0, 700.0 - i as f32 * 14.0, 12.0));
        }
        let page = PageLayout {
            text: String::new(),
            spans,
            rects: Vec::new(),
        };
        let md = format_pages(&[page], &HashMap::new());
        assert!(!md[0].contains("| --- |"), "{}", md[0]);
        let l = md[0].find("of tokens").expect(&md[0]);
        let r = md[0].find("7B model").expect(&md[0]);
        assert!(l < r, "{}", md[0]);
    }

    #[test]
    fn running_headers_are_stripped() {
        let pages: Vec<PageLayout> = (0..3)
            .map(|i| PageLayout {
                text: String::new(),
                spans: vec![
                    sp("CONFIDENTIAL", 50.0, 780.0, 9.0),
                    sp(&format!("Page body {i}"), 50.0, 700.0, 12.0),
                ],
                rects: Vec::new(),
            })
            .collect();
        let md = format_pages(&pages, &HashMap::new());
        for page in &md {
            assert!(!page.contains("CONFIDENTIAL"), "{page}");
            assert!(page.contains("Page body"));
        }
    }

    #[test]
    fn tagged_heading_wins() {
        let mut span = sp("Tagged", 50.0, 700.0, 12.0);
        span.mcid = Some(1);
        let page = PageLayout {
            text: String::new(),
            spans: vec![span],
            rects: Vec::new(),
        };
        let mut roles = RoleMap::new();
        roles.insert((0, 1), Role::Heading(2));
        let md = format_pages(&[page], &roles);
        assert_eq!(md[0], "## Tagged");
    }

    #[test]
    fn bold_and_italic_wrap() {
        let mut bold = sp("bold word.", 50.0, 700.0, 12.0);
        bold.bold = true;
        let mut italic = sp("italic word.", 50.0, 660.0, 12.0);
        italic.italic = true;
        let page = PageLayout {
            text: String::new(),
            spans: vec![bold, italic],
            rects: Vec::new(),
        };
        let md = format_pages(&[page], &HashMap::new());
        assert!(md[0].contains("**bold word.**"), "{}", md[0]);
        assert!(md[0].contains("*italic word.*"), "{}", md[0]);
    }

    #[test]
    fn bold_italic_and_image_spans() {
        let mut both = sp("both.", 50.0, 700.0, 12.0);
        both.bold = true;
        both.italic = true;
        let image = Span {
            text: "img-001.jpg".into(),
            x: 50.0,
            y: 640.0,
            width: 1.0,
            height: 1.0,
            font_size: 1.0,
            bold: false,
            italic: false,
            mono: false,
            kind: SpanKind::Image,
            mcid: None,
            space_before: false,
        };
        let page = PageLayout {
            text: String::new(),
            spans: vec![both, image],
            rects: Vec::new(),
        };
        let md = format_pages(&[page], &HashMap::new());
        assert!(md[0].contains("***both.***"), "{}", md[0]);
        assert!(md[0].contains('\u{0001}'));
    }

    #[test]
    fn running_footers_are_stripped() {
        let pages: Vec<PageLayout> = (0..3)
            .map(|i| PageLayout {
                text: String::new(),
                spans: vec![
                    sp(&format!("Page body {i}"), 50.0, 700.0, 12.0),
                    sp("Page 1 of 3", 50.0, 40.0, 9.0),
                ],
                rects: Vec::new(),
            })
            .collect();
        let md = format_pages(&pages, &HashMap::new());
        for page in &md {
            assert!(!page.contains("Page 1 of 3"), "{page}");
            assert!(page.contains("Page body"));
        }
    }

    #[test]
    fn monospace_block_is_fenced() {
        let mut a = sp("fn main() {", 50.0, 700.0, 10.0);
        a.mono = true;
        let mut b = sp("}", 50.0, 686.0, 10.0);
        b.mono = true;
        let page = PageLayout {
            text: String::new(),
            spans: vec![a, b],
            rects: Vec::new(),
        };
        let md = format_pages(&[page], &HashMap::new());
        assert!(md[0].starts_with("```"));
        assert!(md[0].contains("fn main() {"));
    }

    #[test]
    fn borderless_aligned_columns_become_table() {
        let page = PageLayout {
            text: String::new(),
            spans: vec![
                sp("Name", 20.0, 700.0, 12.0),
                sp("Age", 200.0, 700.0, 12.0),
                sp("Ada", 20.0, 686.0, 12.0),
                sp("36", 200.0, 686.0, 12.0),
            ],
            rects: Vec::new(),
        };
        let md = format_pages(&[page], &HashMap::new());
        assert!(md[0].contains("| Name | Age |"), "{}", md[0]);
        assert!(md[0].contains("| Ada | 36 |"), "{}", md[0]);
    }

    #[test]
    fn ruled_rects_become_table() {
        let page = PageLayout {
            text: String::new(),
            spans: vec![
                sp("A", 10.0, 28.0, 10.0),
                sp("B", 60.0, 28.0, 10.0),
                sp("C", 10.0, 8.0, 10.0),
                sp("D", 60.0, 8.0, 10.0),
            ],
            rects: vec![
                PathRect {
                    x: 0.0,
                    y: 0.0,
                    w: 100.0,
                    h: 40.0,
                },
                PathRect {
                    x: 0.0,
                    y: 0.0,
                    w: 50.0,
                    h: 40.0,
                },
                PathRect {
                    x: 0.0,
                    y: 20.0,
                    w: 100.0,
                    h: 20.0,
                },
            ],
        };
        let md = format_pages(&[page], &HashMap::new());
        assert!(md[0].contains("| A | B |"), "{}", md[0]);
        assert!(md[0].contains("| C | D |"), "{}", md[0]);
    }

    fn page_of(spans: Vec<Span>) -> String {
        let page = PageLayout {
            text: String::new(),
            spans,
            rects: Vec::new(),
        };
        format_pages(&[page], &HashMap::new()).remove(0)
    }

    fn styled(mut span: Span, bold: bool, italic: bool) -> Span {
        span.bold = bold;
        span.italic = italic;
        span
    }

    /// Two columns of prose, then a table spanning both under a caption.
    fn two_columns_over_a_table() -> Vec<Span> {
        let mut spans = Vec::new();
        for i in 0..6 {
            let y = 700.0 - i as f32 * 14.0;
            spans.push(sp(
                &format!("left column prose line number {i} reads on"),
                20.0,
                y,
                10.0,
            ));
            spans.push(sp(
                &format!("right column prose line number {i} reads on"),
                300.0,
                y,
                10.0,
            ));
        }
        spans.push(sp(
            "Table 1: a caption that runs the full width of the page under the prose",
            20.0,
            600.0,
            10.0,
        ));
        for (r, row) in [
            ["Model", "Score", "Cost"],
            ["Base", "27.3", "3.3"],
            ["Big", "28.4", "2.3"],
        ]
        .iter()
        .enumerate()
        {
            let y = 580.0 - r as f32 * 14.0;
            for (c, cell) in row.iter().enumerate() {
                spans.push(sp(cell, 20.0 + c as f32 * 200.0, y, 10.0));
            }
        }
        spans
    }

    #[test]
    fn full_width_table_under_two_columns_stays_whole() {
        let md = page_of(two_columns_over_a_table());
        assert!(md.contains("| Model | Score | Cost |"), "{md}");
        assert!(md.contains("| Big | 28.4 | 2.3 |"), "{md}");
        let left = md.find("left column prose line number 5").expect(&md);
        let right = md.find("right column prose line number 0").expect(&md);
        let table = md.find("| Model").expect(&md);
        assert!(left < right && right < table, "{md}");
    }

    #[test]
    fn table_rows_may_leave_cells_empty() {
        let mut spans = Vec::new();
        let rows: [&[(&str, f32)]; 4] = [
            &[("Dataset", 20.0), ("Method", 120.0), ("Score", 260.0)],
            &[("CISC", 120.0), ("13.0", 260.0)],
            &[("AQuA", 20.0), ("Vec", 120.0), ("8.9", 260.0)],
            &[("CISC", 120.0), ("11.0", 260.0)],
        ];
        for (r, row) in rows.iter().enumerate() {
            for &(text, x) in row.iter() {
                spans.push(sp(text, x, 700.0 - r as f32 * 12.0, 10.0));
            }
        }
        let md = page_of(spans);
        assert!(md.contains("|  | CISC | 13.0 |"), "{md}");
        assert!(md.contains("| AQuA | Vec | 8.9 |"), "{md}");
    }

    #[test]
    fn a_label_between_rows_folds_into_the_nearer_one() {
        let mut spans = vec![
            sp("Set", 20.0, 714.0, 10.0),
            sp("Method", 120.0, 714.0, 10.0),
            sp("Score", 260.0, 714.0, 10.0),
        ];
        for r in 0..4 {
            let y = 700.0 - r as f32 * 14.0;
            spans.push(sp(&format!("m{r}"), 120.0, y, 10.0));
            spans.push(sp(&format!("{r}.5"), 260.0, y, 10.0));
        }
        // A multirow label centred between the middle rows, nearer the third.
        spans.push(sp("Group", 20.0, 700.0 - 14.0 * 1.6, 10.0));
        let md = page_of(spans);
        assert!(md.contains("| Group | m2 | 2.5 |"), "{md}");
        // Header, separator, and four rows: the label took no row of its own.
        assert_eq!(md.lines().count(), 6, "{md}");
    }

    #[test]
    fn justified_prose_is_not_a_table() {
        // Loose justification opens word gaps wider than a table gutter;
        // the next line's words ignore those gutters.
        let first = ["We", "computed", "the", "reduction", "for", "each"];
        let second = ["(dataset, model)", "combination by", "running"];
        let mut spans = Vec::new();
        for (i, w) in first.iter().enumerate() {
            spans.push(sp(w, 20.0 + i as f32 * 45.0, 700.0, 10.0));
        }
        for (i, w) in second.iter().enumerate() {
            spans.push(sp(w, 20.0 + i as f32 * 95.0, 686.0, 10.0));
        }
        let md = page_of(spans);
        assert!(!md.contains("| --- |"), "{md}");
    }

    #[test]
    fn section_numbers_and_equations_are_not_tables() {
        let md = page_of(vec![
            sp("3", 20.0, 700.0, 10.0),
            sp("Experiments", 40.0, 700.0, 10.0),
            sp("3.1", 20.0, 686.0, 10.0),
            sp("Datasets", 40.0, 686.0, 10.0),
        ]);
        assert!(!md.contains("| --- |"), "{md}");
        let md = page_of(vec![
            sp("x = y", 120.0, 700.0, 10.0),
            sp("(11)", 300.0, 700.0, 10.0),
            sp("a + b", 120.0, 686.0, 10.0),
            sp("(12)", 300.0, 686.0, 10.0),
        ]);
        assert!(!md.contains("| --- |"), "{md}");
    }

    #[test]
    fn math_stacked_under_a_numbered_equation_is_not_a_table() {
        let md = page_of(vec![
            sp("c = exp", 120.0, 700.0, 10.0),
            sp("(11)", 300.0, 700.0, 10.0),
            sp("P K", 110.0, 690.0, 10.0),
            sp("c j", 160.0, 690.0, 10.0),
            sp("j=1 exp", 110.0, 680.0, 8.0),
            sp("T", 160.0, 680.0, 8.0),
        ]);
        assert!(!md.contains("| --- |"), "{md}");
    }

    #[test]
    fn superscripts_join_their_line() {
        let md = page_of(vec![
            sp("Model", 20.0, 700.0, 10.0),
            sp("Cost", 200.0, 700.0, 10.0),
            sp("Base", 20.0, 686.0, 10.0),
            sp("3.3 · 10", 200.0, 686.0, 10.0),
            sp("18", 240.0, 689.6, 7.0),
            sp("Big", 20.0, 672.0, 10.0),
            sp("2.3 · 10", 200.0, 672.0, 10.0),
            sp("19", 240.0, 675.6, 7.0),
        ]);
        assert!(md.contains("| Base | 3.3 · 1018 |"), "{md}");
        assert!(md.contains("| Big | 2.3 · 1019 |"), "{md}");
    }

    #[test]
    fn body_text_opening_with_a_hash_is_escaped() {
        let md = page_of(vec![sp("# of calls in the pipeline", 20.0, 700.0, 10.0)]);
        assert_eq!(md, "\\# of calls in the pipeline");
    }

    #[test]
    fn neighbouring_runs_share_one_emphasis() {
        let mut second = styled(sp("words", 90.0, 700.0, 10.0), true, false);
        second.space_before = true;
        let mut tail = sp("mid sentence.", 120.0, 700.0, 10.0);
        tail.space_before = true;
        let md = page_of(vec![
            sp("We set", 20.0, 700.0, 10.0),
            styled(sp(" Bold", 50.0, 700.0, 10.0), true, false),
            second,
            tail,
        ]);
        assert_eq!(md, "We set **Bold words** mid sentence.");
    }

    #[test]
    fn hyphenated_italic_word_rejoins_across_lines() {
        let md = page_of(vec![
            sp("In", 20.0, 700.0, 10.0),
            styled(sp("Pro-", 40.0, 700.0, 10.0), false, true),
            styled(sp("ceedings of ACL", 20.0, 686.0, 10.0), false, true),
        ]);
        assert!(md.contains("*Proceedings of ACL*"), "{md}");
    }

    #[test]
    fn rotated_margin_text_leaves_the_flow() {
        let mut stamp = sp("arXiv:1706.03762v7", 5.0, 700.0, 20.0);
        stamp.width = 0.0;
        let md = page_of(vec![
            stamp,
            sp("First line of the abstract text.", 60.0, 700.0, 10.0),
            sp("Second line of the abstract text.", 60.0, 686.0, 10.0),
        ]);
        assert_eq!(
            md,
            "First line of the abstract text. Second line of the abstract text.\n\narXiv:1706.03762v7"
        );
    }

    #[test]
    fn a_larger_or_bold_heading_line_is_its_own_block() {
        let md = page_of(vec![
            sp("References", 20.0, 700.0, 12.0),
            sp("Samir Abdaljalil and others. 2025.", 20.0, 684.0, 10.0),
            sp("Sindex: semantic inconsistency.", 20.0, 672.0, 10.0),
        ]);
        assert!(md.starts_with("### References\n\n"), "{md}");
        let md = page_of(vec![
            styled(
                sp("A.1 Model Hyperparameters", 20.0, 700.0, 10.0),
                true,
                false,
            ),
            sp("For all our experiments, we set n = 20.", 20.0, 686.0, 10.0),
        ]);
        assert!(md.starts_with("## Model Hyperparameters\n\n"), "{md}");
    }

    #[test]
    fn a_blank_line_or_a_long_run_ends_a_table() {
        let mut spans = Vec::new();
        for r in 0..70 {
            let y = 1200.0 - r as f32 * 14.0;
            spans.push(sp(&format!("r{r}"), 20.0, y, 10.0));
            spans.push(sp(&format!("{r}.0"), 200.0, y, 10.0));
        }
        let md = page_of(spans);
        // Tables cap at 64 rows; the rest start a fresh table.
        assert_eq!(md.matches("| --- |").count(), 2, "{md}");
        let md = page_of(vec![
            sp("a", 20.0, 700.0, 10.0),
            sp("1", 200.0, 700.0, 10.0),
            sp("b", 20.0, 686.0, 10.0),
            sp("2", 200.0, 686.0, 10.0),
            sp("c", 20.0, 600.0, 10.0),
            sp("3", 200.0, 600.0, 10.0),
        ]);
        assert!(
            md.contains("| b | 2 |") && !md.contains("| c | 3 |"),
            "{md}"
        );
    }

    #[test]
    fn a_mostly_rotated_page_is_read_as_is() {
        let mut spans = Vec::new();
        for i in 0..3 {
            let mut s = sp(
                &format!("sideways {i}"),
                20.0 + i as f32 * 30.0,
                700.0,
                10.0,
            );
            s.width = 0.0;
            s.space_before = i > 0;
            spans.push(s);
        }
        let md = page_of(spans);
        assert_eq!(md, "sideways 0 sideways 1 sideways 2");
    }

    #[test]
    fn bold_labels_are_not_headings() {
        let md = page_of(vec![styled(
            sp("Cluster ID: 6", 20.0, 700.0, 10.0),
            true,
            false,
        )]);
        assert_eq!(md, "**Cluster ID: 6**");
        let md = page_of(vec![styled(
            sp("{question}", 20.0, 700.0, 10.0),
            true,
            false,
        )]);
        assert_eq!(md, "**{question}**");
    }
}
