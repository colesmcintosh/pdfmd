//! Ruled (path rects) and borderless (aligned x-columns) GFM tables.

use super::{plain_line, text_size, VLine};
use crate::extract::layout::{PathRect, Span, SpanKind};

fn is_hline(r: &PathRect) -> bool {
    r.h < 2.5 && r.w > 8.0
}

fn is_vline(r: &PathRect) -> bool {
    r.w < 2.5 && r.h > 8.0
}

fn is_cell_rect(r: &PathRect) -> bool {
    r.w > 8.0 && r.h > 8.0 && r.w < 280.0 && r.h < 80.0
}

/// Find every ruled table inside `rects`, clustered into connected components
/// so unrelated page rules (headers, footers, underlines, separate tables) do
/// not merge into a single bounding box. Returned top-to-bottom (`bbox[3]` desc).
pub(super) fn ruled_tables(spans: &[&Span], rects: &[PathRect]) -> Vec<(String, [f32; 4])> {
    let kept: Vec<PathRect> = rects
        .iter()
        .copied()
        .filter(|r| is_hline(r) || is_vline(r) || is_cell_rect(r))
        .collect();
    if kept.len() < 3 {
        return Vec::new();
    }

    let components = rect_components(&kept, 4.0);
    let mut found = Vec::new();
    for comp in components {
        if comp.len() < 3 {
            continue;
        }
        if let Some(tbl) = ruled_table_component(spans, &comp) {
            found.push(tbl);
        }
    }
    found.sort_by(|a, b| {
        b.1[3]
            .partial_cmp(&a.1[3])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Drop any secondary table whose bbox overlaps an earlier higher one.
    let mut out: Vec<(String, [f32; 4])> = Vec::new();
    for item in found {
        let overlaps_prev = out.iter().any(|(_, b)| {
            item.1[0] < b[2] && b[0] < item.1[2] && item.1[1] < b[3] && b[1] < item.1[3]
        });
        if !overlaps_prev {
            out.push(item);
        }
    }
    out
}

fn rect_components(rects: &[PathRect], pad: f32) -> Vec<Vec<PathRect>> {
    let n = rects.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let find = |p: &mut [usize], mut x: usize| {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    };
    for (i, &a) in rects.iter().enumerate() {
        for (j, &b) in rects.iter().enumerate().skip(i + 1) {
            if a.x - pad <= b.x + b.w
                && b.x - pad <= a.x + a.w
                && a.y - pad <= b.y + b.h
                && b.y - pad <= a.y + a.h
            {
                let ra = find(&mut parent, i);
                let rb = find(&mut parent, j);
                if ra != rb {
                    parent[rb] = ra;
                }
            }
        }
    }
    let mut groups: Vec<Vec<PathRect>> = vec![Vec::new(); n];
    for (i, &rect) in rects.iter().enumerate() {
        let r = find(&mut parent, i);
        groups[r].push(rect);
    }
    groups.retain(|g| !g.is_empty());
    groups
}

fn ruled_table_component(spans: &[&Span], rects: &[PathRect]) -> Option<(String, [f32; 4])> {
    let mut v_xs = Vec::new();
    let mut h_xs = Vec::new();
    let mut h_ys = Vec::new();
    let mut v_ys = Vec::new();
    let mut hline_count = 0usize;
    let mut cell_count = 0usize;

    for r in rects {
        if is_cell_rect(r) {
            cell_count += 1;
            v_xs.push(r.x);
            v_xs.push(r.x + r.w);
            h_ys.push(r.y);
            h_ys.push(r.y + r.h);
        } else if is_vline(r) {
            v_xs.push(r.x + r.w * 0.5);
            v_ys.push(r.y);
            v_ys.push(r.y + r.h);
        } else if is_hline(r) {
            hline_count += 1;
            h_xs.push(r.x);
            h_xs.push(r.x + r.w);
            h_ys.push(r.y + r.h * 0.5);
        }
    }

    // Horizontal rules alone (booktabs, underlines, header/footer rules) have
    // no vertical column dividers; leave them to borderless table detection.
    if v_xs.is_empty() || (hline_count < 2 && cell_count < 2) {
        return None;
    }

    // Include outer horizontal bounds only when a real interior vertical edge
    // divides the horizontal span into columns.
    if !h_xs.is_empty() {
        let h_min = h_xs.iter().copied().fold(f32::MAX, f32::min);
        let h_max = h_xs.iter().copied().fold(f32::MIN, f32::max);
        let has_interior_v = v_xs.iter().any(|&x| x > h_min + 4.0 && x < h_max - 4.0);
        if has_interior_v {
            v_xs.push(h_min);
            v_xs.push(h_max);
        }
    }
    if !v_ys.is_empty() && !h_ys.is_empty() {
        let v_min = v_ys.iter().copied().fold(f32::MAX, f32::min);
        let v_max = v_ys.iter().copied().fold(f32::MIN, f32::max);
        if h_ys.iter().any(|&y| y > v_min + 4.0 && y < v_max - 4.0) {
            h_ys.push(v_min);
            h_ys.push(v_max);
        }
    }

    let xs = cluster(v_xs, 3.0);
    let ys = cluster(h_ys, 3.0);
    if xs.len() < 3 || ys.len() < 3 {
        return None;
    }
    let cols = xs.len() - 1;
    let rows = ys.len() - 1;
    if cols > 12 || rows > 48 {
        return None;
    }

    let bbox = [
        *xs.first().unwrap(),
        *ys.first().unwrap(),
        *xs.last().unwrap(),
        *ys.last().unwrap(),
    ];
    let interior_xs = &xs[1..cols];
    let mut inside_spans = 0usize;
    let mut crossing_spans = 0usize;
    for span in spans.iter().filter(|s| s.kind == SpanKind::Text) {
        let cx = span.x + span.width * 0.3;
        let cy = span.y + span.height * 0.3;
        if cx < bbox[0] || cx >= bbox[2] || cy < bbox[1] || cy >= bbox[3] {
            continue;
        }
        inside_spans += 1;
        if interior_xs
            .iter()
            .any(|&bx| span.x < bx - 3.0 && span.x + span.width > bx + 3.0)
        {
            crossing_spans += 1;
        }
    }
    // Prose drawn across decorative rects cuts across the supposed column borders.
    if inside_spans == 0 || crossing_spans * 5 > inside_spans {
        return None;
    }
    // Highlighted/shaded row stripes (`\rowcolor`) only draw cell rects on a
    // subset of rows. If aligned multi-cell rows sit flush above or below this
    // box, defer to borderless detection so the whole table stays intact.
    if has_adjacent_tabular_row(spans, &xs, bbox) {
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
    let multi_cell_rows = grid
        .iter()
        .filter(|row| row.iter().filter(|c| !c.is_empty()).count() >= 2)
        .count();
    let max_cell = grid
        .iter()
        .flat_map(|row| row.iter())
        .map(String::len)
        .max()
        .unwrap_or(0);
    if filled < 3 || multi_cell_rows < 2 || filled * 3 < rows * cols {
        return None;
    }
    if max_cell > 350 && filled * 10 < rows * cols * 7 {
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
    Some((md, bbox))
}

/// True when unruled spans immediately above or below `bbox` form a tabular
/// row aligned with at least two distinct columns of `xs` (without straddling
/// column dividers). That signals `bbox` is a shaded row stripe, not a
/// standalone table.
fn has_adjacent_tabular_row(spans: &[&Span], xs: &[f32], bbox: [f32; 4]) -> bool {
    let cols = xs.len().saturating_sub(1);
    if cols < 2 {
        return false;
    }
    let interior = &xs[1..cols];
    let check_band = |min_y: f32, max_y: f32| -> bool {
        let mut band: Vec<&Span> = spans
            .iter()
            .copied()
            .filter(|s| {
                s.kind == SpanKind::Text
                    && !s.text.trim().is_empty()
                    && s.y >= min_y
                    && s.y <= max_y
                    && s.x >= bbox[0] - 8.0
                    && s.x + s.width <= bbox[2] + 8.0
            })
            .collect();
        if band.len() < 2 {
            return false;
        }
        band.sort_by(|a, b| {
            b.y.partial_cmp(&a.y)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
        });
        // Group adjacent band spans into horizontal lines and check if any
        // line cleanly populates >= 2 distinct columns without crossing borders.
        let mut i = 0;
        while i < band.len() {
            let y0 = band[i].y;
            let size = band[i].font_size.max(6.0);
            let mut j = i + 1;
            while j < band.len() && (band[j].y - y0).abs() < size * 0.45 {
                j += 1;
            }
            let row = &band[i..j];
            let mut hit_cols = vec![false; cols];
            let mut straddled = false;
            for s in row {
                if interior
                    .iter()
                    .any(|&bx| s.x < bx - 3.0 && s.x + s.width > bx + 3.0)
                {
                    straddled = true;
                    break;
                }
                if let Some(c) = cell_index(xs, s.x + s.width * 0.3) {
                    hit_cols[c] = true;
                }
            }
            if !straddled && hit_cols.iter().filter(|&&h| h).count() >= 2 {
                return true;
            }
            i = j;
        }
        false
    };
    // Typical single-row pitch is ~10-18pt; look within 22pt above and below.
    check_band(bbox[3] + 0.5, bbox[3] + 22.0) || check_band(bbox[1] - 22.0, bbox[1] - 0.5)
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
    // Drop trailing lines whose cells all collapsed into one column.
    while grid
        .last()
        .is_some_and(|(_, r)| r.iter().filter(|c| !c.is_empty()).count() < 2)
    {
        grid.pop();
    }
    let consumed = grid.len();
    if consumed < 2 {
        return None;
    }
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
    if looks_like_bullet_list(&grid) || looks_like_split_prose(&grid, &columns) {
        return None;
    }
    let md = render_gfm(&grid)?;
    Some((consumed, md))
}

/// A 2-column grid where column 0 is just a repeated bullet glyph (`o`, `•`, `-`)
/// is an indented list with a wide tab, not a table.
fn looks_like_bullet_list(grid: &[Vec<String>]) -> bool {
    if grid.first().map_or(0, Vec::len) != 2 || grid.len() < 2 {
        return false;
    }
    let markers = grid
        .iter()
        .filter(|r| {
            matches!(
                r[0].as_str(),
                "•" | "◦" | "○" | "●" | "▪" | "▫" | "-" | "*" | "o"
            )
        })
        .count();
    markers * 5 >= grid.len() * 4
}

/// Two lines of justified prose can align at a wide inter-sentence gap
/// (`ing epochs.   The dropout...`), forming a bogus 2x2 grid.
fn looks_like_split_prose(grid: &[Vec<String>], columns: &[(f32, f32)]) -> bool {
    if columns.len() != 2 || grid.len() != 2 {
        return false;
    }
    let gutter = columns[1].0 - columns[0].1;
    let sentence_split = grid.iter().any(|r| {
        (r[0].ends_with('.') || r[0].ends_with('?') || r[0].ends_with('!'))
            && r[1].split_whitespace().count() >= 4
    });
    let hyphen_continuation =
        grid[0][1].ends_with('-') && grid[1][0].starts_with(char::is_lowercase) && gutter < 36.0;
    let narrow_prose = gutter < 24.0
        && grid
            .iter()
            .all(|r| r[1].len() > 24 && r[1].split_whitespace().count() >= 4);
    sentence_split || hyphen_continuation || narrow_prose
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
/// its group) into the nearer row, when that row leaves the column empty, and
/// merge staggered disjoint header sub-lines (`N d_model` centred between
/// `train PPL` and `steps (dev)`).
fn fold_row_labels(grid: &mut Vec<(f32, Vec<String>)>) {
    // Measure row pitch from consecutive full (multi-cell) rows so half-row
    // multirow labels do not pull the median down by half.
    let full_ys: Vec<f32> = grid
        .iter()
        .filter(|(_, r)| r.iter().filter(|c| !c.is_empty()).count() >= 2)
        .map(|(y, _)| *y)
        .collect();
    let mut pitches: Vec<f32> = full_ys
        .windows(2)
        .map(|w| (w[0] - w[1]).abs())
        .filter(|&d| d > 0.5)
        .collect();
    if pitches.is_empty() {
        pitches = grid.windows(2).map(|w| (w[0].0 - w[1].0).abs()).collect();
    }
    pitches.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let Some(&pitch) = pitches.get(pitches.len() / 2) else {
        return;
    };

    // Merge a staggered half-row header (disjoint filled columns right below
    // row 0) into row 0 so the table keeps a single complete header line.
    if grid.len() >= 3 && (grid[0].0 - grid[1].0).abs() < pitch * 0.75 {
        let disjoint = grid[0]
            .1
            .iter()
            .zip(grid[1].1.iter())
            .all(|(a, b)| a.is_empty() || b.is_empty());
        let row1_nonempty = grid[1].1.iter().any(|c| !c.is_empty());
        if disjoint && row1_nonempty {
            let moved = grid.remove(1).1;
            for (dst, src) in grid[0].1.iter_mut().zip(moved) {
                if dst.is_empty() {
                    *dst = src;
                }
            }
        }
    }

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
        if is_caption_line(&plain_line(line)) {
            break;
        }
        if i > 0 {
            let size = line_size(line).max(line_size(&lines[i - 1]));
            // Rows further apart than a blank line end the table.
            if (lines[i - 1].y - line.y).abs() > size * 2.5 {
                break;
            }
        }
        let cells = split_cells(line);
        // Full rows tighter than a line of text whose cells overlap in x are
        // stacked math; horizontally disjoint half-rows are staggered headers.
        if cells.len() >= 2 {
            if let Some(prev) = rows.iter().rev().find(|r| r.cells.len() >= 2) {
                if (prev.y - line.y).abs() < prev.size.max(line_size(line)) * 0.9
                    && rows_horizontally_overlap(&prev.cells, &cells)
                {
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

fn rows_horizontally_overlap(a: &[Cell], b: &[Cell]) -> bool {
    a.iter()
        .any(|ca| b.iter().any(|cb| overlaps((ca.x0, ca.x1), (cb.x0, cb.x1))))
}

fn is_caption_line(line: &str) -> bool {
    let trimmed = line.trim();
    let rest = trimmed
        .strip_prefix("Table ")
        .or_else(|| trimmed.strip_prefix("TABLE "))
        .or_else(|| trimmed.strip_prefix("Figure "))
        .or_else(|| trimmed.strip_prefix("FIGURE "))
        .or_else(|| trimmed.strip_prefix("Fig. "));
    let Some(rest) = rest else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && matches!(rest.as_bytes().get(digits), Some(b':' | b'.'))
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
            if span.x - last.x1 < size * 0.6 {
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
        let found = ruled_tables(&spans, &rects);
        assert_eq!(found.len(), 1);
        let md = &found[0].0;
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
        assert!(ruled_tables(&[&a], &rects).is_empty());
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
        assert!(ruled_tables(&refs, &rects).is_empty());
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
        let found = ruled_tables(&refs, &rects);
        assert_eq!(found.len(), 1);
        let md = &found[0].0;
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

    #[test]
    fn ruled_tables_rejects_horizontal_only_oversized_and_sparse_prose_grids() {
        // Horizontal rules only (booktabs / underlines): rejected.
        let h_only = vec![
            PathRect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 1.0,
            },
            PathRect {
                x: 0.0,
                y: 20.0,
                w: 200.0,
                h: 1.0,
            },
            PathRect {
                x: 40.0,
                y: 40.0,
                w: 120.0,
                h: 1.0,
            },
        ];
        let s = [sp("a", 10.0, 25.0), sp("b", 120.0, 25.0)];
        let refs: Vec<&Span> = s.iter().collect();
        assert!(ruled_tables(&refs, &h_only).is_empty());

        // Single vertical bar + 2 hlines at the outer edge only -> < 3 x-edges.
        let two_edges = vec![
            PathRect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 1.0,
            },
            PathRect {
                x: 0.0,
                y: 20.0,
                w: 100.0,
                h: 1.0,
            },
            PathRect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 20.0,
            },
        ];
        assert!(ruled_tables(&refs, &two_edges).is_empty());

        // Oversized column count (> 12 cols): rejected.
        let mut wide = vec![
            PathRect {
                x: 0.0,
                y: 0.0,
                w: 280.0,
                h: 1.0,
            },
            PathRect {
                x: 0.0,
                y: 20.0,
                w: 280.0,
                h: 1.0,
            },
            PathRect {
                x: 0.0,
                y: 40.0,
                w: 280.0,
                h: 1.0,
            },
        ];
        for c in 0..=13 {
            wide.push(PathRect {
                x: c as f32 * 20.0,
                y: 0.0,
                w: 1.0,
                h: 40.0,
            });
        }
        assert!(ruled_tables(&refs, &wide).is_empty());

        // Giant prose cell in a sparse grid (> 350 chars and < 70% filled): rejected.
        let grid_rects = vec![
            PathRect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 1.0,
            },
            PathRect {
                x: 0.0,
                y: 20.0,
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
                y: 60.0,
                w: 200.0,
                h: 1.0,
            },
            PathRect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 60.0,
            },
            PathRect {
                x: 100.0,
                y: 0.0,
                w: 1.0,
                h: 60.0,
            },
            PathRect {
                x: 200.0,
                y: 0.0,
                w: 1.0,
                h: 60.0,
            },
        ];
        let huge = "longword ".repeat(45);
        let mut a = sp(&huge, 5.0, 45.0);
        a.width = 80.0;
        let b = sp("x", 110.0, 45.0);
        let c = sp("y", 5.0, 25.0);
        let d = sp("z", 110.0, 25.0);
        assert!(ruled_tables(&[&a, &b, &c, &d], &grid_rects).is_empty());

        // Sparse grid with < 2 multi-cell rows: rejected.
        let one_row = [
            sp("h1", 10.0, 45.0),
            sp("h2", 110.0, 45.0),
            sp("v1", 10.0, 25.0),
        ];
        let one_refs: Vec<&Span> = one_row.iter().collect();
        assert!(ruled_tables(&one_refs, &grid_rects).is_empty());
    }

    #[test]
    fn adjacent_tabular_row_distinguishes_row_stripes_from_captions() {
        let xs = [0.0, 100.0, 200.0];
        let bbox = [0.0, 100.0, 200.0, 140.0];
        assert!(!has_adjacent_tabular_row(&[], &[0.0, 100.0], bbox));
        // Unruled multi-cell row flush above the shaded band triggers deferral.
        let a = sp("CISC", 10.0, 148.0);
        let b = sp("13042", 110.0, 148.0);
        assert!(has_adjacent_tabular_row(&[&a, &b], &xs, bbox));
        // A caption span straddling the interior border does NOT count as a tabular row.
        let mut cap = sp("Table 1: caption spanning both columns", 10.0, 148.0);
        cap.width = 180.0;
        let tail = sp("end", 170.0, 148.0);
        assert!(!has_adjacent_tabular_row(&[&cap, &tail], &xs, bbox));
    }

    #[test]
    fn table_filters_catch_captions_bullets_and_split_prose() {
        assert!(is_caption_line("Table 7: NER results"));
        assert!(is_caption_line("Fig. 2. Pipeline"));
        assert!(!is_caption_line("Table manners"));

        assert!(looks_like_bullet_list(&[
            vec!["o".into(), "Item one".into()],
            vec!["o".into(), "Item two".into()],
        ]));
        assert!(!looks_like_bullet_list(&[
            vec!["A".into(), "1".into()],
            vec!["B".into(), "2".into()],
        ]));

        let cols = [(72.0, 130.0), (145.0, 260.0)];
        assert!(looks_like_split_prose(
            &[
                vec!["ing epochs.".into(), "The dropout rate was fixed".into()],
                vec![
                    "kept at 0.1.".into(),
                    "The optimal values vary widely".into()
                ],
            ],
            &cols
        ));
        assert!(looks_like_split_prose(
            &[
                vec!["We use".into(), "hyphen-".into()],
                vec!["ated words".into(), "across lines".into()],
            ],
            &cols
        ));
        assert!(!looks_like_split_prose(
            &[
                vec!["Model".into(), "Score".into()],
                vec!["Base".into(), "27.3".into()],
            ],
            &[(72.0, 110.0), (200.0, 240.0)]
        ));
    }
}
