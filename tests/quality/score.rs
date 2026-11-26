//! Quality metrics for one fixture.
//!
//! Expected Markdown is the ideal document, comments stripped. Metrics are
//! in `0.0..=1.0`. `None` means the fixture does not exercise that metric
//! (no table, no links, …) and the gate skips it.

use std::collections::BTreeMap;

pub const TOLERANCE: f64 = 0.02;

pub struct Scores {
    pub text_similarity: f64,
    pub word_f1: f64,
    pub paragraph_f1: f64,
    pub heading_recall: Option<f64>,
    pub heading_precision: Option<f64>,
    pub heading_level_accuracy: Option<f64>,
    pub list_recall: Option<f64>,
    pub list_depth_accuracy: Option<f64>,
    pub table_cell_accuracy: Option<f64>,
    pub table_span_accuracy: Option<f64>,
    pub code_recall: Option<f64>,
    pub reading_order: f64,
    pub boilerplate_absent: Option<f64>,
    pub link_recall: Option<f64>,
}

pub struct Metric {
    pub name: &'static str,
    pub value: Option<f64>,
}

impl Scores {
    pub fn metric_names() -> [&'static str; 14] {
        [
            "text_similarity",
            "word_f1",
            "paragraph_f1",
            "heading_recall",
            "heading_precision",
            "heading_level_accuracy",
            "list_recall",
            "list_depth_accuracy",
            "table_cell_accuracy",
            "table_span_accuracy",
            "code_recall",
            "reading_order",
            "boilerplate_absent",
            "link_recall",
        ]
    }

    /// Rebuild a score row from values in [`Self::metric_names`] order.
    /// Required metrics (`None` in the array) become `0.0`.
    pub fn from_options(values: [Option<f64>; 14]) -> Self {
        Self {
            text_similarity: values[0].unwrap_or(0.0),
            word_f1: values[1].unwrap_or(0.0),
            paragraph_f1: values[2].unwrap_or(0.0),
            heading_recall: values[3],
            heading_precision: values[4],
            heading_level_accuracy: values[5],
            list_recall: values[6],
            list_depth_accuracy: values[7],
            table_cell_accuracy: values[8],
            table_span_accuracy: values[9],
            code_recall: values[10],
            reading_order: values[11].unwrap_or(0.0),
            boilerplate_absent: values[12],
            link_recall: values[13],
        }
    }

    pub fn metrics(&self) -> [Metric; 14] {
        [
            m("text_similarity", Some(self.text_similarity)),
            m("word_f1", Some(self.word_f1)),
            m("paragraph_f1", Some(self.paragraph_f1)),
            m("heading_recall", self.heading_recall),
            m("heading_precision", self.heading_precision),
            m("heading_level_accuracy", self.heading_level_accuracy),
            m("list_recall", self.list_recall),
            m("list_depth_accuracy", self.list_depth_accuracy),
            m("table_cell_accuracy", self.table_cell_accuracy),
            m("table_span_accuracy", self.table_span_accuracy),
            m("code_recall", self.code_recall),
            m("reading_order", Some(self.reading_order)),
            m("boilerplate_absent", self.boilerplate_absent),
            m("link_recall", self.link_recall),
        ]
    }

    pub fn mean(&self) -> f64 {
        let vals: Vec<f64> = self.metrics().iter().filter_map(|m| m.value).collect();
        if vals.is_empty() {
            0.0
        } else {
            unit(vals.iter().sum::<f64>() / vals.len() as f64)
        }
    }
}

fn m(name: &'static str, value: Option<f64>) -> Metric {
    Metric { name, value }
}

pub struct Prepared {
    pub body: String,
    pub boilerplate: Vec<String>,
}

