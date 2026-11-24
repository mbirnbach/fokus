//! Filling a shape with as many copies of a piece as fit (nesting).
//!
//! The shape, the piece in each allowed rotation and everything already in
//! the shape are drawn onto a fine grid, each cell marked when a shape
//! touches it at all, so pieces that don't share a cell never overlap.
//! Spacing grows everything by half of it, so two pieces (or a piece and the
//! shape's edge) stay at least that far apart. Then copies go wherever they
//! fit, row by row.
//!
//! With turning allowed, the angle is up to the fill: identical pieces pack
//! densest in one direction, or alternating with the same piece turned 180°
//! (stars point to notch, L shapes nesting). So every angle in small steps
//! is tried, alone and paired with its opposite, and the arrangement with the
//! most copies wins; of equally many, the one that leaves the free space in
//! one piece at the end.
//!
//! Cells are a fortieth of the piece or finer, so the fill is close but not
//! perfect: shapes may be up to one cell further apart than asked for, never
//! closer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::design::Pt;

/// How to fill.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Least distance between pieces, and to the shape's edge, mm.
    pub spacing: f64,
    /// Copies may be turned: the fill finds the best angles itself.
    pub rotate: bool,
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
/// Angles tried when turning is allowed, degrees apart.
const ANGLE_STEP: u32 = 5;
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

/// What a fill starts from, owned, so a search can run on a thread of its own.
#[derive(Debug, Clone)]
pub struct Problem {
    /// Closed outlines, even-odd, so holes stay free.
    pub container: Vec<Vec<Pt>>,
    /// What already lies in the shape.
    pub obstacles: Vec<Vec<Pt>>,
    /// The piece's outlines; closed ones count filled.
    pub piece: Vec<Vec<Pt>>,
    /// The piece turns around this point.
    pub centre: Pt,
    pub settings: Settings,
}

/// The shape and what's taken in it, drawn at one cell size.
struct Board {
    taken: Grid,
    origin: Pt,
    c: f64,
    size: (usize, usize),
    margin: i64,
    grow: i64,
}

/// The piece at one angle, drawn and grown like the board.
struct Mask {
    angle: f64,
    grid: Grid,
    origin: Pt,
    first: (i64, i64),
}

impl Problem {
    /// The board with cells `per_piece` to the piece's size.
    fn board(&self, per_piece: f64) -> Option<Board> {
        let ((cx0, cy0), (cx1, cy1)) = bounds(&self.container)?;
        let ((px0, py0), (px1, py1)) = bounds(&self.piece)?;
        let size = (px1 - px0).min(py1 - py0).max((px1 - px0).max(py1 - py0) / 4.0);
        let c = (size / per_piece).max(MIN_CELL_MM).max((cx1 - cx0).max(cy1 - cy0) / MAX_GRID);
        // Each shape grows by half the spacing.
        let grow = (self.settings.spacing / 2.0 / c).ceil().max(0.0) as i64;
        let margin = grow + 1;
        // What is taken: outside the shape, its edge, and what's already in it.
        let origin = (cx0 - margin as f64 * c, cy0 - margin as f64 * c);
        let raster = Raster { origin, c };
        let size = (((cx1 - cx0) / c).ceil() as usize + 2 * margin as usize, ((cy1 - cy0) / c).ceil() as usize + 2 * margin as usize);
        let mut inside = Grid::new(size.0, size.1);
        raster.fill(&mut inside, &self.container);
        let mut taken = inside.inverted();
        raster.edges(&mut taken, &self.container);
        for o in &self.obstacles {
            raster.edges(&mut taken, std::slice::from_ref(o));
            if closed(o) {
                raster.fill(&mut taken, std::slice::from_ref(o));
            }
        }
        Some(Board { taken: taken.grown(grow), origin, c, size, margin, grow })
    }

    fn mask(&self, board: &Board, angle: f64) -> Option<Mask> {
        let (c, margin) = (board.c, board.margin);
        let turned: Vec<Vec<Pt>> = self.piece.iter().map(|pl| pl.iter().map(|&p| rotate(p, self.centre, angle)).collect()).collect();
        let ((bx0, by0), (bx1, by1)) = bounds(&turned)?;
        let origin = (bx0 - margin as f64 * c, by0 - margin as f64 * c);
        let raster = Raster { origin, c };
        let mut grid = Grid::new(((bx1 - bx0) / c).ceil() as usize + 2 * margin as usize, ((by1 - by0) / c).ceil() as usize + 2 * margin as usize);
        raster.edges(&mut grid, &turned);
        for pl in turned.iter().filter(|pl| closed(pl)) {
            raster.fill(&mut grid, std::slice::from_ref(pl));
        }
        let grid = grid.grown(board.grow);
        let first = grid.first_set()?;
        Some(Mask { angle, grid, origin, first })
    }

