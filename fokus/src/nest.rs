//! Filling a shape with as many copies of a piece as fit (nesting).
//!
//! The shape, the piece in each allowed rotation and everything already in
//! the shape are drawn onto a fine grid, each cell marked when a shape
//! touches it at all, so pieces that don't share a cell never overlap.
//! Spacing grows everything by half of it, so two pieces (or a piece and the
//! shape's edge) stay at least that far apart. Then copies go wherever they
//! fit, row by row, trying each rotation in turn: pieces like stars end up
//! in each other's gaps. Which rotation goes first changes the result, so
//! the fill is tried with each rotation alone and with each one first, and
//! the fullest wins.
//!
//! Cells are a fortieth of the piece or finer, so the fill is close but not
//! perfect: shapes may be up to one cell further apart than asked for, never
//! closer.

use crate::design::Pt;

/// How to fill.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Least distance between pieces, and to the shape's edge, mm.
    pub spacing: f64,
    /// Rotations to try, degrees clockwise.
    pub rotations: Vec<f64>,
    /// Stop after this many copies.
    pub max: Option<usize>,
}

/// One copy: turned by `angle` degrees around the piece's centre, then moved
/// by `offset` mm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub angle: f64,
    pub offset: Pt,
}

/// Cells per piece size; finer fills tighter and takes longer.
const CELLS_PER_PIECE: f64 = 40.0;
/// Finest cell, mm.
const MIN_CELL_MM: f64 = 0.1;
/// Largest grid side, cells.
const MAX_GRID: f64 = 3000.0;
/// Tolerance for edges lying exactly on a cell border, in cells.
const EDGE_EPS: f64 = 1e-6;

/// A grid of cells, one bit each, rows of 64-bit words.
#[derive(Clone)]
struct Grid {
    w: usize,
    h: usize,
    words: usize,
    bits: Vec<u64>,
}

impl Grid {
    fn new(w: usize, h: usize) -> Self {
        let words = w.div_ceil(64).max(1);
        Grid { w, h, words, bits: vec![0; words * h] }
    }

    fn get(&self, x: i64, y: i64) -> bool {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return true;
        }
        self.bits[y as usize * self.words + x as usize / 64] >> (x as usize % 64) & 1 == 1
    }

    fn set(&mut self, x: i64, y: i64) {
        if x >= 0 && y >= 0 && x < self.w as i64 && y < self.h as i64 {
            self.bits[y as usize * self.words + x as usize / 64] |= 1 << (x as usize % 64);
        }
    }

    /// Word `k` of row `y`, with everything outside the grid set.
    fn word(&self, y: usize, k: i64) -> u64 {
        if k < 0 || k as usize >= self.words {
            return !0;
        }
        let v = self.bits[y * self.words + k as usize];
        // Bits past the right edge count as set.
        let last = self.w % 64;
        if k as usize == self.words - 1 && last != 0 { v | (!0u64 << last) } else { v }
    }

    /// 64 cells of row `y` from column `x` on.
    fn window(&self, y: usize, x: i64) -> u64 {
        let (k, s) = (x.div_euclid(64), x.rem_euclid(64) as u32);
        let lo = self.word(y, k) >> s;
        if s == 0 { lo } else { lo | self.word(y, k + 1) << (64 - s) }
    }

    /// Whether `mask` placed at (ox, oy) covers no set cell (outside counts as set).
    fn fits(&self, mask: &Grid, ox: i64, oy: i64) -> bool {
        for r in 0..mask.h {
            let row = &mask.bits[r * mask.words..(r + 1) * mask.words];
            if row.iter().all(|&v| v == 0) {
                continue;
            }
            let y = oy + r as i64;
            if y < 0 || y >= self.h as i64 {
                return false;
            }
            for (j, &v) in row.iter().enumerate() {
                if v != 0 && v & self.window(y as usize, ox + j as i64 * 64) != 0 {
                    return false;
                }
            }
        }
        true
    }

    /// Set the cells of `src` placed at (ox, oy).
    fn or_at(&mut self, src: &Grid, ox: i64, oy: i64) {
        for r in 0..src.h {
            let y = oy + r as i64;
            if y < 0 || y >= self.h as i64 {
                continue;
            }
            let base = y as usize * self.words;
            for j in 0..src.words {
                let v = src.bits[r * src.words + j];
                if v == 0 {
                    continue;
                }
                let x = ox + j as i64 * 64;
                let (k, s) = (x.div_euclid(64), x.rem_euclid(64) as u32);
                if k >= 0 && (k as usize) < self.words {
                    self.bits[base + k as usize] |= v << s;
                }
                if s != 0 && k + 1 >= 0 && ((k + 1) as usize) < self.words {
                    self.bits[base + (k + 1) as usize] |= v >> (64 - s);
                }
            }
        }
    }

    /// Every set cell grown by a disc of `r` cells.
    fn grown(&self, r: i64) -> Grid {
        if r <= 0 {
            return self.clone();
        }
        let mut out = Grid::new(self.w, self.h);
        let limit = (r as f64 + 0.5).powi(2);
        for dy in -r..=r {
            for dx in -r..=r {
                if (dx * dx + dy * dy) as f64 <= limit {
                    out.or_at(self, dx, dy);
                }
            }
        }
        out
    }

    fn inverted(&self) -> Grid {
        let mut out = self.clone();
        for v in &mut out.bits {
            *v = !*v;
        }
        out
    }

    fn first_set(&self) -> Option<(i64, i64)> {
        (0..self.h as i64).find_map(|y| (0..self.w as i64).find(|&x| self.get(x, y)).map(|x| (x, y)))
    }
}