/// Drop HTML comments. Lines `boilerplate: ...` inside them are the strings
/// that must not survive extraction (running headers, footers, page numbers).
pub fn prepare(expected: &str) -> Prepared {
    let expected = normalize_newlines(expected);
    let mut body = String::new();
    let mut boilerplate = Vec::new();
    let mut rest = expected.as_str();
    while let Some(start) = rest.find("<!--") {
        body.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        let (comment, tail) = match after.find("-->") {
            Some(end) => (&after[..end], &after[end + 3..]),
            None => (after, ""),
        };
        for line in comment.lines() {
            let line = line.trim();
            if let Some(text) = line.strip_prefix("boilerplate:") {
                let text = text.trim();
                if !text.is_empty() {
                    boilerplate.push(text.to_owned());
                }
            }
        }
        rest = tail;
    }
    body.push_str(rest);
    Prepared { body, boilerplate }
}

pub fn score(expected: &str, actual: &str) -> Scores {
    let prepared = prepare(expected);
    score_prepared(&prepared, actual)
}

pub fn score_prepared(prepared: &Prepared, actual: &str) -> Scores {
    let actual = normalize_newlines(actual);
    let expect = view(&prepared.body);
    let got = view(&actual);

    let expect_text = expect.paragraphs.join("\n");
    let got_text = got.paragraphs.join("\n");
    let text_similarity = text_similarity(&expect_text, &got_text);

    let expect_words = tokens(&expect.paragraphs);
    let got_words = tokens(&got.paragraphs);
    let word_overlap = multiset_overlap(&expect_words, &got_words);
    let word_f1 = f1(word_overlap, expect_words.len(), got_words.len());
    let paragraph_overlap = multiset_overlap(&expect.paragraphs, &got.paragraphs);
    let paragraph_f1 = f1(
        paragraph_overlap,
        expect.paragraphs.len(),
        got.paragraphs.len(),
    );

    let (heading_recall, heading_precision, heading_level_accuracy) =
        heading_scores(&expect.headings, &got.headings);
    let (list_recall, list_depth_accuracy) = list_scores(&expect.items, &got.items);
    let (table_cell_accuracy, table_span_accuracy) = table_scores(&expect.tables, &got.tables);
    let code_overlap = multiset_overlap(&expect.codes, &got.codes);
    let code_recall = if expect.codes.is_empty() {
        None
    } else {
        Some(unit(code_overlap as f64 / expect.codes.len() as f64))
    };
    let reading_order = if expect_words.is_empty() {
        1.0
    } else {
        unit(lcs_len(&expect_words, &got_words) as f64 / expect_words.len() as f64)
    };
    let boilerplate_absent = boilerplate_score(&prepared.boilerplate, &actual);
    let link_overlap = link_overlap(&expect.links, &got.links);
    let link_recall = if expect.links.is_empty() {
        None
    } else {
        Some(unit(link_overlap as f64 / expect.links.len() as f64))
    };

    Scores {
        text_similarity: round4(text_similarity),
        word_f1: round4(word_f1),
        paragraph_f1: round4(paragraph_f1),
        heading_recall: round_opt(heading_recall),
        heading_precision: round_opt(heading_precision),
        heading_level_accuracy: round_opt(heading_level_accuracy),
        list_recall: round_opt(list_recall),
        list_depth_accuracy: round_opt(list_depth_accuracy),
        table_cell_accuracy: round_opt(table_cell_accuracy),
        table_span_accuracy: round_opt(table_span_accuracy),
        code_recall: round_opt(code_recall),
        reading_order: round4(reading_order),
        boilerplate_absent: round_opt(boilerplate_absent),
        link_recall: round_opt(link_recall),
    }
}

