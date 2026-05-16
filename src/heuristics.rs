//! Heuristics that turn raw PDF-extracted text into structured Markdown.
//!
//! The extractor emits a flat string with form feeds between pages and best-
//! effort line breaks. We can't recover font sizes from that stream, so the
//! rules below are deliberately conservative: they target patterns that
//! readers reliably interpret as a heading or list rather than guessing at
//! anything more ambitious.

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

    if let Some(numbering) = match_numbered_heading(line) {
        let dots = numbering.capture.matches('.').count();
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

// ---- Hand-rolled matchers --------------------------------------------------
//
// The original implementation used three small regexes. They were cheap, but
// dragging the entire `regex` crate in for a handful of single-pass patterns
// was disproportionate. Each helper returns the captured slice (when needed)
// plus the byte length of the full match, mirroring what `Regex::captures`
// would have given us.

struct Match<'a> {
    capture: &'a str,
    match_len: usize,
}

/// Match `^(\d+(?:\.\d+)*)\.?\s+`: a dotted numbering with an optional
/// trailing period and at least one whitespace character.
fn match_numbered_heading(line: &str) -> Option<Match<'_>> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    // Optional `.digit+` repetitions: greedy, like the regex.
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

/// Match `^([\u{2022}\u{25E6}\u{25AA}\u{2023}\u{2043}\-*])\s+`. ASCII bullet
/// chars are a single byte; the Unicode bullets are multi-byte but each is a
/// single `char`, so we peek the first one and bail otherwise.
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

/// Match `^(\d+)[.)]\s+`.
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

/// Consume Unicode whitespace from the start of `s`. Matches the behaviour
/// of regex `\s+` in default (Unicode) mode.
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
