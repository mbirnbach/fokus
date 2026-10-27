//! Bézier paths: what drawn and imported shapes are made of, and what the
//! point editor works on. A sub-path is a list of nodes; a node may have a
//! handle towards the previous node (`c_in`) and one towards the next
//! (`c_out`). A segment without handles is a straight line.

use serde::{Deserialize, Serialize};

pub type Pt = (f64, f64);

/// Handle length that makes four cubic segments a circle.
const KAPPA: f64 = 0.552_284_749_8;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub p: Pt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c_in: Option<Pt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c_out: Option<Pt>,
    /// The two handles stay in line when one is dragged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub smooth: bool,
}

impl Node {
    pub fn corner(p: Pt) -> Self {
        Node { p, c_in: None, c_out: None, smooth: false }
    }

    fn map(&self, f: &impl Fn(Pt) -> Pt) -> Node {
        Node { p: f(self.p), c_in: self.c_in.map(f), c_out: self.c_out.map(f), smooth: self.smooth }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubPath {
    pub nodes: Vec<Node>,
    pub closed: bool,
}

/// One segment between two nodes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seg {
    Line(Pt, Pt),
    Cubic(Pt, Pt, Pt, Pt),
}

impl Seg {
    pub fn at(&self, t: f64) -> Pt {
        match *self {
            Seg::Line(a, b) => lerp(a, b, t),
            Seg::Cubic(a, b, c, d) => {
                let (ab, bc, cd) = (lerp(a, b, t), lerp(b, c, t), lerp(c, d, t));
                lerp(lerp(ab, bc, t), lerp(bc, cd, t), t)
            }
        }
    }

