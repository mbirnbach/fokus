//! The design being edited: a list of objects (shapes, paths, text), what
//! happens to each colour, and undo history.
//!
//! Design coordinates are mm on the bed as seen from the front of the
//! machine: x to the right, y down, origin at the bed's top-left. The job
//! itself always starts where the laser head is, with the design's top-right
//! corner (the corner nearest our machine's origin) at the head.
//!
//! Burn settings are not stored here: a job uses one material, and each
//! colour layer only says what to do (cut, mark, engrave or skip). The
//! material supplies the speed and power for that operation.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use ruida::writer::{JobLayer, LayerKind};
use serde::{Deserialize, Serialize};

use crate::materials::{Material, OpSettings, Operation};
use crate::t;

pub type Pt = (f64, f64);
pub type Bounds = (Pt, Pt);

/// Curves are flattened to lines no further than this from the true curve.
const FLATTEN_TOLERANCE_MM: f64 = 0.02;
const PX_TO_MM: f64 = 25.4 / 96.0;
/// Hatch spacing shown for engrave layers that have no settings yet.
pub const PREVIEW_LINE_INTERVAL_MM: f32 = 0.1;
/// Time estimate constants, measured on our machine (see `layer_time_s`).
const LINE_TURN_S: f64 = 0.12;
const PATH_START_S: f64 = 0.2;
/// Smallest width / height an object can be resized to.
pub const MIN_SIZE: f64 = 0.1;
pub const DEFAULT_TEXT_HEIGHT: f64 = 10.0;

// ---------------------------------------------------------------- objects

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Font {
    Sans,
    Serif,
    Mono,
}

impl Font {
    pub const ALL: [Font; 3] = [Font::Sans, Font::Serif, Font::Mono];

    pub fn label(self) -> String {
        match self {
            Font::Sans => t!("font-sans"),
            Font::Serif => t!("font-serif"),
            Font::Mono => t!("font-mono"),
        }
    }

