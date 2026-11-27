//! Zero-dependency quality and accuracy evaluator for `pdfmd`.
//!
//! Computes structural and text-fidelity metrics for PDF -> Markdown
//! conversion without external crates:
//! - NID (Normalized Indel Similarity / reading-order fidelity)
//! - TEDS & TEDS-Struct (Tree-Edit-Distance-based Similarity for GFM tables)
//! - Cell-level table Precision / Recall / F1 and false-positive table count
//! - MHS (Markdown Heading Similarity) and Heading F1
//! - List and code-block F1
//! - Structural audit diagnostics (unbalanced tables, prose-in-table traps,
//!   raw unconverted bullets, replacement/control characters)

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use pdfmd::{convert_pdf_to_markdown, ConvertOptions};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        print_usage();
        return;
    }
    let code = match args[0].as_str() {
        "score" => cmd_score(&args[1..]),
        "eval" => cmd_eval(&args[1..]),
        "audit" => cmd_audit(&args[1..]),
        other => {
            eprintln!("error: unknown subcommand `{other}`");
            print_usage();
            2
        }
    };
    if code != 0 {
        process::exit(code);
    }
}

fn print_usage() {
    println!(
        "\
USAGE:
    quality score <actual.md> <expected.md> [FLAGS]
    quality eval <pdfs-dir> <reference-dir> [FLAGS]
    quality audit <pdf-or-dir> [--reference <dir>] [FLAGS]

FLAGS:
    --json                       Emit machine-readable JSON output
    --min-nid <0..1>             Fail if mean NID is below threshold
    --min-teds <0..1>            Fail if mean TEDS is below threshold
    --min-mhs <0..1>             Fail if mean MHS is below threshold
    --max-fp-tables <N>          Fail if false-positive tables exceed N
    --max-suspicious-tables <N>  Fail if suspicious tables exceed N
    --max-bad-chars <N>          Fail if invalid chars exceed N"
    );
}

#[derive(Default)]
struct Thresholds {
    json: bool,
    reference: Option<PathBuf>,
    min_nid: Option<f64>,
    min_teds: Option<f64>,
    min_mhs: Option<f64>,
    max_fp_tables: Option<usize>,
    max_suspicious_tables: Option<usize>,
    max_bad_chars: Option<usize>,
}

fn parse_flags(args: &[String]) -> Result<(Vec<String>, Thresholds), String> {
    let mut pos = Vec::new();
    let mut cfg = Thresholds::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--json" => cfg.json = true,
            "--reference" => {
                i += 1;
                let v = args.get(i).ok_or("missing value for --reference")?;
                cfg.reference = Some(PathBuf::from(v));
            }
            "--min-nid" => {
                i += 1;
                cfg.min_nid = Some(parse_f64(args.get(i), "--min-nid")?);
            }
            "--min-teds" => {
                i += 1;
                cfg.min_teds = Some(parse_f64(args.get(i), "--min-teds")?);
            }
            "--min-mhs" => {
                i += 1;
                cfg.min_mhs = Some(parse_f64(args.get(i), "--min-mhs")?);
            }
            "--max-fp-tables" => {
                i += 1;
                cfg.max_fp_tables = Some(parse_usize(args.get(i), "--max-fp-tables")?);
            }
            "--max-suspicious-tables" => {
                i += 1;
                cfg.max_suspicious_tables =
                    Some(parse_usize(args.get(i), "--max-suspicious-tables")?);
            }
            "--max-bad-chars" => {
                i += 1;
                cfg.max_bad_chars = Some(parse_usize(args.get(i), "--max-bad-chars")?);
            }
            flag if flag.starts_with("--") => return Err(format!("unknown flag `{flag}`")),
            _ => pos.push(args[i].clone()),
        }
        i += 1;
    }
    Ok((pos, cfg))
}

fn parse_f64(val: Option<&String>, flag: &str) -> Result<f64, String> {
    let s = val.ok_or_else(|| format!("missing value for {flag}"))?;
    s.parse::<f64>()
        .map_err(|_| format!("invalid float for {flag}: {s}"))
}

fn parse_usize(val: Option<&String>, flag: &str) -> Result<usize, String> {
    let s = val.ok_or_else(|| format!("missing value for {flag}"))?;
    s.parse::<usize>()
        .map_err(|_| format!("invalid integer for {flag}: {s}"))
}

// ---- Subcommands -----------------------------------------------------------

fn cmd_score(args: &[String]) -> i32 {
    let (pos, cfg) = match parse_flags(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if pos.len() != 2 {
        eprintln!("usage: quality score <actual.md> <expected.md> [FLAGS]");
        return 2;
    }
    let actual = match fs::read_to_string(&pos[0]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: read {}: {e}", pos[0]);
            return 1;
        }
    };
    let expected = match fs::read_to_string(&pos[1]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: read {}: {e}", pos[1]);
            return 1;
        }
    };
    let name = Path::new(&pos[0])
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("doc")
        .to_string();
    let report = score_markdown(&name, &actual, &expected, true);
    print_eval_reports(std::slice::from_ref(&report), cfg.json);
    check_eval_thresholds(std::slice::from_ref(&report), &cfg)
}

fn cmd_eval(args: &[String]) -> i32 {
    let (pos, cfg) = match parse_flags(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if pos.len() != 2 {
        eprintln!("usage: quality eval <pdfs-dir> <reference-dir> [FLAGS]");
        return 2;
    }
    let pdfs = match collect_pdfs(Path::new(&pos[0])) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let ref_dir = Path::new(&pos[1]);
    if !ref_dir.is_dir() {
        eprintln!("error: {} is not a directory", ref_dir.display());
        return 2;
    }
    let opts = ConvertOptions::default();
    let mut reports = Vec::new();
    let mut failed = 0usize;
    for pdf in &pdfs {
        let stem = pdf.file_stem().and_then(|s| s.to_str()).unwrap_or("doc");
        let md_ref = ref_dir.join(format!("{stem}.md"));
        let txt_ref = ref_dir.join(format!("{stem}.txt"));
        let (ref_path, is_markdown_ref) = if md_ref.is_file() {
            (md_ref, true)
        } else if txt_ref.is_file() {
            (txt_ref, false)
        } else {
            eprintln!("warning: skipping {stem} (no .md or .txt reference)");
            continue;
        };
        let bytes = match fs::read(pdf) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: read {}: {e}", pdf.display());
                failed += 1;
                continue;
            }
        };
        let actual = match convert_pdf_to_markdown(&bytes, &opts) {
            Ok(r) => r.markdown,
            Err(e) => {
                eprintln!("error: convert {}: {e}", pdf.display());
                failed += 1;
                continue;
            }
        };
        let expected = match fs::read_to_string(&ref_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: read {}: {e}", ref_path.display());
                failed += 1;
                continue;
            }
        };
        reports.push(score_markdown(stem, &actual, &expected, is_markdown_ref));
    }
    if reports.is_empty() {
        eprintln!("error: no documents evaluated");
        return 1;
    }
    print_eval_reports(&reports, cfg.json);
    let gate = check_eval_thresholds(&reports, &cfg);
    if failed > 0 {
        1
    } else {
        gate
    }
}

