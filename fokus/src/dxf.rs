//! Reading DXF files (ASCII, any AutoCAD version) from CAD programs.
//!
//! What reaches the laser: lines, circles, arcs, ellipses, polylines with
//! their bulges and splines, also inside (nested) blocks, as exact Bézier
//! curves where possible. Each DXF colour becomes one set of curves, which is
//! one layer in Fokus. CAD files often draw an outline as many loose pieces,
//! so pieces that meet are joined into one path (holes can then be cut first
//! and shapes filled), and lines drawn twice count once, so they aren't cut
//! twice.
//!
//! A hatch is usually the fill of an outline that is in the file too: it marks
//! that outline as filled, which suggests engraving it. A hatch without an
//! outline brings its own boundary. Filled and unfilled curves of one colour
//! become separate shapes, as lettering and the plate around it need different
//! operations, so the filled ones may get another colour.
//!
//! Left out on purpose, and reported: text and dimensions (they would have to
//! be set in the CAD program's fonts). Ignored without a word: paper space,
//! layers that are off or frozen, and points.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};

use crate::bezier::{bounds, dist, Node, SubPath};
use crate::t;

type Pt = (f64, f64);
type Tag<'a> = (i32, &'a str);

/// Pieces whose ends are this close (mm) belong together.
const JOIN_TOLERANCE_MM: f64 = 0.01;
/// How deep blocks may nest; also stops blocks that contain themselves.
const MAX_DEPTH: u32 = 32;
/// Most curves one file may make (an array of arrays can multiply them).
const MAX_PATHS: usize = 500_000;
/// A hatch's boundary and an outline with boxes this close (mm) are one shape.
const SAME_SHAPE_MM: f64 = 0.1;
/// Colours for filled curves whose own colour stays with the unfilled ones.
const FILL_COLORS: [[u8; 3]; 6] = [[224, 49, 49], [25, 113, 194], [47, 158, 68], [240, 140, 0], [156, 54, 181], [12, 133, 153]];

/// The curves of one colour.
#[derive(Debug)]
pub struct Shape {
    pub color: [u8; 3],
    /// Hatched in the file; filled by the even-odd rule, so holes stay holes.
    pub filled: bool,
    pub curves: Vec<SubPath>,
}

/// What came out of a file.
#[derive(Debug, Default)]
pub struct Drawing {
    /// In mm, y down, in the order the colours first appear.
    pub shapes: Vec<Shape>,
    /// Entity types left out, with how often.
    pub skipped: Vec<(String, usize)>,
}

pub fn read(data: &[u8]) -> Result<Drawing, String> {
    if data.starts_with(b"AutoCAD Binary DXF") {
        return Err(t!("err-dxf-binary"));
    }
    // Only names can be non-ASCII, and those are only compared.
    let text = String::from_utf8_lossy(data);
    let tags = parse_tags(&text).ok_or_else(|| t!("err-dxf-unreadable"))?;
    let file = File::parse(&tags).ok_or_else(|| t!("err-dxf-unreadable"))?;
    let mut out = Output::default();
    let root = Matrix::scale(file.mm_per_unit, file.mm_per_unit);
    let top = Inherited { layer: "0", color: Color::Aci(7) };
    for e in &file.entities {
        if e.int(67) != Some(1) {
            file.convert(e, root, top, 0, &mut out)?;
        }
    }
    let mut shapes = Vec::new();
    let mut used: Vec<[u8; 3]> = out.order.clone();
    let mut outlines = Vec::new();
    let mut fills = Vec::new();
    for &color in &out.order {
        outlines.push(join(out.paths.remove(&color).unwrap_or_default()));
        fills.push(join(out.hatches.remove(&color).unwrap_or_default()));
    }
    let split = split_filled(outlines, fills);
    for (&color, (plain, filled)) in out.order.iter().zip(split) {
        // y up in the file, y down on the bed.
        let flip = |curves: Vec<SubPath>| curves.iter().map(|s| s.map(&|(x, y)| (x, -y))).collect::<Vec<_>>();
        let fill_color = if plain.is_empty() { color } else { FILL_COLORS.iter().copied().find(|c| !used.contains(c)).unwrap_or(color) };
        used.push(fill_color);
        if !plain.is_empty() {
            shapes.push(Shape { color, filled: false, curves: flip(plain) });
        }
        if !filled.is_empty() {
            shapes.push(Shape { color: fill_color, filled: true, curves: flip(filled) });
        }
    }
    if shapes.is_empty() {
        return Err(t!("err-dxf-empty"));
    }
    let mut skipped: Vec<(String, usize)> = out.skipped.into_iter().collect();
    skipped.sort();
    Ok(Drawing { shapes, skipped })
}

/// The note for what a file's import left out.
pub fn skipped_note(skipped: &[(String, usize)]) -> String {
    let list: Vec<String> = skipped.iter().map(|(kind, n)| format!("{n} × {kind}")).collect();
    t!("dxf-skipped", list = list.join(", "))
}

// ---------------------------------------------------------------- file structure

/// A DXF file is pairs of lines: a number saying what the value is, and the value.
fn parse_tags(text: &str) -> Option<Vec<Tag<'_>>> {
    let mut tags = Vec::new();
    let mut lines = text.lines();
    while let (Some(code), Some(value)) = (lines.next(), lines.next()) {
        let code: i32 = code.trim().parse().ok()?;
        if code != 999 {
            tags.push((code, value.trim()));
        }
    }
    Some(tags)
}

struct Entity<'a> {
    kind: &'a str,
    tags: &'a [Tag<'a>],
    /// A POLYLINE's vertices.
    vertices: Vec<Entity<'a>>,
}

impl<'a> Entity<'a> {
    fn text(&self, code: i32) -> Option<&'a str> {
        self.tags.iter().find(|t| t.0 == code).map(|t| t.1)
    }

    fn num(&self, code: i32) -> Option<f64> {
        self.text(code)?.parse().ok()
    }

    fn int(&self, code: i32) -> Option<i64> {
        self.text(code)?.parse().ok()
    }

    fn pt(&self, x: i32) -> Pt {
        (self.num(x).unwrap_or(0.0), self.num(x + 10).unwrap_or(0.0))
    }

    fn all(&self, code: i32) -> Vec<f64> {
        self.tags.iter().filter(|t| t.0 == code).filter_map(|t| t.1.parse().ok()).collect()
    }

    /// Points from repeated x and y codes, in file order.
    fn points(&self, x: i32) -> Vec<Pt> {
        self.all(x).into_iter().zip(self.all(x + 10)).collect()
    }

    /// Seen from below: arcs and polylines are mirrored in x.
    fn seen_from_below(&self) -> bool {
        self.num(230).is_some_and(|z| z < 0.0)
    }
}