    fn family(self) -> &'static str {
        match self {
            Font::Sans => "Helvetica, Arial, Liberation Sans, DejaVu Sans, sans-serif",
            Font::Serif => "Georgia, Times New Roman, Liberation Serif, DejaVu Serif, serif",
            Font::Mono => "Menlo, Consolas, Liberation Mono, DejaVu Sans Mono, monospace",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Kind {
    Rect { radius: f64 },
    Ellipse,
    Polygon { sides: u32 },
    /// Drawn lines or imported paths, normalised to 0..1 of the object's box.
    Path { polylines: Vec<Vec<Pt>>, filled: bool },
    /// Text as outlines, normalised to 0..1 of the object's box.
    Text { text: String, font: Font, bold: bool, outlines: Vec<Vec<Pt>>, aspect: f64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Object {
    pub name: String,
    pub kind: Kind,
    /// Box (before rotation): top-left corner and size, mm.
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Degrees, clockwise, around the box centre.
    pub rotation: f64,
    pub flip_x: bool,
    pub flip_y: bool,
    pub color: [u8; 3],
    pub group: Option<u32>,
}

impl Object {
    pub fn new(kind: Kind, (x, y, w, h): (f64, f64, f64, f64), color: [u8; 3]) -> Self {
        let name = match &kind {
            Kind::Rect { .. } => t!("object-rect"),
            Kind::Ellipse => t!("object-ellipse"),
            Kind::Polygon { .. } => t!("object-polygon"),
            Kind::Path { .. } => t!("object-path"),
            Kind::Text { .. } => t!("object-text"),
        };
        Object { name, kind, x, y, w, h, rotation: 0.0, flip_x: false, flip_y: false, color, group: None }
    }

    /// A line or polyline through `points` (mm).
    pub fn polyline(points: Vec<Pt>, color: [u8; 3]) -> Self {
        let (polylines, (x, y, w, h)) = normalise(vec![points]);
        let mut o = Object::new(Kind::Path { polylines, filled: false }, (x, y, w, h), color);
        o.name = t!("object-line");
        o
    }

    /// Text with its outlines rendered; `h` is the height of the lettering.
    pub fn text(text: &str, font: Font, bold: bool, (x, y): Pt, h: f64, color: [u8; 3]) -> Result<Self, String> {
        let mut o = Object::new(Kind::Text { text: String::new(), font, bold, outlines: Vec::new(), aspect: 1.0 }, (x, y, h, h), color);
        o.set_text(text, font, bold)?;
        Ok(o)
    }

    /// Change a text object's content or font; keeps the height.
    pub fn set_text(&mut self, text: &str, font: Font, bold: bool) -> Result<(), String> {
        let Kind::Text { .. } = self.kind else { return Err("not a text object".into()) };
        let outlines = text_outlines(text, font, bold)?;
        let (outlines, (_, _, w, h)) = normalise(outlines);
        let aspect = if h > 0.0 { w / h } else { 1.0 };
        self.w = (self.h * aspect).max(MIN_SIZE);
        self.kind = Kind::Text { text: text.to_string(), font, bold, outlines, aspect };
        Ok(())
    }

    /// Filled shapes suggest engraving rather than cutting.
    pub fn fill_hint(&self) -> bool {
        matches!(self.kind, Kind::Path { filled: true, .. } | Kind::Text { .. })
    }

    /// Outline in mm relative to the box's top-left, before rotation.
    fn local(&self) -> Vec<Vec<Pt>> {
        let (w, h) = (self.w, self.h);
        let mut out = match &self.kind {
            Kind::Rect { radius } => vec![rounded_rect(w, h, *radius)],
            Kind::Ellipse => vec![ellipse(w, h)],
            Kind::Polygon { sides } => vec![regular_polygon(w, h, *sides)],
            Kind::Path { polylines, .. } | Kind::Text { outlines: polylines, .. } => {
                polylines.iter().map(|pl| pl.iter().map(|&(u, v)| (u * w, v * h)).collect()).collect()
            }
        };
        if self.flip_x || self.flip_y {
            for p in out.iter_mut().flatten() {
                if self.flip_x {
                    p.0 = w - p.0;
                }
                if self.flip_y {
                    p.1 = h - p.1;
                }
            }
        }
        out
    }

    /// Outline in design coordinates.
    pub fn outline(&self) -> Vec<Vec<Pt>> {
        let (cx, cy) = (self.w / 2.0, self.h / 2.0);
        let (sin, cos) = self.rotation.to_radians().sin_cos();
        self.local()
            .into_iter()
            .map(|pl| {
                pl.into_iter()
                    .map(|(px, py)| {
                        let (dx, dy) = (px - cx, py - cy);
                        (self.x + cx + dx * cos - dy * sin, self.y + cy + dx * sin + dy * cos)
                    })
                    .collect()
            })
            .collect()
    }

    pub fn bounds(&self) -> Bounds {
        if self.rotation.rem_euclid(360.0) == 0.0 {
            return ((self.x, self.y), (self.x + self.w, self.y + self.h));
        }
        points_bounds(self.outline().iter().flatten()).unwrap_or(((self.x, self.y), (self.x, self.y)))
    }
}

fn rounded_rect(w: f64, h: f64, radius: f64) -> Vec<Pt> {
    let r = radius.clamp(0.0, w.min(h) / 2.0);
    if r <= 0.0 {
        return vec![(0.0, 0.0), (w, 0.0), (w, h), (0.0, h), (0.0, 0.0)];
    }
    let n = ((r * std::f64::consts::FRAC_PI_2 / 0.3).ceil() as usize).clamp(4, 64);
    let mut pts = Vec::new();
    // Corners clockwise from top-right, each a quarter arc.
    for (cx, cy, start) in [(w - r, r, -90.0f64), (w - r, h - r, 0.0), (r, h - r, 90.0), (r, r, 180.0)] {
        for i in 0..=n {
            let a = (start + 90.0 * i as f64 / n as f64).to_radians();
            pts.push((cx + r * a.cos(), cy + r * a.sin()));
        }
    }
    pts.push(pts[0]);
    pts
}

fn ellipse(w: f64, h: f64) -> Vec<Pt> {
    let (rx, ry) = (w / 2.0, h / 2.0);
    let perimeter = std::f64::consts::PI * (3.0 * (rx + ry) - ((3.0 * rx + ry) * (rx + 3.0 * ry)).max(0.0).sqrt());
    let n = ((perimeter / 0.4).ceil() as usize).clamp(24, 720);
    (0..=n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            (rx + rx * a.cos(), ry + ry * a.sin())
        })
        .collect()
}

fn regular_polygon(w: f64, h: f64, sides: u32) -> Vec<Pt> {
    let n = sides.max(3);
    (0..=n)
        .map(|i| {
            let a = -std::f64::consts::FRAC_PI_2 + std::f64::consts::TAU * i as f64 / n as f64;
            (w / 2.0 + w / 2.0 * a.cos(), h / 2.0 + h / 2.0 * a.sin())
        })
        .collect()
}

fn points_bounds<'a>(pts: impl Iterator<Item = &'a Pt>) -> Option<Bounds> {
    pts.fold(None, |b, &(x, y)| match b {
        None => Some(((x, y), (x, y))),
        Some(((x0, y0), (x1, y1))) => Some(((x0.min(x), y0.min(y)), (x1.max(x), y1.max(y)))),
    })
}

/// Scale polylines (mm) into 0..1 of their bounds; returns them with the
/// bounds as (x, y, w, h). A zero width or height stays 0.
fn normalise(polylines: Vec<Vec<Pt>>) -> (Vec<Vec<Pt>>, (f64, f64, f64, f64)) {
    let ((x0, y0), (x1, y1)) = points_bounds(polylines.iter().flatten()).unwrap_or(((0.0, 0.0), (0.0, 0.0)));
    let (w, h) = (x1 - x0, y1 - y0);
    let n = |v: f64, lo: f64, size: f64| if size > 0.0 { (v - lo) / size } else { 0.0 };
    let out = polylines.into_iter().map(|pl| pl.into_iter().map(|(x, y)| (n(x, x0, w), n(y, y0, h))).collect()).collect();
    (out, (x0, y0, w, h))
}

pub fn union(a: Bounds, b: Bounds) -> Bounds {
    ((a.0.0.min(b.0.0), a.0.1.min(b.0.1)), (a.1.0.max(b.1.0), a.1.1.max(b.1.1)))
}

// ---------------------------------------------------------------- start point

/// Where on the design the laser head is when the job starts, as in
/// LotusCut: one of 9 points on the box around all objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    /// -1 = left, 0 = centre, 1 = right.
    pub h: i8,
    /// -1 = top, 0 = middle, 1 = bottom.
    pub v: i8,
}

impl Anchor {
    pub const TOP_RIGHT: Anchor = Anchor { h: 1, v: -1 };

