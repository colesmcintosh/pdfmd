//! Ruled (path rects) and borderless (aligned x-columns) GFM tables.

use super::{text_size, VLine};
use crate::extract::layout::{PathRect, Span, SpanKind};

pub(super) fn ruled_table(spans: &[&Span], rects: &[PathRect]) -> Option<(String, [f32; 4])> {
    if rects.len() < 3 {
        return None;
    }
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for r in rects {
        let line_like = (r.w < 2.5 && r.h > 8.0) || (r.h < 2.5 && r.w > 8.0);
        let cell_like = r.w > 8.0 && r.h > 8.0 && r.w < 280.0 && r.h < 80.0;
        if line_like || cell_like {
            xs.push(r.x);
            xs.push(r.x + r.w);
            ys.push(r.y);
            ys.push(r.y + r.h);
        }
    }
    let xs = cluster(xs, 3.0);
    let ys = cluster(ys, 3.0);
    if xs.len() < 3 || ys.len() < 3 {
        return None;
    }
    let cols = xs.len() - 1;
    let rows = ys.len() - 1;
    if cols > 12 || rows > 48 {
        return None;
    }
    let mut grid: Vec<Vec<String>> = vec![vec![String::new(); cols]; rows];
    // Per cell, each text line's baseline and whether it opens with a figure.
    let mut baselines: Vec<Vec<Vec<(f32, bool)>>> = vec![vec![Vec::new(); cols]; rows];
    let mut filled = 0usize;
    for span in spans.iter().filter(|s| s.kind == SpanKind::Text) {
        let Some(c) = cell_index(&xs, span.x + span.width * 0.3) else {
            continue;
        };
        let Some(r) = cell_index(&ys, span.y + span.height * 0.3) else {
            continue;
        };
        // PDF y grows up; `ys` is ascending so row 0 is the bottom band.
        let r = rows - 1 - r;
        if r < rows && c < cols {
            if !grid[r][c].is_empty() {
                grid[r][c].push(' ');
            } else {
                filled += 1;
            }
            if span.space_before && !grid[r][c].is_empty() && !grid[r][c].ends_with(' ') {
                grid[r][c].push(' ');
            }
            grid[r][c].push_str(span.text.trim());
            let seen = &mut baselines[r][c];
            if seen
                .iter()
                .all(|(y, _)| (y - span.y).abs() > span.font_size * 0.45)
            {
                let figure = span
                    .text
                    .trim_start()
                    .starts_with(|c: char| c.is_ascii_digit());
                seen.push((span.y, figure));
            }
        }
    }
    if filled < 2 || filled * 5 < rows * cols {
        return None;
    }
    // Rules around groups of records, not around each row: in one band, a
    // column of figures stacks as many lines as another cell. The text's own
    // rows read better than one cell per band. Wrapped words don't count.
    let stacked = baselines.iter().any(|band| {
        band.iter().any(|cell| {
            cell.len() >= 2
                && cell.iter().all(|&(_, figure)| figure)
                && band
                    .iter()
                    .filter(|other| other.len() == cell.len())
                    .count()
                    >= 2
        })
    });
    if stacked {
        return None;
    }
    let md = render_gfm(&grid)?;
    let bbox = [
        *xs.first().unwrap(),
        *ys.first().unwrap(),
        *xs.last().unwrap(),
        *ys.last().unwrap(),
    ];
    Some((md, bbox))
}

