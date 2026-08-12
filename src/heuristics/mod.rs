//! Heuristics that turn extracted PDF content into structured Markdown.
//!
//! `format_page` keeps the original string-only path (used by unit tests).
//! `format_pages` consumes positioned spans so convert can recover columns,
//! font-size headings, tables, and running headers.

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::extract::layout::{PageLayout, Span, SpanKind};
use crate::extract::structure::{Role, RoleMap};

mod tables;

const IMAGE_MARK: char = '\u{0001}';

/// Format a single page of raw text into a Markdown fragment.
#[cfg(test)]
pub fn format_page(raw: &str) -> String {
    format_page_layout(&layout_from_raw(raw), 0, &HashMap::new(), &[], &[])
}

/// Format each page from positioned spans. Empty pages stay empty strings.
pub fn format_pages(pages: &[PageLayout], roles: &RoleMap) -> Vec<String> {
    let headers = running_margin(pages, true);
    let footers = running_margin(pages, false);
    pages
        .iter()
        .enumerate()
        .map(|(i, page)| format_page_layout(page, i, roles, &headers, &footers))
        .collect()
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

    let gaps = column_gaps(&spans);
    let cols: Vec<usize> = spans
        .iter()
        .map(|s| gaps.iter().take_while(|&&g| s.x >= g).count())
        .collect();
    let n_cols = cols.iter().copied().max().unwrap_or(0) + 1;
    let median = median_size(&spans);

    let mut parts = Vec::new();
    for col in 0..n_cols {
        let col_spans: Vec<&Span> = spans
            .iter()
            .zip(cols.iter())
            .filter_map(|(s, c)| (*c == col).then_some(*s))
            .collect();
        if col_spans.is_empty() {
            continue;
        }
        let md = format_column(&col_spans, &page.rects, page_idx, roles, median);
        if !md.is_empty() {
            parts.push(md);
        }
    }
    parts.join("\n\n")
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
                if rest.iter().any(|s| s.y > bbox[3]) {
                    return [rest_md, table]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                }
                return [table, rest_md]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
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
        if let Some((n, table)) = tables::borderless_run(&lines[i..]) {
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
            if tables::is_table_prefix(&lines[i..]) {
                break;
            }
            i += 1;
        }
        let block = format_line_block(&lines[start..i], page_idx, roles, median);
        if !block.is_empty() {
            out.push(block);
        }
    }
    out.join("\n\n")
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
        return style_line(&lines[0]);
    }
    join_paragraph(lines)
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
    if median > 0.1 && (size >= median * 1.15 || bold) {
        if let Some(level) = heading_level(line) {
            return Some(level);
        }
        if bold
            && line.len() <= 80
            && line.split_whitespace().count() <= 10
            && !line.ends_with('.')
            && !line.ends_with(',')
        {
            return Some(3);
        }
    }
    if match_numbered_heading(line).is_some() {
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

fn named_section(line: &str) -> Option<usize> {
    // Title-case only — all-caps names still go through `is_all_caps_heading`.
    match line.trim() {
        "Abstract" => Some(3),
        "Introduction" => Some(1),
        "References" | "Bibliography" => Some(2),
        "Conclusion" | "Conclusions" | "Acknowledgements" | "Acknowledgments" => Some(2),
        "Related Work" | "Related Works" => Some(2),
        _ => None,
    }
}

fn is_all_caps_heading(line: &str) -> bool {
    if line.len() > 120 || line.ends_with('.') || line.ends_with(',') {
        return false;
    }
    let mut n_alpha = 0usize;
    let mut words = 0usize;
    let mut in_word = false;
    for c in line.chars() {
        if c.is_alphabetic() {
            if !c.is_uppercase() {
                return false;
            }
            n_alpha += 1;
        }
        if c.is_whitespace() {
            in_word = false;
        } else if !in_word {
            in_word = true;
            words += 1;
            if words > 12 {
                return false;
            }
        }
    }
    n_alpha > 0
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
        if out.ends_with('-') {
            let next = t.chars().next();
            if next.map(|c| c.is_lowercase()).unwrap_or(false) {
                out.pop();
                out.push_str(&t);
                continue;
            }
        }
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(&t);
    }
    out
}

fn style_line(line: &VLine<'_>) -> String {
    let mut out = String::new();
    for s in &line.spans {
        if s.kind == SpanKind::Image {
            out.push(IMAGE_MARK);
            out.push_str(&s.text);
            out.push(IMAGE_MARK);
            continue;
        }
        if s.space_before && !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        push_styled(&mut out, s);
    }
    out
}