    pub fn point(self, ((x0, y0), (x1, y1)): Bounds) -> Pt {
        let pick = |a: f64, b: f64, t: i8| match t {
            -1 => a,
            0 => (a + b) / 2.0,
            _ => b,
        };
        (pick(x0, x1, self.h), pick(y0, y1, self.v))
    }

    pub fn label(self) -> String {
        match (self.v, self.h) {
            (-1, -1) => t!("anchor-top-left"),
            (-1, 0) => t!("anchor-top"),
            (-1, _) => t!("anchor-top-right"),
            (0, -1) => t!("anchor-left"),
            (0, 0) => t!("anchor-centre"),
            (0, _) => t!("anchor-right"),
            (_, -1) => t!("anchor-bottom-left"),
            (_, 0) => t!("anchor-bottom"),
            _ => t!("anchor-bottom-right"),
        }
    }
}

// ---------------------------------------------------------------- layers

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Do(Operation),
    Skip,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerSettings {
    pub color: [u8; 3],
    pub action: Action,
    /// Hand-tuned values replacing the material's for this layer.
    pub adjusted: Option<OpSettings>,
}

/// Why a layer can't be lasered as set up.
#[derive(Debug, Clone, PartialEq)]
pub enum LayerProblem {
    NoMaterial,
    NotForMaterial(Operation),
    Unsupported(Operation),
}

impl LayerProblem {
    pub fn message(&self, material: Option<&Material>) -> String {
        match self {
            LayerProblem::NoMaterial => t!("problem-no-material"),
            LayerProblem::NotForMaterial(op) => t!(
                "problem-not-for-material",
                material = material.map(|m| m.name.clone()).unwrap_or_else(|| t!("problem-this-material")),
                operation = op.label()
            ),
            LayerProblem::Unsupported(op) => t!("problem-unsupported", operation = op.label()),
        }
    }
}

impl LayerSettings {
    fn new(color: [u8; 3], filled: bool) -> Self {
        let op = if filled { Operation::Engrave } else { Operation::Cut };
        LayerSettings { color, action: Action::Do(op), adjusted: None }
    }

    /// The settings this layer will be lasered with.
    pub fn resolve(&self, material: Option<&Material>) -> Result<Option<(Operation, OpSettings)>, LayerProblem> {
        let Action::Do(op) = self.action else { return Ok(None) };
        if op == Operation::Photo {
            return Err(LayerProblem::Unsupported(op));
        }
        if let Some(s) = &self.adjusted {
            return Ok(Some((op, s.clone())));
        }
        let material = material.ok_or(LayerProblem::NoMaterial)?;
        material.settings(op).cloned().map(|s| Some((op, s))).ok_or(LayerProblem::NotForMaterial(op))
    }
}

// ---------------------------------------------------------------- design

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Design {
    pub objects: Vec<Object>,
    pub layers: Vec<LayerSettings>,
    /// Bumped on every geometry change, for caches (not saved).
    #[serde(skip)]
    pub revision: u64,
    next_group: u32,
}

impl Design {
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Same drawing and layer setup, ignoring the cache revision.
    pub fn same_content(&self, other: &Design) -> bool {
        self.objects == other.objects && self.layers == other.layers
    }

    pub fn bounds(&self) -> Option<Bounds> {
        self.objects.iter().map(Object::bounds).reduce(union)
    }

    pub fn selection_bounds(&self, sel: &[usize]) -> Option<Bounds> {
        sel.iter().filter_map(|&i| self.objects.get(i)).map(Object::bounds).reduce(union)
    }

    /// Call after changing objects: refreshes caches and the layer list.
    pub fn touch(&mut self) {
        self.revision += 1;
        for o in &self.objects {
            if !self.layers.iter().any(|l| l.color == o.color) {
                self.layers.push(LayerSettings::new(o.color, o.fill_hint()));
            }
        }
        let used: Vec<[u8; 3]> = self.objects.iter().map(|o| o.color).collect();
        self.layers.retain(|l| used.contains(&l.color));
    }