    /// The angle sets to try: each angle alone, and with its opposite (either first).
    fn angle_sets(&self) -> Vec<Vec<f64>> {
        if !self.settings.rotate {
            return vec![vec![0.0]];
        }
        (0..180 / ANGLE_STEP).flat_map(|k| {
            let a = (k * ANGLE_STEP) as f64;
            [vec![a], vec![a, a + 180.0], vec![a + 180.0, a]]
        }).collect()
    }
}

/// Which way copies are placed: rows or columns, from which corner.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Order {
    columns: bool,
    flip_x: bool,
    flip_y: bool,
}

const ORDERS: [Order; 8] = [
    Order { columns: false, flip_x: false, flip_y: false },
    Order { columns: false, flip_x: true, flip_y: false },
    Order { columns: false, flip_x: false, flip_y: true },
    Order { columns: false, flip_x: true, flip_y: true },
    Order { columns: true, flip_x: false, flip_y: false },
    Order { columns: true, flip_x: true, flip_y: false },
    Order { columns: true, flip_x: false, flip_y: true },
    Order { columns: true, flip_x: true, flip_y: true },
];

/// A fill and how good it is.
#[derive(Debug, Clone, Default)]
struct Result {
    placements: Vec<Placement>,
    /// Area of the box around all copies, mm²: smaller keeps the free space together.
    spread: f64,
}

impl Result {
    /// More copies; of equally many, the free space in one piece.
    fn beats(&self, other: &Result) -> bool {
        self.placements.len() > other.placements.len() || (self.placements.len() == other.placements.len() && self.spread < other.spread - 1e-6)
    }
}

/// Copies wherever they fit in `order`, the first angle that fits each time.
fn place(board: &Board, masks: &[&Mask], order: Order, max: usize) -> Result {
    let mut taken = board.taken.clone();
    let (gw, gh) = (board.size.0 as i64, board.size.1 as i64);
    let (mw, mh) = masks.iter().fold((0, 0), |(w, h), m| (w.max(m.grid.w as i64), h.max(m.grid.h as i64)));
    let span = |lo: i64, hi: i64, flip: bool| -> Box<dyn Iterator<Item = i64>> { if flip { Box::new((lo..hi).rev()) } else { Box::new(lo..hi) } };
    let mut placements = Vec::new();
    let (mut lo, mut hi) = ((i64::MAX, i64::MAX), (i64::MIN, i64::MIN));
    let (outer, inner) = if order.columns { ((-mw, gw, order.flip_x), (-mh, gh, order.flip_y)) } else { ((-mh, gh, order.flip_y), (-mw, gw, order.flip_x)) };
    'outer: for a in span(outer.0, outer.1, outer.2) {
        for b in span(inner.0, inner.1, inner.2) {
            let (ox, oy) = if order.columns { (a, b) } else { (b, a) };
            for m in masks {
                // Quick no: the mask's first cell lands on a taken one.
                if taken.get(ox + m.first.0, oy + m.first.1) || !taken.fits(&m.grid, ox, oy) {
                    continue;
                }
                taken.or_at(&m.grid, ox, oy);
                lo = (lo.0.min(ox), lo.1.min(oy));
                hi = (hi.0.max(ox + m.grid.w as i64), hi.1.max(oy + m.grid.h as i64));
                let at = (board.origin.0 + ox as f64 * board.c, board.origin.1 + oy as f64 * board.c);
                placements.push(Placement { angle: m.angle, offset: (at.0 - m.origin.0, at.1 - m.origin.1) });
                if placements.len() >= max {
                    break 'outer;
                }
                break;
            }
        }
    }
    let spread = if placements.is_empty() { 0.0 } else { ((hi.0 - lo.0) * (hi.1 - lo.1)) as f64 * board.c * board.c };
    Result { placements, spread }
}