/// Split tags into entities at each code 0.
fn entities<'a>(tags: &'a [Tag<'a>]) -> Vec<Entity<'a>> {
    let starts: Vec<usize> = (0..tags.len()).filter(|&i| tags[i].0 == 0).collect();
    let mut out: Vec<Entity> = Vec::new();
    let mut in_polyline = false;
    for (n, &s) in starts.iter().enumerate() {
        let end = starts.get(n + 1).copied().unwrap_or(tags.len());
        let e = Entity { kind: tags[s].1, tags: &tags[s + 1..end], vertices: Vec::new() };
        match e.kind {
            "VERTEX" if in_polyline => out.last_mut().expect("a polyline came first").vertices.push(e),
            "SEQEND" => in_polyline = false,
            _ => {
                in_polyline = e.kind == "POLYLINE";
                out.push(e);
            }
        }
    }
    out
}

struct Layer {
    color: i64,
    hidden: bool,
}

struct Block<'a> {
    base: Pt,
    entities: Vec<Entity<'a>>,
}

struct File<'a> {
    mm_per_unit: f64,
    layers: HashMap<&'a str, Layer>,
    blocks: HashMap<&'a str, Block<'a>>,
    entities: Vec<Entity<'a>>,
}

impl<'a> File<'a> {
    fn parse(tags: &'a [Tag<'a>]) -> Option<Self> {
        let mut file = File { mm_per_unit: 1.0, layers: HashMap::new(), blocks: HashMap::new(), entities: Vec::new() };
        let mut found = false;
        let mut i = 0;
        while i + 1 < tags.len() {
            if tags[i] != (0, "SECTION") || tags[i + 1].0 != 2 {
                i += 1;
                continue;
            }
            let name = tags[i + 1].1;
            let start = i + 2;
            let end = (start..tags.len()).find(|&j| tags[j] == (0, "ENDSEC")).unwrap_or(tags.len());
            let body = &tags[start..end];
            found = true;
            match name {
                "HEADER" => file.mm_per_unit = header_units(body),
                "TABLES" => {
                    for e in entities(body).iter().filter(|e| e.kind == "LAYER") {
                        let (Some(name), color) = (e.text(2), e.int(62).unwrap_or(7)) else { continue };
                        // A negative colour means the layer is off; 1 and 2 are frozen flags.
                        file.layers.insert(name, Layer { color: color.abs(), hidden: color < 0 || e.int(70).unwrap_or(0) & 3 != 0 });
                    }
                }
                "BLOCKS" => file.parse_blocks(body),
                "ENTITIES" => file.entities = entities(body),
                _ => {}
            }
            i = end;
        }
        found.then_some(file)
    }

    fn parse_blocks(&mut self, body: &'a [Tag<'a>]) {
        let mut i = 0;
        while i < body.len() {
            if body[i] != (0, "BLOCK") {
                i += 1;
                continue;
            }
            let end = (i..body.len()).find(|&j| body[j] == (0, "ENDBLK")).unwrap_or(body.len());
            // The block's own tags run to the first entity after it.
            let first = (i + 1..end).find(|&j| body[j].0 == 0).unwrap_or(end);
            let head = Entity { kind: "BLOCK", tags: &body[i + 1..first], vertices: Vec::new() };
            if let Some(name) = head.text(2) {
                self.blocks.insert(name, Block { base: head.pt(10), entities: entities(&body[first..end]) });
            }
            i = end + 1;
        }
    }

    /// Turn one entity into curves. `from` is what a block insert passes on.
    fn convert(&self, e: &Entity<'a>, m: Matrix, from: Inherited<'a>, depth: u32, out: &mut Output) -> Result<(), String> {
        // Things on layer 0 in a block belong to the insert's layer.
        let layer = match e.text(8) {
            Some("0") | None if depth > 0 => from.layer,
            Some(l) => self.layers.get_key_value(l).map_or("0", |(k, _)| k),
            None => "0",
        };
        if self.layers.get(layer).is_some_and(|l| l.hidden) || layer == "Defpoints" {
            return Ok(());
        }
        let color = match (e.int(420), e.int(62).unwrap_or(256)) {
            (Some(rgb), _) => Color::Rgb(rgb),
            (None, 0) => from.color,
            (None, 256) => Color::Aci(self.layers.get(layer).map_or(7, |l| l.color)),
            (None, aci) => Color::Aci(aci.abs()),
        };
        // Arcs and polylines lie in their own plane, which can be seen from below.
        let plane = if e.seen_from_below() { m.then(Matrix::scale(-1.0, 1.0)) } else { m };
        let curve = match e.kind {
            "LINE" => {
                let (a, b) = (e.pt(10), e.pt(11));
                finish(vec![Node::corner(a), Node::corner(b)], false).map(|s| s.map(&|p| m.apply(p)))
            }
            "CIRCLE" => {
                let r = e.num(40).unwrap_or(0.0);
                let mut nodes = Vec::new();
                push_arc(&mut nodes, e.pt(10), (r, 0.0), (0.0, r), 0.0, TAU);
                finish(nodes, true).map(|s| s.map(&|p| plane.apply(p)))
            }
            "ARC" => {
                let r = e.num(40).unwrap_or(0.0);
                let (a0, mut a1) = (e.num(50).unwrap_or(0.0).to_radians(), e.num(51).unwrap_or(0.0).to_radians());
                while a1 <= a0 {
                    a1 += TAU;
                }
                let mut nodes = Vec::new();
                push_arc(&mut nodes, e.pt(10), (r, 0.0), (0.0, r), a0, a1);
                finish(nodes, false).map(|s| s.map(&|p| plane.apply(p)))
            }
            "ELLIPSE" => ellipse(e).map(|s| s.map(&|p| m.apply(p))),
            "LWPOLYLINE" => {
                let vertices = lwpolyline_vertices(e);
                polyline(&vertices, e.int(70).unwrap_or(0) & 1 != 0).map(|s| s.map(&|p| plane.apply(p)))
            }
            "POLYLINE" => {
                let flags = e.int(70).unwrap_or(0);
                if flags & (16 | 64) != 0 {
                    // 3D meshes: nothing to follow in 2D.
                    out.skip(e.kind);
                    return Ok(());
                }
                // Flag 16 marks the control points of a smoothed polyline; the rest is its curve.
                let vertices: Vec<(Pt, f64)> = e.vertices.iter().filter(|v| v.int(70).unwrap_or(0) & 16 == 0).map(|v| (v.pt(10), v.num(42).unwrap_or(0.0))).collect();
                polyline(&vertices, flags & 1 != 0).map(|s| s.map(&|p| plane.apply(p)))
            }
            "SPLINE" => spline(e).map(|s| s.map(&|p| m.apply(p))),
            "INSERT" => return self.insert(e, plane, Inherited { layer, color }, depth, out),
            // Not shapes, or the block they point to isn't drawn by them.
            "POINT" | "VIEWPORT" | "ATTDEF" | "SEQEND" => return Ok(()),
            "HATCH" => {
                for path in hatch_boundaries(e, plane) {
                    out.add_hatch(color.rgb(), path)?;
                }
                return Ok(());
            }
            other => {
                out.skip(other);
                return Ok(());
            }
        };
        if let Some(curve) = curve {
            out.add(color.rgb(), curve)?;
        }
        Ok(())
    }

    /// A block placed once or as a grid (rows and columns).
    fn insert(&self, e: &Entity<'a>, m: Matrix, from: Inherited<'a>, depth: u32, out: &mut Output) -> Result<(), String> {
        let Some(block) = e.text(2).and_then(|name| self.blocks.get(name)) else { return Ok(()) };
        if depth >= MAX_DEPTH {
            return Ok(());
        }
        let place = m.then(Matrix::translate(e.pt(10))).then(Matrix::rotate(e.num(50).unwrap_or(0.0).to_radians()));
        let scale = Matrix::scale(e.num(41).unwrap_or(1.0), e.num(42).unwrap_or(1.0)).then(Matrix::translate((-block.base.0, -block.base.1)));
        let (cols, rows) = (e.int(70).unwrap_or(1).max(1), e.int(71).unwrap_or(1).max(1));
        let (dx, dy) = (e.num(44).unwrap_or(0.0), e.num(45).unwrap_or(0.0));
        for r in 0..rows {
            for c in 0..cols {
                let at = place.then(Matrix::translate((c as f64 * dx, r as f64 * dy))).then(scale);
                for child in &block.entities {
                    self.convert(child, at, from, depth + 1, out)?;
                }
            }
        }
        Ok(())
    }
}