/// Where outlines lie on a grid at `origin` with cells `c` mm wide.
struct Raster {
    origin: Pt,
    c: f64,
}

impl Raster {
    fn cell(&self, p: Pt) -> Pt {
        ((p.0 - self.origin.0) / self.c, (p.1 - self.origin.1) / self.c)
    }

    /// Mark every cell an edge passes through (not just touches).
    fn edges(&self, g: &mut Grid, outlines: &[Vec<Pt>]) {
        for pl in outlines {
            for w in pl.windows(2) {
                let (a, b) = (self.cell(w[0]), self.cell(w[1]));
                let (lo, hi) = if a.1 <= b.1 { (a, b) } else { (b, a) };
                let x_at = |y: f64| if (hi.1 - lo.1).abs() < 1e-12 { lo.0 } else { lo.0 + (y - lo.1) / (hi.1 - lo.1) * (hi.0 - lo.0) };
                let (r0, r1) = ((lo.1 + EDGE_EPS).floor() as i64, (hi.1 - EDGE_EPS).ceil() as i64 - 1);
                // A horizontal edge inside a row, not on its border.
                let rows = if (hi.1 - lo.1).abs() < 1e-12 {
                    if (lo.1 - lo.1.round()).abs() < EDGE_EPS { continue } else { lo.1.floor() as i64..=lo.1.floor() as i64 }
                } else {
                    r0..=r1
                };
                for r in rows {
                    let (ya, yb) = ((r as f64).max(lo.1), ((r + 1) as f64).min(hi.1));
                    let (xa, xb) = (x_at(ya), x_at(yb));
                    let (x0, x1) = (xa.min(xb), xa.max(xb));
                    let (c0, c1) = ((x0 + EDGE_EPS).floor() as i64, (x1 - EDGE_EPS).ceil() as i64 - 1);
                    for x in c0..=c1.max(c0) {
                        // A vertical edge on a cell border passes through no cell.
                        if x1 - x0 < EDGE_EPS && (x0 - x0.round()).abs() < EDGE_EPS {
                            break;
                        }
                        g.set(x, r);
                    }
                }
            }
        }
    }

    /// Mark cells whose centre is inside, by the even-odd rule over `outlines`.
    fn fill(&self, g: &mut Grid, outlines: &[Vec<Pt>]) {
        for r in 0..g.h {
            let y = r as f64 + 0.5;
            let mut xs: Vec<f64> = Vec::new();
            for pl in outlines.iter().filter(|p| p.len() >= 3) {
                let n = pl.len();
                for i in 0..n {
                    let (a, b) = (self.cell(pl[i]), self.cell(pl[(i + 1) % n]));
                    if (a.1 <= y) != (b.1 <= y) {
                        xs.push(a.0 + (y - a.1) / (b.1 - a.1) * (b.0 - a.0));
                    }
                }
            }
            xs.sort_by(f64::total_cmp);
            for span in xs.chunks_exact(2) {
                let (c0, c1) = ((span[0] - 0.5).ceil() as i64, (span[1] - 0.5).floor() as i64);
                for x in c0..=c1 {
                    g.set(x, r as i64);
                }
            }
        }
    }
}

