//! Line-level Markdown shapes.
//!
//! Everything here works on a single already-joined line of text: is it a
//! heading, and at what level; is it a list item, and how does it render.
//! Nothing in this module knows about spans, columns, or page geometry.

pub(super) fn named_section(line: &str) -> Option<usize> {
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

/// A heading never runs long or trails off in sentence punctuation.
fn heading_shaped(line: &str) -> bool {
    line.len() <= 120 && !line.ends_with('.') && !line.ends_with(',')
}

pub(super) fn is_all_caps_heading(line: &str) -> bool {
    if !heading_shaped(line) {
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

/// Estimate a heading level (1-6) for a standalone line, or `None` if the
/// line doesn't look like a heading.
pub(super) fn heading_level(line: &str) -> Option<usize> {
    if !heading_shaped(line) {
        return None;
    }
    if let Some(numbering) = match_numbered_heading(line) {
        return Some((numbering.capture.matches('.').count() + 1).min(6));
    }
    if is_all_caps_heading(line) {
        return Some(2);
    }
    if short_title_case(line)
        && line.chars().next().is_some_and(|c| c.is_uppercase())
        && !line.contains(';')
    {
        return Some(3);
    }
    None
}

/// Short enough, and few enough words, to read as a heading rather than prose.
pub(super) fn short_title_case(line: &str) -> bool {
    heading_shaped(line) && line.len() <= 80 && line.split_whitespace().count() <= 10
}

pub(super) fn strip_heading_prefix(line: &str) -> String {
    if let Some(m) = match_numbered_heading(line) {
        line[m.match_len..].trim_start().to_string()
    } else {
        line.to_string()
    }
}

pub(super) fn is_list_item(line: &str) -> bool {
    match_bullet(line).is_some() || match_ordered_list(line).is_some()
}

pub(super) fn format_list_item(line: &str) -> String {
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

/// Length of the leading run of ASCII digits.
fn digit_run(b: &[u8]) -> usize {
    b.iter().take_while(|c| c.is_ascii_digit()).count()
}

/// Index past the mandatory whitespace at `i`, or `None` if there is none —
/// `1.Introduction` is prose, `1. Introduction` is a heading.
fn after_separator(line: &str, i: usize) -> Option<usize> {
    let ws = skip_whitespace(line.get(i..)?);
    (ws > 0).then_some(i + ws)
}

/// Does the line open with a section number (`2.` / `3.1 `)?
pub(super) fn is_numbered_heading(line: &str) -> bool {
    match_numbered_heading(line).is_some()
}

fn match_numbered_heading(line: &str) -> Option<Match<'_>> {
    let b = line.as_bytes();
    let mut i = digit_run(b);
    if i == 0 {
        return None;
    }
    // Dotted sub-numbering: `1.2.3`.
    while i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
        i += 1 + digit_run(&b[i + 1..]);
    }
    let capture_end = i;
    if b.get(i) == Some(&b'.') {
        i += 1;
    }
    Some(Match {
        capture: &line[..capture_end],
        match_len: after_separator(line, i)?,
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
    let capture_end = digit_run(b);
    if capture_end == 0 || !matches!(b.get(capture_end), Some(b'.' | b')')) {
        return None;
    }
    Some(Match {
        capture: &line[..capture_end],
        match_len: after_separator(line, capture_end + 1)?,
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
}