/// A quick fill: each angle set once, row by row. `search` starts the same
/// way and then keeps looking.
#[cfg(test)]
pub fn fill(problem: &Problem) -> Vec<Placement> {
    let Some(board) = problem.board(CELLS_PER_PIECE) else { return Vec::new() };
    let max = problem.settings.max.unwrap_or(usize::MAX);
    let mut masks: Vec<Mask> = Vec::new();
    let mut best = Result::default();
    for set in problem.angle_sets() {
        for &a in &set {
            if !masks.iter().any(|m| m.angle == a) {
                masks.extend(problem.mask(&board, a));
            }
        }
        let chosen: Vec<&Mask> = set.iter().filter_map(|a| masks.iter().find(|m| m.angle == *a)).collect();
        if chosen.len() < set.len() {
            continue;
        }
        let r = place(&board, &chosen, Order::default(), max);
        if r.beats(&best) {
            best = r;
        }
    }
    best.placements
}

/// A search running on another thread: its best fill so far, and a way to stop it.
#[derive(Default)]
pub struct Search {
    best: Mutex<Vec<Placement>>,
    pub stop: AtomicBool,
    pub done: AtomicBool,
}

impl Search {
    pub fn best(&self) -> Vec<Placement> {
        self.best.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn count(&self) -> usize {
        self.best.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// Cells per piece for the closer looks after the quick fill.
const FINER: [f64; 2] = [70.0, 100.0];
/// Angle sets that go on to the finer grids.
const KEEP_SETS: usize = 6;

/// Look for a better fill until `budget` is used up, nothing has improved for
/// `plateau`, or `search.stop` is set. Starts with the quick fill, then tries
/// every placing order, then finer grids for the best angles, then random
/// angles and orders. The best so far is in `search` all along.
pub fn search(problem: &Problem, search: &Search, budget: Duration, plateau: Duration) {
    let started = Instant::now();
    let mut improved = started;
    let max = problem.settings.max.unwrap_or(usize::MAX);
    let mut best = Result::default();
    let mut rng = Rng::new();
    // Each try; false when it's time to stop.
    let run = |board: &Board, masks: &[&Mask], order: Order, best: &mut Result, improved: &mut Instant, patient: bool| -> (bool, usize) {
        if search.stop.load(Ordering::Relaxed) || started.elapsed() > budget || (patient && improved.elapsed() > plateau) {
            return (false, 0);
        }
        let r = place(board, masks, order, max);
        let n = r.placements.len();
        if r.beats(best) {
            *best = r;
            *improved = Instant::now();
            *search.best.lock().unwrap_or_else(|e| e.into_inner()) = best.placements.clone();
        }
        (true, n)
    };
    let sets = problem.angle_sets();
    let masks_for = |board: &Board, cache: &mut Vec<Mask>, set: &[f64]| -> bool {
        for &a in set {
            if !cache.iter().any(|m| (m.angle - a).abs() < 1e-9) {
                match problem.mask(board, a) {
                    Some(m) => cache.push(m),
                    None => return false,
                }
            }
        }
        true
    };

    // Every angle set and placing order on the quick grid; remember how each set did.
    let Some(board) = problem.board(CELLS_PER_PIECE) else { return finish(search) };
    let mut cache = Vec::new();
    let mut scores: Vec<(usize, Vec<f64>)> = Vec::new();
    'quick: for order in ORDERS {
        for set in &sets {
            if !masks_for(&board, &mut cache, set) {
                continue;
            }
            let chosen: Vec<&Mask> = set.iter().filter_map(|a| cache.iter().find(|m| (m.angle - a).abs() < 1e-9)).collect();
            let (go, n) = run(&board, &chosen, order, &mut best, &mut improved, false);
            if !go {
                break 'quick;
            }
            match scores.iter_mut().find(|(_, s)| s == set) {
                Some(entry) => entry.0 = entry.0.max(n),
                None => scores.push((n, set.clone())),
            }
        }
    }
    scores.sort_by(|a, b| b.0.cmp(&a.0));
    let top: Vec<Vec<f64>> = scores.into_iter().take(KEEP_SETS).map(|(_, s)| s).collect();

    // The best angle sets again on finer grids, every order.
    let mut finest = None;
    for per_piece in FINER {
        let Some(board) = problem.board(per_piece) else { break };
        let mut cache = Vec::new();
        for set in &top {
            if !masks_for(&board, &mut cache, set) {
                continue;
            }
            let chosen: Vec<&Mask> = set.iter().filter_map(|a| cache.iter().find(|m| (m.angle - a).abs() < 1e-9)).collect();
            for order in ORDERS {
                if !run(&board, &chosen, order, &mut best, &mut improved, true).0 {
                    return finish(search);
                }
            }
        }
        finest = Some((board, cache));
    }

    // Random angles (also between the steps, near the best ones half the time) and orders.
    let Some((board, mut cache)) = finest else { return finish(search) };
    if !problem.settings.rotate {
        return finish(search);
    }
    loop {
        let base = if rng.below(2) == 0 && !top.is_empty() { top[rng.below(top.len())][0] + (rng.below(21) as f64 - 10.0) * 0.5 } else { rng.below(360) as f64 * 0.5 };
        let a = base.rem_euclid(180.0);
        let set = match rng.below(3) {
            0 => vec![a],
            1 => vec![a, a + 180.0],
            _ => vec![a + 180.0, a],
        };
        if cache.len() > 400 {
            cache.clear();
        }
        if !masks_for(&board, &mut cache, &set) {
            continue;
        }
        let chosen: Vec<&Mask> = set.iter().filter_map(|x| cache.iter().find(|m| (m.angle - x).abs() < 1e-9)).collect();
        if !run(&board, &chosen, ORDERS[rng.below(ORDERS.len())], &mut best, &mut improved, true).0 {
            return finish(search);
        }
    }
}

fn finish(search: &Search) {
    search.done.store(true, Ordering::Relaxed);
}

/// A small random number generator (xorshift), enough for picking tries.
struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
        Rng(seed | 1)
    }

    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
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