/// A borderless table: consecutive rows of short cells whose columns line
/// up. Rows may leave cells empty (a row label centred on a group of rows,
/// a blank header corner), so columns come from the fullest rows and every
/// other row's cells are placed by horizontal overlap.
fn borderless_run(lines: &[VLine<'_>]) -> Option<(usize, String)> {
    let rows = table_rows(lines);
    if rows.iter().filter(|r| r.cells.len() >= 2).count() < 2 {
        return None;
    }
    let widest = rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
    let mut columns: Vec<(f32, f32)> = Vec::new();
    let fullest = rows.iter().filter(|r| r.cells.len() == widest);
    for cell in fullest.flat_map(|r| &r.cells) {
        match columns
            .iter_mut()
            .find(|c| overlaps(**c, (cell.x0, cell.x1)))
        {
            Some(c) => *c = (c.0.min(cell.x0), c.1.max(cell.x1)),
            None => columns.push((cell.x0, cell.x1)),
        }
    }
    columns.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if columns.len() < 2 {
        return None;
    }

    let mut grid: Vec<(f32, Vec<String>)> = Vec::new();
    for row in &rows {
        let Some(placed) = row
            .cells
            .iter()
            .map(|cell| column_of(&columns, cell))
            .collect::<Option<Vec<usize>>>()
        else {
            break;
        };
        let mut out = vec![String::new(); columns.len()];
        for (cell, c) in row.cells.iter().zip(placed) {
            if !out[c].is_empty() {
                out[c].push(' ');
            }
            out[c].push_str(cell.text.trim());
        }
        grid.push((row.y, out));
    }
    let consumed = grid.len();
    // Prose broken at wide word gaps has no gutters that hold from row to
    // row; a real table's cells stay inside their columns (a spanning
    // header or two aside).
    let straddles = |row: &Row| {
        row.cells
            .iter()
            .any(|cell| columns.iter().filter(|&&c| overlap(cell, c) > 1.0).count() > 1)
    };
    let placed = &rows[..consumed];
    let crossing = placed.iter().filter(|r| straddles(r)).count();
    let clean = placed
        .iter()
        .filter(|r| r.cells.len() >= 2 && !straddles(r))
        .count();
    if clean < 2 || crossing * 10 > consumed * 3 {
        return None;
    }
    fold_row_labels(&mut grid);
    let grid: Vec<Vec<String>> = grid.into_iter().map(|(_, r)| r).collect();
    if grid
        .iter()
        .filter(|r| r.iter().filter(|c| !c.is_empty()).count() >= 2)
        .count()
        < 2
    {
        return None;
    }
    let md = render_gfm(&grid)?;
    Some((consumed, md))
}

/// The borderless table starting at `lines[i]`, if any. The stacked
/// pieces of a displayed formula also line up in columns, so nothing
/// hanging just under a numbered equation counts.
pub(super) fn table_at(lines: &[VLine<'_>], i: usize) -> Option<(usize, String)> {
    let table = borderless_run(&lines[i..])?;
    if i > 0 {
        let prev = &lines[i - 1];
        let numbered = split_cells(prev)
            .last()
            .is_some_and(|c| is_equation_number(&c.text));
        if numbered && (prev.y - lines[i].y).abs() < line_size(prev) * 2.0 {
            return None;
        }
    }
    Some(table)
}

struct Cell {
    x0: f32,
    x1: f32,
    text: String,
}

struct Row {
    y: f32,
    size: f32,
    cells: Vec<Cell>,
}

/// Fold a lone label drawn between two rows (a `\multirow` cell centred on
/// its group) into the nearer row, when that row leaves the column empty.
fn fold_row_labels(grid: &mut Vec<(f32, Vec<String>)>) {
    let mut pitches: Vec<f32> = grid.windows(2).map(|w| (w[0].0 - w[1].0).abs()).collect();
    pitches.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let Some(&pitch) = pitches.get(pitches.len() / 2) else {
        return;
    };
    let mut i = 1;
    while i + 1 < grid.len() {
        let filled: Vec<usize> = (0..grid[i].1.len())
            .filter(|&c| !grid[i].1[c].is_empty())
            .collect();
        let [c] = filled[..] else {
            i += 1;
            continue;
        };
        let y = grid[i].0;
        let target = [i - 1, i + 1]
            .into_iter()
            .filter(|&j| grid[j].1[c].is_empty())
            .min_by(|&a, &b| {
                let (da, db) = ((grid[a].0 - y).abs(), (grid[b].0 - y).abs());
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            });
        match target {
            Some(j) if (grid[j].0 - y).abs() < pitch * 0.75 => {
                let label = std::mem::take(&mut grid[i].1[c]);
                grid[j].1[c] = label;
                grid.remove(i);
            }
            _ => i += 1,
        }
    }
}

/// Leading lines that read as table rows: every cell short, and no gap
/// between rows much taller than the text. A single-cell line is kept
/// only between multi-cell rows.
fn table_rows(lines: &[VLine<'_>]) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            let size = line_size(line).max(line_size(&lines[i - 1]));
            // Rows further apart than a blank line end the table.
            if (lines[i - 1].y - line.y).abs() > size * 2.5 {
                break;
            }
        }
        let cells = split_cells(line);
        // Full rows tighter than a line of text are stacked math. Lone
        // labels may sit half a row off, between the rows they span.
        if cells.len() >= 2 {
            if let Some(prev) = rows.iter().rev().find(|r| r.cells.len() >= 2) {
                if (prev.y - line.y).abs() < prev.size.max(line_size(line)) * 0.9 {
                    break;
                }
            }
        }
        let fits = if cells.len() >= 2 {
            is_table_row(&cells)
        } else {
            !rows.is_empty() && cells.len() == 1 && short_cell(&cells[0].text)
        };
        if !fits {
            break;
        }
        rows.push(Row {
            y: line.y,
            size: line_size(line),
            cells,
        });
        if rows.len() >= 64 {
            break;
        }
    }
    while rows.last().is_some_and(|r| r.cells.len() < 2) {
        rows.pop();
    }
    rows
}

fn line_size(line: &VLine<'_>) -> f32 {
    text_size(line).max(1.0)
}

fn overlaps(a: (f32, f32), b: (f32, f32)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// The column a cell sits in: the one it overlaps most, or the nearest
/// within a few points for a cell that slips into a gutter.
fn column_of(columns: &[(f32, f32)], cell: &Cell) -> Option<usize> {
    let (best, amount) = columns
        .iter()
        .enumerate()
        .map(|(i, &c)| (i, overlap(cell, c)))
        .fold((0, f32::MIN), |acc, x| if x.1 > acc.1 { x } else { acc });
    (amount > -6.0).then_some(best)
}

/// Horizontal overlap of a cell with a column; negative is the gap between.
fn overlap(cell: &Cell, column: (f32, f32)) -> f32 {
    cell.x1.min(column.1) - cell.x0.max(column.0)
}

fn is_table_row(row: &[Cell]) -> bool {
    if row.len() < 2 {
        return false;
    }
    if !row.iter().all(|c| short_cell(&c.text)) {
        return false;
    }
    if row.len() == 2 {
        let gap = row[1].x0 - row[0].x0;
        // A page-level column gutter with long-ish cells is prose, not a table.
        if gap > 90.0 && row.iter().any(|c| c.text.len() > 20) {
            return false;
        }
        // `3.1  Datasets`: a section number set off by an em quad.
        if is_section_number(&row[0].text) && row[1].text.starts_with(char::is_uppercase) {
            return false;
        }
    }
    // A displayed equation ends in its number, `(11)`.
    !row.last().is_some_and(|c| is_equation_number(&c.text))
}

fn is_section_number(t: &str) -> bool {
    let t = t.trim().trim_end_matches('.');
    let mut parts = t.split('.');
    let first = parts.next().unwrap_or("");
    let lead = first.len() == 1 && first.starts_with(|c: char| c.is_ascii_uppercase())
        || (!first.is_empty() && first.len() <= 2 && first.bytes().all(|b| b.is_ascii_digit()));
    lead && parts.all(|p| !p.is_empty() && p.len() <= 2 && p.bytes().all(|b| b.is_ascii_digit()))
}

fn is_equation_number(t: &str) -> bool {
    let Some(inner) = t.trim().strip_prefix('(').and_then(|t| t.strip_suffix(')')) else {
        return false;
    };
    let digits = inner.trim_end_matches(|c: char| c.is_ascii_lowercase());
    !digits.is_empty() && digits.len() <= 3 && digits.bytes().all(|b| b.is_ascii_digit())
}

fn short_cell(t: &str) -> bool {
    let t = t.trim();
    t.len() <= 48 && t.split_whitespace().count() <= 8
}

fn split_cells(line: &VLine<'_>) -> Vec<Cell> {
    let mut cells: Vec<Cell> = Vec::new();
    let size = line
        .spans
        .iter()
        .map(|s| s.font_size)
        .fold(12.0f32, f32::max)
        .max(1.0);
    for span in line.spans.iter().filter(|s| s.kind == SpanKind::Text) {
        if let Some(last) = cells.last_mut() {
            if span.x - last.x0 < size * 1.4 || span.x - last.x1 < span.font_size * 0.6 {
                if span.space_before && !last.text.ends_with(' ') {
                    last.text.push(' ');
                }
                last.text.push_str(&span.text);
                last.x1 = last.x1.max(span.x + span.width);
                continue;
            }
        }
        cells.push(Cell {
            x0: span.x,
            x1: span.x + span.width,
            text: span.text.clone(),
        });
    }
    cells
}

fn cluster(mut vals: Vec<f32>, eps: f32) -> Vec<f32> {
    if vals.is_empty() {
        return vals;
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = vec![vals[0]];
    let mut acc = vals[0];
    let mut n = 1.0;
    for v in vals.into_iter().skip(1) {
        if v - acc / n <= eps {
            acc += v;
            n += 1.0;
            *out.last_mut().unwrap() = acc / n;
        } else {
            out.push(v);
            acc = v;
            n = 1.0;
        }
    }
    out
}

fn cell_index(edges: &[f32], v: f32) -> Option<usize> {
    (0..edges.len().saturating_sub(1)).find(|&i| v >= edges[i] && v < edges[i + 1])
}

fn render_gfm(rows: &[Vec<String>]) -> Option<String> {
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols < 2 || rows.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        out.push('|');
        for c in 0..cols {
            let cell = row
                .get(c)
                .map(|s| s.replace('|', "\\|"))
                .unwrap_or_default();
            out.push(' ');
            out.push_str(cell.trim());
            out.push_str(" |");
        }
        out.push('\n');
        if i == 0 {
            out.push('|');
            for _ in 0..cols {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    Some(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::layout::Span;

    fn sp(text: &str, x: f32, y: f32) -> Span {
        Span {
            text: text.into(),
            x,
            y,
            width: 20.0,
            height: 10.0,
            font_size: 10.0,
            bold: false,
            italic: false,
            mono: false,
            kind: SpanKind::Text,
            mcid: None,
            space_before: false,
        }
    }

    #[test]
    fn ruled_grid_becomes_gfm() {
        let rects = vec![
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
        ];
        let a = sp("Name", 10.0, 28.0);
        let b = sp("Age", 60.0, 28.0);
        let c = sp("Ada", 10.0, 8.0);
        let d = sp("36", 60.0, 8.0);
        let spans = [&a, &b, &c, &d];
        let (md, _) = ruled_table(&spans, &rects).expect("table");
        assert!(md.contains("| Name | Age |"));
        assert!(md.contains("| Ada | 36 |"));
        assert!(md.contains("| --- | --- |"));
    }

    #[test]
    fn sparse_rects_are_not_tables() {
        let rects = vec![PathRect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        }];
        let a = sp("x", 1.0, 1.0);
        assert!(ruled_table(&[&a], &rects).is_none());
    }

    #[test]
    fn ruled_band_of_stacked_records_is_left_to_the_text() {
        // Rules frame two records per band: names and scores stack alike.
        let rects = vec![
            PathRect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 1.0,
            },
            PathRect {
                x: 0.0,
                y: 40.0,
                w: 200.0,
                h: 1.0,
            },
            PathRect {
                x: 0.0,
                y: 80.0,
                w: 200.0,
                h: 1.0,
            },
            PathRect {
                x: 100.0,
                y: 0.0,
                w: 1.0,
                h: 80.0,
            },
        ];
        let spans = [
            sp("Model", 10.0, 60.0),
            sp("Score", 110.0, 60.0),
            sp("Base", 10.0, 28.0),
            sp("27.3", 110.0, 28.0),
            sp("Big", 10.0, 14.0),
            sp("28.4", 110.0, 14.0),
        ];
        let refs: Vec<&Span> = spans.iter().collect();
        assert!(ruled_table(&refs, &rects).is_none());
        // Wrapped words in two cells are one record, not a stack.
        let spans = [
            sp("Model", 10.0, 60.0),
            sp("Status", 110.0, 60.0),
            sp("Concept", 10.0, 28.0),
            sp("Fully", 110.0, 28.0),
            sp("Studies", 10.0, 14.0),
            sp("Compliant", 110.0, 14.0),
        ];
        let refs: Vec<&Span> = spans.iter().collect();
        let (md, _) = ruled_table(&refs, &rects).expect("table");
        assert!(md.contains("| Concept Studies | Fully Compliant |"), "{md}");
    }

    #[test]
    fn label_folding_leaves_distant_or_crowded_rows_alone() {
        let row = |y: f32, cells: &[&str]| (y, cells.iter().map(|c| c.to_string()).collect());
        let mut one: Vec<(f32, Vec<String>)> = vec![row(10.0, &["a", "b"])];
        fold_row_labels(&mut one);
        assert_eq!(one.len(), 1);
        // A lone cell a full row from its neighbours is a row of its own,
        // and one whose neighbours already fill that column stays put.
        let mut grid = vec![
            row(100.0, &["", "m0"]),
            row(86.0, &["Group", ""]),
            row(72.0, &["", "m1"]),
            row(58.0, &["x", "m2"]),
            row(51.0, &["y", ""]),
            row(44.0, &["z", "m3"]),
        ];
        fold_row_labels(&mut grid);
        assert_eq!(grid.len(), 6);
    }

    #[test]
    fn section_and_equation_numbers() {
        for yes in ["3", "3.", "3.1", "12.4.2", "A", "A.1", "B.2."] {
            assert!(is_section_number(yes), "{yes}");
        }
        for no in ["", "a", "AB", "3.x", "123", "1..2"] {
            assert!(!is_section_number(no), "{no}");
        }
        for yes in ["(1)", "(11)", "(12a)", " (3) "] {
            assert!(is_equation_number(yes), "{yes}");
        }
        for no in ["()", "(a)", "(1234)", "11", "(1", "(x1)"] {
            assert!(!is_equation_number(no), "{no}");
        }
    }
}