    pub fn add(&mut self, object: Object) -> usize {
        self.objects.push(object);
        self.touch();
        self.objects.len() - 1
    }

    /// Import an SVG as a group, its top-right corner at `top_right`.
    /// Returns the new objects' indices.
    pub fn import_svg(&mut self, path: &Path, top_right: Pt) -> Result<Vec<usize>, String> {
        let data = std::fs::read(path).map_err(|e| t!("err-could-not-read", file = path.display().to_string(), error = e.to_string()))?;
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.import_svg_data(&data, &name, top_right)
    }

    fn import_svg_data(&mut self, data: &[u8], name: &str, top_right: Pt) -> Result<Vec<usize>, String> {
        let shapes = svg_shapes(data)?;
        let ((_, y0), (x1, _)) = points_bounds(shapes.iter().flat_map(|s| s.polylines.iter().flatten())).unwrap();
        let (dx, dy) = (top_right.0 - x1, top_right.1 - y0);
        let group = self.new_group();
        let first = self.objects.len();
        for s in shapes {
            let moved = s.polylines.into_iter().map(|pl| pl.into_iter().map(|(x, y)| (x + dx, y + dy)).collect()).collect();
            let (polylines, rect) = normalise(moved);
            let mut o = Object::new(Kind::Path { polylines, filled: s.filled }, rect, s.color);
            o.name = name.to_string();
            o.group = Some(group);
            self.objects.push(o);
        }
        self.touch();
        Ok((first..self.objects.len()).collect())
    }

    fn new_group(&mut self) -> u32 {
        self.next_group += 1;
        self.next_group
    }

    /// The object and every other member of its group.
    pub fn with_group(&self, i: usize) -> Vec<usize> {
        match self.objects[i].group {
            Some(g) => (0..self.objects.len()).filter(|&j| self.objects[j].group == Some(g)).collect(),
            None => vec![i],
        }
    }

    pub fn group(&mut self, sel: &[usize]) {
        if sel.len() < 2 {
            return;
        }
        let g = self.new_group();
        for &i in sel {
            self.objects[i].group = Some(g);
        }
    }

    pub fn ungroup(&mut self, sel: &[usize]) {
        for &i in sel {
            self.objects[i].group = None;
        }
    }

    /// Copies of the selection, offset a little; returns their indices.
    pub fn duplicate(&mut self, sel: &[usize]) -> Vec<usize> {
        let mut groups = std::collections::HashMap::new();
        let first = self.objects.len();
        for &i in sel {
            let mut o = self.objects[i].clone();
            o.x += 5.0;
            o.y += 5.0;
            if let Some(g) = o.group {
                let next = *groups.entry(g).or_insert_with(|| {
                    self.next_group += 1;
                    self.next_group
                });
                o.group = Some(next);
            }
            self.objects.push(o);
        }
        self.touch();
        (first..self.objects.len()).collect()
    }

    pub fn remove(&mut self, sel: &[usize]) {
        let mut sel = sel.to_vec();
        sel.sort_unstable();
        for i in sel.into_iter().rev() {
            if i < self.objects.len() {
                self.objects.remove(i);
            }
        }
        self.touch();
    }

    pub fn translate(&mut self, sel: &[usize], (dx, dy): Pt) {
        for &i in sel {
            self.objects[i].x += dx;
            self.objects[i].y += dy;
        }
        self.revision += 1;
    }

    /// Scale the selection so its bounds become `to`; `originals` are the
    /// objects as they were when the resize started, `from` their bounds.
    pub fn resize(&mut self, originals: &[(usize, Object)], from: Bounds, to: Bounds) {
        let ((fx0, fy0), (fx1, fy1)) = from;
        let ((tx0, ty0), (tx1, ty1)) = to;
        let sx = (tx1 - tx0) / (fx1 - fx0).max(1e-9);
        let sy = (ty1 - ty0) / (fy1 - fy0).max(1e-9);
        for (i, o) in originals {
            let obj = &mut self.objects[*i];
            obj.x = tx0 + (o.x - fx0) * sx;
            obj.y = ty0 + (o.y - fy0) * sy;
            obj.w = (o.w * sx).max(if o.w > 0.0 { MIN_SIZE } else { 0.0 });
            obj.h = (o.h * sy).max(if o.h > 0.0 { MIN_SIZE } else { 0.0 });
        }
        self.revision += 1;
    }

    /// Outlines of one layer in design coordinates.
    pub fn layer_paths(&self, color: [u8; 3]) -> Vec<Vec<Pt>> {
        self.objects.iter().filter(|o| o.color == color).flat_map(|o| o.outline()).collect()
    }

    pub fn layer_length_mm(&self, color: [u8; 3]) -> f64 {
        self.layer_paths(color).iter().map(|pl| pl.windows(2).map(|w| dist(w[0], w[1])).sum::<f64>()).sum()
    }