fn heading_scores(expect: &[Heading], got: &[Heading]) -> (Option<f64>, Option<f64>, Option<f64>) {
    if expect.is_empty() && got.is_empty() {
        return (None, None, None);
    }
    let mut used = vec![false; got.len()];
    let mut hit = 0usize;
    let mut level_hit = 0usize;
    for exp in expect {
        if let Some(i) = got
            .iter()
            .enumerate()
            .position(|(i, g)| !used[i] && g.text == exp.text)
        {
            used[i] = true;
            hit += 1;
            if got[i].level == exp.level {
                level_hit += 1;
            }
        }
    }
    let recall = if expect.is_empty() {
        None
    } else {
        Some(unit(hit as f64 / expect.len() as f64))
    };
    let precision = if got.is_empty() {
        if expect.is_empty() {
            None
        } else {
            Some(1.0)
        }
    } else {
        Some(unit(hit as f64 / got.len() as f64))
    };
    let level = if expect.is_empty() {
        None
    } else if hit == 0 {
        Some(0.0)
    } else {
        Some(unit(level_hit as f64 / hit as f64))
    };
    (recall, precision, level)
}

fn list_scores(expect: &[Item], got: &[Item]) -> (Option<f64>, Option<f64>) {
    if expect.is_empty() {
        return (None, None);
    }
    let mut used = vec![false; got.len()];
    let mut hit = 0usize;
    let mut depth_hit = 0usize;
    for exp in expect {
        if let Some(i) = got
            .iter()
            .enumerate()
            .position(|(i, g)| !used[i] && g.text == exp.text)
        {
            used[i] = true;
            hit += 1;
            if got[i].depth == exp.depth {
                depth_hit += 1;
            }
        }
    }
    let recall = unit(hit as f64 / expect.len() as f64);
    let depth = if hit == 0 {
        0.0
    } else {
        unit(depth_hit as f64 / hit as f64)
    };
    (Some(recall), Some(depth))
}

fn table_scores(expect: &[Table], got: &[Table]) -> (Option<f64>, Option<f64>) {
    let mut cells = 0usize;
    let mut hit = 0usize;
    let mut spanned = 0usize;
    let mut span_hit = 0usize;
    for (i, table) in expect.iter().enumerate() {
        let actual = got.get(i);
        for cell in &table.cells {
            cells += 1;
            let matched = actual.is_some_and(|t| {
                t.cells
                    .iter()
                    .any(|c| c.row == cell.row && c.col == cell.col && c.text == cell.text)
            });
            if matched {
                hit += 1;
            }
            if cell.colspan > 1 || cell.rowspan > 1 {
                spanned += 1;
                let span_ok = actual.is_some_and(|t| {
                    t.cells.iter().any(|c| {
                        c.row == cell.row
                            && c.col == cell.col
                            && c.text == cell.text
                            && c.colspan == cell.colspan
                            && c.rowspan == cell.rowspan
                    })
                });
                if span_ok {
                    span_hit += 1;
                }
            }
        }
    }
    let cell_acc = (cells > 0).then(|| unit(hit as f64 / cells as f64));
    let span_acc = (spanned > 0).then(|| unit(span_hit as f64 / spanned as f64));
    (cell_acc, span_acc)
}

fn boilerplate_score(needles: &[String], actual: &str) -> Option<f64> {
    if needles.is_empty() {
        return None;
    }
    let absent = needles
        .iter()
        .filter(|n| !actual.contains(n.as_str()))
        .count();
    Some(unit(absent as f64 / needles.len() as f64))
}

fn link_overlap(expect: &[Link], got: &[Link]) -> usize {
    let mut used = vec![false; got.len()];
    let mut hit = 0usize;
    for exp in expect {
        if let Some(i) = got
            .iter()
            .enumerate()
            .position(|(i, g)| !used[i] && g.url == exp.url && g.text == exp.text)
        {
            used[i] = true;
            hit += 1;
        }
    }
    hit
}

struct View {
    paragraphs: Vec<String>,
    headings: Vec<Heading>,
    items: Vec<Item>,
    tables: Vec<Table>,
    codes: Vec<String>,
    links: Vec<Link>,
}

struct Heading {
    level: usize,
    text: String,
}

struct Item {
    depth: usize,
    text: String,
}

struct Table {
    cells: Vec<Cell>,
}

