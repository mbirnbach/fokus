//! Scanline fill for engraving ("Scan" layers).
//!
//! Outlines are filled with the even-odd rule: horizontal lines every
//! `interval` µm, burned in alternating directions, starting at the line
//! furthest from the origin in Y (the order LightBurn uses).

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
    let mut xs: Vec<f64> = Vec::new();
    for poly in polys.iter().filter(|p| p.len() >= 3) {
        let n = poly.len();
        for i in 0..n {
            let (x0, y0) = poly[i];
            let (x1, y1) = poly[(i + 1) % n];
            let (y0, y1, x0, x1) = (y0 as f64, y1 as f64, x0 as f64, x1 as f64);
            // Half-open in y, so a vertex exactly on the line counts once.
            if (y0 <= y && y < y1) || (y1 <= y && y < y0) {
                xs.push(x0 + (y - y0) / (y1 - y0) * (x1 - x0));
            }
        }
    }
    xs.sort_by(f64::total_cmp);
    xs.chunks_exact(2)
        .map(|c| (c[0].round() as i32, c[1].round() as i32))
        .filter(|(a, b)| b > a)
        .collect()
}

/// Fill `polys` with lines `interval` µm apart, half an interval inside the
/// outline's top and bottom. Empty lines are left out.
pub fn scan(polys: &[Vec<(i32, i32)>], interval: i32) -> Vec<ScanLine> {
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
        let mut spans = spans_at(polys, y as f64);
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
