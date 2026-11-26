//! Output-quality regression gate.
//!
//! `cargo test --test quality` converts every fixture, scores it against
//! the hand-written Markdown, and fails if a metric falls more than the
//! baseline tolerance. `UPDATE_QUALITY_BASELINE=1` rewrites the baseline
//! and the snapshots. `UPDATE_QUALITY_FIXTURES=1` rewrites synthetic PDFs
//! from [`gen`].

mod gen;
mod score;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use score::{Scores, TOLERANCE};

const FIXTURES: &[&str] = &[
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
    "field-note",
];

#[test]
fn quality_suite() {
    let update_fixtures = env_flag("UPDATE_QUALITY_FIXTURES");
    let update_baseline = env_flag("UPDATE_QUALITY_BASELINE");
    sync_synthetic_pdfs(update_fixtures);

    let mut errors = Vec::new();
    let mut rows = Vec::new();
    for id in FIXTURES {
        let pdf_path = fixture_dir().join(format!("{id}.pdf"));
        let expected_path = fixture_dir().join(format!("{id}.md"));
        let pdf =
            fs::read(&pdf_path).unwrap_or_else(|e| panic!("read {}: {e}", pdf_path.display()));
        let expected = fs::read_to_string(&expected_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", expected_path.display()));
        let actual = match pdfmd::convert_pdf_to_markdown(&pdf, &pdfmd::ConvertOptions::default()) {
            Ok(result) => result.markdown,
            Err(err) => {
                errors.push(format!("{id}: {err}"));
                String::new()
            }
        };
        let scores = score::score(&expected, &actual);
        rows.push(Row {
            id,
            scores,
            expected,
            actual,
        });
    }

    let baseline_path = root().join("tests/quality/baseline.json");
    let (tolerance, regressions, improvements) = if update_baseline {
        write_baseline(&baseline_path, &rows);
        write_snapshots(&rows);
        (TOLERANCE, Vec::new(), Vec::new())
    } else {
        let (tolerance, baseline) = read_baseline(&baseline_path);
        let (regressions, improvements) = compare(&baseline, &rows, tolerance);
        (tolerance, regressions, improvements)
    };

    let report = render_report(
        &rows,
        tolerance,
        &errors,
        &regressions,
        &improvements,
        update_baseline,
    );
    write_reports(&report);
    write_json(&rows, tolerance, &errors, &regressions, &improvements);
    print_table(&rows, &regressions);

    assert!(
        errors.is_empty(),
        "conversion failed:\n{}",
        errors.join("\n")
    );
    if !update_baseline {
        assert!(
            regressions.is_empty(),
            "quality regressed by more than {tolerance}:\n{}\n\nReport: {}\nUpdate with UPDATE_QUALITY_BASELINE=1 cargo test --test quality",
            regressions.join("\n"),
            report_dir().join("quality-report.md").display()
        );
    }
}

struct Row<'a> {
    id: &'a str,
    scores: Scores,
    expected: String,
    actual: String,
}

fn sync_synthetic_pdfs(update: bool) {
    for id in gen::synthetic_ids() {
        let generated = gen::build(id).unwrap_or_else(|| panic!("missing generator for {id}"));
        let path = fixture_dir().join(format!("{id}.pdf"));
        if update {
            fs::create_dir_all(fixture_dir()).expect("fixture dir");
            fs::write(&path, &generated)
                .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
            continue;
        }
        let on_disk = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert!(
            on_disk == generated,
            "{id}.pdf does not match tests/quality/gen.rs (disk {} bytes, generator {} bytes). \
             Regenerate with UPDATE_QUALITY_FIXTURES=1 cargo test --test quality",
            on_disk.len(),
            generated.len()
        );
    }
}