struct Cell {
    row: usize,
    col: usize,
    text: String,
    colspan: usize,
    rowspan: usize,
}

struct Link {
    text: String,
    url: String,
}

fn view(md: &str) -> View {
    let lines: Vec<&str> = md.lines().collect();
    let mut paragraphs = Vec::new();
    let mut headings = Vec::new();
    let mut items = Vec::new();
    let mut tables = Vec::new();
    let mut codes = Vec::new();
    let mut links = Vec::new();
    let mut para = String::new();
    let mut i = 0;
    let flush = |para: &mut String, paragraphs: &mut Vec<String>| {
        if !para.is_empty() {
            paragraphs.push(std::mem::take(para));
        }
    };
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            flush(&mut para, &mut paragraphs);
            i += 1;
            let mut code = String::new();
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                if !code.is_empty() {
                    code.push('\n');
                }
                code.push_str(lines[i].trim_end());
                i += 1;
            }
            if i < lines.len() {
                i += 1;
            }
            let code = code.trim_end().to_owned();
            if !code.is_empty() {
                let plain = collapse_ws(&code.replace('\n', " "));
                if !plain.is_empty() {
                    paragraphs.push(plain);
                }
                codes.push(code);
            }
            continue;
        }
        if trimmed.to_ascii_lowercase().starts_with("<table") {
            flush(&mut para, &mut paragraphs);
            let mut buf = String::new();
            while i < lines.len() {
                buf.push_str(lines[i]);
                buf.push('\n');
                let done = lines[i].to_ascii_lowercase().contains("</table>");
                i += 1;
                if done {
                    break;
                }
            }
            let table = parse_html_table(&buf);
            push_table_paragraph(&table, &mut paragraphs);
            tables.push(table);
            continue;
        }
        if trimmed.starts_with('|') {
            flush(&mut para, &mut paragraphs);
            let mut rows = Vec::new();
            while i < lines.len() && lines[i].contains('|') {
                rows.push(lines[i]);
                i += 1;
            }
            let table = parse_gfm(&rows);
            push_table_paragraph(&table, &mut paragraphs);
            tables.push(table);
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut para, &mut paragraphs);
            i += 1;
            continue;
        }
        if let Some(heading) = parse_heading(trimmed) {
            headings.push(heading);
        }
        if let Some(item) = parse_list_item(line) {
            items.push(item);
        }
        links.extend(parse_links(line));
        let plain = line_to_plain(line);
        if !plain.is_empty() {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(&plain);
        }
        i += 1;
    }
    flush(&mut para, &mut paragraphs);
    View {
        paragraphs,
        headings,
        items,
        tables,
        codes,
        links,
    }
}

fn push_table_paragraph(table: &Table, paragraphs: &mut Vec<String>) {
    let mut text = String::new();
    for cell in &table.cells {
        if cell.text.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(&cell.text);
    }
    if !text.is_empty() {
        paragraphs.push(text);
    }
}

fn parse_heading(trimmed: &str) -> Option<Heading> {
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = trimmed[hashes..].strip_prefix(char::is_whitespace)?;
    let text = clean_label(rest);
    if text.is_empty() {
        None
    } else {
        Some(Heading {
            level: hashes,
            text,
        })
    }
}

