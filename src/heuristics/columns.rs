//! Page columns: find gutters where prose leaves the page empty, then slice
//! the page into horizontal bands so full-width content (tables, captions,
//! title blocks) is read whole instead of being cut down a gutter.

use super::{visual_lines, VLine};
use crate::extract::layout::{Span, SpanKind};

/// A stretch of one line's text with no gap wider than about an em. Prose
/// lines are one long run; table rows break into short cell-sized runs.
#[derive(Debug, Clone, Copy)]
struct Run {
    x0: f32,
    x1: f32,
}

impl Run {
    fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    fn crosses(&self, gap: f32) -> bool {
        self.x0 < gap - 1.0 && self.x1 > gap + 1.0
    }
}

/// A horizontal slice of the page: multi-column (`split`) or one full-width
/// column. `top`/`bottom` are the baselines of its first and last line.
pub(super) struct Band<'a> {
    pub(super) spans: Vec<&'a Span>,
    pub(super) split: bool,
    pub(super) top: f32,
    pub(super) bottom: f32,
}

/// Gutter x positions and the page cut into bands, top to bottom.
pub(super) fn page_bands<'a>(spans: &[&'a Span]) -> (Vec<f32>, Vec<Band<'a>>) {
    let lines = visual_lines(spans, &vec![0usize; spans.len()]);
    let runs: Vec<Vec<Run>> = lines.iter().map(line_runs).collect();
    let Some((gaps, long)) = gutters(&runs) else {
        return (Vec::new(), vec![whole_page(spans, false)]);
    };

    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        Wide,
        Prose,
        Neutral,
    }
    let kinds: Vec<Kind> = runs
        .iter()
        .map(|line| {
            if line.iter().any(|r| gaps.iter().any(|&g| r.crosses(g))) {
                Kind::Wide
            } else if line.iter().any(|r| r.width() >= long) {
                Kind::Prose
            } else {
                Kind::Neutral
            }
        })
        .collect();

    // Consecutive non-wide lines are columns only when enough of them are
    // prose; a stretch of short table cells stays full-width.
    let mut stretches: Vec<(bool, usize)> = Vec::new();
    let mut i = 0;
    while i < kinds.len() {
        let start = i;
        if kinds[i] == Kind::Wide {
            while i < kinds.len() && kinds[i] == Kind::Wide {
                i += 1;
            }
            push_stretch(&mut stretches, false, i - start);
            continue;
        }
        while i < kinds.len() && kinds[i] != Kind::Wide {
            i += 1;
        }
        let prose = kinds[start..i]
            .iter()
            .filter(|&&k| k == Kind::Prose)
            .count();
        let split = prose >= 2 && prose * 10 >= (i - start) * 3;
        push_stretch(&mut stretches, split, i - start);
    }
    // One stray full-width line inside prose must not break the reading order.
    let n = stretches.len();
    let stretches = merge_stretches(stretches.iter().enumerate().map(|(i, &(split, len))| {
        let sandwiched = i > 0 && i + 1 < n && stretches[i - 1].0 && stretches[i + 1].0;
        (split || (len == 1 && sandwiched), len)
    }));

    if stretches.len() == 1 {
        let split = stretches[0].0;
        return (gaps, vec![whole_page(spans, split)]);
    }
    let mut bands = Vec::new();
    let mut rest = lines.as_slice();
    for (split, len) in stretches {
        let (band_lines, tail) = rest.split_at(len);
        rest = tail;
        bands.push(Band {
            spans: band_lines
                .iter()
                .flat_map(|l| l.spans.iter().copied())
                .collect(),
            split,
            top: band_lines[0].y,
            bottom: band_lines[len - 1].y,
        });
    }
    (gaps, bands)
}

/// Assign each span to the column its left edge falls in.
pub(super) fn split_columns<'a>(spans: &[&'a Span], gaps: &[f32]) -> Vec<Vec<&'a Span>> {
    let mut cols: Vec<Vec<&Span>> = vec![Vec::new(); gaps.len() + 1];
    for s in spans {
        cols[gaps.iter().take_while(|&&g| s.x >= g).count()].push(s);
    }
    cols.retain(|c| !c.is_empty());
    cols
}

fn line_runs(line: &VLine<'_>) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let text = line
        .spans
        .iter()
        .filter(|s| s.kind == SpanKind::Text && !s.text.trim().is_empty());
    for s in text {
        let (x0, x1) = (s.x, s.x + s.width);
        match runs.last_mut() {
            Some(run) if x0 - run.x1 < s.font_size.max(1.0) => run.x1 = run.x1.max(x1),
            _ => runs.push(Run { x0, x1 }),
        }
    }
    runs
}