    fn settings(spacing: f64, rotate: bool) -> Settings {
        Settings { spacing, rotate, max: None }
    }

    fn problem(container: &[Vec<Pt>], obstacles: &[Vec<Pt>], piece: &[Vec<Pt>], centre: Pt, settings: &Settings) -> Problem {
        Problem { container: container.to_vec(), obstacles: obstacles.to_vec(), piece: piece.to_vec(), centre, settings: settings.clone() }
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
        let out = fill(&problem(&[rect(0.0, 0.0, 100.0, 50.0)], &[], &sq, (5.0, 5.0), &settings(0.0, false)));
        assert_eq!(out.len(), 50);
    }

    #[test]
    fn spacing_keeps_pieces_and_edges_apart() {
        let sq = vec![rect(0.0, 0.0, 10.0, 10.0)];
        let container = rect(0.0, 0.0, 100.0, 50.0);
        let out = fill(&problem(std::slice::from_ref(&container), &[], &sq, (5.0, 5.0), &settings(2.0, false)));
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
        assert!(fill(&problem(&[rect(0.0, 0.0, 12.0, 32.0)], &[], &bar, (15.0, 5.0), &settings(0.0, false))).is_empty());
        let out = fill(&problem(&[rect(0.0, 0.0, 12.0, 32.0)], &[], &bar, (15.0, 5.0), &settings(0.0, true)));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].angle.rem_euclid(180.0), 90.0);
    }

    #[test]
    fn stars_fill_a_board_without_touching() {
        let s = vec![star(0.0, 0.0, 20.0)];
        let board = rect(0.0, 0.0, 200.0, 120.0);
        let spacing = 1.0;
        let upright = fill(&problem(std::slice::from_ref(&board), &[], &s, (0.0, 0.0), &settings(spacing, false)));
        let turned = fill(&problem(std::slice::from_ref(&board), &[], &s, (0.0, 0.0), &settings(spacing, true)));
        assert!(upright.len() >= 12, "{}", upright.len());
        assert!(turned.len() > upright.len(), "turning fits more stars: {} vs {}", turned.len(), upright.len());
        let shapes: Vec<Vec<Pt>> = turned.iter().map(|p| placed(&s, (0.0, 0.0), p)[0].clone()).collect();
        for (i, a) in shapes.iter().enumerate() {
            assert!(a.iter().all(|&(x, y)| x >= spacing - 1e-6 && y >= spacing - 1e-6 && x <= 200.0 - spacing + 1e-6 && y <= 120.0 - spacing + 1e-6));
            for b in &shapes[i + 1..] {
                assert!(distance(a, b) >= spacing - 1e-6, "{}", distance(a, b));
            }
        }
    }

    #[test]
    fn turning_finds_the_dense_way_for_l_shapes() {
        // An L 20 × 20 with 10 mm legs: half of its box is empty, so pairs turned 180° fill it.
        let l = vec![vec![(0.0, 0.0), (20.0, 0.0), (20.0, 10.0), (10.0, 10.0), (10.0, 20.0), (0.0, 20.0), (0.0, 0.0)]];
        let board = [rect(0.0, 0.0, 120.0, 80.0)];
        let upright = fill(&problem(&board, &[], &l, (10.0, 10.0), &settings(0.0, false))).len();
        let turned = fill(&problem(&board, &[], &l, (10.0, 10.0), &settings(0.0, true))).len();
        assert_eq!(upright, 24, "one per 20 × 20 box");
        // 120 × 80 holds 9600 mm², an L 300 mm²: 32 is every bit of the board.
        assert_eq!(turned, 32, "pairs turned 180° fill the board completely");
    }

    #[test]
    fn of_equally_many_the_free_space_stays_together() {
        // Three squares fit either way; the fill leaves the rest of the board in one piece.
        let sq = vec![rect(0.0, 0.0, 10.0, 10.0)];
        let mut s = settings(0.0, true);
        s.max = Some(3);
        let out = fill(&problem(&[rect(0.0, 0.0, 100.0, 50.0)], &[], &sq, (5.0, 5.0), &s));
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|p| p.offset.1.abs() < 1e-6), "all in the first row: {out:?}");
    }

    #[test]
    fn searching_longer_never_does_worse_and_stays_valid() {
        let s = vec![star(0.0, 0.0, 20.0)];
        let board = rect(0.0, 0.0, 200.0, 120.0);
        let spacing = 1.0;
        let p = problem(std::slice::from_ref(&board), &[], &s, (0.0, 0.0), &settings(spacing, true));
        let quick = fill(&p).len();
        let search = Search::default();
        let started = Instant::now();
        super::search(&p, &search, Duration::from_secs(4), Duration::from_secs(2));
        assert!(started.elapsed() < Duration::from_secs(6), "keeps to its time");
        assert!(search.done.load(Ordering::Relaxed));
        let best = search.best();
        assert!(best.len() >= quick, "{} vs {quick}", best.len());
        let shapes: Vec<Vec<Pt>> = best.iter().map(|q| placed(&s, (0.0, 0.0), q)[0].clone()).collect();
        for (i, a) in shapes.iter().enumerate() {
            assert!(a.iter().all(|&(x, y)| x >= spacing - 1e-6 && y >= spacing - 1e-6 && x <= 200.0 - spacing + 1e-6 && y <= 120.0 - spacing + 1e-6));
            for b in &shapes[i + 1..] {
                assert!(distance(a, b) >= spacing - 1e-6, "{}", distance(a, b));
            }
        }
    }

    #[test]
    fn a_search_stops_when_told() {
        let sq = vec![rect(0.0, 0.0, 10.0, 10.0)];
        let p = problem(&[rect(0.0, 0.0, 300.0, 200.0)], &[], &sq, (5.0, 5.0), &settings(1.0, true));
        let search = Search::default();
        search.stop.store(true, Ordering::Relaxed);
        let started = Instant::now();
        super::search(&p, &search, Duration::from_secs(30), Duration::from_secs(10));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(search.done.load(Ordering::Relaxed));
    }

    #[test]
    fn what_is_already_there_stays_free_and_holes_too() {
        let sq = vec![rect(0.0, 0.0, 10.0, 10.0)];
        // A 50 × 50 board with a 30 × 30 hole in the middle: only the 10 mm frame is left.
        let board = vec![rect(0.0, 0.0, 50.0, 50.0), rect(10.0, 10.0, 30.0, 30.0)];
        assert_eq!(fill(&problem(&board, &[], &sq, (5.0, 5.0), &settings(0.0, false))).len(), 16);
        // Something lying in the top-left corner.
        let out = fill(&problem(&[rect(0.0, 0.0, 30.0, 10.0)], &[rect(0.0, 0.0, 10.0, 10.0)], &sq, (5.0, 5.0), &settings(0.0, false)));
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|p| p.offset.0 >= 10.0 - 1e-9));
    }

    #[test]
    fn a_limit_stops_early_and_nothing_fits_into_too_little() {
        let sq = vec![rect(0.0, 0.0, 10.0, 10.0)];
        let mut s = settings(0.0, false);
        s.max = Some(3);
        assert_eq!(fill(&problem(&[rect(0.0, 0.0, 100.0, 50.0)], &[], &sq, (5.0, 5.0), &s)).len(), 3);
        assert!(fill(&problem(&[rect(0.0, 0.0, 9.0, 9.0)], &[], &sq, (5.0, 5.0), &settings(0.0, false))).is_empty());
    }
}