fn push_styled(out: &mut String, s: &Span) {
    let t = s.text.as_str();
    if t.trim().is_empty() {
        out.push_str(t);
        return;
    }
    if s.bold && s.italic {
        out.push_str("***");
        out.push_str(t);
        out.push_str("***");
    } else if s.bold {
        out.push_str("**");
        out.push_str(t);
        out.push_str("**");
    } else if s.italic {
        out.push('*');
        out.push_str(t);
        out.push('*');
    } else {
        out.push_str(t);
    }
}

fn image_block(lines: &[VLine<'_>]) -> String {
    let mut out = String::new();
    for line in lines {
        for s in &line.spans {
            if s.kind == SpanKind::Image {
                out.push(IMAGE_MARK);
                out.push_str(&s.text);
                out.push(IMAGE_MARK);
            }
        }
    }
    out
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

fn column_gaps(spans: &[&Span]) -> Vec<f32> {
    let mut min = f32::MAX;
    let mut max = f32::MIN;
    let mut n = 0usize;
    for s in spans {
        if s.kind != SpanKind::Text {
            continue;
        }
        n += 1;
        min = min.min(s.x);
        max = max.max(s.x);
    }
    if n < 8 || max - min < 180.0 {
        return Vec::new();
    }
    const B: usize = 24;
    let mut hist = [0u32; B];
    let width = (max - min).max(1.0);
    for s in spans {
        if s.kind != SpanKind::Text {
            continue;
        }
        let i = (((s.x - min) / width) * B as f32) as usize;
        hist[i.min(B - 1)] += 1;
    }
    let total = n as u32;
    let empty = (total / 40).max(2);
    let mut gaps = Vec::new();
    let mut i = 1usize;
    while i + 1 < B {
        if hist[i] > empty {
            i += 1;
            continue;
        }
        let start = i;
        while i + 1 < B && hist[i] <= empty {
            i += 1;
        }
        let len = i - start;
        if len < 2 {
            continue;
        }
        let left: u32 = hist[..start].iter().sum();
        let right: u32 = hist[i..].iter().sum();
        if left * 5 >= total && right * 5 >= total {
            let mid_bucket = start + len / 2;
            gaps.push(min + width * (mid_bucket as f32 + 0.5) / B as f32);
        }
    }
    gaps.truncate(2);
    gaps
}

fn running_margin(pages: &[PageLayout], header: bool) -> Vec<String> {
    if pages.len() < 3 {
        return Vec::new();
    }
    let mut freq: HashMap<String, usize> = HashMap::new();
    for page in pages {
        let (first, last) = first_last(page);
        let line = if header { first } else { last };
        if let Some(t) = line {
            if !t.is_empty() && t.len() < 80 {
                *freq.entry(t).or_insert(0) += 1;
            }
        }
    }
    freq.into_iter()
        .filter(|(_, n)| *n >= 3)
        .map(|(t, _)| t)
        .collect()
}

fn first_last(page: &PageLayout) -> (Option<String>, Option<String>) {
    let refs: Vec<&Span> = page
        .spans
        .iter()
        .filter(|s| s.kind == SpanKind::Text)
        .collect();
    if refs.is_empty() {
        return (None, None);
    }
    let cols = vec![0usize; refs.len()];
    let lines = visual_lines(&refs, &cols);
    (lines.first().map(plain_line), lines.last().map(plain_line))
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
    for i in order {
        let s = spans[i];
        let col = cols[i];
        let thresh = (s.font_size * 0.45).max(2.0);
        if let Some(last) = lines.last_mut() {
            if last.col == col && (last.y - s.y).abs() < thresh {
                last.spans.push(s);
                continue;
            }
        }
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
        if s.kind == SpanKind::Image {
            continue;
        }
        if s.space_before && !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(&s.text);
    }
    let trimmed = out.trim();
    if trimmed.len() == out.len() {
        out
    } else {
        trimmed.to_string()
    }
}

/// Group consecutive non-blank lines into blocks. A run of blank lines
/// separates one block from the next.
#[cfg(test)]
fn group_into_blocks<'a>(lines: &[&'a str]) -> Vec<Vec<&'a str>> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();

    for line in lines {
        if line.is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
        } else {
            current.push(*line);
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

/// Decide what kind of Markdown element a block represents and render it.
#[cfg(test)]
fn format_block(block: Vec<&str>) -> String {
    if block.is_empty() {
        return String::new();
    }

    if block.iter().all(|line| is_list_item(line)) {
        return block
            .iter()
            .map(|line| format_list_item(line))
            .collect::<Vec<_>>()
            .join("\n");
    }

    if block.len() == 1 {
        let line = block[0];
        if let Some(level) = heading_level(line) {
            return format!("{} {}", "#".repeat(level), strip_heading_prefix(line));
        }
    }

    block.join(" ")
}

/// Estimate a heading level (1-6) for a standalone line, or `None` if the
/// line doesn't look like a heading.
fn heading_level(line: &str) -> Option<usize> {
    if line.len() > 120 || line.ends_with('.') || line.ends_with(',') {
        return None;
    }

    if let Some(numbering) = match_numbered_heading(line) {
        let dots = numbering.capture.matches('.').count();
        let level = (dots + 1).min(6);
        return Some(level);
    }

    if is_all_caps_heading(line) {
        return Some(2);
    }

    if line.len() <= 80
        && line.split_whitespace().count() <= 10
        && line.chars().next().is_some_and(|c| c.is_uppercase())
        && !line.contains(';')
    {
        return Some(3);
    }

    None
}

fn strip_heading_prefix(line: &str) -> String {
    if let Some(m) = match_numbered_heading(line) {
        line[m.match_len..].trim_start().to_string()
    } else {
        line.to_string()
    }
}

fn is_list_item(line: &str) -> bool {
    match_bullet(line).is_some() || match_ordered_list(line).is_some()
}

fn format_list_item(line: &str) -> String {
    if let Some(len) = match_bullet(line) {
        let rest = line[len..].trim();
        return format!("- {rest}");
    }
    if let Some(m) = match_ordered_list(line) {
        let rest = line[m.match_len..].trim();
        return format!("{}. {rest}", m.capture);
    }
    line.to_string()
}

struct Match<'a> {
    capture: &'a str,
    match_len: usize,
}

