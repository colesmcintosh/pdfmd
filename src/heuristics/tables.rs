//! Ruled (path rects) and borderless (aligned x-columns) GFM tables.

use super::{plain_line, VLine};
use crate::extract::layout::{PathRect, Span, SpanKind};

pub(super) fn ruled_table(spans: &[&Span], rects: &[PathRect]) -> Option<(String, [f32; 4])> {
    if rects.len() < 3 {
        return None;
    }
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for r in rects {
        let line_like = r.w < 2.5 && r.h > 8.0 || r.h < 2.5 && r.w > 8.0;
        let cell_like = r.w > 8.0 && r.h > 8.0;
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
    let mut grid: Vec<Vec<String>> = vec![vec![String::new(); cols]; rows];
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
        }
    }
    if filled < 2 || filled * 5 < rows * cols {
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

pub(super) fn borderless_run(lines: &[VLine<'_>]) -> Option<(usize, String)> {
    if lines.len() < 2 {
        return None;
    }
    let parsed: Vec<Vec<(f32, String)>> = lines.iter().map(split_cells).collect();
    let n = parsed[0].len();
    if n < 2 {
        return None;
    }
    let mut end = 0;
    for (i, row) in parsed.iter().enumerate() {
        if row.len() != n {
            break;
        }
        let aligned = row
            .iter()
            .zip(parsed[0].iter())
            .all(|(a, b)| (a.0 - b.0).abs() < 18.0);
        if !aligned {
            break;
        }
        end = i + 1;
    }
    if end < 2 {
        return None;
    }
    let grid: Vec<Vec<String>> = parsed[..end]
        .iter()
        .map(|row| row.iter().map(|(_, t)| t.clone()).collect())
        .collect();
    let md = render_gfm(&grid)?;
    Some((end, md))
}

fn split_cells(line: &VLine<'_>) -> Vec<(f32, String)> {
    let mut cells: Vec<(f32, String)> = Vec::new();
    let size = line
        .spans
        .iter()
        .map(|s| s.font_size)
        .fold(12.0f32, f32::max)
        .max(1.0);
    for span in line.spans.iter().filter(|s| s.kind == SpanKind::Text) {
        if let Some(last) = cells.last_mut() {
            if span.x - last.0 < size * 1.4 {
                if span.space_before && !last.1.ends_with(' ') {
                    last.1.push(' ');
                }
                last.1.push_str(&span.text);
                continue;
            }
        }
        cells.push((span.x, span.text.clone()));
    }
    if cells.is_empty() {
        let t = plain_line(line);
        if !t.is_empty() {
            cells.push((line.spans.first().map(|s| s.x).unwrap_or(0.0), t));
        }
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
}