fn parse_list_item(line: &str) -> Option<Item> {
    let mut depth_spaces = 0usize;
    let mut rest = line;
    loop {
        if let Some(tail) = rest.strip_prefix(' ') {
            depth_spaces += 1;
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix('\t') {
            depth_spaces += 2;
            rest = tail;
        } else {
            break;
        }
    }
    let bytes = rest.as_bytes();
    let marker_len = if matches!(bytes.first(), Some(b'-' | b'*' | b'+')) {
        1
    } else {
        let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 || !matches!(bytes.get(digits), Some(b'.' | b')')) {
            return None;
        }
        digits + 1
    };
    let after = rest.get(marker_len..)?;
    let after = after.strip_prefix(char::is_whitespace)?;
    let text = clean_label(after);
    if text.is_empty() {
        return None;
    }
    Some(Item {
        depth: depth_spaces / 2,
        text,
    })
}

fn line_to_plain(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() || is_gfm_sep(trimmed) {
        return String::new();
    }
    let without_heading = strip_heading(trimmed);
    let without_list = strip_list(without_heading);
    let source = if without_list.trim_start().starts_with('|') {
        gfm_cells_text(without_list)
    } else {
        without_list.to_owned()
    };
    clean_label(&strip_tags(&replace_md_links(&source)))
}

fn strip_heading(trimmed: &str) -> &str {
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) {
        if let Some(rest) = trimmed[hashes..].strip_prefix(char::is_whitespace) {
            return rest;
        }
    }
    trimmed
}

fn strip_list(line: &str) -> &str {
    let rest = line.trim_start();
    let bytes = rest.as_bytes();
    let marker_len = if matches!(bytes.first(), Some(b'-' | b'*' | b'+')) {
        1
    } else {
        let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 || !matches!(bytes.get(digits), Some(b'.' | b')')) {
            return line.trim();
        }
        digits + 1
    };
    match rest
        .get(marker_len..)
        .and_then(|s| s.strip_prefix(char::is_whitespace))
    {
        Some(after) => after,
        None => line.trim(),
    }
}

fn is_gfm_sep(line: &str) -> bool {
    let inner = line.trim().trim_matches('|');
    let mut any = false;
    for cell in inner.split('|') {
        let cell = cell.trim().trim_matches(':');
        if cell.is_empty() || !cell.bytes().all(|b| b == b'-') {
            return false;
        }
        any = true;
    }
    any
}

