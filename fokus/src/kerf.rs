//! Kerf compensation: the beam burns away a line of material, so a part cut
//! along its drawn outline comes out too small and a hole too big. Cutting
//! half the kerf further out around parts and further in around holes gives
//! both their drawn size, which press fits and finger joints need.
//!
//! What is a part and what a hole follows from nesting: a shape inside an odd
//! number of other cut shapes is a hole (or something else cut out of a part),
//! inside an even number it is a part. Open lines stay as they are: they have
//! no side to keep. A shape too small to shrink is cut as drawn rather than
//! not at all.

use cavalier_contours::polyline::{PlineSource, PlineSourceMut, PlineVertex, Polyline};

use crate::design::Pt;

/// How far offset corners may stray from their true arc, mm.
const ARC_ERROR_MM: f64 = 0.005;
/// Points closer than this are one point, mm.
const SAME_POINT_MM: f64 = 1e-5;

fn is_closed(path: &[Pt]) -> bool {
    path.len() >= 4 && path.first() == path.last()
}

/// Twice the signed area; positive for one direction, negative for the other.
fn signed_area(path: &[Pt]) -> f64 {
    path.windows(2).map(|w| w[0].0 * w[1].1 - w[1].0 * w[0].1).sum()
}

fn inside(p: Pt, poly: &[Pt]) -> bool {
    let mut odd = false;
    for w in poly.windows(2) {
        let (a, b) = (w[0], w[1]);
        if (a.1 <= p.1) != (b.1 <= p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            odd = !odd;
        }
    }
    odd
}

fn bounds(path: &[Pt]) -> (Pt, Pt) {
    path.iter().fold(((f64::MAX, f64::MAX), (f64::MIN, f64::MIN)), |(lo, hi), p| ((lo.0.min(p.0), lo.1.min(p.1)), (hi.0.max(p.0), hi.1.max(p.1))))
}

/// All cut shapes of a design, for telling parts from holes.
pub struct Nesting {
    shapes: Vec<(Vec<Pt>, (Pt, Pt), f64)>,
}

impl Nesting {
    pub fn new(paths: impl IntoIterator<Item = Vec<Pt>>) -> Self {
        let shapes = paths
            .into_iter()
            .filter(|p| is_closed(p))
            .map(|p| {
                let (b, a) = (bounds(&p), signed_area(&p).abs());
                (p, b, a)
            })
            .collect();
        Nesting { shapes }
    }

    /// Whether a closed path is a hole: inside an odd number of larger shapes.
    fn is_hole(&self, path: &[Pt]) -> bool {
        let (b, area) = (bounds(path), signed_area(path).abs());
        let p = path[0];
        let around = self.shapes.iter().filter(|(shape, sb, sa)| *sa > area && sb.0.0 <= b.0.0 && sb.0.1 <= b.0.1 && b.1.0 <= sb.1.0 && b.1.1 <= sb.1.1 && inside(p, shape));
        around.count() % 2 == 1
    }
}

/// The path to cut for a closed outline, `by` mm outward (or inward if
/// negative). Empty if nothing is left of the shape.
fn offset(path: &[Pt], by: f64) -> Vec<Vec<Pt>> {
    let mut pline = Polyline::<f64>::new_closed();
    for &(x, y) in &path[..path.len() - 1] {
        pline.add_or_replace_vertex(PlineVertex::new(x, y, 0.0), SAME_POINT_MM);
    }
    if pline.vertex_count() < 3 {
        return Vec::new();
    }
    // Offsets go to the left of the direction of travel: outside of a shape
    // only when it runs the other way round.
    let area = pline.area();
    if area == 0.0 {
        return Vec::new();
    }
    let left = if area > 0.0 { -by } else { by };
    pline
        .parallel_offset(left)
        .into_iter()
        .map(|o| {
            let flat = o.arcs_to_approx_lines(ARC_ERROR_MM).unwrap_or(o);
            let mut pts: Vec<Pt> = (0..flat.vertex_count()).map(|i| flat.at(i)).map(|v| (v.x, v.y)).collect();
            if let Some(&first) = pts.first() {
                pts.push(first);
            }
            pts
        })
        .filter(|p| p.len() >= 4)
        .collect()
}

