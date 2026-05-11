//! Heuristics that turn raw PDF-extracted text into structured Markdown.
//!
//! `pdf-extract` emits a flat string with form feeds between pages and best-
//! effort line breaks. We can't recover font sizes from that stream, so the
//! rules below are deliberately conservative: they target patterns that
//! readers reliably interpret as a heading or list rather than guessing at
//! anything more ambitious.

use regex::Regex;
use std::sync::OnceLock;

/// PDFs delimit pages with the ASCII form feed character.
pub const PAGE_BREAK: char = '\u{000C}';

/// Format a single page of raw text into a Markdown fragment.
pub fn format_page(raw: &str) -> String {
    let lines: Vec<&str> = raw.lines().map(str::trim).collect();
    let blocks = group_into_blocks(&lines);
    blocks
        .into_iter()
        .map(format_block)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Group consecutive non-blank lines into blocks. A run of blank lines
/// separates one block from the next.
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

    // Default: join wrapped lines back into one paragraph.
    block.join(" ")
}

/// Estimate a heading level (1-6) for a standalone line, or `None` if the
/// line doesn't look like a heading.
fn heading_level(line: &str) -> Option<usize> {
    if line.len() > 120 || line.ends_with('.') || line.ends_with(',') {
        return None;
    }

    if let Some(caps) = numbered_heading_regex().captures(line) {
        let dots = caps[1].matches('.').count();
        // "1." → level 1, "1.1" → level 2, "1.1.1" → level 3, ...
        let level = (dots + 1).min(6);
        return Some(level);
    }

    let alpha: String = line.chars().filter(|c| c.is_alphabetic()).collect();
    if !alpha.is_empty()
        && alpha.chars().all(|c| c.is_uppercase())
        && line.split_whitespace().count() <= 12
    {
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

/// Drop a leading "1.2.3" numbering, if present, so the rendered heading
/// stays readable.
fn strip_heading_prefix(line: &str) -> String {
    if let Some(caps) = numbered_heading_regex().captures(line) {
        let prefix_len = caps.get(0).unwrap().as_str().len();
        line[prefix_len..].trim_start().to_string()
    } else {
        line.to_string()
    }
}

fn is_list_item(line: &str) -> bool {
    bullet_regex().is_match(line) || ordered_list_regex().is_match(line)
}

fn format_list_item(line: &str) -> String {
    if let Some(caps) = bullet_regex().captures(line) {
        let rest = line[caps.get(0).unwrap().end()..].trim();
        return format!("- {rest}");
    }
    if let Some(caps) = ordered_list_regex().captures(line) {
        let number = &caps[1];
        let rest = line[caps.get(0).unwrap().end()..].trim();
        return format!("{number}. {rest}");
    }
    line.to_string()
}

fn numbered_heading_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\d+(?:\.\d+)*)\.?\s+").unwrap())
}

fn bullet_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Common bullet glyphs plus ASCII dash/asterisk.
    RE.get_or_init(|| Regex::new(r"^([\u{2022}\u{25E6}\u{25AA}\u{2023}\u{2043}\-*])\s+").unwrap())
}

fn ordered_list_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\d+)[.)]\s+").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn long_lines_are_not_headings() {
        let line = "This is a very long sentence that should clearly remain a paragraph and never be misinterpreted as a heading regardless of capitalization rules.";
        assert!(heading_level(line).is_none());
    }
}
