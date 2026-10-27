//! Scanline fill for engraving ("Scan" layers).
//!
//! Horizontal lines every `interval` µm, burned in alternating directions,
//! starting at the line furthest from the origin in Y (the order LightBurn
//! uses). Outlines form shapes; each shape is filled with its own rule
//! (even-odd, or non-zero as fonts and SVG need for overlapping contours)
//! and the shapes are united, so overlapping shapes never cancel out.

/// Which points an outline set covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillRule {
    /// Inside where a line out crosses the outlines an odd number of times.
    #[default]
    EvenOdd,
    /// Inside where the outlines wind around the point (fonts, SVG default):
    /// overlapping contours of one letter stay filled.
    NonZero,
}

/// The next `len` outlines form one shape, filled with `rule`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub len: usize,
    pub rule: FillRule,
}

/// One engraving line: the spans to burn, in burn order. Each span runs
/// from `.0` to `.1` (so x decreases on lines burned right to left).
#[derive(Debug, Clone, PartialEq)]
pub struct ScanLine {
    pub y: i32,
    pub spans: Vec<(i32, i32)>,
}

/// Where the even-odd fill of `polys` covers the horizontal line at `y`,
/// as sorted, non-overlapping (x_start, x_end) pairs. Polygons are closed
/// implicitly. Coordinates in µm.
pub fn spans_at(polys: &[Vec<(i32, i32)>], y: f64) -> Vec<(i32, i32)> {
    spans_shapes(polys, &[], y)
}

/// Crossings of the line at `y`: (x, +1 going down / -1 going up).
fn crossings(polys: &[Vec<(i32, i32)>], y: f64) -> Vec<(f64, i32)> {
    let mut xs = Vec::new();
    for poly in polys.iter().filter(|p| p.len() >= 3) {
        let n = poly.len();
        for i in 0..n {
            let (x0, y0) = poly[i];
            let (x1, y1) = poly[(i + 1) % n];
            let (y0, y1, x0, x1) = (y0 as f64, y1 as f64, x0 as f64, x1 as f64);
            // Half-open in y, so a vertex exactly on the line counts once.
            if (y0 <= y && y < y1) || (y1 <= y && y < y0) {
                xs.push((x0 + (y - y0) / (y1 - y0) * (x1 - x0), if y1 > y0 { 1 } else { -1 }));
            }
        }
    }
    xs.sort_by(|a, b| a.0.total_cmp(&b.0));
    xs
}

/// Spans of one shape.
fn shape_spans(polys: &[Vec<(i32, i32)>], rule: FillRule, y: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut winding = 0;
    let mut start = 0.0;
    for (x, dir) in crossings(polys, y) {
        let inside = |w: i32| match rule {
            FillRule::EvenOdd => w % 2 != 0,
            FillRule::NonZero => w != 0,
        };
        let was = inside(winding);
        winding += match rule {
            FillRule::EvenOdd => 1,
            FillRule::NonZero => dir,
        };
        match (was, inside(winding)) {
            (false, true) => start = x,
            (true, false) => out.push((start, x)),
            _ => {}
        }
    }
    out
}

