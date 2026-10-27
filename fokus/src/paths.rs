//! Ordering cut paths to keep travel moves short.
//!
//! Safety first: a shape inside another closed shape (a hole in a part) is
//! always cut before the shape around it, because a part that is already cut
//! free can drop or shift. Within that rule, the next path is the nearest
//! one; closed shapes start at their point nearest the head, and open lines
//! may be cut from either end.

pub type P = (i32, i32);

fn dist2(a: P, b: P) -> i64 {
    let (dx, dy) = ((a.0 - b.0) as i64, (a.1 - b.1) as i64);
    dx * dx + dy * dy
}

fn is_closed(path: &[P]) -> bool {
    path.len() >= 3 && path.first() == path.last()
}

fn bounds(path: &[P]) -> (P, P) {
    path.iter().fold(((i32::MAX, i32::MAX), (i32::MIN, i32::MIN)), |(lo, hi), &(x, y)| {
        ((lo.0.min(x), lo.1.min(y)), (hi.0.max(x), hi.1.max(y)))
    })
}

/// How many closed paths contain this one (by bounding box): 0 = outermost.
fn depths(paths: &[Vec<P>]) -> Vec<usize> {
    let boxes: Vec<(P, P)> = paths.iter().map(|p| bounds(p)).collect();
    let area = |b: &(P, P)| (b.1.0 - b.0.0) as i64 * (b.1.1 - b.0.1) as i64;
    (0..paths.len())
        .map(|i| {
            (0..paths.len())
                .filter(|&j| {
                    let (a, b) = (boxes[i], boxes[j]);
                    j != i
                        && is_closed(&paths[j])
                        && b.0.0 <= a.0.0
                        && b.0.1 <= a.0.1
                        && a.1.0 <= b.1.0
                        && a.1.1 <= b.1.1
                        // Identical boxes: only the larger index counts as outside, so two
                        // equal shapes don't both claim to be inside each other.
                        && (area(&b) > area(&a) || j < i)
                })
                .count()
        })
        .collect()
}

/// Rotate a closed path so it starts (and ends) at `start_at`.
fn rotate_closed(path: &[P], start_at: usize) -> Vec<P> {
    let ring = &path[..path.len() - 1];
    let mut out: Vec<P> = ring[start_at..].iter().chain(&ring[..start_at]).copied().collect();
    out.push(out[0]);
    out
}

/// Order (and orient) paths starting from `start`; innermost paths first.
pub fn order(paths: Vec<Vec<P>>, start: P) -> Vec<Vec<P>> {
    let paths: Vec<Vec<P>> = paths.into_iter().filter(|p| p.len() >= 2).collect();
    let depth = depths(&paths);
    let mut remaining: Vec<usize> = (0..paths.len()).collect();
    let mut out = Vec::with_capacity(paths.len());
    let mut pos = start;
    while !remaining.is_empty() {
        // Only the innermost paths left may go next.
        let deepest = remaining.iter().map(|&i| depth[i]).max().unwrap();
        let mut best: Option<(i64, usize, usize, bool)> = None; // (distance, slot in remaining, start vertex, reversed)
        for (slot, &i) in remaining.iter().enumerate().filter(|(_, i)| depth[**i] == deepest) {
            let p = &paths[i];
            let candidates: Vec<(i64, usize, bool)> = if is_closed(p) {
                (0..p.len() - 1).map(|v| (dist2(pos, p[v]), v, false)).collect()
            } else {
                vec![(dist2(pos, p[0]), 0, false), (dist2(pos, *p.last().unwrap()), 0, true)]
            };
            for (d, v, rev) in candidates {
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, slot, v, rev));
                }
            }
        }
        let (_, slot, v, rev) = best.unwrap();
        let i = remaining.swap_remove(slot);
        let p = &paths[i];
        let oriented = if is_closed(p) {
            rotate_closed(p, v)
        } else if rev {
            p.iter().rev().copied().collect()
        } else {
            p.clone()
        };
        pos = *oriented.last().unwrap();
        out.push(oriented);
    }
    out
}

/// Total length of the moves between paths (laser off), starting at `start`.
#[cfg(test)]
fn travel(paths: &[Vec<P>], start: P) -> f64 {
    let mut pos = start;
    let mut total = 0.0;
    for p in paths {
        total += (dist2(pos, p[0]) as f64).sqrt();
        pos = *p.last().unwrap();
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: i32, y: i32, s: i32) -> Vec<P> {
        vec![(x, y), (x + s, y), (x + s, y + s), (x, y + s), (x, y)]
    }

    #[test]
    fn holes_before_the_part_around_them() {
        let part = square(0, 0, 100);
        let hole_a = square(10, 10, 10);
        let hole_b = square(70, 70, 10);
        let ordered = order(vec![part.clone(), hole_a, hole_b], (0, 0));
        assert_eq!(bounds(&ordered[2]), bounds(&part), "the outline comes last");
    }

    #[test]
    fn nearest_next_and_less_travel() {
        // A 5 x 5 grid of small squares, listed in a scattered order.
        let mut grid = Vec::new();
        for i in [3, 0, 4, 1, 2] {
            for j in [2, 4, 0, 3, 1] {
                grid.push(square(i * 20, j * 20, 5));
            }
        }
        let before = travel(&grid, (0, 0));
        let ordered = order(grid, (0, 0));
        let after = travel(&ordered, (0, 0));
        assert_eq!(ordered.len(), 25);
        assert!(after < before / 2.0, "travel {before:.0} → {after:.0}");
    }

    #[test]
    fn closed_paths_start_near_the_head_and_stay_closed() {
        let s = square(100, 100, 50); // starts at (100, 100)
        let ordered = order(vec![s], (160, 160));
        let p = &ordered[0];
        assert_eq!(p[0], (150, 150), "starts at the corner nearest the head");
        assert_eq!(p.first(), p.last());
        assert_eq!(p.len(), 5);
    }

    #[test]
    fn open_lines_may_be_reversed() {
        let line = vec![(0, 0), (100, 0)];
        let ordered = order(vec![line], (110, 0));
        assert_eq!(ordered[0], vec![(100, 0), (0, 0)]);
    }

    #[test]
    fn identical_shapes_are_both_kept() {
        let ordered = order(vec![square(0, 0, 10), square(0, 0, 10)], (0, 0));
        assert_eq!(ordered.len(), 2);
    }
}