fn gfm_cells_text(line: &str) -> String {
    parse_gfm(&[line])
        .cells
        .into_iter()
        .filter(|c| !c.text.is_empty())
        .map(|c| c.text)
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_gfm(lines: &[&str]) -> Table {
    let mut cells = Vec::new();
    let mut row = 0usize;
    for line in lines {
        if is_gfm_sep(line) {
            continue;
        }
        let inner = line.trim().trim_start_matches('|').trim_end_matches('|');
        for (col, cell) in inner.split('|').enumerate() {
            cells.push(Cell {
                row,
                col,
                text: clean_label(cell),
                colspan: 1,
                rowspan: 1,
            });
        }
        row += 1;
    }
    Table { cells }
}

fn parse_html_table(block: &str) -> Table {
    let mut raw_rows = Vec::new();
    let lower_block = block.to_ascii_lowercase();
    let mut search_from = 0usize;
    while let Some(rel) = lower_block[search_from..].find("<tr") {
        let start = search_from + rel;
        let after = &block[start..];
        let lower_after = after.to_ascii_lowercase();
        let end = lower_after.find("</tr>").map_or(after.len(), |n| n);
        raw_rows.push(&after[..end]);
        search_from = start + end;
    }
    let mut grid = vec![vec![false; 48]; 64];
    let mut cells = Vec::new();
    for (r, row) in raw_rows.iter().enumerate() {
        if r >= grid.len() {
            break;
        }
        let mut c = 0usize;
        for (text, colspan, rowspan) in html_row_cells(row) {
            while c < grid[r].len() && grid[r][c] {
                c += 1;
            }
            if c >= grid[r].len() {
                break;
            }
            let colspan = colspan.max(1);
            let rowspan = rowspan.max(1);
            cells.push(Cell {
                row: r,
                col: c,
                text,
                colspan,
                rowspan,
            });
            for rr in r..(r + rowspan).min(grid.len()) {
                for cc in c..(c + colspan).min(grid[rr].len()) {
                    grid[rr][cc] = true;
                }
            }
            c += colspan;
        }
    }
    Table { cells }
}

fn html_row_cells(row: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let lower = row.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut i = 0usize;
    while i + 3 < bytes.len() {
        let name = if bytes[i] == b'<' {
            if bytes[i + 1..].starts_with(b"td") {
                Some("td")
            } else if bytes[i + 1..].starts_with(b"th") {
                Some("th")
            } else {
                None
            }
        } else {
            None
        };
        let Some(_) = name else {
            i += 1;
            continue;
        };
        let Some(tag_end) = lower[i..].find('>') else {
            break;
        };
        let attrs = &row[i + 1..i + tag_end];
        let content_start = i + tag_end + 1;
        let after = &lower[content_start..];
        let close = ["</td", "</th"]
            .iter()
            .filter_map(|tag| after.find(tag))
            .min()
            .unwrap_or(after.len());
        let text = clean_label(&strip_tags(&row[content_start..content_start + close]));
        out.push((
            text,
            attr_usize(attrs, "colspan"),
            attr_usize(attrs, "rowspan"),
        ));
        i = content_start + close;
    }
    out
}

fn is_quote(c: char) -> bool {
    c == '"' || c == '\''
}

fn is_tag_edge(c: char) -> bool {
    c == '>' || c == '/'
}

/// Strip `a` and `b` from both ends. A two-char array is not a
/// double-ended searcher on 1.70, so this walks with `strip_prefix`.
fn trim_set(s: &str, a: char, b: char) -> &str {
    let mut out = s;
    while let Some(rest) = out.strip_prefix(a).or_else(|| out.strip_prefix(b)) {
        out = rest;
    }
    while let Some(rest) = out.strip_suffix(a).or_else(|| out.strip_suffix(b)) {
        out = rest;
    }
    out
}

fn attr_usize(attrs: &str, name: &str) -> usize {
    let lower = attrs.to_ascii_lowercase();
    let Some(idx) = lower.find(name) else {
        return 1;
    };
    let after = lower[idx + name.len()..].trim_start();
    let after = after.strip_prefix('=').unwrap_or(after).trim_start();
    let after = after.trim_start_matches(is_quote);
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(1).max(1)
}

fn parse_links(line: &str) -> Vec<Link> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'[' && (i == 0 || bytes[i - 1] != b'!') {
            if let Some((text, url, consumed)) = md_link(&line[i..]) {
                let text = clean_label(&text);
                if !text.is_empty() && !url.is_empty() {
                    out.push(Link { text, url });
                }
                i += consumed;
                continue;
            }
        }
        i += 1;
    }
    let lower = line.to_ascii_lowercase();
    let mut search = 0usize;
    while let Some(rel) = lower[search..].find("<a ") {
        let start = search + rel;
        let Some(tag_end) = lower[start..].find('>') else {
            break;
        };
        let attrs = &line[start..start + tag_end];
        let content_start = start + tag_end + 1;
        let Some(close) = lower[content_start..].find("</a>") else {
            break;
        };
        let text = clean_label(&strip_tags(&line[content_start..content_start + close]));
        if let Some(url) = attr_value(attrs, "href") {
            if !text.is_empty() && !url.is_empty() {
                out.push(Link { text, url });
            }
        }
        search = content_start + close;
    }
    out
}

fn md_link(s: &str) -> Option<(String, String, usize)> {
    if !s.starts_with('[') {
        return None;
    }
    let close = s.find("](")?;
    let text = s[1..close].to_owned();
    let url_start = close + 2;
    let url_end = s[url_start..].find(')')? + url_start;
    let url = trim_set(s[url_start..url_end].trim(), '<', '>').to_owned();
    Some((text, url, url_end + 1))
}

fn attr_value(attrs: &str, name: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let idx = lower.find(name)?;
    let after = attrs[idx + name.len()..].trim_start();
    let after = after.strip_prefix('=')?.trim_start();
    let quote = after.chars().next()?;
    if quote == '"' || quote == '\'' {
        let rest = &after[quote.len_utf8()..];
        let end = rest.find(quote)?;
        Some(rest[..end].trim().to_owned())
    } else {
        Some(
            after
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(is_tag_edge)
                .to_owned(),
        )
    }
}