/// Where the shapes cover the line at `y`, united. `shapes` splits `polys`
/// into shapes in order; outlines not covered by it (all of them when it is
/// empty) form one even-odd shape.
pub fn spans_shapes(polys: &[Vec<(i32, i32)>], shapes: &[Shape], y: f64) -> Vec<(i32, i32)> {
    let mut all: Vec<(f64, f64)> = Vec::new();
    let mut start = 0;
    for s in shapes {
        let end = (start + s.len).min(polys.len());
        all.extend(shape_spans(&polys[start..end], s.rule, y));
        start = end;
    }
    if start < polys.len() {
        all.extend(shape_spans(&polys[start..], FillRule::EvenOdd, y));
    }
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Unite overlapping spans.
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (a, b) in all {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged.into_iter().map(|(a, b)| (a.round() as i32, b.round() as i32)).filter(|(a, b)| b > a).collect()
}

/// Fill `polys` (one even-odd shape) with lines `interval` µm apart.
pub fn scan(polys: &[Vec<(i32, i32)>], interval: i32) -> Vec<ScanLine> {
    scan_shapes(polys, &[], interval)
}

/// Fill shapes (see [`spans_shapes`]) with lines `interval` µm apart, half
/// an interval inside the outline's top and bottom. Empty lines are left out.
pub fn scan_shapes(polys: &[Vec<(i32, i32)>], shapes: &[Shape], interval: i32) -> Vec<ScanLine> {
    assert!(interval > 0);
    let Some((y_min, y_max)) = polys.iter().flatten().fold(None, |acc: Option<(i32, i32)>, &(_, y)| {
        Some(acc.map_or((y, y), |(lo, hi)| (lo.min(y), hi.max(y))))
    }) else {
        return Vec::new();
    };

    let mut lines = Vec::new();
    let mut leftwards = true;
    let mut y = y_max - interval / 2;
    while y > y_min {
        let mut spans = spans_shapes(polys, shapes, y as f64);
        if !spans.is_empty() {
            if leftwards {
                spans.reverse();
                for s in &mut spans {
                    *s = (s.1, s.0);
                }
            }
            lines.push(ScanLine { y, spans });
            leftwards = !leftwards;
        }
        y -= interval;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: i32, y: i32, size: i32) -> Vec<(i32, i32)> {
        vec![(x, y), (x + size, y), (x + size, y + size), (x, y + size)]
    }

    #[test]
    fn square_with_hole() {
        let polys = vec![square(0, 0, 10_000), square(4_000, 4_000, 2_000)];
        assert_eq!(spans_at(&polys, 5_000.0), vec![(0, 4_000), (6_000, 10_000)]);
        assert_eq!(spans_at(&polys, 1_000.0), vec![(0, 10_000)]);
        assert_eq!(spans_at(&polys, 11_000.0), vec![]);
    }

    #[test]
    fn overlapping_contours_with_non_zero() {
        // A "t": stem and crossbar drawn as two overlapping contours, same direction.
        let stem = square(4_000, 0, 2_000);
        let bar = vec![(0, 500), (10_000, 500), (10_000, 1_500), (0, 1_500)];
        let polys = vec![stem.clone(), bar.clone()];
        let even_odd = spans_at(&polys, 1_000.0);
        assert_eq!(even_odd, vec![(0, 4_000), (6_000, 10_000)], "even-odd leaves a hole");
        let non_zero = spans_shapes(&polys, &[Shape { len: 2, rule: FillRule::NonZero }], 1_000.0);
        assert_eq!(non_zero, vec![(0, 10_000)]);
        // A real hole (opposite direction) stays a hole with non-zero.
        let mut hole = square(4_000, 4_000, 2_000);
        hole.reverse();
        let ring = vec![square(0, 0, 10_000), hole];
        assert_eq!(spans_shapes(&ring, &[Shape { len: 2, rule: FillRule::NonZero }], 5_000.0), vec![(0, 4_000), (6_000, 10_000)]);
    }

    #[test]
    fn separate_shapes_are_united() {
        // Two overlapping squares as two even-odd shapes: no hole where they overlap.
        let polys = vec![square(0, 0, 6_000), square(4_000, 0, 6_000)];
        let shapes = [Shape { len: 1, rule: FillRule::EvenOdd }, Shape { len: 1, rule: FillRule::EvenOdd }];
        assert_eq!(spans_shapes(&polys, &shapes, 1_000.0), vec![(0, 10_000)]);
    }

    #[test]
    fn lines_alternate_and_cover() {
        let lines = scan(&[square(0, 0, 1_000)], 100);
        assert_eq!(lines.len(), 10);
        assert_eq!(lines[0].y, 950);
        assert_eq!(lines[9].y, 50);
        assert_eq!(lines[0].spans, vec![(1_000, 0)]);
        assert_eq!(lines[1].spans, vec![(0, 1_000)]);
    }

    #[test]
    fn leftward_lines_reverse_span_order() {
        let polys = vec![square(0, 0, 10_000), square(4_000, 4_000, 2_000)];
        // Lines from the top: 9.5, 8.5, 7.5, 6.5, 5.5 mm; the 5th (even index) runs right to left.
        let line = scan(&polys, 1_000).into_iter().find(|l| l.y == 5_500).unwrap();
        assert_eq!(line.spans, vec![(10_000, 6_000), (4_000, 0)]);
    }
}