    /// Points after the start, up to and including the end, no further
    /// than `tol` from the curve.
    fn flatten_into(&self, tol: f64, out: &mut Vec<Pt>) {
        match *self {
            Seg::Line(_, b) => out.push(b),
            Seg::Cubic(a, b, c, d) => flatten_cubic(a, b, c, d, tol, 0, out),
        }
    }
}

fn lerp(a: Pt, b: Pt, t: f64) -> Pt {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

pub fn dist(a: Pt, b: Pt) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// Distance from `p` to the line through `a` and `b`.
fn line_dist(p: Pt, a: Pt, b: Pt) -> f64 {
    let len = dist(a, b);
    if len < 1e-12 {
        return dist(p, a);
    }
    ((b.0 - a.0) * (a.1 - p.1) - (a.0 - p.0) * (b.1 - a.1)).abs() / len
}

fn flatten_cubic(a: Pt, b: Pt, c: Pt, d: Pt, tol: f64, depth: u32, out: &mut Vec<Pt>) {
    if depth >= 16 || (line_dist(b, a, d) <= tol && line_dist(c, a, d) <= tol) {
        out.push(d);
        return;
    }
    let (ab, bc, cd) = (lerp(a, b, 0.5), lerp(b, c, 0.5), lerp(c, d, 0.5));
    let (abc, bcd) = (lerp(ab, bc, 0.5), lerp(bc, cd, 0.5));
    let m = lerp(abc, bcd, 0.5);
    flatten_cubic(a, ab, abc, m, tol, depth + 1, out);
    flatten_cubic(m, bcd, cd, d, tol, depth + 1, out);
}

impl SubPath {
    /// Straight lines through `pts`; closed when the last point repeats the first.
    pub fn polyline(pts: &[Pt]) -> SubPath {
        let closed = pts.len() >= 4 && pts.first() == pts.last();
        let end = if closed { pts.len() - 1 } else { pts.len() };
        SubPath { nodes: pts[..end].iter().map(|&p| Node::corner(p)).collect(), closed }
    }

    pub fn segment_count(&self) -> usize {
        match self.nodes.len() {
            0 | 1 => 0,
            n if self.closed => n,
            n => n - 1,
        }
    }

    /// Segment `i` runs from node `i` to the next node.
    pub fn segment(&self, i: usize) -> Seg {
        let a = &self.nodes[i];
        let b = &self.nodes[(i + 1) % self.nodes.len()];
        match (a.c_out, b.c_in) {
            (None, None) => Seg::Line(a.p, b.p),
            (c1, c2) => Seg::Cubic(a.p, c1.unwrap_or(a.p), c2.unwrap_or(b.p), b.p),
        }
    }

    /// The path as points; a closed path ends on its first point again.
    pub fn flatten(&self, tol: f64) -> Vec<Pt> {
        let Some(first) = self.nodes.first() else { return Vec::new() };
        let mut out = vec![first.p];
        for i in 0..self.segment_count() {
            self.segment(i).flatten_into(tol, &mut out);
        }
        out
    }

    pub fn map(&self, f: &impl Fn(Pt) -> Pt) -> SubPath {
        SubPath { nodes: self.nodes.iter().map(|n| n.map(f)).collect(), closed: self.closed }
    }

    /// Split segment `seg` at `t` (0..1) with a new node; returns its index.
    /// The curve keeps its shape.
    pub fn split(&mut self, seg: usize, t: f64) -> usize {
        let next = (seg + 1) % self.nodes.len();
        match self.segment(seg) {
            Seg::Line(a, b) => {
                self.nodes.insert(seg + 1, Node::corner(lerp(a, b, t)));
            }
            Seg::Cubic(a, b, c, d) => {
                let (ab, bc, cd) = (lerp(a, b, t), lerp(b, c, t), lerp(c, d, t));
                let (abc, bcd) = (lerp(ab, bc, t), lerp(bc, cd, t));
                let m = lerp(abc, bcd, t);
                self.nodes[seg].c_out = Some(ab);
                self.nodes[next].c_in = Some(cd);
                self.nodes.insert(seg + 1, Node { p: m, c_in: Some(abc), c_out: Some(bcd), smooth: true });
            }
        }
        seg + 1
    }

    /// The point on the path nearest `p`: (segment, t, distance).
    pub fn nearest(&self, p: Pt) -> Option<(usize, f64, f64)> {
        let mut best: Option<(usize, f64, f64)> = None;
        for i in 0..self.segment_count() {
            let s = self.segment(i);
            // Coarse samples, then refine around the best one.
            let mut t_best = 0.0;
            let mut d_best = f64::MAX;
            for k in 0..=32 {
                let t = k as f64 / 32.0;
                let d = dist(s.at(t), p);
                if d < d_best {
                    (t_best, d_best) = (t, d);
                }
            }
            let mut step = 1.0 / 64.0;
            for _ in 0..12 {
                for t in [t_best - step, t_best + step] {
                    let t = t.clamp(0.0, 1.0);
                    let d = dist(s.at(t), p);
                    if d < d_best {
                        (t_best, d_best) = (t, d);
                    }
                }
                step /= 2.0;
            }
            if best.is_none_or(|b| d_best < b.2) {
                best = Some((i, t_best, d_best));
            }
        }
        best
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    In,
    Out,
}

impl SubPath {
    fn neighbours(&self, i: usize) -> (Option<Pt>, Option<Pt>) {
        let n = self.nodes.len();
        let prev = if i > 0 { Some(self.nodes[i - 1].p) } else if self.closed && n > 1 { Some(self.nodes[n - 1].p) } else { None };
        let next = if i + 1 < n { Some(self.nodes[i + 1].p) } else if self.closed && n > 1 { Some(self.nodes[0].p) } else { None };
        (prev, next)
    }

    /// A corner becomes smooth (handles along its neighbours), a node with
    /// handles becomes a sharp corner.
    pub fn toggle_smooth(&mut self, i: usize) {
        let (prev, next) = self.neighbours(i);
        let node = &mut self.nodes[i];
        if node.c_in.is_some() || node.c_out.is_some() {
            *node = Node::corner(node.p);
            return;
        }
        let p = node.p;
        let (a, b) = (prev.unwrap_or(p), next.unwrap_or(p));
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = dx.hypot(dy);
        if len < 1e-9 {
            return;
        }
        let (ux, uy) = (dx / len, dy / len);
        node.c_out = next.map(|n| {
            let l = dist(p, n) / 3.0;
            (p.0 + ux * l, p.1 + uy * l)
        });
        node.c_in = prev.map(|n| {
            let l = dist(p, n) / 3.0;
            (p.0 - ux * l, p.1 - uy * l)
        });
        node.smooth = true;
    }

    /// Move one handle of node `i` to `to`. On a smooth node the other
    /// handle turns with it (keeping its length) unless `independent`.
    pub fn move_handle(&mut self, i: usize, side: Side, to: Pt, independent: bool) {
        let node = &mut self.nodes[i];
        let p = node.p;
        let (this, other) = match side {
            Side::In => (&mut node.c_in, &mut node.c_out),
            Side::Out => (&mut node.c_out, &mut node.c_in),
        };
        *this = Some(to);
        if independent {
            node.smooth = false;
            return;
        }
        if node.smooth {
            if let Some(o) = other.as_mut() {
                let l = dist(p, *o);
                let (dx, dy) = (p.0 - to.0, p.1 - to.1);
                let d = dx.hypot(dy);
                if d > 1e-9 {
                    *o = (p.0 + dx / d * l, p.1 + dy / d * l);
                }
            }
        }
    }

    /// Remove the nodes at `indices`; returns false when too little is left
    /// to draw (the sub-path should go).
    pub fn remove_nodes(&mut self, indices: &[usize]) -> bool {
        let mut idx = indices.to_vec();
        idx.sort_unstable();
        idx.dedup();
        for i in idx.into_iter().rev() {
            if i < self.nodes.len() {
                self.nodes.remove(i);
            }
        }
        if self.nodes.len() < 3 {
            self.closed = false;
        }
        self.nodes.len() >= 2
    }
}

pub fn flatten_all(subs: &[SubPath], tol: f64) -> Vec<Vec<Pt>> {
    subs.iter().map(|s| s.flatten(tol)).filter(|pl| pl.len() >= 2).collect()
}

/// Bounds of the drawn curve (not of the handles).
pub fn bounds(subs: &[SubPath], tol: f64) -> Option<(Pt, Pt)> {
    subs.iter().flat_map(|s| s.flatten(tol)).fold(None, |b, (x, y)| match b {
        None => Some(((x, y), (x, y))),
        Some(((x0, y0), (x1, y1))) => Some(((x0.min(x), y0.min(y)), (x1.max(x), y1.max(y)))),
    })
}

// ---------------------------------------------------------------- shapes

pub fn rect(w: f64, h: f64, radius: f64) -> SubPath {
    let r = radius.clamp(0.0, w.min(h) / 2.0);
    if r <= 0.0 {
        return SubPath { nodes: [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)].map(Node::corner).to_vec(), closed: true };
    }
    let k = r * KAPPA;
    // Clockwise from the top edge; each corner is a quarter circle between two nodes.
    let n = |p: Pt, c_in: Option<Pt>, c_out: Option<Pt>| Node { p, c_in, c_out, smooth: false };
    let nodes = vec![
        n((r, 0.0), Some((r - k, 0.0)), None),
        n((w - r, 0.0), None, Some((w - r + k, 0.0))),
        n((w, r), Some((w, r - k)), None),
        n((w, h - r), None, Some((w, h - r + k))),
        n((w - r, h), Some((w - r + k, h)), None),
        n((r, h), None, Some((r - k, h))),
        n((0.0, h - r), Some((0.0, h - r + k)), None),
        n((0.0, r), None, Some((0.0, r - k))),
    ];
    SubPath { nodes, closed: true }
}

pub fn ellipse(w: f64, h: f64) -> SubPath {
    let (rx, ry) = (w / 2.0, h / 2.0);
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    let nodes = vec![
        Node { p: (rx, 0.0), c_in: Some((rx - kx, 0.0)), c_out: Some((rx + kx, 0.0)), smooth: true },
        Node { p: (w, ry), c_in: Some((w, ry - ky)), c_out: Some((w, ry + ky)), smooth: true },
        Node { p: (rx, h), c_in: Some((rx + kx, h)), c_out: Some((rx - kx, h)), smooth: true },
        Node { p: (0.0, ry), c_in: Some((0.0, ry + ky)), c_out: Some((0.0, ry - ky)), smooth: true },
    ];
    SubPath { nodes, closed: true }
}

pub fn polygon(w: f64, h: f64, sides: u32) -> SubPath {
    let n = sides.max(3);
    let nodes = (0..n)
        .map(|i| {
            let a = -std::f64::consts::FRAC_PI_2 + std::f64::consts::TAU * i as f64 / n as f64;
            Node::corner((w / 2.0 + w / 2.0 * a.cos(), h / 2.0 + h / 2.0 * a.sin()))
        })
        .collect();
    SubPath { nodes, closed: true }
}

/// An SVG path (from usvg) as sub-paths, with `ts` applied.
pub fn from_usvg(data: &usvg::tiny_skia_path::Path, ts: usvg::Transform) -> Vec<SubPath> {
    use usvg::tiny_skia_path::PathSegment;
    let map = |p: usvg::tiny_skia_path::Point| {
        let mut q = p;
        ts.map_point(&mut q);
        (q.x as f64, q.y as f64)
    };
    let mut out = Vec::new();
    let mut cur: Option<SubPath> = None;
    let finish = |cur: &mut Option<SubPath>, out: &mut Vec<SubPath>| {
        if let Some(mut s) = cur.take() {
            // A closing line back onto the start point is implied by `closed`.
            if s.closed && s.nodes.len() > 1 && dist(s.nodes[0].p, s.nodes.last().unwrap().p) < 1e-9 {
                let last = s.nodes.pop().unwrap();
                s.nodes[0].c_in = last.c_in;
            }
            if s.nodes.len() >= 2 {
                out.push(s);
            }
        }
    };
    for seg in data.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                finish(&mut cur, &mut out);
                cur = Some(SubPath { nodes: vec![Node::corner(map(p))], closed: false });
            }
            PathSegment::LineTo(p) => {
                if let Some(s) = cur.as_mut() {
                    s.nodes.push(Node::corner(map(p)));
                }
            }
            PathSegment::QuadTo(c, p) => {
                if let Some(s) = cur.as_mut() {
                    let a = s.nodes.last().unwrap().p;
                    let (c, p) = (map(c), map(p));
                    let c1 = lerp(a, c, 2.0 / 3.0);
                    let c2 = lerp(p, c, 2.0 / 3.0);
                    s.nodes.last_mut().unwrap().c_out = Some(c1);
                    s.nodes.push(Node { p, c_in: Some(c2), c_out: None, smooth: false });
                }
            }
            PathSegment::CubicTo(c1, c2, p) => {
                if let Some(s) = cur.as_mut() {
                    s.nodes.last_mut().unwrap().c_out = Some(map(c1));
                    s.nodes.push(Node { p: map(p), c_in: Some(map(c2)), c_out: None, smooth: false });
                }
            }
            PathSegment::Close => {
                if let Some(s) = cur.as_mut() {
                    s.closed = true;
                }
                let start = cur.as_ref().and_then(|s| s.nodes.first()).map(|n| n.p);
                finish(&mut cur, &mut out);
                // Drawing may continue from the start point without a MoveTo.
                cur = start.map(|p| SubPath { nodes: vec![Node::corner(p)], closed: false });
            }
        }
    }
    finish(&mut cur, &mut out);
    for s in &mut out {
        mark_smooth(s);
    }
    out
}