fn closed(pl: &[Pt]) -> bool {
    pl.len() >= 4 && pl.first() == pl.last()
}

fn bounds(outlines: &[Vec<Pt>]) -> Option<(Pt, Pt)> {
    outlines.iter().flatten().fold(None, |b, &(x, y)| Some(b.map_or(((x, y), (x, y)), |((x0, y0), (x1, y1)): (Pt, Pt)| ((x0.min(x), y0.min(y)), (x1.max(x), y1.max(y))))))
}

fn rotate((x, y): Pt, (cx, cy): Pt, deg: f64) -> Pt {
    let (s, c) = deg.to_radians().sin_cos();
    let (dx, dy) = (x - cx, y - cy);
    (cx + dx * c - dy * s, cy + dx * s + dy * c)
}

/// Fill `container` (closed outlines, even-odd, so holes stay free) with
/// copies of `piece` (outlines; closed ones count filled), turned around
/// `centre`, keeping clear of `obstacles`.
pub fn fill(container: &[Vec<Pt>], obstacles: &[Vec<Pt>], piece: &[Vec<Pt>], centre: Pt, settings: &Settings) -> Vec<Placement> {
    let (Some(((cx0, cy0), (cx1, cy1))), Some(((px0, py0), (px1, py1)))) = (bounds(container), bounds(piece)) else { return Vec::new() };
    let size = (px1 - px0).min(py1 - py0).max((px1 - px0).max(py1 - py0) / 4.0);
    let c = (size / CELLS_PER_PIECE).max(MIN_CELL_MM).max((cx1 - cx0).max(cy1 - cy0) / MAX_GRID);
    // Each shape grows by half the spacing.
    let grow = (settings.spacing / 2.0 / c).ceil().max(0.0) as i64;
    let margin = grow + 1;

    // What is taken: outside the shape, its edge, and what's already in it.
    let origin = (cx0 - margin as f64 * c, cy0 - margin as f64 * c);
    let raster = Raster { origin, c };
    let (gw, gh) = (((cx1 - cx0) / c).ceil() as usize + 2 * margin as usize, ((cy1 - cy0) / c).ceil() as usize + 2 * margin as usize);
    let mut inside = Grid::new(gw, gh);
    raster.fill(&mut inside, container);
    let mut taken = inside.inverted();
    raster.edges(&mut taken, container);
    for o in obstacles {
        raster.edges(&mut taken, std::slice::from_ref(o));
        if closed(o) {
            raster.fill(&mut taken, std::slice::from_ref(o));
        }
    }
    let taken = taken.grown(grow);

    // The piece in each rotation, grown the same way.
    let masks: Vec<(f64, Grid, Pt, (i64, i64))> = settings
        .rotations
        .iter()
        .filter_map(|&angle| {
            let turned: Vec<Vec<Pt>> = piece.iter().map(|pl| pl.iter().map(|&p| rotate(p, centre, angle)).collect()).collect();
            let ((bx0, by0), (bx1, by1)) = bounds(&turned)?;
            let origin = (bx0 - margin as f64 * c, by0 - margin as f64 * c);
            let raster = Raster { origin, c };
            let mut mask = Grid::new(((bx1 - bx0) / c).ceil() as usize + 2 * margin as usize, ((by1 - by0) / c).ceil() as usize + 2 * margin as usize);
            raster.edges(&mut mask, &turned);
            for pl in turned.iter().filter(|pl| closed(pl)) {
                raster.fill(&mut mask, std::slice::from_ref(pl));
            }
            let mask = mask.grown(grow);
            let first = mask.first_set()?;
            Some((angle, mask, origin, first))
        })
        .collect();

    // Each rotation alone, and all of them with each one tried first.
    let n = masks.len();
    let mut orders: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    if n > 1 {
        orders.extend((0..n).map(|first| (0..n).map(|k| (first + k) % n).collect()));
    }
    let max = settings.max.unwrap_or(usize::MAX);
    let mut best: Vec<Placement> = Vec::new();
    for order in orders {
        let chosen: Vec<&(f64, Grid, Pt, (i64, i64))> = order.iter().map(|&i| &masks[i]).collect();
        let out = place(taken.clone(), &chosen, origin, c, (gw, gh), max);
        if out.len() > best.len() {
            best = out;
        }
        if best.len() >= max {
            break;
        }
    }
    best
}