/// The paths to cut for one layer's outlines, `kerf` mm wide.
pub fn compensate(paths: Vec<Vec<Pt>>, kerf: f64, nesting: &Nesting) -> Vec<Vec<Pt>> {
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        if !is_closed(&path) {
            out.push(path);
            continue;
        }
        let by = if nesting.is_hole(&path) { -kerf / 2.0 } else { kerf / 2.0 };
        let offsets = offset(&path, by);
        if offsets.is_empty() {
            out.push(path);
        } else {
            out.extend(offsets);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, size: f64) -> Vec<Pt> {
        vec![(x, y), (x + size, y), (x + size, y + size), (x, y + size), (x, y)]
    }

    fn run(paths: Vec<Vec<Pt>>, kerf: f64) -> Vec<Vec<Pt>> {
        let nesting = Nesting::new(paths.clone());
        compensate(paths, kerf, &nesting)
    }

    fn size(path: &[Pt]) -> Pt {
        let (lo, hi) = bounds(path);
        (hi.0 - lo.0, hi.1 - lo.1)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn parts_grow_in_either_direction() {
        let mut backwards = square(0.0, 0.0, 20.0);
        backwards.reverse();
        for path in [square(0.0, 0.0, 20.0), backwards] {
            let out = run(vec![path], 0.2);
            assert_eq!(out.len(), 1);
            let (w, h) = size(&out[0]);
            assert!(close(w, 20.2) && close(h, 20.2), "{w} {h}");
            assert_eq!(out[0].first(), out[0].last());
        }
    }

    #[test]
    fn holes_shrink_and_islands_grow_again() {
        let out = run(vec![square(0.0, 0.0, 30.0), square(5.0, 5.0, 20.0), square(10.0, 10.0, 10.0)], 0.2);
        let widths: Vec<f64> = out.iter().map(|p| size(p).0).collect();
        assert!(close(widths[0], 30.2) && close(widths[1], 19.8) && close(widths[2], 10.2), "{widths:?}");
    }

    #[test]
    fn a_part_in_the_notch_of_another_is_no_hole() {
        // An L, and a square in its empty corner: inside the L's box, but not in the L.
        let l = vec![(0.0, 0.0), (30.0, 0.0), (30.0, 10.0), (10.0, 10.0), (10.0, 30.0), (0.0, 30.0), (0.0, 0.0)];
        let out = run(vec![l, square(15.0, 15.0, 10.0)], 0.2);
        assert!(close(size(&out[1]).0, 10.2), "{:?}", size(&out[1]));
    }

    #[test]
    fn identical_shapes_are_not_inside_each_other() {
        let out = run(vec![square(0.0, 0.0, 10.0), square(0.0, 0.0, 10.0)], 0.2);
        assert!(out.iter().all(|p| close(size(p).0, 10.2)));
    }

    #[test]
    fn open_lines_stay_and_tiny_holes_are_still_cut() {
        let line = vec![(0.0, 0.0), (5.0, 5.0)];
        assert_eq!(run(vec![line.clone()], 0.2), vec![line]);
        let tiny = square(5.0, 5.0, 0.1);
        let out = run(vec![square(0.0, 0.0, 10.0), tiny.clone()], 0.2);
        assert_eq!(out[1], tiny, "a hole smaller than the kerf is cut as drawn");
    }

    #[test]
    fn corners_of_parts_keep_the_drawn_shape() {
        // Every point of the cut stays half the kerf from the drawn square.
        let out = run(vec![square(0.0, 0.0, 10.0)], 0.4);
        for &(x, y) in &out[0] {
            let (dx, dy) = ((-x).max(x - 10.0).max(0.0), (-y).max(y - 10.0).max(0.0));
            assert!((dx.hypot(dy) - 0.2).abs() < 0.01, "{x} {y}");
        }
    }

    #[test]
    fn curved_outlines() {
        let circle: Vec<Pt> = (0..=128).map(|i| {
            let t = i as f64 * std::f64::consts::TAU / 128.0;
            if i == 128 { (5.0, 0.0) } else { (5.0 * t.cos(), 5.0 * t.sin()) }
        }).collect();
        let out = run(vec![circle], 0.2);
        for &(x, y) in &out[0] {
            assert!((x.hypot(y) - 5.1).abs() < 0.01, "{x} {y}");
        }
    }
}