/// Nodes whose two handles point in opposite directions count as smooth.
fn mark_smooth(s: &mut SubPath) {
    for n in &mut s.nodes {
        if let (Some(a), Some(b)) = (n.c_in, n.c_out) {
            let (ax, ay) = (a.0 - n.p.0, a.1 - n.p.1);
            let (bx, by) = (b.0 - n.p.0, b.1 - n.p.1);
            let (la, lb) = (ax.hypot(ay), bx.hypot(by));
            if la > 1e-9 && lb > 1e-9 {
                let cos = (ax * bx + ay * by) / (la * lb);
                n.smooth = cos < -0.999;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polyline_round_trip() {
        let pts = [(0.0, 0.0), (10.0, 0.0), (10.0, 5.0), (0.0, 0.0)];
        let s = SubPath::polyline(&pts);
        assert!(s.closed);
        assert_eq!(s.nodes.len(), 3);
        assert_eq!(s.flatten(0.01), pts.to_vec());
        let open = SubPath::polyline(&[(0.0, 0.0), (1.0, 1.0)]);
        assert!(!open.closed);
        assert_eq!(open.segment_count(), 1);
    }

    #[test]
    fn ellipse_is_round_within_tolerance() {
        let e = ellipse(20.0, 20.0);
        for p in e.flatten(0.01) {
            let r = dist(p, (10.0, 10.0));
            assert!((r - 10.0).abs() < 0.03, "radius {r}");
        }
    }

    #[test]
    fn splitting_keeps_the_shape() {
        let mut e = ellipse(20.0, 10.0);
        let before = e.segment(0).at(0.5);
        let i = e.split(0, 0.5);
        assert_eq!(e.nodes.len(), 5);
        assert!(dist(e.nodes[i].p, before) < 1e-9);
        // The new halves trace the old curve.
        let quarter = e.segment(0).at(0.5);
        let old = ellipse(20.0, 10.0).segment(0).at(0.25);
        assert!(dist(quarter, old) < 1e-9);
        let mut line = SubPath::polyline(&[(0.0, 0.0), (10.0, 0.0)]);
        line.split(0, 0.3);
        assert_eq!(line.nodes[1].p, (3.0, 0.0));
    }

    #[test]
    fn nearest_point_on_a_curve() {
        let e = ellipse(20.0, 20.0);
        let (_, _, d) = e.nearest((10.0, -3.0)).unwrap();
        assert!((d - 3.0).abs() < 1e-3);
    }

    #[test]
    fn smooth_nodes_keep_handles_in_line() {
        let mut s = SubPath::polyline(&[(0.0, 0.0), (10.0, 0.0), (20.0, 10.0)]);
        s.toggle_smooth(1);
        assert!(s.nodes[1].smooth && s.nodes[1].c_in.is_some() && s.nodes[1].c_out.is_some());
        s.move_handle(1, Side::Out, (15.0, -5.0), false);
        let n = s.nodes[1];
        let (a, b) = (n.c_in.unwrap(), n.c_out.unwrap());
        let cross = (a.0 - n.p.0) * (b.1 - n.p.1) - (a.1 - n.p.1) * (b.0 - n.p.0);
        assert!(cross.abs() < 1e-9, "handles stay on one line");
        s.move_handle(1, Side::Out, (15.0, 5.0), true);
        assert!(!s.nodes[1].smooth);
        s.toggle_smooth(1);
        assert_eq!(s.nodes[1], Node::corner((10.0, 0.0)));
    }

    #[test]
    fn removing_nodes() {
        let mut s = ellipse(10.0, 10.0);
        assert!(s.remove_nodes(&[0]));
        assert!(s.closed && s.nodes.len() == 3);
        assert!(!s.remove_nodes(&[0, 1]));
    }

    #[test]
    fn rounded_rect_has_straight_sides() {
        let r = rect(30.0, 20.0, 5.0);
        let (lo, hi) = bounds(&[r], 0.01).unwrap();
        assert!(dist(lo, (0.0, 0.0)) < 1e-6 && dist(hi, (30.0, 20.0)) < 1e-6);
    }
}