    /// A layer's outlines in design µm, for filling.
    pub fn layer_outlines_um(&self, color: [u8; 3]) -> Vec<Vec<(i32, i32)>> {
        let um = |v: f64| (v * 1000.0).round() as i32;
        self.layer_paths(color).into_iter().map(|pl| pl.into_iter().map(|(x, y)| (um(x), um(y))).collect()).collect()
    }

    /// Engraving geometry that drives the time: total width of all scan
    /// lines (from first to last span, gaps included) in mm, and line count.
    pub fn engrave_stats(&self, color: [u8; 3], interval_mm: f32) -> (f64, usize) {
        let interval = (interval_mm * 1000.0).round().max(1.0) as i32;
        let lines = ruida::fill::scan(&self.layer_outlines_um(color), interval);
        let extent: f64 = lines
            .iter()
            .map(|l| {
                let xs = l.spans.iter().flat_map(|&(a, b)| [a, b]);
                let (lo, hi) = xs.fold((i32::MAX, i32::MIN), |(lo, hi), x| (lo.min(x), hi.max(x)));
                (hi - lo) as f64 / 1000.0
            })
            .sum();
        (extent, lines.len())
    }

    /// Expected time for one layer with these settings, in seconds.
    ///
    /// Calibrated on our machine: engraving goes over the full width of every
    /// line (gaps included) and needs about 0.12 s to turn around per line;
    /// each cut path takes about 0.2 s extra to start and stop.
    pub fn layer_time_s(&self, color: [u8; 3], op: Operation, s: &OpSettings) -> f64 {
        let one_pass = match op {
            Operation::Engrave | Operation::Photo => {
                let (extent, lines) = self.engrave_stats(color, s.line_interval_mm);
                Self::engrave_time_s(extent, lines, s.speed)
            }
            Operation::Cut | Operation::Mark => {
                self.layer_length_mm(color) / s.speed + self.layer_paths(color).len() as f64 * PATH_START_S
            }
        };
        one_pass * s.passes as f64
    }

    /// Engraving time from [`Self::engrave_stats`] (split out so callers can cache the stats).
    pub fn engrave_time_s(extent_mm: f64, lines: usize, speed: f64) -> f64 {
        extent_mm / speed + lines as f64 * LINE_TURN_S
    }

    /// Every layer's problem, if any; empty means the job can be made.
    pub fn problems(&self, material: Option<&Material>) -> Vec<(usize, LayerProblem)> {
        self.layers.iter().enumerate().filter_map(|(i, l)| l.resolve(material).err().map(|e| (i, e))).collect()
    }

    /// Convert to job layers in controller µm, relative to the head, which
    /// sits at `anchor` on the box around all objects.
    /// `origin_right`: the machine's X axis counts from the right.
    ///
    /// Burn order: engraving first, then marking, then cutting (a part that
    /// is already cut free can shift), and within a layer smaller shapes go
    /// first so holes are cut before the outline around them.
    pub fn to_job(&self, material: Option<&Material>, origin_right: bool, anchor: Anchor) -> Result<Vec<JobLayer>, String> {
        let (ax, ay) = anchor.point(self.bounds().ok_or_else(|| t!("err-design-empty"))?);
        let um = |v: f64| (v * 1000.0).round() as i32;
        let to_ctrl = |(x, y): Pt| {
            let cx = if origin_right { ax - x } else { x - ax };
            (um(cx), um(y - ay))
        };
        let mut layers = Vec::new();
        for l in &self.layers {
            match l.resolve(material) {
                Ok(Some((op, s))) => layers.push((l, op, s)),
                Ok(None) => {}
                Err(e) => return Err(e.message(material)),
            }
        }
        if layers.is_empty() {
            return Err(t!("err-all-skipped"));
        }
        layers.sort_by_key(|(_, op, _)| match op {
            Operation::Engrave | Operation::Photo => 0,
            Operation::Mark => 1,
            Operation::Cut => 2,
        });
        Ok(layers
            .into_iter()
            .map(|(l, op, s)| JobLayer {
                kind: match op {
                    Operation::Engrave => LayerKind::Engrave { interval: (s.line_interval_mm * 1000.0).round() as i32 },
                    _ => LayerKind::Cut,
                },
                color: l.color,
                speed: s.speed,
                min_power: s.min_power,
                max_power: s.max_power,
                passes: s.passes,
                air_assist: s.air_assist,
                paths: {
                    let mut paths = self.layer_paths(l.color);
                    paths.sort_by(|a, b| area(a).total_cmp(&area(b)));
                    paths.into_iter().map(|pl| pl.into_iter().map(to_ctrl).collect()).collect()
                },
            })
            .collect())
    }
}

/// Undo / redo by whole-design snapshots (designs are small).
#[derive(Default)]
pub struct History {
    undo: Vec<Design>,
    redo: Vec<Design>,
}

impl History {
    const LIMIT: usize = 100;