fn match_numbered_heading(line: &str) -> Option<Match<'_>> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    loop {
        if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        } else {
            break;
        }
    }
    let capture_end = i;
    if i < b.len() && b[i] == b'.' {
        i += 1;
    }
    let ws_start = i;
    i += skip_whitespace(&line[i..]);
    if i == ws_start {
        return None;
    }
    Some(Match {
        capture: &line[..capture_end],
        match_len: i,
    })
}

fn match_bullet(line: &str) -> Option<usize> {
    let mut chars = line.chars();
    let first = chars.next()?;
    let bullet_len = match first {
        '-' | '*' | '\u{2022}' | '\u{25E6}' | '\u{25AA}' | '\u{2023}' | '\u{2043}' => {
            first.len_utf8()
        }
        _ => return None,
    };
    let rest = &line[bullet_len..];
    let ws = skip_whitespace(rest);
    if ws == 0 {
        return None;
    }
    Some(bullet_len + ws)
}

fn match_ordered_list(line: &str) -> Option<Match<'_>> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let capture_end = i;
    if i >= b.len() || (b[i] != b'.' && b[i] != b')') {
        return None;
    }
    i += 1;
    let ws_start = i;
    i += skip_whitespace(&line[i..]);
    if i == ws_start {
        return None;
    }
    Some(Match {
        capture: &line[..capture_end],
        match_len: i,
    })
}

fn skip_whitespace(s: &str) -> usize {
    let mut n = 0;
    for c in s.chars() {
        if c.is_whitespace() {
            n += c.len_utf8();
        } else {
            break;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn long_lines_are_not_headings() {
        let line = "This is a very long sentence that should clearly remain a paragraph and never be misinterpreted as a heading regardless of capitalization rules.";
        assert!(heading_level(line).is_none());
    }

    #[test]
    fn match_numbered_heading_requires_digit_prefix() {
        assert!(match_numbered_heading("Intro 1").is_none());
        assert!(match_numbered_heading("1.").is_none());
    }

    #[test]
    fn match_bullet_rejects_non_bullet_and_missing_space() {
        assert!(match_bullet("").is_none());
        assert!(match_bullet("alpha").is_none());
        assert!(match_bullet("-foo").is_none());
    }

    #[test]
    fn match_ordered_list_rejects_non_digit_prefix() {
        assert!(match_ordered_list("alpha. one").is_none());
        assert!(match_ordered_list("12 alpha").is_none());
        assert!(match_ordered_list("12.alpha").is_none());
    }

    #[test]
    fn format_list_item_returns_input_for_non_matching_lines() {
        assert_eq!(format_list_item("nope"), "nope");
    }

    #[test]
    fn empty_blocks_produce_no_markdown() {
        assert_eq!(format_page("\n\n"), "");
    }

    #[test]
    fn format_block_returns_empty_string_for_empty_block() {
        assert_eq!(format_block(Vec::new()), "");
    }

    #[test]
    fn group_into_blocks_splits_on_blank_lines() {
        let lines = ["a", "", "b"];
        assert_eq!(group_into_blocks(&lines), vec![vec!["a"], vec!["b"]]);
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
}