/// Copies wherever they fit, row by row, the first rotation that fits each time.
fn place(mut taken: Grid, masks: &[&(f64, Grid, Pt, (i64, i64))], origin: Pt, c: f64, (gw, gh): (usize, usize), max: usize) -> Vec<Placement> {
    let mut out = Vec::new();
    let (mw, mh) = masks.iter().fold((0, 0), |(w, h), m| (w.max(m.1.w as i64), h.max(m.1.h as i64)));
    'rows: for oy in -mh..gh as i64 {
        for ox in -mw..gw as i64 {
            for &(angle, mask, mask_origin, (fx, fy)) in masks {
                // Quick no: the mask's first cell lands on a taken one.
                if taken.get(ox + fx, oy + fy) || !taken.fits(mask, ox, oy) {
                    continue;
                }
                taken.or_at(mask, ox, oy);
                let at = (origin.0 + ox as f64 * c, origin.1 + oy as f64 * c);
                out.push(Placement { angle: *angle, offset: (at.0 - mask_origin.0, at.1 - mask_origin.1) });
                if out.len() >= max {
                    break 'rows;
                }
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Vec<Pt> {
        vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)]
    }

    fn star(cx: f64, cy: f64, r: f64) -> Vec<Pt> {
        let mut pts: Vec<Pt> = (0..10)
            .map(|i| {
                let a = (i as f64 * 36.0 - 90.0).to_radians();
                let rr = if i % 2 == 0 { r } else { r * 0.45 };
                (cx + rr * a.cos(), cy + rr * a.sin())
            })
            .collect();
        pts.push(pts[0]);
        pts
    }

    fn settings(spacing: f64, rotations: &[f64]) -> Settings {
        Settings { spacing, rotations: rotations.to_vec(), max: None }
    }

    fn placed(piece: &[Vec<Pt>], centre: Pt, p: &Placement) -> Vec<Vec<Pt>> {
        piece.iter().map(|pl| pl.iter().map(|&q| { let r = rotate(q, centre, p.angle); (r.0 + p.offset.0, r.1 + p.offset.1) }).collect()).collect()
    }

    fn seg_dist(p: Pt, a: Pt, b: Pt) -> f64 {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy).max(1e-12)).clamp(0.0, 1.0);
        (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
    }

    fn inside(p: Pt, poly: &[Pt]) -> bool {
        poly.windows(2).fold(false, |odd, w| if (w[0].1 <= p.1) != (w[1].1 <= p.1) && p.0 < w[0].0 + (p.1 - w[0].1) / (w[1].1 - w[0].1) * (w[1].0 - w[0].0) { !odd } else { odd })
    }

    /// Least distance between the outlines of two polygons (0 if they overlap).
    fn distance(a: &[Pt], b: &[Pt]) -> f64 {
        if inside(a[0], b) || inside(b[0], a) {
            return 0.0;
        }
        let one = |p: &[Pt], q: &[Pt]| p.iter().flat_map(|&v| q.windows(2).map(move |w| seg_dist(v, w[0], w[1]))).fold(f64::MAX, f64::min);
        one(a, b).min(one(b, a))
    }

    #[test]
    fn squares_fill_a_rectangle_exactly() {
        let sq = vec![rect(0.0, 0.0, 10.0, 10.0)];
        let out = fill(&[rect(0.0, 0.0, 100.0, 50.0)], &[], &sq, (5.0, 5.0), &settings(0.0, &[0.0]));
        assert_eq!(out.len(), 50);
    }

    #[test]
    fn spacing_keeps_pieces_and_edges_apart() {
        let sq = vec![rect(0.0, 0.0, 10.0, 10.0)];
        let container = rect(0.0, 0.0, 100.0, 50.0);
        let out = fill(std::slice::from_ref(&container), &[], &sq, (5.0, 5.0), &settings(2.0, &[0.0]));
        // 8 × 4: 2 mm from the edge and between squares.
        assert_eq!(out.len(), 32);
        let shapes: Vec<Vec<Pt>> = out.iter().map(|p| placed(&sq, (5.0, 5.0), p)[0].clone()).collect();
        for (i, a) in shapes.iter().enumerate() {
            assert!(a.iter().all(|&(x, y)| x >= 2.0 - 1e-9 && y >= 2.0 - 1e-9 && x <= 98.0 + 1e-9 && y <= 48.0 + 1e-9), "{a:?}");
            for b in &shapes[i + 1..] {
                assert!(distance(a, b) >= 2.0 - 1e-9);
            }
        }
    }

    #[test]
    fn turning_lets_a_piece_fit() {
        // 30 × 10 into 12 × 32: only upright.
        let bar = vec![rect(0.0, 0.0, 30.0, 10.0)];
        assert!(fill(&[rect(0.0, 0.0, 12.0, 32.0)], &[], &bar, (15.0, 5.0), &settings(0.0, &[0.0])).is_empty());
        let out = fill(&[rect(0.0, 0.0, 12.0, 32.0)], &[], &bar, (15.0, 5.0), &settings(0.0, &[0.0, 90.0]));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].angle, 90.0);
    }

    #[test]
    fn stars_fill_a_board_without_touching() {
        let s = vec![star(0.0, 0.0, 20.0)];
        let board = rect(0.0, 0.0, 200.0, 120.0);
        let spacing = 1.0;
        let upright = fill(std::slice::from_ref(&board), &[], &s, (0.0, 0.0), &settings(spacing, &[0.0]));
        let turned = fill(std::slice::from_ref(&board), &[], &s, (0.0, 0.0), &settings(spacing, &[0.0, 36.0, 180.0]));
        assert!(upright.len() >= 12, "{}", upright.len());
        assert!(turned.len() >= upright.len(), "more rotations never fit fewer: {} vs {}", turned.len(), upright.len());
        let shapes: Vec<Vec<Pt>> = turned.iter().map(|p| placed(&s, (0.0, 0.0), p)[0].clone()).collect();
        for (i, a) in shapes.iter().enumerate() {
            assert!(a.iter().all(|&(x, y)| x >= spacing - 1e-6 && y >= spacing - 1e-6 && x <= 200.0 - spacing + 1e-6 && y <= 120.0 - spacing + 1e-6));
            for b in &shapes[i + 1..] {
                assert!(distance(a, b) >= spacing - 1e-6, "{}", distance(a, b));
            }
        }
    }

    #[test]
    fn what_is_already_there_stays_free_and_holes_too() {
        let sq = vec![rect(0.0, 0.0, 10.0, 10.0)];
        // A 50 × 50 board with a 30 × 30 hole in the middle: only the 10 mm frame is left.
        let board = vec![rect(0.0, 0.0, 50.0, 50.0), rect(10.0, 10.0, 30.0, 30.0)];
        assert_eq!(fill(&board, &[], &sq, (5.0, 5.0), &settings(0.0, &[0.0])).len(), 16);
        // Something lying in the top-left corner.
        let out = fill(&[rect(0.0, 0.0, 30.0, 10.0)], &[rect(0.0, 0.0, 10.0, 10.0)], &sq, (5.0, 5.0), &settings(0.0, &[0.0]));
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|p| p.offset.0 >= 10.0 - 1e-9));
    }

    #[test]
    fn a_limit_stops_early_and_nothing_fits_into_too_little() {
        let sq = vec![rect(0.0, 0.0, 10.0, 10.0)];
        let mut s = settings(0.0, &[0.0]);
        s.max = Some(3);
        assert_eq!(fill(&[rect(0.0, 0.0, 100.0, 50.0)], &[], &sq, (5.0, 5.0), &s).len(), 3);
        assert!(fill(&[rect(0.0, 0.0, 9.0, 9.0)], &[], &sq, (5.0, 5.0), &settings(0.0, &[0.0])).is_empty());
    }
}