    /// Remember `design` as it is now, before a change.
    pub fn checkpoint(&mut self, design: &Design) {
        if self.undo.last() == Some(design) {
            return;
        }
        self.undo.push(design.clone());
        if self.undo.len() > Self::LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self, design: &mut Design) -> bool {
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(design, prev));
        design.revision += 1;
        true
    }

    pub fn redo(&mut self, design: &mut Design) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(design, next));
        design.revision += 1;
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

/// Bounding-box area, used to cut inner shapes before outer ones.
fn area(pl: &[Pt]) -> f64 {
    points_bounds(pl.iter()).map(|((x0, y0), (x1, y1))| (x1 - x0) * (y1 - y0)).unwrap_or(0.0)
}

fn dist(a: Pt, b: Pt) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

// ---------------------------------------------------------------- SVG and text

struct Shape {
    color: [u8; 3],
    /// Any path of this colour has a fill (suggests engraving).
    filled: bool,
    polylines: Vec<Vec<Pt>>,
}

/// usvg options with the system fonts, loaded once.
fn svg_options() -> usvg::Options<'static> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let mut opt = usvg::Options::default();
    opt.fontdb = FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone();
    opt
}

fn svg_shapes(data: &[u8]) -> Result<Vec<Shape>, String> {
    let tree = usvg::Tree::from_data(data, &svg_options()).map_err(|e| t!("err-svg-unreadable", error = e.to_string()))?;
    let mut shapes: Vec<Shape> = Vec::new();
    collect(tree.root(), &mut shapes);
    shapes.retain(|s| !s.polylines.is_empty());
    if shapes.is_empty() {
        return Err(t!("err-svg-empty"));
    }
    Ok(shapes)
}

fn text_outlines(text: &str, font: Font, bold: bool) -> Result<Vec<Vec<Pt>>, String> {
    let placeholder = t!("default-text");
    let text = if text.trim().is_empty() { placeholder.as_str() } else { text };
    let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="4000" height="200"><text x="10" y="120" font-family="{}" font-weight="{}" font-size="100">{escaped}</text></svg>"#,
        font.family(),
        if bold { "bold" } else { "normal" }
    );
    let shapes = svg_shapes(svg.as_bytes()).map_err(|_| t!("err-no-font"))?;
    Ok(shapes.into_iter().flat_map(|s| s.polylines).collect())
}

fn collect(group: &usvg::Group, shapes: &mut Vec<Shape>) {
    for node in group.children() {
        match node {
            usvg::Node::Group(g) => collect(g, shapes),
            usvg::Node::Text(t) => collect(t.flattened(), shapes),
            usvg::Node::Path(p) => {
                let color = path_color(p);
                let filled = p.fill().is_some();
                let polylines = flatten(p.data(), p.abs_transform());
                match shapes.iter_mut().find(|s| s.color == color) {
                    Some(s) => {
                        s.polylines.extend(polylines);
                        s.filled |= filled;
                    }
                    None => shapes.push(Shape { color, filled, polylines }),
                }
            }
            usvg::Node::Image(_) => {}
        }
    }
}

/// Layer colour: the stroke colour, else the fill colour, else black.
fn path_color(p: &usvg::Path) -> [u8; 3] {
    let paint = p.stroke().map(|s| s.paint()).or_else(|| p.fill().map(|f| f.paint()));
    match paint {
        Some(usvg::Paint::Color(c)) => [c.red, c.green, c.blue],
        _ => [0, 0, 0],
    }
}

fn flatten(path: &usvg::tiny_skia_path::Path, t: usvg::Transform) -> Vec<Vec<Pt>> {
    use usvg::tiny_skia_path::PathSegment;
    let map = |p: usvg::tiny_skia_path::Point| {
        let mut p = p;
        t.map_point(&mut p);
        (p.x as f64 * PX_TO_MM, p.y as f64 * PX_TO_MM)
    };
    let mut out: Vec<Vec<Pt>> = Vec::new();
    let mut current: Vec<Pt> = Vec::new();
    let mut start = (0.0, 0.0);
    let finish = |current: &mut Vec<Pt>, out: &mut Vec<Vec<Pt>>| {
        if current.len() >= 2 {
            out.push(std::mem::take(current));
        } else {
            current.clear();
        }
    };
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                finish(&mut current, &mut out);
                start = map(p);
                current.push(start);
            }
            PathSegment::LineTo(p) => current.push(map(p)),
            PathSegment::QuadTo(c, p) => {
                let p0 = *current.last().unwrap_or(&start);
                let (c, p) = (map(c), map(p));
                subdivide(&mut current, p0, |t| {
                    let u = 1.0 - t;
                    (u * u * p0.0 + 2.0 * u * t * c.0 + t * t * p.0, u * u * p0.1 + 2.0 * u * t * c.1 + t * t * p.1)
                }, dist(p0, c) + dist(c, p));
            }
            PathSegment::CubicTo(c1, c2, p) => {
                let p0 = *current.last().unwrap_or(&start);
                let (c1, c2, p) = (map(c1), map(c2), map(p));
                subdivide(&mut current, p0, |t| {
                    let u = 1.0 - t;
                    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    (a * p0.0 + b * c1.0 + c * c2.0 + d * p.0, a * p0.1 + b * c1.1 + c * c2.1 + d * p.1)
                }, dist(p0, c1) + dist(c1, c2) + dist(c2, p));
            }
            PathSegment::Close => {
                if current.last() != Some(&start) {
                    current.push(start);
                }
                finish(&mut current, &mut out);
            }
        }
    }
    finish(&mut current, &mut out);
    out
}