fn header_units(body: &[Tag]) -> f64 {
    let at = body.iter().position(|t| *t == (9, "$INSUNITS"));
    let code = at.and_then(|i| body.get(i + 1)).and_then(|t| t.1.parse::<i64>().ok()).unwrap_or(0);
    match code {
        1 => 25.4,
        2 => 304.8,
        5 => 10.0,
        6 => 1000.0,
        8 => 0.0000254,
        9 => 0.0254,
        10 => 914.4,
        14 => 100.0,
        // Millimetres, or no unit at all: laser files are nearly always in mm.
        _ => 1.0,
    }
}

// ---------------------------------------------------------------- colours and output

#[derive(Clone, Copy)]
struct Inherited<'a> {
    layer: &'a str,
    color: Color,
}

#[derive(Clone, Copy)]
enum Color {
    Aci(i64),
    Rgb(i64),
}

impl Color {
    fn rgb(self) -> [u8; 3] {
        match self {
            Color::Rgb(c) => [(c >> 16) as u8, (c >> 8) as u8, c as u8],
            Color::Aci(i) => aci_rgb(i),
        }
    }
}

/// AutoCAD's colour index as RGB. 7 is "the opposite of the background",
/// which is black on our bed. The numbers from 10 to 249 are a colour wheel
/// in steps of 15°, in five shades, each also half saturated.
fn aci_rgb(i: i64) -> [u8; 3] {
    const FIXED: [[u8; 3]; 9] = [[255, 0, 0], [255, 255, 0], [0, 255, 0], [0, 255, 255], [0, 0, 255], [255, 0, 255], [0, 0, 0], [128, 128, 128], [192, 192, 192]];
    match i {
        1..=9 => FIXED[i as usize - 1],
        10..=249 => {
            let hue = ((i - 10) / 10) as f64 * 15.0 / 60.0;
            let shade = [1.0, 0.65, 0.5, 0.3, 0.15][(i % 10 / 2) as usize];
            let saturation = if i % 2 == 0 { 1.0 } else { 0.5 };
            let x = 1.0 - (hue.rem_euclid(2.0) - 1.0).abs();
            let (r, g, b) = match hue as u32 {
                0 => (1.0, x, 0.0),
                1 => (x, 1.0, 0.0),
                2 => (0.0, 1.0, x),
                3 => (0.0, x, 1.0),
                4 => (x, 0.0, 1.0),
                _ => (1.0, 0.0, x),
            };
            let c = |v: f64| ((1.0 - saturation * (1.0 - v)) * shade * 255.0).round() as u8;
            [c(r), c(g), c(b)]
        }
        250..=255 => {
            let v = [51, 80, 105, 130, 190, 255][i as usize - 250];
            [v, v, v]
        }
        _ => [0, 0, 0],
    }
}

#[derive(Default)]
struct Output {
    paths: HashMap<[u8; 3], Vec<SubPath>>,
    order: Vec<[u8; 3]>,
    count: usize,
    /// Hatch boundaries per colour.
    hatches: HashMap<[u8; 3], Vec<SubPath>>,
    skipped: HashMap<String, usize>,
}

impl Output {
    fn add(&mut self, color: [u8; 3], path: SubPath) -> Result<(), String> {
        self.count += 1;
        if self.count > MAX_PATHS {
            return Err(t!("err-dxf-too-large"));
        }
        if !self.order.contains(&color) {
            self.order.push(color);
        }
        self.paths.entry(color).or_default().push(path);
        Ok(())
    }

    fn add_hatch(&mut self, color: [u8; 3], path: SubPath) -> Result<(), String> {
        self.count += 1;
        if self.count > MAX_PATHS {
            return Err(t!("err-dxf-too-large"));
        }
        if !self.order.contains(&color) {
            self.order.push(color);
        }
        self.hatches.entry(color).or_default().push(path);
        Ok(())
    }

    fn skip(&mut self, kind: &str) {
        *self.skipped.entry(kind.to_string()).or_default() += 1;
    }
}

/// x' = a x + c y + e, y' = b x + d y + f
#[derive(Clone, Copy)]
struct Matrix {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Matrix {
    fn scale(x: f64, y: f64) -> Self {
        Matrix { a: x, b: 0.0, c: 0.0, d: y, e: 0.0, f: 0.0 }
    }

    fn translate((x, y): Pt) -> Self {
        Matrix { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: x, f: y }
    }

    fn rotate(rad: f64) -> Self {
        let (s, c) = rad.sin_cos();
        Matrix { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 }
    }