fn cmd_audit(args: &[String]) -> i32 {
    let (pos, cfg) = match parse_flags(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if pos.len() != 1 {
        eprintln!("usage: quality audit <pdf-or-dir> [--reference <dir>] [FLAGS]");
        return 2;
    }
    let pdfs = match collect_pdfs(Path::new(&pos[0])) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let opts = ConvertOptions::default();
    let mut rows = Vec::new();
    let mut failed = 0usize;
    for pdf in &pdfs {
        let stem = pdf.file_stem().and_then(|s| s.to_str()).unwrap_or("doc");
        let bytes = match fs::read(pdf) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: read {}: {e}", pdf.display());
                failed += 1;
                continue;
            }
        };
        let actual = match convert_pdf_to_markdown(&bytes, &opts) {
            Ok(r) => r.markdown,
            Err(e) => {
                eprintln!("error: convert {}: {e}", pdf.display());
                failed += 1;
                continue;
            }
        };
        let audit = audit_markdown(&actual);
        let text_score = cfg.reference.as_ref().and_then(|dir| {
            let md = dir.join(format!("{stem}.md"));
            let txt = dir.join(format!("{stem}.txt"));
            let path = if md.is_file() {
                md
            } else if txt.is_file() {
                txt
            } else {
                return None;
            };
            fs::read_to_string(path)
                .ok()
                .map(|r| score_text(&actual, &r))
        });
        rows.push((stem.to_string(), audit, text_score));
    }
    if cfg.json {
        print_audit_json(&rows);
    } else {
        print_audit_table(&rows);
    }
    let mut exit_code = if failed > 0 { 1 } else { 0 };
    let total_suspicious: usize = rows.iter().map(|(_, a, _)| a.suspicious_tables).sum();
    let total_bad_chars: usize = rows
        .iter()
        .map(|(_, a, _)| a.replacement_chars + a.control_chars)
        .sum();
    if let Some(max) = cfg.max_suspicious_tables {
        if total_suspicious > max {
            eprintln!("gate failed: suspicious_tables={total_suspicious} > max={max}");
            exit_code = 1;
        }
    }
    if let Some(max) = cfg.max_bad_chars {
        if total_bad_chars > max {
            eprintln!("gate failed: bad_chars={total_bad_chars} > max={max}");
            exit_code = 1;
        }
    }
    if let Some(min) = cfg.min_nid {
        let scored: Vec<f64> = rows
            .iter()
            .filter_map(|(_, _, t)| t.as_ref().map(|s| s.nid))
            .collect();
        if !scored.is_empty() {
            let mean = scored.iter().sum::<f64>() / scored.len() as f64;
            if mean < min {
                eprintln!("gate failed: mean NID={mean:.4} < min={min:.4}");
                exit_code = 1;
            }
        }
    }
    exit_code
}

fn collect_pdfs(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        return Err(format!(
            "{} is neither a file nor a directory",
            path.display()
        ));
    }
    let mut out: Vec<PathBuf> = fs::read_dir(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("pdf"))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    Ok(out)
}

// ---- Markdown parser -------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct GfmTable {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    balanced: bool,
}

impl GfmTable {
    fn all_rows(&self) -> Vec<&[String]> {
        let mut out = Vec::with_capacity(1 + self.rows.len());
        out.push(self.headers.as_slice());
        for r in &self.rows {
            out.push(r.as_slice());
        }
        out
    }