fn replace_md_links(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('[') {
        if start > 0 && rest.as_bytes()[start - 1] == b'!' {
            out.push_str(&rest[..=start]);
            rest = &rest[start + 1..];
            continue;
        }
        if let Some((text, _, consumed)) = md_link(&rest[start..]) {
            out.push_str(&rest[..start]);
            out.push_str(&text);
            rest = &rest[start + consumed..];
        } else {
            out.push_str(&rest[..=start]);
            rest = &rest[start + 1..];
        }
    }
    out.push_str(rest);
    out
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            for d in chars.by_ref() {
                if d == '>' {
                    break;
                }
            }
            if out.ends_with(|ch: char| !ch.is_whitespace()) {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn clean_label(s: &str) -> String {
    let stripped: String = s.chars().filter(|c| *c != '*' && *c != '`').collect();
    collapse_ws(stripped.trim())
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    out
}

fn tokens(paragraphs: &[String]) -> Vec<String> {
    paragraphs
        .iter()
        .flat_map(|p| p.split_whitespace())
        .map(trim_token)
        .filter(|t| !t.is_empty())
        .collect()
}

fn trim_token(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '\u{2019}')
        .to_owned()
}

fn text_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let a_len = a.chars().count();
    let b_len = b.chars().count();
    let denom = a_len.max(b_len);
    if denom == 0 {
        return 1.0;
    }
    let dist = edit_distance(a, b);
    unit(1.0 - dist as f64 / denom as f64)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn f1(overlap: usize, expected: usize, actual: usize) -> f64 {
    if expected == 0 && actual == 0 {
        return 1.0;
    }
    let precision = if actual == 0 {
        0.0
    } else {
        overlap as f64 / actual as f64
    };
    let recall = if expected == 0 {
        0.0
    } else {
        overlap as f64 / expected as f64
    };
    if precision + recall == 0.0 {
        0.0
    } else {
        unit(2.0 * precision * recall / (precision + recall))
    }
}

fn multiset_overlap(expected: &[String], actual: &[String]) -> usize {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for item in actual {
        *counts.entry(item.as_str()).or_insert(0) += 1;
    }
    let mut n = 0usize;
    for item in expected {
        if let Some(c) = counts.get_mut(item.as_str()) {
            if *c > 0 {
                *c -= 1;
                n += 1;
            }
        }
    }
    n
}

fn lcs_len(a: &[String], b: &[String]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    for ea in a {
        for (j, eb) in b.iter().enumerate() {
            cur[j + 1] = if ea == eb {
                prev[j] + 1
            } else {
                prev[j + 1].max(cur[j])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

fn unit(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

fn round4(v: f64) -> f64 {
    (unit(v) * 10_000.0).round() / 10_000.0
}

fn round_opt(v: Option<f64>) -> Option<f64> {
    v.map(round4)
}

/// Line diff of the comment-free expected Markdown against converter output.
/// Capped so the CI summary stays readable.
pub fn unified_diff(expected_body: &str, actual: &str) -> String {
    let actual = normalize_newlines(actual);
    let expected_body = normalize_newlines(expected_body);
    if expected_body.trim_end() == actual.trim_end() {
        return "identical\n".to_owned();
    }
    let a: Vec<&str> = expected_body.lines().collect();
    let b: Vec<&str> = actual.lines().collect();
    if a.len().saturating_mul(b.len()) > 160_000 {
        return format!(
            "(diff omitted: {} expected lines, {} actual lines)\n",
            a.len(),
            b.len()
        );
    }
    let cols = b.len() + 1;
    let mut dp = vec![0u32; (a.len() + 1) * cols];
    let at = |i: usize, j: usize| i * cols + j;
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            dp[at(i, j)] = if a[i] == b[j] {
                dp[at(i + 1, j + 1)] + 1
            } else {
                dp[at(i + 1, j)].max(dp[at(i, j + 1)])
            };
        }
    }
    let mut out = String::new();
    let mut i = 0usize;
    let mut j = 0usize;
    let mut shown = 0usize;
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
        } else if dp[at(i + 1, j)] >= dp[at(i, j + 1)] {
            push_diff(&mut out, &mut shown, &format!("- {}", a[i]));
            i += 1;
        } else {
            push_diff(&mut out, &mut shown, &format!("+ {}", b[j]));
            j += 1;
        }
        if shown > 80 {
            out.push_str("...\n");
            return out;
        }
    }
    while i < a.len() {
        push_diff(&mut out, &mut shown, &format!("- {}", a[i]));
        i += 1;
        if shown > 80 {
            out.push_str("...\n");
            return out;
        }
    }
    while j < b.len() {
        push_diff(&mut out, &mut shown, &format!("+ {}", b[j]));
        j += 1;
        if shown > 80 {
            out.push_str("...\n");
            return out;
        }
    }
    if out.is_empty() {
        out.push_str("(whitespace-only difference)\n");
    }
    out
}

fn push_diff(out: &mut String, shown: &mut usize, line: &str) {
    out.push_str(line);
    out.push('\n');
    *shown += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_markdown_scores_one() {
        let md = "# Title\n\nA **bold** word.\n\n- Apple\n  - Pear\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n";
        let scores = score(md, md);
        assert_eq!(scores.text_similarity, 1.0);
        assert_eq!(scores.word_f1, 1.0);
        assert_eq!(scores.paragraph_f1, 1.0);
        assert_eq!(scores.heading_recall, Some(1.0));
        assert_eq!(scores.heading_precision, Some(1.0));
        assert_eq!(scores.heading_level_accuracy, Some(1.0));
        assert_eq!(scores.list_recall, Some(1.0));
        assert_eq!(scores.list_depth_accuracy, Some(1.0));
        assert_eq!(scores.table_cell_accuracy, Some(1.0));
        assert_eq!(scores.reading_order, 1.0);
        assert!(scores.link_recall.is_none());
        assert!(scores.boilerplate_absent.is_none());
    }

    #[test]
    fn swapped_tokens_keep_word_f1_and_lose_order() {
        let scores = score("alpha bravo", "bravo alpha");
        assert_eq!(scores.word_f1, 1.0);
        assert!(scores.reading_order < 1.0, "{}", scores.reading_order);
    }

    #[test]
    fn edit_distance_matches_kitten() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn boilerplate_and_links_and_spans() {
        let expected = "<!--\nboilerplate: SECRET\n-->\nSee the [note](https://example.com/n).\n\n<table><tr><td colspan=\"2\">Item</td><td>Qty</td></tr></table>\n";
        let actual = "See the note.\n\n| Item |  | Qty |\n| --- | --- | --- |\n\nSECRET\n";
        let scores = score(expected, actual);
        assert_eq!(scores.link_recall, Some(0.0));
        assert_eq!(scores.boilerplate_absent, Some(0.0));
        assert_eq!(scores.table_span_accuracy, Some(0.0));
        // The header text still sits in the starting column; the colspan does not.
        assert_eq!(scores.table_cell_accuracy, Some(1.0));
    }

    #[test]
    fn flat_list_keeps_recall_and_drops_depth() {
        let scores = score("- Fruit\n  - Apple\n", "- Fruit\n- Apple\n");
        assert_eq!(scores.list_recall, Some(1.0));
        assert!(
            scores.list_depth_accuracy.unwrap() < 1.0,
            "{:?}",
            scores.list_depth_accuracy
        );
    }

    #[test]
    fn missing_fence_drops_code_recall() {
        let scores = score("```\nfn a() {}\n```\n", "fn a() {}\n");
        assert_eq!(scores.code_recall, Some(0.0));
        assert_eq!(scores.word_f1, 1.0);
    }
}