fn compare(
    baseline: &BTreeMap<String, BTreeMap<String, Option<f64>>>,
    rows: &[Row<'_>],
    tolerance: f64,
) -> (Vec<String>, Vec<String>) {
    let mut regressions = Vec::new();
    let mut improvements = Vec::new();
    let mut seen = BTreeMap::new();
    for row in rows {
        seen.insert(row.id.to_owned(), ());
        let Some(base) = baseline.get(row.id) else {
            regressions.push(format!(
                "{}: missing from baseline; lock it with UPDATE_QUALITY_BASELINE=1",
                row.id
            ));
            continue;
        };
        for metric in row.scores.metrics() {
            match (base.get(metric.name).copied(), metric.value) {
                (Some(None), None) => {}
                (Some(Some(before)), Some(now)) => {
                    if now + tolerance + 1e-9 < before {
                        regressions.push(format!(
                            "{}.{} {now:.4} < baseline {before:.4} (tolerance {tolerance})",
                            row.id, metric.name
                        ));
                    } else if now > before + 1e-9 {
                        improvements.push(format!(
                            "{}.{} {before:.4} -> {now:.4}",
                            row.id, metric.name
                        ));
                    }
                }
                (Some(_), _) | (None, _) => regressions.push(format!(
                    "{}.{} applicability changed (baseline {:?}, now {:?}); update the baseline",
                    row.id,
                    metric.name,
                    base.get(metric.name).copied().flatten(),
                    metric.value
                )),
            }
        }
    }
    for id in baseline.keys() {
        if !seen.contains_key(id) {
            regressions.push(format!("{id}: still in the baseline but not in the suite"));
        }
    }
    (regressions, improvements)
}

fn render_report(
    rows: &[Row<'_>],
    tolerance: f64,
    errors: &[String],
    regressions: &[String],
    improvements: &[String],
    updated: bool,
) -> String {
    let mut md = String::new();
    md.push_str("# pdfmd output quality\n\n");
    if updated {
        md.push_str("Baseline and snapshots were rewritten from this run.\n\n");
    }
    md.push_str(&format!(
        "Gate tolerance is **{tolerance:.2}** absolute. A metric fails when it drops by more than that from `tests/quality/baseline.json`. Higher scores pass without an update. Snapshot diffs are informational.\n\n"
    ));
    if !errors.is_empty() {
        md.push_str("## Conversion errors\n\n");
        for err in errors {
            md.push_str(&format!("- {err}\n"));
        }
        md.push('\n');
    }
    md.push_str("## Aggregate\n\n");
    md.push_str(&metric_header());
    md.push_str(&metric_row("all", &aggregate(rows), aggregate_mean(rows)));
    md.push_str("\n## Fixtures\n\n");
    md.push_str("| fixture | mean | what it covers | snapshot |\n| --- | ---: | --- | --- |\n");
    for row in rows {
        let snap = snapshot_note(row);
        md.push_str(&format!(
            "| {} | {:.4} | {} | {snap} |\n",
            row.id,
            row.scores.mean(),
            blurb(row.id)
        ));
    }
    md.push('\n');
    md.push_str(&metric_header());
    for row in rows {
        md.push_str(&metric_row(row.id, &row.scores, row.scores.mean()));
    }
    md.push_str("\n## Regressions\n\n");
    if regressions.is_empty() {
        md.push_str("None.\n");
    } else {
        for item in regressions {
            md.push_str(&format!("- {item}\n"));
        }
    }
    md.push_str("\n## Improvements not yet in the baseline\n\n");
    if improvements.is_empty() {
        md.push_str("None.\n");
    } else {
        for item in improvements {
            md.push_str(&format!("- {item}\n"));
        }
    }
    md.push_str("\n## Diffs against the expected Markdown\n\n");
    for row in rows {
        let prepared = score::prepare(&row.expected);
        let diff = score::unified_diff(&prepared.body, &row.actual);
        md.push_str(&format!("### {}\n\n```diff\n{diff}```\n\n", row.id));
    }
    md
}

fn metric_header() -> String {
    let mut s = String::from("| fixture |");
    for metric in Scores::metric_names() {
        s.push_str(&format!(" {metric} |"));
    }
    s.push_str(" mean |\n| --- |");
    for _ in Scores::metric_names() {
        s.push_str(" ---: |");
    }
    s.push_str(" ---: |\n");
    s
}

fn metric_row(name: &str, scores: &Scores, mean: f64) -> String {
    let mut s = format!("| {name} |");
    for metric in scores.metrics() {
        s.push(' ');
        s.push_str(&fmt_opt(metric.value));
        s.push_str(" |");
    }
    s.push_str(&format!(" {mean:.4} |\n"));
    s
}

fn fmt_opt(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{v:.4}"),
        None => "—".to_owned(),
    }
}

fn aggregate(rows: &[Row<'_>]) -> Scores {
    let mut sums = [0.0f64; 14];
    let mut counts = [0usize; 14];
    for row in rows {
        for (i, metric) in row.scores.metrics().iter().enumerate() {
            if let Some(v) = metric.value {
                sums[i] += v;
                counts[i] += 1;
            }
        }
    }
    let mut values = [None; 14];
    for i in 0..14 {
        if counts[i] > 0 {
            values[i] = Some((sums[i] / counts[i] as f64 * 10_000.0).round() / 10_000.0);
        }
    }
    Scores::from_options(values)
}

fn aggregate_mean(rows: &[Row<'_>]) -> f64 {
    if rows.is_empty() {
        return 0.0;
    }
    let sum: f64 = rows.iter().map(|r| r.scores.mean()).sum();
    (sum / rows.len() as f64 * 10_000.0).round() / 10_000.0
}

fn snapshot_note(row: &Row<'_>) -> &'static str {
    let path = snapshot_path(row.id);
    match fs::read_to_string(&path) {
        Ok(prev) if prev == row.actual => "matches",
        Ok(_) => "drift",
        Err(_) => "missing",
    }
}

fn print_table(rows: &[Row<'_>], regressions: &[String]) {
    println!("\noutput quality (tolerance {TOLERANCE})");
    println!(
        "{:<14} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "fixture", "text", "word", "para", "hR", "hP", "hLv", "list", "depth", "cell", "span",
        "code", "order", "boil", "link"
    );
    for row in rows {
        let s = &row.scores;
        println!(
            "{:<14} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}  mean {:.4}",
            row.id,
            fmt_opt(Some(s.text_similarity)),
            fmt_opt(Some(s.word_f1)),
            fmt_opt(Some(s.paragraph_f1)),
            fmt_opt(s.heading_recall),
            fmt_opt(s.heading_precision),
            fmt_opt(s.heading_level_accuracy),
            fmt_opt(s.list_recall),
            fmt_opt(s.list_depth_accuracy),
            fmt_opt(s.table_cell_accuracy),
            fmt_opt(s.table_span_accuracy),
            fmt_opt(s.code_recall),
            fmt_opt(Some(s.reading_order)),
            fmt_opt(s.boilerplate_absent),
            fmt_opt(s.link_recall),
            s.mean()
        );
    }
    println!(
        "{:<14} mean {:.4}   regressions {}",
        "aggregate",
        aggregate_mean(rows),
        regressions.len()
    );
    println!(
        "report: {}",
        report_dir().join("quality-report.md").display()
    );
}

fn write_json(
    rows: &[Row<'_>],
    tolerance: f64,
    errors: &[String],
    regressions: &[String],
    improvements: &[String],
) {
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"tolerance\": {tolerance:.4},\n"));
    out.push_str("  \"errors\": [");
    push_string_array(&mut out, errors);
    out.push_str("],\n  \"regressions\": [");
    push_string_array(&mut out, regressions);
    out.push_str("],\n  \"improvements\": [");
    push_string_array(&mut out, improvements);
    out.push_str("],\n  \"aggregate\": ");
    push_metrics(&mut out, &aggregate(rows), aggregate_mean(rows));
    out.push_str(",\n  \"fixtures\": [\n");
    for (i, row) in rows.iter().enumerate() {
        let prepared = score::prepare(&row.expected);
        out.push_str("    {\n      \"id\": ");
        out.push_str(&json_string(row.id));
        out.push_str(",\n      \"metrics\": ");
        push_metrics(&mut out, &row.scores, row.scores.mean());
        out.push_str(",\n      \"diff\": ");
        out.push_str(&json_string(&score::unified_diff(
            &prepared.body,
            &row.actual,
        )));
        out.push_str(",\n      \"actual\": ");
        out.push_str(&json_string(&row.actual));
        out.push_str("\n    }");
        if i + 1 != rows.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    let path = report_dir().join("quality-report.json");
    fs::write(&path, out).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn push_string_array(out: &mut String, items: &[String]) {
    if items.is_empty() {
        return;
    }
    out.push('\n');
    for (i, item) in items.iter().enumerate() {
        out.push_str("    ");
        out.push_str(&json_string(item));
        if i + 1 != items.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ");
}

fn push_metrics(out: &mut String, scores: &Scores, mean: f64) {
    out.push_str("{\n");
    for metric in scores.metrics() {
        out.push_str("    ");
        out.push_str(&json_string(metric.name));
        out.push_str(": ");
        out.push_str(&json_opt(metric.value));
        out.push_str(",\n");
    }
    out.push_str(&format!("    \"mean\": {mean:.4}\n  }}"));
}

fn write_reports(markdown: &str) {
    let dir = report_dir();
    fs::create_dir_all(&dir).expect("report dir");
    fs::write(dir.join("quality-report.md"), markdown).expect("write markdown report");
    fs::write(dir.join("quality-summary.md"), summary_only(markdown)).expect("write summary");
}

/// The step summary stops before the per-fixture diffs.
fn summary_only(markdown: &str) -> String {
    match markdown.split_once("## Diffs against the expected Markdown") {
        Some((head, _)) => head.to_owned(),
        None => markdown.to_owned(),
    }
}

fn write_baseline(path: &Path, rows: &[Row<'_>]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("baseline dir");
    }
    let mut out =
        String::from("{\n  \"version\": 1,\n  \"tolerance\": 0.0200,\n  \"fixtures\": {\n");
    for (i, row) in rows.iter().enumerate() {
        out.push_str("    ");
        out.push_str(&json_string(row.id));
        out.push_str(": {\n");
        let metrics = row.scores.metrics();
        for (j, metric) in metrics.iter().enumerate() {
            out.push_str("      ");
            out.push_str(&json_string(metric.name));
            out.push_str(": ");
            out.push_str(&json_opt(metric.value));
            if j + 1 != metrics.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("    }");
        if i + 1 != rows.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  }\n}\n");
    fs::write(path, out).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn write_snapshots(rows: &[Row<'_>]) {
    let dir = root().join("tests/quality/snapshots");
    fs::create_dir_all(&dir).expect("snapshot dir");
    for row in rows {
        let path = snapshot_path(row.id);
        fs::write(&path, &row.actual).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
}

fn read_baseline(path: &Path) -> (f64, BTreeMap<String, BTreeMap<String, Option<f64>>>) {
    let text = fs::read_to_string(path).unwrap_or_else(|_| {
        panic!(
            "missing {}. Create it with UPDATE_QUALITY_BASELINE=1 cargo test --test quality",
            path.display()
        )
    });
    let value = parse_json(&text).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()));
    let root = value
        .as_object()
        .unwrap_or_else(|| panic!("baseline root must be an object"));
    let tolerance = root
        .iter()
        .find(|(k, _)| k == "tolerance")
        .and_then(|(_, v)| v.as_number())
        .unwrap_or(TOLERANCE);
    let fixtures = root
        .iter()
        .find(|(k, _)| k == "fixtures")
        .and_then(|(_, v)| v.as_object())
        .unwrap_or_else(|| panic!("baseline fixtures must be an object"));
    let mut out = BTreeMap::new();
    for (id, body) in fixtures {
        let metrics = body
            .as_object()
            .unwrap_or_else(|| panic!("fixture {id} must be an object"));
        let mut map = BTreeMap::new();
        for (name, value) in metrics {
            let score = match value {
                Json::Null => None,
                Json::Num(n) => Some(*n),
                _ => panic!("fixture {id} metric {name} must be a number or null"),
            };
            map.insert(name.clone(), score);
        }
        out.insert(id.clone(), map);
    }
    (tolerance, out)
}

fn blurb(id: &str) -> &'static str {
    match id {
        "headings" => "H1-H3 by font size",
        "paragraphs" => "wrapped lines join into paragraphs",
        "emphasis" => "bold, italic, and bold-italic",
        "lists" => "ordered list and nested bullets",
        "table-simple" => "ruled two-column table",
        "table-merged" => "colspan header and an empty cell",
        "columns" => "two-column reading order",
        "margins" => "repeating header, footer, and page numbers",
        "page-break" => "paragraph split across a page break",
        "code" => "monospace fenced block",
        "links" => "URI annotation over anchor text",
        "unicode" => "accents, CJK, and fi/fl ligatures",
        "hyphen" => "hyphenated line-break word",
        "field-note" => "Chrome print of field-note.html",
        _ => "",
    }
}

fn env_flag(name: &str) -> bool {
    matches!(std::env::var(name).as_deref(), Ok("1"))
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_dir() -> PathBuf {
    root().join("tests/fixtures/quality")
}

fn report_dir() -> PathBuf {
    std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root().join("target"))
}

fn snapshot_path(id: &str) -> PathBuf {
    root()
        .join("tests/quality/snapshots")
        .join(format!("{id}.md"))
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
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

fn json_opt(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{v:.4}"),
        None => "null".to_owned(),
    }
}

enum Json {
    Null,
    Num(f64),
    Str(String),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Self::Obj(items) => Some(items),
            _ => None,
        }
    }

    fn as_number(&self) -> Option<f64> {
        match self {
            Self::Num(n) => Some(*n),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

fn parse_json(text: &str) -> Result<Json, String> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
    };
    let value = p.value()?;
    p.skip();
    if p.i != p.s.len() {
        return Err(format!("trailing bytes at {}", p.i));
    }
    Ok(value)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn value(&mut self) -> Result<Json, String> {
        self.skip();
        match self.peek() {
            Some(b'n') => self.ident(b"null").map(|()| Json::Null),
            Some(b'{') => self.object(),
            Some(b'"') => self.string().map(Json::Str),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b) => Err(format!("unexpected byte {b} at {}", self.i)),
            None => Err("unexpected end".to_owned()),
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.bump(b'{')?;
        let mut items = Vec::new();
        loop {
            self.skip();
            if self.peek() == Some(b'}') {
                self.i += 1;
                break;
            }
            let key = self.string()?;
            self.skip();
            self.bump(b':')?;
            let value = self.value()?;
            items.push((key, value));
            self.skip();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    break;
                }
                _ => return Err(format!("expected , or }} at {}", self.i)),
            }
        }
        Ok(Json::Obj(items))
    }

    fn string(&mut self) -> Result<String, String> {
        self.bump(b'"')?;
        let mut out = String::new();
        while let Some(b) = self.peek() {
            self.i += 1;
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    let esc = self.peek().ok_or_else(|| "truncated escape".to_owned())?;
                    self.i += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hex = self.slice(4)?;
                            let code = u32::from_str_radix(hex, 16)
                                .map_err(|_| format!("bad unicode escape at {}", self.i))?;
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        _ => return Err(format!("bad escape at {}", self.i)),
                    }
                }
                b => out.push(b as char),
            }
        }
        Err("unterminated string".to_owned())
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.i += 1;
            }
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| format!("bad number {text}"))
    }

    fn ident(&mut self, lit: &[u8]) -> Result<(), String> {
        if self.s.get(self.i..self.i + lit.len()) == Some(lit) {
            self.i += lit.len();
            Ok(())
        } else {
            Err(format!(
                "expected {} at {}",
                String::from_utf8_lossy(lit),
                self.i
            ))
        }
    }

    fn slice(&mut self, n: usize) -> Result<&str, String> {
        let end = self.i + n;
        let text = self
            .s
            .get(self.i..end)
            .ok_or_else(|| "truncated escape".to_owned())?;
        self.i = end;
        std::str::from_utf8(text).map_err(|_| "non-utf8 escape".to_owned())
    }

    fn bump(&mut self, byte: u8) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected byte {byte} at {}", self.i))
        }
    }

    fn skip(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_parser_reads_the_shape_we_write() {
        let text = r#"{
  "version": 1,
  "tolerance": 0.0200,
  "note": "ok",
  "fixtures": {
    "headings": {
      "text_similarity": 0.5000,
      "link_recall": null
    }
  }
}
"#;
        let value = parse_json(text).expect("parse");
        let root = value.as_object().expect("object");
        let tolerance = root
            .iter()
            .find(|(k, _)| k == "tolerance")
            .and_then(|(_, v)| v.as_number());
        assert_eq!(tolerance, Some(0.02));
        let note = root
            .iter()
            .find(|(k, _)| k == "note")
            .and_then(|(_, v)| v.as_str());
        assert_eq!(note, Some("ok"));
    }
}