    /// First `o`, then this one.
    fn then(self, o: Matrix) -> Matrix {
        Matrix {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }

    fn apply(&self, (x, y): Pt) -> Pt {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }
}

// ---------------------------------------------------------------- curves

/// Add an arc of `c + u cos t + v sin t` from `t0` to `t1`, as cubic segments
/// of at most 90°. Starts at the last node, or a new one.
fn push_arc(nodes: &mut Vec<Node>, c: Pt, u: Pt, v: Pt, t0: f64, t1: f64) {
    let at = |t: f64| {
        let (s, co) = t.sin_cos();
        (c.0 + u.0 * co + v.0 * s, c.1 + u.1 * co + v.1 * s)
    };
    let tangent = |t: f64| {
        let (s, co) = t.sin_cos();
        (-u.0 * s + v.0 * co, -u.1 * s + v.1 * co)
    };
    if nodes.is_empty() {
        nodes.push(Node::corner(at(t0)));
    }
    let n = ((t1 - t0).abs() / FRAC_PI_2 - 1e-9).ceil().max(1.0) as usize;
    let step = (t1 - t0) / n as f64;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    for i in 0..n {
        let (a, b) = (t0 + step * i as f64, t0 + step * (i + 1) as f64);
        let (ta, tb) = (tangent(a), tangent(b));
        let last = nodes.last_mut().expect("a node was added");
        last.c_out = Some((last.p.0 + ta.0 * k, last.p.1 + ta.1 * k));
        let p = at(b);
        nodes.push(Node { p, c_in: Some((p.0 - tb.0 * k, p.1 - tb.1 * k)), c_out: None, smooth: i + 1 < n });
    }
}

/// A path from nodes: None if it has no length; closed when asked or when it
/// ends where it started.
fn finish(mut nodes: Vec<Node>, closed: bool) -> Option<SubPath> {
    nodes.dedup_by(|b, a| {
        let same = a.p == b.p && a.c_out.is_none() && b.c_in.is_none();
        if same {
            a.c_out = b.c_out;
        }
        same
    });
    if nodes.len() < 2 {
        return None;
    }
    let meets = nodes.len() > 2 && dist(nodes[0].p, nodes[nodes.len() - 1].p) < 1e-9;
    if meets {
        let last = nodes.pop().expect("there are nodes");
        nodes[0].c_in = last.c_in;
    }
    Some(SubPath { nodes, closed: closed || meets })
}

fn ellipse(e: &Entity) -> Option<SubPath> {
    let (c, major) = (e.pt(10), e.pt(11));
    let ratio = e.num(40).unwrap_or(1.0);
    let (t0, mut t1) = (e.num(41).unwrap_or(0.0), e.num(42).unwrap_or(TAU));
    while t1 <= t0 {
        t1 += TAU;
    }
    // The minor axis is the major one turned 90°, the other way if seen from below.
    let turn = if e.seen_from_below() { -1.0 } else { 1.0 };
    let minor = (-major.1 * ratio * turn, major.0 * ratio * turn);
    let mut nodes = Vec::new();
    push_arc(&mut nodes, c, major, minor, t0, t1);
    finish(nodes, false)
}

/// An LWPOLYLINE's vertices. A bulge (42) belongs to the vertex before it.
fn lwpolyline_vertices(e: &Entity) -> Vec<(Pt, f64)> {
    let mut out: Vec<(Pt, f64)> = Vec::new();
    for &(code, value) in e.tags {
        let Ok(v) = value.parse::<f64>() else { continue };
        match code {
            10 => out.push(((v, 0.0), 0.0)),
            20 => {
                if let Some(last) = out.last_mut() {
                    last.0.1 = v;
                }
            }
            42 => {
                if let Some(last) = out.last_mut() {
                    last.1 = v;
                }
            }
            _ => {}
        }
    }
    out
}

/// Vertices joined by lines or, with a bulge, by arcs: a bulge is the tangent
/// of a quarter of the arc's angle, positive for counter-clockwise.
fn polyline(vertices: &[(Pt, f64)], closed: bool) -> Option<SubPath> {
    let mut nodes: Vec<Node> = vertices.first().map(|v| Node::corner(v.0)).into_iter().collect();
    let count = if closed { vertices.len() } else { vertices.len().saturating_sub(1) };
    for i in 0..count {
        let (a, bulge) = vertices[i];
        let b = vertices[(i + 1) % vertices.len()].0;
        if bulge.abs() < 1e-9 || dist(a, b) < 1e-12 {
            nodes.push(Node::corner(b));
            continue;
        }
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        // The centre, from the middle of the chord along its left normal.
        let s = (1.0 - bulge * bulge) / (4.0 * bulge);
        let c = ((a.0 + b.0) / 2.0 - dy * s, (a.1 + b.1) / 2.0 + dx * s);
        let r = dist(a, c);
        let t0 = (a.1 - c.1).atan2(a.0 - c.0);
        push_arc(&mut nodes, c, (r, 0.0), (0.0, r), t0, t0 + 4.0 * bulge.atan());
        // Exactly on the next vertex, so the following piece starts there.
        nodes.last_mut().expect("the arc added nodes").p = b;
    }
    finish(nodes, closed)
}

/// A spline as Bézier curves. On each knot span a polynomial spline of degree
/// 3 or less is one cubic, exactly, from the ends and tangents of that span.
/// Rational or higher splines are cut into a few parts, which stays far
/// closer than the laser can tell.
fn spline(e: &Entity) -> Option<SubPath> {
    spline_curve(e.int(71).unwrap_or(3) as usize, &e.all(40), &e.points(10), &e.all(41), &e.points(11), e.int(70).unwrap_or(0) & 1 != 0)
}

fn spline_curve(degree: usize, knots: &[f64], ctrl: &[Pt], weights: &[f64], fit: &[Pt], closed: bool) -> Option<SubPath> {
    let polygon = |pts: &[Pt]| finish(pts.iter().map(|&p| Node::corner(p)).collect(), closed);
    if ctrl.is_empty() {
        return polygon(fit);
    }
    if degree < 1 || knots.len() != ctrl.len() + degree + 1 {
        return if fit.is_empty() { polygon(ctrl) } else { polygon(fit) };
    }
    if degree == 1 {
        return polygon(ctrl);
    }
    let weighted = weights.len() == ctrl.len();
    let w = |i: usize| if weighted { weights[i] } else { 1.0 };
    if (0..ctrl.len()).any(|i| w(i) <= 0.0) {
        return polygon(ctrl);
    }
    let rational = weighted && weights.iter().any(|&v| (v - weights[0]).abs() > 1e-9);
    let parts = if rational || degree > 3 { 8 } else { 1 };
    let homogeneous: Vec<[f64; 3]> = ctrl.iter().enumerate().map(|(i, p)| [p.0 * w(i), p.1 * w(i), w(i)]).collect();

    let mut nodes: Vec<Node> = Vec::new();
    for k in degree..ctrl.len() {
        if knots[k + 1] <= knots[k] {
            continue;
        }
        let h = (knots[k + 1] - knots[k]) / parts as f64;
        for part in 0..parts {
            let u0 = knots[k] + h * part as f64;
            let (p0, d0) = spline_at(&homogeneous, knots, degree, k, u0);
            let (p1, d1) = spline_at(&homogeneous, knots, degree, k, u0 + h);
            if nodes.is_empty() {
                nodes.push(Node::corner(p0));
            }
            nodes.last_mut().expect("a node was added").c_out = Some((p0.0 + d0.0 * h / 3.0, p0.1 + d0.1 * h / 3.0));
            nodes.push(Node { p: p1, c_in: Some((p1.0 - d1.0 * h / 3.0, p1.1 - d1.1 * h / 3.0)), c_out: None, smooth: false });
        }
    }
    finish(nodes, closed)
}

/// Point and tangent at `u` of the spline's polynomial on knot span `k` (de
/// Boor), which also gives the span's own values at its far end.
fn spline_at(ctrl: &[[f64; 3]], knots: &[f64], p: usize, k: usize, u: f64) -> (Pt, Pt) {
    let mut d: Vec<[f64; 3]> = ctrl[k - p..=k].to_vec();
    let mut slope = [0.0; 3];
    for r in 1..=p {
        // The last step's two inputs give the derivative.
        if r == p {
            let scale = p as f64 / (knots[k + 1] - knots[k]);
            slope = [0, 1, 2].map(|i| (d[p][i] - d[p - 1][i]) * scale);
        }
        for j in (r..=p).rev() {
            let (lo, hi) = (knots[j + k - p], knots[j + 1 + k - r]);
            let a = if hi > lo { (u - lo) / (hi - lo) } else { 0.0 };
            for i in 0..3 {
                d[j][i] = (1.0 - a) * d[j - 1][i] + a * d[j][i];
            }
        }
    }
    let [x, y, w] = d[p];
    let pos = (x / w, y / w);
    (pos, ((slope[0] - slope[2] * pos.0) / w, (slope[1] - slope[2] * pos.1) / w))
}

// ---------------------------------------------------------------- hatches

/// A hatch's boundaries in mm: each is a polyline, or edges (lines, arcs,
/// ellipses, splines) that may come in any order.
fn hatch_boundaries(e: &Entity, m: Matrix) -> Vec<SubPath> {
    let tags = e.tags;
    let Some(first) = tags.iter().position(|t| t.0 == 92) else { return Vec::new() };
    // Pattern and seed data follow the boundaries, from code 75 on.
    let end = (first..tags.len()).find(|&i| tags[i].0 == 75).unwrap_or(tags.len());
    let starts: Vec<usize> = (first..end).filter(|&i| tags[i].0 == 92).collect();
    let mut out = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        let path = &tags[s..starts.get(n + 1).copied().unwrap_or(end)];
        let flags: i64 = path[0].1.parse().unwrap_or(0);
        if flags & 2 != 0 {
            let boundary = Entity { kind: "", tags: path, vertices: Vec::new() };
            out.extend(polyline(&lwpolyline_vertices(&boundary), true).map(|c| c.map(&|p| m.apply(p))));
            continue;
        }
        let edges: Vec<usize> = (0..path.len()).filter(|&i| path[i].0 == 72).collect();
        let mut pieces = Vec::new();
        for (k, &at) in edges.iter().enumerate() {
            let edge = Entity { kind: "", tags: &path[at + 1..edges.get(k + 1).copied().unwrap_or(path.len())], vertices: Vec::new() };
            let piece = hatch_edge(path[at].1, &edge);
            pieces.extend(piece.map(|c| c.map(&|p| m.apply(p))));
        }
        out.extend(join(pieces).into_iter().filter(|c| c.closed));
    }
    out
}

fn hatch_edge(kind: &str, e: &Entity) -> Option<SubPath> {
    // Angles of clockwise edges are written mirrored.
    let angles = || {
        let ccw = e.int(73).unwrap_or(1) != 0;
        let (a, b) = (e.num(50).unwrap_or(0.0).to_radians(), e.num(51).unwrap_or(0.0).to_radians());
        if ccw {
            (a, if b <= a { b + TAU } else { b })
        } else {
            (-a, if -b >= -a { -b - TAU } else { -b })
        }
    };
    let mut nodes = Vec::new();
    match kind {
        "1" => finish(vec![Node::corner(e.pt(10)), Node::corner(e.pt(11))], false),
        "2" => {
            let r = e.num(40).unwrap_or(0.0);
            let (t0, t1) = angles();
            push_arc(&mut nodes, e.pt(10), (r, 0.0), (0.0, r), t0, t1);
            finish(nodes, false)
        }
        "3" => {
            let major = e.pt(11);
            let ratio = e.num(40).unwrap_or(1.0);
            let (t0, t1) = angles();
            push_arc(&mut nodes, e.pt(10), major, (-major.1 * ratio, major.0 * ratio), t0, t1);
            finish(nodes, false)
        }
        "4" => spline_curve(e.int(94).unwrap_or(3) as usize, &e.all(40), &e.points(10), &e.all(42), &e.points(11), false),
        _ => None,
    }
}

/// Sort curves into (unfilled, filled) per colour. An outline that a fill's
/// boundary matches counts as filled, in the fill's colour: a shape stroked in
/// one colour and filled in another should be engraved, not cut out as well. A
/// fill that matches no outline brings its own boundary.
fn split_filled(outlines: Vec<Vec<SubPath>>, fills: Vec<Vec<SubPath>>) -> Vec<(Vec<SubPath>, Vec<SubPath>)> {
    let bbox = |s: &SubPath| bounds(std::slice::from_ref(s), 0.01);
    let same = |a: (Pt, Pt), b: (Pt, Pt)| dist(a.0, b.0) <= SAME_SHAPE_MM && dist(a.1, b.1) <= SAME_SHAPE_MM;
    let boxes: Vec<Vec<Option<(Pt, Pt)>>> = outlines.iter().map(|paths| paths.iter().map(|s| s.closed.then(|| bbox(s)).flatten()).collect()).collect();
    // For each outline, the colour of the fill that claims it.
    let mut claimed: Vec<Vec<Option<usize>>> = outlines.iter().map(|paths| vec![None; paths.len()]).collect();
    let mut result: Vec<(Vec<SubPath>, Vec<SubPath>)> = vec![(Vec::new(), Vec::new()); outlines.len()];
    for (color, paths) in fills.into_iter().enumerate() {
        for fill in paths {
            let Some(fb) = bbox(&fill) else { continue };
            let mut matched = false;
            for (c, colour_boxes) in boxes.iter().enumerate() {
                for (i, b) in colour_boxes.iter().enumerate() {
                    if b.is_some_and(|b| same(b, fb)) {
                        claimed[c][i].get_or_insert(color);
                        matched = true;
                    }
                }
            }
            if !matched {
                result[color].1.push(fill);
            }
        }
    }
    for (c, paths) in outlines.into_iter().enumerate() {
        for (i, s) in paths.into_iter().enumerate() {
            match claimed[c][i] {
                Some(color) => result[color].1.push(s),
                None => result[c].0.push(s),
            }
        }
    }
    result
}

// ---------------------------------------------------------------- joining

fn reversed(s: &SubPath) -> SubPath {
    let nodes = s.nodes.iter().rev().map(|n| Node { p: n.p, c_in: n.c_out, c_out: n.c_in, smooth: n.smooth }).collect();
    SubPath { nodes, closed: s.closed }
}

/// Paths made of the same points, in either direction.
fn same_key(s: &SubPath) -> Vec<(i64, i64)> {
    let round = |p: Pt| ((p.0 / JOIN_TOLERANCE_MM).round() as i64, (p.1 / JOIN_TOLERANCE_MM).round() as i64);
    let mut pts: Vec<(i64, i64)> = Vec::new();
    for n in &s.nodes {
        pts.extend(n.c_in.map(round));
        pts.push(round(n.p));
        pts.extend(n.c_out.map(round));
    }
    let mut back = pts.clone();
    back.reverse();
    pts.min(back)
}

/// Drop paths drawn twice and join open paths that meet, end to end.
fn join(paths: Vec<SubPath>) -> Vec<SubPath> {
    let mut seen = std::collections::HashSet::new();
    let paths: Vec<SubPath> = paths.into_iter().filter(|s| seen.insert((same_key(s), s.closed))).collect();
    let (closed, mut open): (Vec<SubPath>, Vec<Option<SubPath>>) = {
        let (c, o): (Vec<_>, Vec<_>) = paths.into_iter().partition(|s| s.closed);
        (c, o.into_iter().map(Some).collect())
    };
    // Ends by grid cell. A cell's neighbours are searched too, so ends just
    // over a cell border are found.
    let cell = |p: Pt| ((p.0 / JOIN_TOLERANCE_MM).floor() as i64, (p.1 / JOIN_TOLERANCE_MM).floor() as i64);
    let ends = |s: &SubPath| (s.nodes[0].p, s.nodes[s.nodes.len() - 1].p);
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, s) in open.iter().enumerate() {
        let (a, b) = ends(s.as_ref().expect("all are still there"));
        grid.entry(cell(a)).or_default().push(i);
        grid.entry(cell(b)).or_default().push(i);
    }
    let take = |at: Pt, open: &mut Vec<Option<SubPath>>| -> Option<SubPath> {
        let (cx, cy) = cell(at);
        for x in cx - 1..=cx + 1 {
            for y in cy - 1..=cy + 1 {
                for &i in grid.get(&(x, y)).map_or(&[][..], |v| v) {
                    let Some(s) = &open[i] else { continue };
                    let (a, b) = ends(s);
                    if dist(a, at) <= JOIN_TOLERANCE_MM {
                        return open[i].take();
                    }
                    if dist(b, at) <= JOIN_TOLERANCE_MM {
                        return open[i].take().map(|s| reversed(&s));
                    }
                }
            }
        }
        None
    };
    let mut out = closed;
    for i in 0..open.len() {
        let Some(mut path) = open[i].take() else { continue };
        for backwards in [false, true] {
            if backwards {
                path = reversed(&path);
            }
            while !closes(&path) {
                let Some(next) = take(path.nodes[path.nodes.len() - 1].p, &mut open) else { break };
                let last = path.nodes.last_mut().expect("paths have nodes");
                last.c_out = next.nodes[0].c_out;
                path.nodes.extend_from_slice(&next.nodes[1..]);
            }
        }
        // Looked at both ends; back to the direction it was drawn in.
        path = reversed(&path);
        if closes(&path) {
            let last = path.nodes.pop().expect("paths have nodes");
            path.nodes[0].c_in = last.c_in;
            path.closed = true;
        }
        out.push(path);
    }
    out
}