/// Gutters are x ranges that almost no long run covers, with a real share
/// of long runs wholly on each side. Returns them with the length a run
/// needs to count as prose.
fn gutters(lines: &[Vec<Run>]) -> Option<(Vec<f32>, f32)> {
    let all = lines.iter().flatten();
    let min = all.clone().map(|r| r.x0).fold(f32::MAX, f32::min);
    let max = all.map(|r| r.x1).fold(f32::MIN, f32::max);
    let extent = max - min;
    if extent.is_nan() || extent < 180.0 {
        return None;
    }
    let long = extent * 0.2;
    let prose: Vec<Run> = lines
        .iter()
        .flatten()
        .copied()
        .filter(|r| r.width() >= long)
        .collect();
    if prose.len() < 8 {
        return None;
    }

    const STEP: f32 = 2.0;
    let buckets = (extent / STEP).ceil() as usize + 1;
    let mut cover = vec![0usize; buckets];
    for r in &prose {
        let a = ((r.x0 - min) / STEP) as usize;
        let b = (((r.x1 - min) / STEP) as usize).min(buckets - 1);
        for c in &mut cover[a..=b] {
            *c += 1;
        }
    }
    // Full-width captions and titles may cross a real gutter; a few do.
    let allowed = prose.len() * 15 / 100;
    let mut gaps = Vec::new();
    let mut i = 1;
    while i + 1 < buckets {
        if cover[i] > allowed {
            i += 1;
            continue;
        }
        let start = i;
        while i + 1 < buckets && cover[i] <= allowed {
            i += 1;
        }
        if (i - start) as f32 * STEP < 6.0 {
            continue;
        }
        let gap = min + (start + i) as f32 * STEP / 2.0;
        let left = prose.iter().filter(|r| r.x1 <= gap).count();
        let right = prose.iter().filter(|r| r.x0 >= gap).count();
        if left * 5 >= prose.len() && right * 5 >= prose.len() {
            gaps.push(gap);
        }
    }
    gaps.truncate(2);
    (!gaps.is_empty()).then_some((gaps, long))
}

fn whole_page<'a>(spans: &[&'a Span], split: bool) -> Band<'a> {
    Band {
        spans: spans.to_vec(),
        split,
        top: f32::MAX,
        bottom: f32::MIN,
    }
}

fn push_stretch(stretches: &mut Vec<(bool, usize)>, split: bool, len: usize) {
    match stretches.last_mut() {
        Some(last) if last.0 == split => last.1 += len,
        _ => stretches.push((split, len)),
    }
}

fn merge_stretches(stretches: impl Iterator<Item = (bool, usize)>) -> Vec<(bool, usize)> {
    let mut out = Vec::new();
    for (split, len) in stretches {
        push_stretch(&mut out, split, len);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(x0: f32, x1: f32) -> Vec<Run> {
        vec![Run { x0, x1 }]
    }

    #[test]
    fn single_column_prose_has_no_gutter() {
        let lines: Vec<Vec<Run>> = (0..20).map(|_| run(72.0, 540.0)).collect();
        assert!(gutters(&lines).is_none());
    }

    #[test]
    fn two_columns_share_a_gutter_even_under_a_caption() {
        let mut lines: Vec<Vec<Run>> = (0..20)
            .map(|_| {
                vec![
                    Run {
                        x0: 72.0,
                        x1: 290.0,
                    },
                    Run {
                        x0: 306.0,
                        x1: 524.0,
                    },
                ]
            })
            .collect();
        lines.push(run(72.0, 524.0));
        let (gaps, long) = gutters(&lines).expect("gutter");
        assert_eq!(gaps.len(), 1);
        assert!(gaps[0] > 290.0 && gaps[0] < 306.0, "{gaps:?}");
        assert!(long < 218.0);
    }

    #[test]
    fn short_table_cells_are_not_columns() {
        let lines: Vec<Vec<Run>> = (0..20)
            .map(|_| {
                (0..6)
                    .map(|c| Run {
                        x0: 72.0 + c as f32 * 75.0,
                        x1: 110.0 + c as f32 * 75.0,
                    })
                    .collect()
            })
            .collect();
        assert!(gutters(&lines).is_none());
    }
}