    fn cols(&self) -> usize {
        self.headers.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Heading {
    level: usize,
    text: String,
}

#[derive(Debug, Default)]
struct ParsedDoc {
    tables: Vec<GfmTable>,
    headings: Vec<Heading>,
    list_items: Vec<String>,
    code_blocks: Vec<String>,
    raw_bullet_lines: usize,
}

fn parse_markdown(md: &str) -> ParsedDoc {
    let mut doc = ParsedDoc::default();
    let lines: Vec<&str> = md.lines().collect();
    let mut i = 0;
    let mut in_fence = false;
    let mut fence_char = '`';
    let mut code_buf = String::new();

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if in_fence {
            if is_fence_delimiter(trimmed, Some(fence_char)) {
                doc.code_blocks.push(code_buf.trim().to_string());
                code_buf.clear();
                in_fence = false;
            } else {
                if !code_buf.is_empty() {
                    code_buf.push('\n');
                }
                code_buf.push_str(line);
            }
            i += 1;
            continue;
        }

        if let Some(fc) = fence_opener(trimmed) {
            in_fence = true;
            fence_char = fc;
            code_buf.clear();
            i += 1;
            continue;
        }

        // GFM table: header row + separator row + 0..N data rows.
        if i + 1 < lines.len() && is_table_separator(lines[i + 1]) {
            if let Some(headers) = parse_table_row(line) {
                let sep_cols = parse_table_row(lines[i + 1]).map_or(0, |v| v.len());
                let mut balanced = sep_cols == headers.len();
                let mut rows = Vec::new();
                i += 2;
                while i < lines.len() {
                    let row_line = lines[i];
                    if row_line.trim().is_empty() {
                        break;
                    }
                    let Some(cells) = parse_table_row(row_line) else {
                        break;
                    };
                    if cells.len() != headers.len() {
                        balanced = false;
                    }
                    rows.push(cells);
                    i += 1;
                }
                doc.tables.push(GfmTable {
                    headers,
                    rows,
                    balanced,
                });
                continue;
            }
        }

        if let Some((level, text)) = parse_heading(trimmed) {
            doc.headings.push(Heading { level, text });
            i += 1;
            continue;
        }

        if let Some(item) = parse_list_item(trimmed) {
            doc.list_items.push(item);
            i += 1;
            continue;
        }

        if starts_with_raw_unicode_bullet(trimmed) {
            doc.raw_bullet_lines += 1;
        }

        i += 1;
    }

    doc
}

fn fence_opener(trimmed: &str) -> Option<char> {
    if trimmed.starts_with("```") {
        Some('`')
    } else if trimmed.starts_with("~~~") {
        Some('~')
    } else {
        None
    }
}

fn is_fence_delimiter(trimmed: &str, ch: Option<char>) -> bool {
    match ch {
        Some('`') => trimmed.starts_with("```") && trimmed.chars().all(|c| c == '`'),
        Some('~') => trimmed.starts_with("~~~") && trimmed.chars().all(|c| c == '~'),
        _ => false,
    }
}

fn parse_table_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let body = trimmed
        .strip_prefix('|')
        .unwrap_or(trimmed)
        .strip_suffix('|')
        .unwrap_or_else(|| trimmed.strip_prefix('|').unwrap_or(trimmed));
    // Require outer pipes or at least one unescaped inner pipe.
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

fn is_table_separator(line: &str) -> bool {
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

fn parse_heading(trimmed: &str) -> Option<(usize, String)> {
    let level = trimmed.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = trimmed.get(level..)?;
    if !rest.starts_with(' ') {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim().to_string();
    (!text.is_empty()).then_some((level, text))
}

fn parse_list_item(trimmed: &str) -> Option<String> {
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        return Some(rest.trim().to_string());
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 {
        let tail = &trimmed[digits..];
        if let Some(rest) = tail.strip_prefix(". ").or_else(|| tail.strip_prefix(") ")) {
            return Some(rest.trim().to_string());
        }
    }
    None
}

fn starts_with_raw_unicode_bullet(trimmed: &str) -> bool {
    let mut chars = trimmed.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    matches!(
        first,
        '\u{2022}' | '\u{25E6}' | '\u{25AA}' | '\u{2023}' | '\u{2043}'
    ) && chars.next().is_some_and(char::is_whitespace)
}

// ---- Metrics ---------------------------------------------------------------

#[derive(Debug, Clone)]
struct EvalReport {
    name: String,
    is_markdown_ref: bool,
    text: TextScore,
    tables: TableScore,
    headings: HeadingScore,
    list_f1: Option<f64>,
    code_f1: Option<f64>,
    audit: AuditReport,
}

#[derive(Debug, Clone)]
struct TextScore {
    ref_tokens: usize,
    actual_tokens: usize,
    token_recall: f64,
    token_precision: f64,
    token_f1: f64,
    nid: f64,
    anchor_order: f64,
    anchors: usize,
}

#[derive(Debug, Clone)]
struct TableScore {
    ref_tables: usize,
    actual_tables: usize,
    matched_tables: usize,
    false_positives: usize,
    missed_tables: usize,
    teds: f64,
    teds_struct: f64,
    cell_precision: f64,
    cell_recall: f64,
    cell_f1: f64,
}

#[derive(Debug, Clone)]
struct HeadingScore {
    ref_headings: usize,
    actual_headings: usize,
    mhs: f64,
    heading_f1: f64,
}

#[derive(Debug, Clone)]
struct AuditReport {
    tables: usize,
    table_rows: usize,
    unbalanced_tables: usize,
    suspicious_tables: usize,
    max_cell_len: usize,
    headings: usize,
    suspicious_headings: usize,
    list_items: usize,
    raw_bullet_lines: usize,
    code_blocks: usize,
    replacement_chars: usize,
    control_chars: usize,
}

fn score_markdown(name: &str, actual: &str, expected: &str, is_markdown_ref: bool) -> EvalReport {
    let actual_doc = parse_markdown(actual);
    let expected_doc = parse_markdown(expected);
    let text = score_text(actual, expected);
    let tables = score_tables(&actual_doc.tables, &expected_doc.tables);
    let headings = score_headings(&actual_doc.headings, &expected_doc.headings);
    let list_f1 = if expected_doc.list_items.is_empty() && actual_doc.list_items.is_empty() {
        None
    } else {
        Some(multiset_f1(
            &actual_doc
                .list_items
                .iter()
                .map(|s| normalize_text(s))
                .collect::<Vec<_>>(),
            &expected_doc
                .list_items
                .iter()
                .map(|s| normalize_text(s))
                .collect::<Vec<_>>(),
        ))
    };
    let code_f1 = if expected_doc.code_blocks.is_empty() && actual_doc.code_blocks.is_empty() {
        None
    } else {
        Some(multiset_f1(
            &actual_doc
                .code_blocks
                .iter()
                .map(|s| normalize_text(s))
                .collect::<Vec<_>>(),
            &expected_doc
                .code_blocks
                .iter()
                .map(|s| normalize_text(s))
                .collect::<Vec<_>>(),
        ))
    };
    let audit = audit_parsed(actual, &actual_doc);
    EvalReport {
        name: name.to_string(),
        is_markdown_ref,
        text,
        tables,
        headings,
        list_f1,
        code_f1,
        audit,
    }
}

fn audit_markdown(md: &str) -> AuditReport {
    let parsed = parse_markdown(md);
    audit_parsed(md, &parsed)
}

fn audit_parsed(md: &str, doc: &ParsedDoc) -> AuditReport {
    let mut table_rows = 0usize;
    let mut unbalanced_tables = 0usize;
    let mut suspicious_tables = 0usize;
    let mut max_cell_len = 0usize;

    for t in &doc.tables {
        let rows = t.all_rows();
        table_rows += rows.len();
        if !t.balanced {
            unbalanced_tables += 1;
        }
        let mut table_max_cell = 0usize;
        let mut total_cells = 0usize;
        let mut empty_cells = 0usize;
        for row in &rows {
            for cell in *row {
                total_cells += 1;
                let len = cell.chars().count();
                if len == 0 {
                    empty_cells += 1;
                }
                table_max_cell = table_max_cell.max(len);
            }
        }
        max_cell_len = max_cell_len.max(table_max_cell);
        if is_suspicious_table(t, table_max_cell, empty_cells, total_cells) {
            suspicious_tables += 1;
        }
    }

    let suspicious_headings = doc
        .headings
        .iter()
        .filter(|h| h.text.len() > 120 || h.text.ends_with('.'))
        .count();

    let replacement_chars = md.chars().filter(|&c| c == '\u{FFFD}').count();
    let control_chars = md
        .chars()
        .filter(|&c| (c as u32) < 32 && !matches!(c, '\n' | '\r' | '\t'))
        .count();

    AuditReport {
        tables: doc.tables.len(),
        table_rows,
        unbalanced_tables,
        suspicious_tables,
        max_cell_len,
        headings: doc.headings.len(),
        suspicious_headings,
        list_items: doc.list_items.len(),
        raw_bullet_lines: doc.raw_bullet_lines,
        code_blocks: doc.code_blocks.len(),
        replacement_chars,
        control_chars,
    }
}

fn is_suspicious_table(
    t: &GfmTable,
    max_cell: usize,
    empty_cells: usize,
    total_cells: usize,
) -> bool {
    if !t.balanced {
        return true;
    }
    // Prose swallowed into a sparse multi-column ruled grid.
    if max_cell > 220 && total_cells > 0 && empty_cells * 100 > total_cells * 35 {
        return true;
    }
    if max_cell > 400 && t.cols() >= 4 {
        return true;
    }
    let rows = t.all_rows();
    // Bullet list misclassified as a 2-column table (`| o | text |`).
    if t.cols() == 2 && rows.len() >= 2 {
        let bullet_like = rows
            .iter()
            .filter(|r| {
                matches!(
                    r.first().map(String::as_str),
                    Some("•" | "◦" | "○" | "●" | "▪" | "▫" | "-" | "*" | "o")
                )
            })
            .count();
        if bullet_like * 5 >= rows.len() * 4 {
            return true;
        }
        // Two-row split prose sentences (`| ing epochs. | The dropout... |`).
        if rows.len() == 2
            && rows
                .iter()
                .any(|r| r[0].ends_with('.') && r[1].split_whitespace().count() >= 4)
        {
            return true;
        }
    }
    false
}

// ---- Text / Reading-order scoring (NID + Token F1 + Anchor Order) ----------

fn word_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        if is_table_separator(line) {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        for (idx, &ch) in chars.iter().enumerate() {
            if ch.is_alphanumeric() {
                for lc in ch.to_lowercase() {
                    cur.push(lc);
                }
            } else if matches!(ch, '\'' | '\u{2019}')
                && !cur.is_empty()
                && chars.get(idx + 1).is_some_and(|n| n.is_alphanumeric())
            {
                cur.push('\'');
            } else if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    out
}

fn score_text(actual: &str, expected: &str) -> TextScore {
    let a = word_tokens(actual);
    let b = word_tokens(expected);
    let overlap = multiset_overlap(&a, &b);
    let recall = ratio(overlap, b.len());
    let precision = ratio(overlap, a.len());
    let f1 = f1_from_pr(precision, recall);
    let nid = normalized_indel_similarity(&a, &b);
    let (anchor_order, anchors) = ordered_anchor_score(&b, &a, 5);
    TextScore {
        ref_tokens: b.len(),
        actual_tokens: a.len(),
        token_recall: recall,
        token_precision: precision,
        token_f1: f1,
        nid,
        anchor_order,
        anchors,
    }
}

/// Normalized Indel Similarity: `2 * LCS(a, b) / (|a| + |b|)`.
/// Uses exact DP for small/medium sequences and anchor-partitioned exact LCS
/// for very large sequences so long documents stay fast.
fn normalized_indel_similarity(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let lcs = lcs_len_tokens(a, b);
    (2 * lcs) as f64 / (a.len() + b.len()) as f64
}

fn lcs_len_tokens(a: &[String], b: &[String]) -> usize {
    const EXACT_CELL_LIMIT: usize = 2_000_000;
    if a.len().saturating_mul(b.len()) <= EXACT_CELL_LIMIT {
        return exact_lcs_len(a, b);
    }
    // Patience-style anchor decomposition: find unique shared tokens, take
    // their LIS as exact anchors, and run exact/bounded LCS between anchors.
    let anchors = unique_token_anchors(a, b);
    if anchors.is_empty() {
        return multiset_overlap(a, b).min(a.len().min(b.len()));
    }
    let mut total = 0usize;
    let mut prev_a = 0usize;
    let mut prev_b = 0usize;
    for (ai, bi) in anchors {
        total += gap_lcs(&a[prev_a..ai], &b[prev_b..bi], EXACT_CELL_LIMIT);
        total += 1;
        prev_a = ai + 1;
        prev_b = bi + 1;
    }
    total += gap_lcs(&a[prev_a..], &b[prev_b..], EXACT_CELL_LIMIT);
    total
}

fn gap_lcs(a: &[String], b: &[String], limit: usize) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    if a.len().saturating_mul(b.len()) <= limit {
        return exact_lcs_len(a, b);
    }
    // Further split big gaps on 3-gram anchors; fallback to greedy match.
    let sub = ngram_anchors(a, b, 3);
    if sub.is_empty() {
        return greedy_in_order_matches(a, b);
    }
    let mut total = 0usize;
    let mut pa = 0usize;
    let mut pb = 0usize;
    for (ai, bi) in sub {
        if ai < pa || bi < pb {
            continue;
        }
        let ga = &a[pa..ai];
        let gb = &b[pb..bi];
        if ga.len().saturating_mul(gb.len()) <= limit {
            total += exact_lcs_len(ga, gb);
        } else {
            total += greedy_in_order_matches(ga, gb);
        }
        total += 1;
        pa = ai + 1;
        pb = bi + 1;
    }
    let ga = &a[pa..];
    let gb = &b[pb..];
    if ga.len().saturating_mul(gb.len()) <= limit {
        total += exact_lcs_len(ga, gb);
    } else {
        total += greedy_in_order_matches(ga, gb);
    }
    total
}

fn greedy_in_order_matches(a: &[String], b: &[String]) -> usize {
    let mut positions: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, tok) in b.iter().enumerate() {
        positions.entry(tok.as_str()).or_default().push(i);
    }
    let mut cur = 0usize;
    let mut matched = 0usize;
    for tok in a {
        if let Some(pos) = positions.get(tok.as_str()) {
            let slot = pos.partition_point(|&p| p < cur);
            if let Some(&next_pos) = pos.get(slot) {
                matched += 1;
                cur = next_pos + 1;
            }
        }
    }
    matched
}

fn exact_lcs_len<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut prev = vec![0usize; short.len() + 1];
    let mut curr = vec![0usize; short.len() + 1];
    for x in long {
        for (j, y) in short.iter().enumerate() {
            curr[j + 1] = if x == y {
                prev[j] + 1
            } else {
                prev[j + 1].max(curr[j])
            };
        }
        std::mem::swap(&mut prev, &mut curr);
        curr.fill(0);
    }
    prev[short.len()]
}

fn unique_token_anchors(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let pos_a = unique_positions(a.iter().map(String::as_str));
    let pos_b = unique_positions(b.iter().map(String::as_str));
    let mut pairs: Vec<(usize, usize)> = pos_a
        .into_iter()
        .filter_map(|(k, ia)| pos_b.get(k).map(|&ib| (ia, ib)))
        .collect();
    pairs.sort_unstable_by_key(|&(ia, _)| ia);
    lis_pairs(&pairs)
}

fn ngram_anchors(a: &[String], b: &[String], width: usize) -> Vec<(usize, usize)> {
    if a.len() < width || b.len() < width {
        return Vec::new();
    }
    let pos_a = unique_positions(a.windows(width));
    let pos_b = unique_positions(b.windows(width));
    let mut pairs: Vec<(usize, usize)> = pos_a
        .into_iter()
        .filter_map(|(k, ia)| pos_b.get(k).map(|&ib| (ia, ib)))
        .collect();
    pairs.sort_unstable_by_key(|&(ia, _)| ia);
    lis_pairs(&pairs)
}

fn unique_positions<K: Eq + std::hash::Hash>(iter: impl Iterator<Item = K>) -> HashMap<K, usize> {
    let mut pos: HashMap<K, Option<usize>> = HashMap::new();
    for (idx, key) in iter.enumerate() {
        pos.entry(key)
            .and_modify(|slot| *slot = None)
            .or_insert(Some(idx));
    }
    pos.into_iter()
        .filter_map(|(k, v)| v.map(|idx| (k, idx)))
        .collect()
}

fn lis_pairs(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    if pairs.is_empty() {
        return Vec::new();
    }
    let mut tails_idx: Vec<usize> = Vec::new();
    let mut prev: Vec<Option<usize>> = vec![None; pairs.len()];
    for (i, &(_, b_val)) in pairs.iter().enumerate() {
        let slot = tails_idx.partition_point(|&ti| pairs[ti].1 < b_val);
        if slot > 0 {
            prev[i] = Some(tails_idx[slot - 1]);
        }
        if slot == tails_idx.len() {
            tails_idx.push(i);
        } else {
            tails_idx[slot] = i;
        }
    }
    let mut out = Vec::with_capacity(tails_idx.len());
    let mut cur = tails_idx.last().copied();
    while let Some(idx) = cur {
        out.push(pairs[idx]);
        cur = prev[idx];
    }
    out.reverse();
    out
}

fn ordered_anchor_score(reference: &[String], actual: &[String], width: usize) -> (f64, usize) {
    if reference.len() < width || actual.len() < width {
        return (if reference.is_empty() { 1.0 } else { 0.0 }, 0);
    }
    let ref_pos = unique_positions(reference.windows(width));
    let act_pos = unique_positions(actual.windows(width));
    let mut pairs: Vec<(usize, usize)> = ref_pos
        .into_iter()
        .filter_map(|(gram, ri)| act_pos.get(gram).map(|&ai| (ri, ai)))
        .collect();
    if pairs.is_empty() {
        return (0.0, 0);
    }
    pairs.sort_unstable_by_key(|&(ri, _)| ri);
    let mut tails: Vec<usize> = Vec::new();
    for &(_, pos) in &pairs {
        let slot = tails.partition_point(|&v| v < pos);
        if slot == tails.len() {
            tails.push(pos);
        } else {
            tails[slot] = pos;
        }
    }
    (tails.len() as f64 / pairs.len() as f64, pairs.len())
}

// ---- Table scoring (TEDS, TEDS-Struct, Cell F1) ----------------------------

fn score_tables(pred: &[GfmTable], gold: &[GfmTable]) -> TableScore {
    if pred.is_empty() && gold.is_empty() {
        return TableScore {
            ref_tables: 0,
            actual_tables: 0,
            matched_tables: 0,
            false_positives: 0,
            missed_tables: 0,
            teds: 1.0,
            teds_struct: 1.0,
            cell_precision: 1.0,
            cell_recall: 1.0,
            cell_f1: 1.0,
        };
    }
    let mut candidates: Vec<(f64, f64, usize, usize)> = Vec::new();
    for (pi, pt) in pred.iter().enumerate() {
        for (gi, gt) in gold.iter().enumerate() {
            let (teds, teds_s) = table_teds(pt, gt);
            candidates.push((teds, teds_s, pi, gi));
        }
    }
    candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut used_p = vec![false; pred.len()];
    let mut used_g = vec![false; gold.len()];
    let mut sum_teds = 0.0;
    let mut sum_struct = 0.0;
    let mut matched = 0usize;
    let mut matched_cells = 0usize;

    for (teds, teds_s, pi, gi) in candidates {
        if used_p[pi] || used_g[gi] {
            continue;
        }
        used_p[pi] = true;
        used_g[gi] = true;
        sum_teds += teds;
        sum_struct += teds_s;
        if teds > 0.25 {
            matched += 1;
        }
        matched_cells += multiset_overlap(&table_cells(&pred[pi]), &table_cells(&gold[gi]));
    }

    let denom = pred.len().max(gold.len()) as f64;
    let total_pred_cells: usize = pred.iter().map(|t| table_cells(t).len()).sum();
    let total_gold_cells: usize = gold.iter().map(|t| table_cells(t).len()).sum();
    let cell_p = ratio(matched_cells, total_pred_cells);
    let cell_r = ratio(matched_cells, total_gold_cells);
    TableScore {
        ref_tables: gold.len(),
        actual_tables: pred.len(),
        matched_tables: matched,
        false_positives: pred.len().saturating_sub(matched),
        missed_tables: gold.len().saturating_sub(matched),
        teds: sum_teds / denom,
        teds_struct: sum_struct / denom,
        cell_precision: cell_p,
        cell_recall: cell_r,
        cell_f1: f1_from_pr(cell_p, cell_r),
    }
}

fn table_cells(t: &GfmTable) -> Vec<String> {
    t.all_rows()
        .into_iter()
        .flat_map(|r| r.iter())
        .map(|c| normalize_text(c))
        .filter(|c| !c.is_empty())
        .collect()
}

/// Exact two-level Zhang-Shasha Tree Edit Distance similarity for GFM tables.
/// Returns `(teds, teds_struct)`.
fn table_teds(a: &GfmTable, b: &GfmTable) -> (f64, f64) {
    let ra = a.all_rows();
    let rb = b.all_rows();
    let nodes_a = 1 + ra.iter().map(|r| 1 + r.len()).sum::<usize>();
    let nodes_b = 1 + rb.iter().map(|r| 1 + r.len()).sum::<usize>();
    let max_nodes = nodes_a.max(nodes_b).max(1) as f64;

    let dist_full = row_tree_edit_distance(&ra, &rb, false);
    let dist_struct = row_tree_edit_distance(&ra, &rb, true);
    (
        (1.0 - dist_full / max_nodes).clamp(0.0, 1.0),
        (1.0 - dist_struct / max_nodes).clamp(0.0, 1.0),
    )
}

fn row_tree_edit_distance(ra: &[&[String]], rb: &[&[String]], struct_only: bool) -> f64 {
    let m = ra.len();
    let n = rb.len();
    let mut dp = vec![vec![0.0f64; n + 1]; m + 1];
    for i in 1..=m {
        dp[i][0] = dp[i - 1][0] + (1 + ra[i - 1].len()) as f64;
    }
    for j in 1..=n {
        dp[0][j] = dp[0][j - 1] + (1 + rb[j - 1].len()) as f64;
    }
    for i in 1..=m {
        for j in 1..=n {
            let del = dp[i - 1][j] + (1 + ra[i - 1].len()) as f64;
            let ins = dp[i][j - 1] + (1 + rb[j - 1].len()) as f64;
            let sub = dp[i - 1][j - 1] + cell_row_edit_distance(ra[i - 1], rb[j - 1], struct_only);
            dp[i][j] = del.min(ins).min(sub);
        }
    }
    dp[m][n]
}

fn cell_row_edit_distance(ca: &[String], cb: &[String], struct_only: bool) -> f64 {
    let m = ca.len();
    let n = cb.len();
    let mut prev: Vec<f64> = (0..=n).map(|j| j as f64).collect();
    let mut curr = vec![0.0f64; n + 1];
    for i in 1..=m {
        curr[0] = i as f64;
        for j in 1..=n {
            let cost = if struct_only {
                0.0
            } else {
                normalized_levenshtein(&normalize_text(&ca[i - 1]), &normalize_text(&cb[j - 1]))
            };
            curr[j] = (prev[j] + 1.0)
                .min(curr[j - 1] + 1.0)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

// ---- Heading scoring (MHS + Heading F1) ------------------------------------

fn score_headings(pred: &[Heading], gold: &[Heading]) -> HeadingScore {
    if pred.is_empty() && gold.is_empty() {
        return HeadingScore {
            ref_headings: 0,
            actual_headings: 0,
            mhs: 1.0,
            heading_f1: 1.0,
        };
    }
    let m = pred.len();
    let n = gold.len();
    let mut prev: Vec<f64> = (0..=n).map(|j| j as f64).collect();
    let mut curr = vec![0.0f64; n + 1];
    for i in 1..=m {
        curr[0] = i as f64;
        let pa = normalize_text(&pred[i - 1].text);
        for j in 1..=n {
            let gb = normalize_text(&gold[j - 1].text);
            let text_dist = normalized_levenshtein(&pa, &gb);
            let level_pen = if pred[i - 1].level == gold[j - 1].level {
                0.0
            } else {
                1.0
            };
            let sub_cost = 0.75 * text_dist + 0.25 * level_pen;
            curr[j] = (prev[j] + 1.0)
                .min(curr[j - 1] + 1.0)
                .min(prev[j - 1] + sub_cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    let mhs = (1.0 - prev[n] / m.max(n) as f64).clamp(0.0, 1.0);
    let p_texts: Vec<String> = pred.iter().map(|h| normalize_text(&h.text)).collect();
    let g_texts: Vec<String> = gold.iter().map(|h| normalize_text(&h.text)).collect();
    let heading_f1 = multiset_f1(&p_texts, &g_texts);
    HeadingScore {
        ref_headings: gold.len(),
        actual_headings: pred.len(),
        mhs,
        heading_f1,
    }
}

// ---- String / multiset utilities -------------------------------------------

fn normalize_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = true;
    for c in s.chars() {
        if matches!(c, '*' | '_' | '`' | '\\') {
            continue;
        }
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            prev_ws = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn normalized_levenshtein(a: &str, b: &str) -> f64 {
    if a == b {
        return 0.0;
    }
    let ca: Vec<char> = a.chars().collect();
    let cb: Vec<char> = b.chars().collect();
    let max_len = ca.len().max(cb.len());
    if max_len == 0 {
        return 0.0;
    }
    let mut prev: Vec<usize> = (0..=cb.len()).collect();
    let mut curr = vec![0usize; cb.len() + 1];
    for (i, &ac) in ca.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &bc) in cb.iter().enumerate() {
            let cost = usize::from(ac != bc);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[cb.len()] as f64 / max_len as f64
}

fn multiset_overlap(a: &[String], b: &[String]) -> usize {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for s in b {
        *counts.entry(s.as_str()).or_insert(0) += 1;
    }
    let mut overlap = 0usize;
    for s in a {
        if let Some(c) = counts.get_mut(s.as_str()) {
            if *c > 0 {
                *c -= 1;
                overlap += 1;
            }
        }
    }
    overlap
}

fn multiset_f1(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let overlap = multiset_overlap(a, b);
    f1_from_pr(ratio(overlap, a.len()), ratio(overlap, b.len()))
}

fn ratio(num: usize, den: usize) -> f64 {
    if den == 0 {
        if num == 0 {
            1.0
        } else {
            0.0
        }
    } else {
        num as f64 / den as f64
    }
}

fn f1_from_pr(p: f64, r: f64) -> f64 {
    if p + r == 0.0 {
        0.0
    } else {
        2.0 * p * r / (p + r)
    }
}

// ---- Output formatting & threshold gates -----------------------------------

fn check_eval_thresholds(reports: &[EvalReport], cfg: &Thresholds) -> i32 {
    let mut exit_code = 0;
    let n = reports.len() as f64;
    let mean_nid = reports.iter().map(|r| r.text.nid).sum::<f64>() / n;
    let md_reports: Vec<&EvalReport> = reports.iter().filter(|r| r.is_markdown_ref).collect();
    if let Some(min) = cfg.min_nid {
        if mean_nid < min {
            eprintln!("gate failed: mean NID={mean_nid:.4} < min={min:.4}");
            exit_code = 1;
        }
    }
    if let Some(min) = cfg.min_teds {
        if !md_reports.is_empty() {
            let mean =
                md_reports.iter().map(|r| r.tables.teds).sum::<f64>() / md_reports.len() as f64;
            if mean < min {
                eprintln!("gate failed: mean TEDS={mean:.4} < min={min:.4}");
                exit_code = 1;
            }
        }
    }
    if let Some(min) = cfg.min_mhs {
        if !md_reports.is_empty() {
            let mean =
                md_reports.iter().map(|r| r.headings.mhs).sum::<f64>() / md_reports.len() as f64;
            if mean < min {
                eprintln!("gate failed: mean MHS={mean:.4} < min={min:.4}");
                exit_code = 1;
            }
        }
    }
    if let Some(max) = cfg.max_fp_tables {
        let fp: usize = md_reports.iter().map(|r| r.tables.false_positives).sum();
        if fp > max {
            eprintln!("gate failed: false_positive_tables={fp} > max={max}");
            exit_code = 1;
        }
    }
    if let Some(max) = cfg.max_suspicious_tables {
        let susp: usize = reports.iter().map(|r| r.audit.suspicious_tables).sum();
        if susp > max {
            eprintln!("gate failed: suspicious_tables={susp} > max={max}");
            exit_code = 1;
        }
    }
    if let Some(max) = cfg.max_bad_chars {
        let bad: usize = reports
            .iter()
            .map(|r| r.audit.replacement_chars + r.audit.control_chars)
            .sum();
        if bad > max {
            eprintln!("gate failed: bad_chars={bad} > max={max}");
            exit_code = 1;
        }
    }
    exit_code
}

fn print_eval_reports(reports: &[EvalReport], json: bool) {
    if json {
        println!("[");
        for (idx, r) in reports.iter().enumerate() {
            let comma = if idx + 1 < reports.len() { "," } else { "" };
            let list_f1 = r
                .list_f1
                .map_or_else(|| "null".to_string(), |v| format!("{v:.4}"));
            let code_f1 = r
                .code_f1
                .map_or_else(|| "null".to_string(), |v| format!("{v:.4}"));
            println!(
                "  {{\"name\":{},\"markdown_ref\":{},\"nid\":{:.4},\"token_f1\":{:.4},\"token_recall\":{:.4},\"token_precision\":{:.4},\"anchor_order\":{:.4},\"teds\":{:.4},\"teds_struct\":{:.4},\"cell_precision\":{:.4},\"cell_recall\":{:.4},\"cell_f1\":{:.4},\"ref_tables\":{},\"actual_tables\":{},\"matched_tables\":{},\"fp_tables\":{},\"missed_tables\":{},\"mhs\":{:.4},\"heading_f1\":{:.4},\"ref_headings\":{},\"actual_headings\":{},\"list_f1\":{list_f1},\"code_f1\":{code_f1},\"suspicious_tables\":{},\"raw_bullets\":{},\"bad_chars\":{}}}{comma}",
                json_escape(&r.name),
                r.is_markdown_ref,
                r.text.nid,
                r.text.token_f1,
                r.text.token_recall,
                r.text.token_precision,
                r.text.anchor_order,
                r.tables.teds,
                r.tables.teds_struct,
                r.tables.cell_precision,
                r.tables.cell_recall,
                r.tables.cell_f1,
                r.tables.ref_tables,
                r.tables.actual_tables,
                r.tables.matched_tables,
                r.tables.false_positives,
                r.tables.missed_tables,
                r.headings.mhs,
                r.headings.heading_f1,
                r.headings.ref_headings,
                r.headings.actual_headings,
                r.audit.suspicious_tables,
                r.audit.raw_bullet_lines,
                r.audit.replacement_chars + r.audit.control_chars,
            );
        }
        println!("]");
        return;
    }

    println!(
        "{:<34} {:>6} {:>6} {:>6} {:>6} {:>6} {:>7} {:>5} {:>4}",
        "Document", "NID", "TokF1", "TEDS", "CellF1", "MHS", "Tables", "Susp", "Bad"
    );
    println!("{}", "-".repeat(92));
    for r in reports {
        let teds = if r.is_markdown_ref {
            format!("{:.3}", r.tables.teds)
        } else {
            "-".to_string()
        };
        let cell_f1 = if r.is_markdown_ref {
            format!("{:.3}", r.tables.cell_f1)
        } else {
            "-".to_string()
        };
        let mhs = if r.is_markdown_ref {
            format!("{:.3}", r.headings.mhs)
        } else {
            "-".to_string()
        };
        let tbl = if r.is_markdown_ref {
            format!("{}/{}", r.tables.actual_tables, r.tables.ref_tables)
        } else {
            format!("{}", r.audit.tables)
        };
        println!(
            "{:<34} {:>6.3} {:>6.3} {:>6} {:>6} {:>6} {:>7} {:>5} {:>4}",
            truncate(&r.name, 34),
            r.text.nid,
            r.text.token_f1,
            teds,
            cell_f1,
            mhs,
            tbl,
            r.audit.suspicious_tables,
            r.audit.replacement_chars + r.audit.control_chars,
        );
    }
    if reports.len() > 1 {
        let n = reports.len() as f64;
        let mean_nid = reports.iter().map(|r| r.text.nid).sum::<f64>() / n;
        let mean_tok = reports.iter().map(|r| r.text.token_f1).sum::<f64>() / n;
        let md_only: Vec<&EvalReport> = reports.iter().filter(|r| r.is_markdown_ref).collect();
        let mean_teds = if md_only.is_empty() {
            "-".to_string()
        } else {
            format!(
                "{:.3}",
                md_only.iter().map(|r| r.tables.teds).sum::<f64>() / md_only.len() as f64
            )
        };
        let mean_cell = if md_only.is_empty() {
            "-".to_string()
        } else {
            format!(
                "{:.3}",
                md_only.iter().map(|r| r.tables.cell_f1).sum::<f64>() / md_only.len() as f64
            )
        };
        let mean_mhs = if md_only.is_empty() {
            "-".to_string()
        } else {
            format!(
                "{:.3}",
                md_only.iter().map(|r| r.headings.mhs).sum::<f64>() / md_only.len() as f64
            )
        };
        let total_susp: usize = reports.iter().map(|r| r.audit.suspicious_tables).sum();
        let total_bad: usize = reports
            .iter()
            .map(|r| r.audit.replacement_chars + r.audit.control_chars)
            .sum();
        println!("{}", "-".repeat(92));
        println!(
            "{:<34} {:>6.3} {:>6.3} {:>6} {:>6} {:>6} {:>7} {:>5} {:>4}",
            "MEAN / TOTAL",
            mean_nid,
            mean_tok,
            mean_teds,
            mean_cell,
            mean_mhs,
            "",
            total_susp,
            total_bad
        );
    }
}

fn print_audit_table(rows: &[(String, AuditReport, Option<TextScore>)]) {
    let has_ref = rows.iter().any(|(_, _, t)| t.is_some());
    if has_ref {
        println!(
            "{:<34} {:>5} {:>5} {:>6} {:>5} {:>5} {:>4} {:>6} {:>6} {:>6}",
            "Document", "Tbls", "Susp", "MaxC", "Hdgs", "RawB", "Bad", "Rec", "Prec", "Ord"
        );
        println!("{}", "-".repeat(96));
    } else {
        println!(
            "{:<34} {:>5} {:>5} {:>6} {:>5} {:>5} {:>5} {:>4}",
            "Document", "Tbls", "Susp", "MaxC", "Hdgs", "List", "RawB", "Bad"
        );
        println!("{}", "-".repeat(76));
    }
    let mut totals: BTreeMap<&str, usize> = BTreeMap::new();
    for (name, a, t) in rows {
        let bad = a.replacement_chars + a.control_chars;
        *totals.entry("tables").or_insert(0) += a.tables;
        *totals.entry("susp").or_insert(0) += a.suspicious_tables;
        *totals.entry("hdgs").or_insert(0) += a.headings;
        *totals.entry("rawb").or_insert(0) += a.raw_bullet_lines;
        *totals.entry("bad").or_insert(0) += bad;
        if has_ref {
            let (rec, prec, ord) = match t {
                Some(s) => (
                    format!("{:.3}", s.token_recall),
                    format!("{:.3}", s.token_precision),
                    format!("{:.3}", s.anchor_order),
                ),
                None => ("-".into(), "-".into(), "-".into()),
            };
            println!(
                "{:<34} {:>5} {:>5} {:>6} {:>5} {:>5} {:>4} {:>6} {:>6} {:>6}",
                truncate(name, 34),
                a.tables,
                a.suspicious_tables,
                a.max_cell_len,
                a.headings,
                a.raw_bullet_lines,
                bad,
                rec,
                prec,
                ord
            );
        } else {
            println!(
                "{:<34} {:>5} {:>5} {:>6} {:>5} {:>5} {:>5} {:>4}",
                truncate(name, 34),
                a.tables,
                a.suspicious_tables,
                a.max_cell_len,
                a.headings,
                a.list_items,
                a.raw_bullet_lines,
                bad
            );
        }
    }
    if rows.len() > 1 {
        println!("{}", "-".repeat(if has_ref { 96 } else { 76 }));
        let t = totals.get("tables").copied().unwrap_or(0);
        let s = totals.get("susp").copied().unwrap_or(0);
        let h = totals.get("hdgs").copied().unwrap_or(0);
        let rb = totals.get("rawb").copied().unwrap_or(0);
        let bad = totals.get("bad").copied().unwrap_or(0);
        println!(
            "{:<34} {:>5} {:>5} {:>6} {:>5} {:>5} {:>4}",
            "TOTAL", t, s, "", h, rb, bad
        );
    }
}

fn print_audit_json(rows: &[(String, AuditReport, Option<TextScore>)]) {
    println!("[");
    for (idx, (name, a, t)) in rows.iter().enumerate() {
        let comma = if idx + 1 < rows.len() { "," } else { "" };
        let ref_fields = match t {
            Some(s) => format!(
                ",\"ref_tokens\":{},\"actual_tokens\":{},\"token_recall\":{:.4},\"token_precision\":{:.4},\"token_f1\":{:.4},\"nid\":{:.4},\"anchor_order\":{:.4},\"anchors\":{}",
                s.ref_tokens,
                s.actual_tokens,
                s.token_recall,
                s.token_precision,
                s.token_f1,
                s.nid,
                s.anchor_order,
                s.anchors
            ),
            None => String::new(),
        };
        println!(
            "  {{\"name\":{},\"tables\":{},\"table_rows\":{},\"unbalanced_tables\":{},\"suspicious_tables\":{},\"max_cell_len\":{},\"headings\":{},\"suspicious_headings\":{},\"list_items\":{},\"raw_bullet_lines\":{},\"code_blocks\":{},\"replacement_chars\":{},\"control_chars\":{}{ref_fields}}}{comma}",
            json_escape(name),
            a.tables,
            a.table_rows,
            a.unbalanced_tables,
            a.suspicious_tables,
            a.max_cell_len,
            a.headings,
            a.suspicious_headings,
            a.list_items,
            a.raw_bullet_lines,
            a.code_blocks,
            a.replacement_chars,
            a.control_chars,
        );
    }
    println!("]");
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('~');
        out
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gfm_tables_including_escaped_pipes_and_ignores_code_fences() {
        let md = "\
# Report

```markdown
# Not a heading
| Not | A Table |
| --- | --- |
| 1 | 2 |
```

| Model | Expr | Score |
| --- | :---: | ---: |
| Base | a \\| b | 91.5 |
| Big | x | 94.2 |

- item one
- item two
";
        let doc = parse_markdown(md);
        assert_eq!(doc.headings.len(), 1);
        assert_eq!(doc.headings[0].level, 1);
        assert_eq!(doc.headings[0].text, "Report");
        assert_eq!(doc.code_blocks.len(), 1);
        assert_eq!(doc.tables.len(), 1);
        let t = &doc.tables[0];
        assert!(t.balanced);
        assert_eq!(t.headers, vec!["Model", "Expr", "Score"]);
        assert_eq!(t.rows[0], vec!["Base", "a | b", "91.5"]);
        assert_eq!(t.rows[1], vec!["Big", "x", "94.2"]);
        assert_eq!(doc.list_items, vec!["item one", "item two"]);
    }

    #[test]
    fn teds_scores_identical_edited_and_false_positive_tables() {
        let gold = "\
| A | B |
| --- | --- |
| 1 | 2 |
| 3 | 4 |
";
        let same = score_markdown("same", gold, gold, true);
        assert!((same.tables.teds - 1.0).abs() < 1e-9);
        assert!((same.tables.teds_struct - 1.0).abs() < 1e-9);
        assert!((same.tables.cell_f1 - 1.0).abs() < 1e-9);
        assert_eq!(same.tables.false_positives, 0);
        assert_eq!(same.tables.missed_tables, 0);

        let edited = "\
| A | B |
| --- | --- |
| 1 | 99 |
| 3 | 4 |
";
        let diff = score_markdown("diff", edited, gold, true);
        assert!(diff.tables.teds > 0.8 && diff.tables.teds < 1.0);
        assert!((diff.tables.teds_struct - 1.0).abs() < 1e-9);

        let extra = format!("{gold}\n| X | Y |\n| --- | --- |\n| 5 | 6 |\n");
        let fp = score_markdown("fp", &extra, gold, true);
        assert_eq!(fp.tables.false_positives, 1);
        assert!((fp.tables.teds - 0.5).abs() < 1e-9);
    }

    #[test]
    fn nid_penalizes_swapped_reading_order_and_drops() {
        let gold = "alpha beta gamma delta epsilon zeta eta theta";
        let same = score_text(gold, gold);
        assert!((same.nid - 1.0).abs() < 1e-9);
        assert!((same.token_f1 - 1.0).abs() < 1e-9);

        let swapped = "epsilon zeta eta theta alpha beta gamma delta";
        let reordered = score_text(swapped, gold);
        assert!((reordered.token_f1 - 1.0).abs() < 1e-9);
        assert!((reordered.nid - 0.5).abs() < 1e-9);
    }

    #[test]
    fn mhs_penalizes_wrong_heading_level_and_missing_headings() {
        let gold = "# Title\n\n## Methods\n\n### Data\n";
        let exact = score_markdown("exact", gold, gold, true);
        assert!((exact.headings.mhs - 1.0).abs() < 1e-9);

        let wrong_level = "# Title\n\n### Methods\n\n### Data\n";
        let level = score_markdown("level", wrong_level, gold, true);
        assert!(level.headings.mhs > 0.85 && level.headings.mhs < 1.0);
        assert!((level.headings.heading_f1 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn audit_flags_suspicious_tables_and_raw_bullets() {
        let prose = "word ".repeat(60);
        let bad = format!(
            "\
|  | {prose} |  |  |
| --- | --- | --- | --- |
|  |  |  | x |

| o | Item one description |
| --- | --- |
| o | Item two description |

• Unconverted bullet
"
        );
        let report = audit_markdown(&bad);
        assert_eq!(report.tables, 2);
        assert_eq!(report.suspicious_tables, 2);
        assert_eq!(report.raw_bullet_lines, 1);
    }
}