fn closes(s: &SubPath) -> bool {
    s.nodes.len() > 2 && dist(s.nodes[0].p, s.nodes[s.nodes.len() - 1].p) <= JOIN_TOLERANCE_MM
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bezier::flatten_all;

    /// A DXF file from pairs, with the section around the entities.
    fn file(header: &str, tables: &str, blocks: &str, entities: &str) -> Vec<u8> {
        let section = |name: &str, body: &str| format!("0\nSECTION\n2\n{name}\n{body}0\nENDSEC\n");
        format!("{}{}{}{}0\nEOF\n", section("HEADER", header), section("TABLES", tables), section("BLOCKS", blocks), section("ENTITIES", entities)).into_bytes()
    }

    fn only(entities: &str) -> Drawing {
        read(&file("", "", "", entities)).unwrap()
    }

    fn paths(d: &Drawing) -> Vec<SubPath> {
        d.shapes.iter().flat_map(|s| s.curves.iter().cloned()).collect()
    }

    fn close(a: Pt, b: Pt) -> bool {
        dist(a, b) < 1e-3
    }

    fn line(a: Pt, b: Pt) -> String {
        format!("0\nLINE\n10\n{}\n20\n{}\n11\n{}\n21\n{}\n", a.0, a.1, b.0, b.1)
    }

    #[test]
    fn lines_flip_to_y_down_and_units_become_mm() {
        let d = read(&file("9\n$INSUNITS\n70\n1\n", "", "", &line((0.0, 0.0), (1.0, 2.0)))).unwrap();
        let p = &paths(&d)[0];
        assert!(close(p.nodes[1].p, (25.4, -50.8)), "{:?}", p.nodes[1].p);
    }

    #[test]
    fn loose_lines_join_into_a_closed_outline_in_any_direction() {
        // A square of four lines, one drawn the other way round and one 5 µm off.
        let mut e = line((0.0, 0.0), (10.0, 0.0));
        e += &line((10.0, 10.0), (10.0, 0.0));
        e += &line((10.0, 10.005), (0.0, 10.0));
        e += &line((0.0, 10.0), (0.0, 0.0));
        let all = paths(&only(&e));
        assert_eq!(all.len(), 1);
        assert!(all[0].closed);
        assert_eq!(all[0].nodes.len(), 4);
    }

    #[test]
    fn lines_drawn_twice_count_once() {
        let mut e = line((0.0, 0.0), (10.0, 0.0));
        e += &line((10.0, 0.0), (0.0, 0.0));
        e += &line((0.0, 0.0), (10.0, 0.0));
        let all = paths(&only(&e));
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].nodes.len(), 2);
    }

    #[test]
    fn circles_and_arcs_are_true_to_size() {
        let d = only("0\nCIRCLE\n10\n5\n20\n5\n40\n5\n");
        let c = &paths(&d)[0];
        assert!(c.closed);
        let ((x0, y0), (x1, y1)) = bounds(std::slice::from_ref(c), 0.001).unwrap();
        assert!(close((x0, y0), (0.0, -10.0)) && close((x1, y1), (10.0, 0.0)));
        for p in flatten_all(std::slice::from_ref(c), 0.001).concat() {
            assert!((dist(p, (5.0, -5.0)) - 5.0).abs() < 0.002, "{p:?}");
        }
        // A quarter arc from 0° to 90° (counter-clockwise, y up).
        let a = &paths(&only("0\nARC\n10\n0\n20\n0\n40\n2\n50\n0\n51\n90\n"))[0];
        assert!(close(a.nodes[0].p, (2.0, 0.0)) && close(a.nodes[1].p, (0.0, -2.0)));
    }

    #[test]
    fn bulges_make_arcs() {
        // Bulge 1 is a half circle; from (0,0) to (2,0) counter-clockwise it passes below (y up).
        let e = "0\nLWPOLYLINE\n90\n2\n70\n0\n10\n0\n20\n0\n42\n1\n10\n2\n20\n0\n";
        let p = &paths(&only(e))[0];
        let pts = flatten_all(std::slice::from_ref(p), 0.001).concat();
        let mid = pts.iter().fold(pts[0], |best, q| if q.1 > best.1 { *q } else { best });
        // y down on the bed, so "below" has a positive y.
        assert!(close(mid, (1.0, 1.0)), "{mid:?}");
        for q in &pts {
            assert!((dist(*q, (1.0, 0.0)) - 1.0).abs() < 0.002);
        }
    }

    #[test]
    fn closed_polylines_and_old_polylines() {
        let lw = "0\nLWPOLYLINE\n90\n3\n70\n1\n10\n0\n20\n0\n10\n4\n20\n0\n10\n4\n20\n3\n";
        let p = &paths(&only(lw))[0];
        assert!(p.closed);
        assert_eq!(p.nodes.len(), 3);
        let old = "0\nPOLYLINE\n70\n1\n0\nVERTEX\n10\n0\n20\n0\n0\nVERTEX\n10\n4\n20\n0\n0\nVERTEX\n10\n4\n20\n3\n0\nSEQEND\n";
        let p = &paths(&only(old))[0];
        assert!(p.closed);
        assert_eq!(p.nodes.len(), 3);
    }

    #[test]
    fn cubic_splines_are_exact() {
        // One Bézier curve written as a clamped spline, then split at an interior knot.
        let (a, b, c, d) = ((0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 0.0));
        let e = format!("0\nSPLINE\n70\n8\n71\n3\n72\n8\n73\n4\n40\n0\n40\n0\n40\n0\n40\n0\n40\n1\n40\n1\n40\n1\n40\n1\n10\n{}\n20\n{}\n10\n{}\n20\n{}\n10\n{}\n20\n{}\n10\n{}\n20\n{}\n", a.0, a.1, b.0, b.1, c.0, c.1, d.0, d.1);
        let p = &paths(&only(&e))[0];
        assert_eq!(p.nodes.len(), 2);
        assert!(close(p.nodes[0].c_out.unwrap(), (0.0, -10.0)) && close(p.nodes[1].c_in.unwrap(), (10.0, -10.0)));
        // Two spans: passes through the same curve's middle at the knot.
        let e = "0\nSPLINE\n70\n8\n71\n3\n72\n9\n73\n5\n40\n0\n40\n0\n40\n0\n40\n0\n40\n1\n40\n2\n40\n2\n40\n2\n40\n2\n10\n0\n20\n0\n10\n1\n20\n3\n10\n4\n20\n4\n10\n7\n20\n3\n10\n8\n20\n0\n";
        let p = &paths(&only(e))[0];
        assert_eq!(p.nodes.len(), 3);
        assert!(close(p.nodes[0].p, (0.0, 0.0)) && close(p.nodes[2].p, (8.0, 0.0)));
        // Smooth where the two parts meet.
        let n = &p.nodes[1];
        let (i, o) = (n.c_in.unwrap(), n.c_out.unwrap());
        assert!(close((n.p.0 * 2.0 - i.0, n.p.1 * 2.0 - i.1), o), "{n:?}");
    }

    #[test]
    fn weighted_splines_keep_their_shape() {
        // A quarter circle as a rational quadratic spline.
        let w = std::f64::consts::FRAC_1_SQRT_2;
        let e = format!("0\nSPLINE\n70\n12\n71\n2\n72\n6\n73\n3\n40\n0\n40\n0\n40\n0\n40\n1\n40\n1\n40\n1\n41\n1\n41\n{w}\n41\n1\n10\n5\n20\n0\n10\n5\n20\n5\n10\n0\n20\n5\n");
        let p = &paths(&only(&e))[0];
        for q in flatten_all(std::slice::from_ref(p), 0.001).concat() {
            assert!((dist(q, (0.0, 0.0)) - 5.0).abs() < 0.005, "{q:?}");
        }
    }

    #[test]
    fn nested_blocks_are_placed_scaled_and_turned() {
        let inner = "0\nBLOCK\n2\nINNER\n10\n1\n20\n0\n0\nLINE\n10\n1\n20\n0\n11\n2\n21\n0\n0\nENDBLK\n";
        let outer = "0\nBLOCK\n2\nOUTER\n10\n0\n20\n0\n0\nINSERT\n2\nINNER\n10\n0\n20\n0\n41\n2\n42\n2\n0\nENDBLK\n";
        let d = read(&file("", "", &format!("{inner}{outer}"), "0\nINSERT\n2\nOUTER\n10\n10\n20\n0\n50\n90\n")).unwrap();
        // From the block's base, scaled by 2 and turned 90° to y up, at (10, 0).
        let p = &paths(&d)[0];
        assert!(close(p.nodes[0].p, (10.0, 0.0)) && close(p.nodes[1].p, (10.0, -2.0)), "{p:?}");
    }

    #[test]
    fn blocks_in_grids_and_blocks_that_contain_themselves() {
        let b = "0\nBLOCK\n2\nA\n0\nLINE\n11\n1\n0\nENDBLK\n";
        let d = read(&file("", "", b, "0\nINSERT\n2\nA\n70\n3\n71\n2\n44\n5\n45\n7\n")).unwrap();
        assert_eq!(paths(&d).len(), 6);
        // Doesn't hang or overflow.
        let selfish = "0\nBLOCK\n2\nA\n0\nLINE\n11\n1\n0\nINSERT\n2\nA\n10\n2\n0\nENDBLK\n";
        assert!(read(&file("", "", selfish, "0\nINSERT\n2\nA\n")).is_ok());
    }

    #[test]
    fn mirrored_circles_end_up_on_the_mirrored_side() {
        let c = |extra: &str| paths(&only(&format!("0\nCIRCLE\n10\n5\n20\n0\n40\n1\n{extra}")))[0].clone();
        let right = bounds(&[c("")], 0.01).unwrap();
        let left = bounds(&[c("230\n-1\n")], 0.01).unwrap();
        assert!(right.0.0 > 0.0 && left.1.0 < 0.0, "{right:?} {left:?}");
    }

    #[test]
    fn colours_become_shapes_by_entity_layer_and_block() {
        let tables = "0\nTABLE\n2\nLAYER\n0\nLAYER\n2\nCUT\n62\n1\n70\n0\n0\nLAYER\n2\nOFF\n62\n-3\n70\n0\n0\nLAYER\n2\nFROZEN\n62\n3\n70\n1\n0\nENDTAB\n";
        let on = |layer: &str, x: f64, extra: &str| format!("0\nLINE\n8\n{layer}\n{extra}10\n{x}\n20\n0\n11\n{x}\n21\n1\n");
        let blocks = "0\nBLOCK\n2\nB\n0\nLINE\n8\n0\n62\n0\n11\n1\n0\nENDBLK\n";
        let mut e = on("CUT", 0.0, "");
        e += &on("CUT", 1.0, "62\n5\n");
        e += &on("CUT", 2.0, "420\n65280\n");
        e += &on("OFF", 3.0, "");
        e += &on("FROZEN", 4.0, "");
        e += &on("CUT", 5.0, "67\n1\n");
        e += "0\nINSERT\n2\nB\n8\nCUT\n62\n6\n";
        let d = read(&file("", tables, blocks, &e)).unwrap();
        let colors: Vec<[u8; 3]> = d.shapes.iter().map(|s| s.color).collect();
        // The layer's red, a blue of its own, true-colour green, and the
        // magenta the block's line takes from its insert. Hidden layers and
        // paper space are gone.
        assert_eq!(colors, vec![[255, 0, 0], [0, 0, 255], [0, 255, 0], [255, 0, 255]]);
        assert_eq!(paths(&d).len(), 4);
    }

    #[test]
    fn what_is_left_out_is_reported() {
        let d = only(&format!("{}0\nTEXT\n1\nHi\n0\nTEXT\n1\nYo\n0\nPOINT\n", line((0.0, 0.0), (1.0, 0.0))));
        assert_eq!(d.skipped, vec![("TEXT".to_string(), 2)]);
    }

    #[test]
    fn hatched_outlines_separate_from_the_plain_ones() {
        // A plate and a hatched square of the same colour: the square gets its own colour.
        let square = "0\nLWPOLYLINE\n70\n1\n10\n1\n20\n1\n10\n3\n20\n1\n10\n3\n20\n3\n10\n1\n20\n3\n";
        let plate = "0\nLWPOLYLINE\n70\n1\n10\n0\n20\n0\n10\n9\n20\n0\n10\n9\n20\n9\n10\n0\n20\n9\n";
        let hatch = "0\nHATCH\n10\n0\n20\n0\n91\n1\n92\n2\n72\n0\n73\n1\n93\n4\n10\n1\n20\n1\n10\n3\n20\n1\n10\n3\n20\n3\n10\n1\n20\n3\n75\n1\n98\n1\n10\n50\n20\n50\n";
        let d = only(&format!("{plate}{square}{hatch}"));
        assert_eq!(d.shapes.len(), 2);
        let (plain, filled) = (&d.shapes[0], &d.shapes[1]);
        assert!(!plain.filled && filled.filled);
        assert_ne!(plain.color, filled.color);
        // One curve each: the hatch didn't add a second square, and the seed point is no vertex.
        assert_eq!((plain.curves.len(), filled.curves.len()), (1, 1));
        assert_eq!(filled.curves[0].nodes.len(), 4);
        assert!(close(filled.curves[0].nodes[0].p, (1.0, -1.0)));
        assert!(d.skipped.is_empty());
    }

    #[test]
    fn a_stroke_in_one_colour_and_a_fill_in_another_is_engraved_only() {
        // Black letter outline, its fill in blue, and a black plate around them.
        let square = "0\nLWPOLYLINE\n62\n7\n70\n1\n10\n1\n20\n1\n10\n3\n20\n1\n10\n3\n20\n3\n10\n1\n20\n3\n";
        let plate = "0\nLWPOLYLINE\n62\n7\n70\n1\n10\n0\n20\n0\n10\n9\n20\n0\n10\n9\n20\n9\n10\n0\n20\n9\n";
        let hatch = "0\nHATCH\n62\n5\n91\n1\n92\n2\n72\n0\n73\n1\n93\n4\n10\n1\n20\n1\n10\n3\n20\n1\n10\n3\n20\n3\n10\n1\n20\n3\n75\n1\n";
        let d = only(&format!("{plate}{square}{hatch}"));
        let (black, blue) = (d.shapes.iter().find(|s| s.color == [0, 0, 0]).unwrap(), d.shapes.iter().find(|s| s.color == [0, 0, 255]).unwrap());
        assert!(!black.filled && blue.filled);
        // The plate stays to be cut, and the letter is not cut as well.
        assert_eq!((black.curves.len(), blue.curves.len()), (1, 1));
        assert_eq!(d.shapes.len(), 2);
    }

    #[test]
    fn a_hatch_without_outline_brings_its_boundary() {
        // Edges out of order: a line, a half circle on top (clockwise), a closing line is not needed.
        let edges = "0\nHATCH\n91\n1\n92\n1\n93\n2\n72\n2\n10\n1\n20\n0\n40\n1\n50\n0\n51\n180\n73\n1\n72\n1\n10\n0\n20\n0\n11\n2\n21\n0\n75\n1\n";
        let d = only(edges);
        assert_eq!(d.shapes.len(), 1);
        assert!(d.shapes[0].filled);
        // Keeps the file's colour: nothing else uses it.
        assert_eq!(d.shapes[0].color, [0, 0, 0]);
        let c = &d.shapes[0].curves[0];
        assert!(c.closed);
        let ((x0, y0), (x1, y1)) = bounds(std::slice::from_ref(c), 0.001).unwrap();
        assert!(close((x0, y0), (0.0, -1.0)) && close((x1, y1), (2.0, 0.0)), "{x0} {y0} {x1} {y1}");
    }

    #[test]
    fn clockwise_hatch_arcs_go_the_written_way() {
        // Angles 0 to 90 written for a clockwise edge run from 0° to -90°, below the centre (y up).
        let e = Entity { kind: "", tags: &[(10, "0"), (20, "0"), (40, "1"), (50, "0"), (51, "90"), (73, "0")], vertices: Vec::new() };
        let c = hatch_edge("2", &e).unwrap();
        assert!(close(c.nodes[0].p, (1.0, 0.0)) && close(c.nodes[c.nodes.len() - 1].p, (0.0, -1.0)), "{c:?}");
    }

    #[test]
    fn spline_edges_in_hatches() {
        let tags: &[Tag] = &[(94, "3"), (73, "0"), (74, "0"), (95, "8"), (96, "4"), (40, "0"), (40, "0"), (40, "0"), (40, "0"), (40, "1"), (40, "1"), (40, "1"), (40, "1"), (10, "0"), (20, "0"), (10, "0"), (20, "1"), (10, "1"), (20, "1"), (10, "1"), (20, "0")];
        let e = Entity { kind: "", tags, vertices: Vec::new() };
        let c = hatch_edge("4", &e).unwrap();
        assert_eq!(c.nodes.len(), 2);
        assert!(close(c.nodes[1].p, (1.0, 0.0)));
    }

    #[test]
    fn bad_files_get_a_clear_message() {
        assert_eq!(read(b"AutoCAD Binary DXF\r\n\x1a\0...").unwrap_err(), t!("err-dxf-binary"));
        assert_eq!(read(b"not a dxf at all").unwrap_err(), t!("err-dxf-unreadable"));
        assert_eq!(read(&file("", "", "", "0\nTEXT\n")).unwrap_err(), t!("err-dxf-empty"));
        assert_eq!(read(&file("", "", "", "0\nHATCH\n")).unwrap_err(), t!("err-dxf-empty"));
    }

    #[test]
    fn windows_line_ends_work() {
        let text = String::from_utf8(file("", "", "", &line((0.0, 0.0), (1.0, 0.0)))).unwrap().replace('\n', "\r\n");
        assert_eq!(paths(&read(text.as_bytes()).unwrap()).len(), 1);
    }

    #[test]
    fn colour_index_samples() {
        assert_eq!(aci_rgb(1), [255, 0, 0]);
        assert_eq!(aci_rgb(7), [0, 0, 0]);
        assert_eq!(aci_rgb(10), [255, 0, 0]);
        assert_eq!(aci_rgb(130), [0, 255, 255]);
        assert_eq!(aci_rgb(255), [255, 255, 255]);
    }
}