/// Append points along a curve; the segment count grows with the control
/// polygon length so flattening error stays around the tolerance.
fn subdivide(out: &mut Vec<Pt>, _p0: Pt, curve: impl Fn(f64) -> Pt, control_len: f64) {
    let n = ((control_len / FLATTEN_TOLERANCE_MM).sqrt().ceil() as usize).clamp(1, 500);
    for i in 1..=n {
        out.push(curve(i as f64 / n as f64));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials::reference_materials;

    /// A tag: cut outline, cut hanging hole, filled (engraved) bar.
    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="50mm" height="20mm" viewBox="0 0 50 20">
        <rect x="0" y="0" width="50" height="20" fill="none" stroke="#000000"/>
        <circle cx="5" cy="10" r="2" fill="none" stroke="#000000"/>
        <rect x="15" y="8" width="30" height="4" fill="#ff0000"/>
    </svg>"##;

    fn design() -> Design {
        let mut d = Design::default();
        d.import_svg_data(SVG.as_bytes(), "tag", (100.0, 10.0)).unwrap();
        d
    }

    fn plywood() -> Material {
        reference_materials().into_iter().find(|m| m.name == "Pappelsperrholz 4 mm").unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn svg_import_is_mm_grouped_and_placed() {
        let d = design();
        assert_eq!(d.objects.len(), 2, "one object per colour");
        let ((x0, y0), (x1, y1)) = d.bounds().unwrap();
        assert!(close(x1, 100.0) && close(y0, 10.0), "top-right at the requested point");
        assert!(close(x1 - x0, 50.0) && close(y1 - y0, 20.0));
        assert_eq!(d.with_group(0), vec![0, 1]);
    }

    #[test]
    fn default_actions_from_fill() {
        let d = design();
        let actions: Vec<_> = d.layers.iter().map(|l| (l.color, l.action)).collect();
        assert_eq!(actions, vec![([0, 0, 0], Action::Do(Operation::Cut)), ([255, 0, 0], Action::Do(Operation::Engrave))]);
    }

    #[test]
    fn shapes_have_exact_geometry() {
        let rect = Object::new(Kind::Rect { radius: 0.0 }, (10.0, 20.0, 30.0, 15.0), [0, 0, 0]);
        let pl = &rect.outline()[0];
        assert_eq!(pl.first(), pl.last(), "closed");
        let len: f64 = pl.windows(2).map(|w| dist(w[0], w[1])).sum();
        assert!(close(len, 90.0));

        let circle = Object::new(Kind::Ellipse, (0.0, 0.0, 20.0, 20.0), [0, 0, 0]);
        let len: f64 = circle.outline()[0].windows(2).map(|w| dist(w[0], w[1])).sum();
        assert!((len - std::f64::consts::PI * 20.0).abs() < 0.05, "{len}");

        let hex = Object::new(Kind::Polygon { sides: 6 }, (0.0, 0.0, 20.0, 20.0), [0, 0, 0]);
        assert_eq!(hex.outline()[0].len(), 7);

        let mut r = rect.clone();
        r.rotation = 90.0;
        let ((x0, y0), (x1, y1)) = r.bounds();
        assert!(close(x1 - x0, 15.0) && close(y1 - y0, 30.0), "rotated box swaps size");
    }

    #[test]
    fn text_renders_to_outlines() {
        let t = Object::text("FOKUS", Font::Sans, true, (0.0, 0.0), 10.0, [0, 0, 0]).unwrap();
        assert!(close(t.h, 10.0));
        assert!(t.w > 25.0 && t.w < 60.0, "{}", t.w);
        assert!(t.outline().len() >= 5, "at least one outline per letter");
    }

    #[test]
    fn resize_group_and_undo() {
        let mut d = design();
        let mut history = History::default();
        let sel = d.with_group(0);
        let from = d.selection_bounds(&sel).unwrap();
        let originals: Vec<(usize, Object)> = sel.iter().map(|&i| (i, d.objects[i].clone())).collect();
        history.checkpoint(&d);
        // Double the size, keeping the top-left corner.
        let to = (from.0, (from.0.0 + 100.0, from.0.1 + 40.0));
        d.resize(&originals, from, to);
        let b = d.bounds().unwrap();
        assert!(close(b.1.0 - b.0.0, 100.0) && close(b.1.1 - b.0.1, 40.0));
        assert!(history.undo(&mut d));
        assert!(close(d.bounds().unwrap().1.0 - d.bounds().unwrap().0.0, 50.0));
        assert!(history.redo(&mut d));
    }

    #[test]
    fn duplicate_gets_its_own_group() {
        let mut d = design();
        let copies = d.duplicate(&[0, 1]);
        assert_eq!(copies, vec![2, 3]);
        assert_ne!(d.objects[2].group, d.objects[0].group);
        assert_eq!(d.with_group(2), vec![2, 3]);
    }

    #[test]
    fn colour_change_updates_layers() {
        let mut d = design();
        d.objects[1].color = [0, 0, 255];
        d.touch();
        let colors: Vec<_> = d.layers.iter().map(|l| l.color).collect();
        assert_eq!(colors, vec![[0, 0, 0], [0, 0, 255]]);
    }

    #[test]
    fn one_material_gives_each_operation_its_settings() {
        let job = design().to_job(Some(&plywood()), true, Anchor::TOP_RIGHT).unwrap();
        assert_eq!(job[0].kind, LayerKind::Engrave { interval: 150 });
        assert_eq!((job[0].speed, job[0].max_power), (300.0, 30.0));
        assert_eq!(job[1].kind, LayerKind::Cut);
        assert_eq!((job[1].speed, job[1].max_power), (25.0, 100.0));
        assert!(job[1].paths[0].len() > 5, "the hole is cut before the outline");
    }

    #[test]
    fn missing_operation_and_material_are_reported() {
        let d = design();
        assert_eq!(d.problems(None).len(), 2);
        let paper = reference_materials().into_iter().find(|m| m.name == "Papier").unwrap();
        assert_eq!(d.problems(Some(&paper)), vec![(1, LayerProblem::NotForMaterial(Operation::Engrave))]);
        assert!(d.to_job(Some(&paper), true, Anchor::TOP_RIGHT).is_err());
    }

    #[test]
    fn job_is_relative_to_top_right_corner() {
        let job = design().to_job(Some(&plywood()), true, Anchor::TOP_RIGHT).unwrap();
        let pts: Vec<_> = job.iter().flat_map(|l| l.paths.iter().flatten()).copied().collect();
        assert_eq!(pts.iter().map(|p| p.0).min(), Some(0));
        assert_eq!(pts.iter().map(|p| p.1).min(), Some(0));
        // The hole is near the left edge, so far from the head (large controller X).
        assert!(job[1].paths[0].iter().all(|p| p.0 >= 42_900), "{:?}", &job[1].paths[0][..3]);
    }

    #[test]
    fn anchor_moves_the_job_origin() {
        let d = design();
        let centre = Anchor { h: 0, v: 0 };
        let job = d.to_job(Some(&plywood()), true, centre).unwrap();
        let pts: Vec<_> = job.iter().flat_map(|l| l.paths.iter().flatten()).copied().collect();
        let (xmin, xmax) = (pts.iter().map(|p| p.0).min().unwrap(), pts.iter().map(|p| p.0).max().unwrap());
        let (ymin, ymax) = (pts.iter().map(|p| p.1).min().unwrap(), pts.iter().map(|p| p.1).max().unwrap());
        // The 50 x 20 mm tag centred on the head.
        assert!((xmin + 25_000).abs() <= 1 && (xmax - 25_000).abs() <= 1, "{xmin}..{xmax}");
        assert!((ymin + 10_000).abs() <= 1 && (ymax - 10_000).abs() <= 1, "{ymin}..{ymax}");
        assert_eq!(Anchor { h: -1, v: 1 }.label(), "bottom-left corner");
    }

    #[test]
    fn time_estimate_matches_the_live_tag_job() {
        // examples/tag.svg on "Pappelsperrholz 4 mm" took about 42 s on the
        // machine (46 s minus a 4 s pause).
        let mut d = Design::default();
        d.import_svg(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/tag.svg")), (100.0, 10.0)).unwrap();
        let ply = plywood();
        let t: f64 = d.layers.iter().map(|l| {
            let (op, s) = l.resolve(Some(&ply)).unwrap().unwrap();
            d.layer_time_s(l.color, op, &s)
        }).sum();
        assert!((t - 42.0).abs() < 4.0, "estimate {t:.1} s");
    }

    #[test]
    fn exported_job_decodes() {
        let job = design().to_job(Some(&plywood()), true, Anchor::TOP_RIGHT).unwrap();
        let file = ruida::writer::write_file(&job).unwrap();
        let records = ruida::command::parse(&ruida::scramble::unscramble(&file, ruida::scramble::MAGIC_644X));
        let tp = ruida::job::Toolpath::from_records(&records);
        assert_eq!(tp.unknown_commands, 0);
        assert_eq!(tp.layers.len(), 2);
        let engraved = tp.layers[&0].cut_mm;
        assert!((engraved - 26.0 * 30.0).abs() < 35.0, "{engraved}");
    }
}
