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

use ruida::fill::{FillRule, Shape as FillShape};
use ruida::writer::{JobLayer, LayerKind};
use serde::{Deserialize, Serialize};

use crate::bezier::{self, SubPath};
use crate::fonts::{self, Ink};
use crate::materials::{Material, OpSettings, Operation};
use crate::paths;
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
/// Font size of new text (one em), mm.
pub const DEFAULT_TEXT_SIZE: f64 = 12.0;
/// Speed of the Frame move around the design (mm/s, laser off).
pub const FRAME_SPEED: f64 = 100.0;

// ---------------------------------------------------------------- objects

/// Text settings: family name, weight and slant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub family: String,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
}

impl TextStyle {
    pub fn fonts(&self) -> fonts::Style {
        fonts::Style { family: self.family.clone(), bold: self.bold, italic: self.italic }
    }
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle { family: fonts::DEFAULT_FAMILY.into(), bold: false, italic: false }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "KindFile")]
pub enum Kind {
    Rect { radius: f64 },
    Ellipse,
    Polygon { sides: u32 },
    /// Drawn or imported paths, normalised to 0..1 of the object's box.
    /// Engraving fills them with the non-zero rule unless `even_odd` (as the
    /// SVG they came from asked).
    Path { subpaths: Vec<SubPath>, filled: bool, even_odd: bool },
    /// Text as outlines, normalised to 0..1 of the object's box. `ink` is
    /// the outlines' box in layout units, `scale` mm per layout unit (x, y).
    Text { text: String, style: TextStyle, outlines: Vec<SubPath>, ink: Ink, scale: (f64, f64) },
}

/// How objects are read from project files; older files stored paths as
/// point lists and text with one of three fonts.
#[derive(Deserialize)]
enum KindFile {
    Rect { radius: f64 },
    Ellipse,
    Polygon { sides: u32 },
    Path {
        #[serde(default)]
        subpaths: Vec<SubPath>,
        #[serde(default)]
        polylines: Vec<Vec<Pt>>,
        filled: bool,
        #[serde(default)]
        even_odd: bool,
    },
    Text {
        text: String,
        #[serde(default)]
        style: Option<TextStyle>,
        /// Version 1: "Sans", "Serif" or "Mono".
        #[serde(default)]
        font: Option<String>,
        #[serde(default)]
        bold: bool,
        outlines: OutlinesFile,
        #[serde(default)]
        ink: Option<Ink>,
        #[serde(default)]
        scale: Option<(f64, f64)>,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OutlinesFile {
    Curves(Vec<SubPath>),
    Points(Vec<Vec<Pt>>),
}

impl OutlinesFile {
    fn curves(self) -> Vec<SubPath> {
        match self {
            OutlinesFile::Curves(c) => c,
            OutlinesFile::Points(p) => p.iter().map(|pl| SubPath::polyline(pl)).collect(),
        }
    }
}

impl From<KindFile> for Kind {
    fn from(k: KindFile) -> Self {
        match k {
            KindFile::Rect { radius } => Kind::Rect { radius },
            KindFile::Ellipse => Kind::Ellipse,
            KindFile::Polygon { sides } => Kind::Polygon { sides },
            KindFile::Path { mut subpaths, polylines, filled, even_odd } => {
                subpaths.extend(polylines.iter().map(|pl| SubPath::polyline(pl)));
                Kind::Path { subpaths, filled, even_odd }
            }
            KindFile::Text { text, style, font, bold, outlines, ink, scale } => {
                let style = style.unwrap_or_else(|| TextStyle { family: fonts::legacy_family(font.as_deref().unwrap_or("Sans")), bold, italic: false });
                // Old files have no layout box: recompute it below (`Object::fix_text`).
                Kind::Text { text, style, outlines: outlines.curves(), ink: ink.unwrap_or_default(), scale: scale.unwrap_or((0.0, 0.0)) }
            }
        }
    }
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

    /// A path from sub-paths in design mm.
    pub fn path(subpaths: Vec<SubPath>, color: [u8; 3]) -> Self {
        let (subpaths, rect) = normalise(subpaths);
        Object::new(Kind::Path { subpaths, filled: false, even_odd: false }, rect, color)
    }

    /// Text whose first baseline starts at `origin`; `size` is the font size
    /// (one em) in mm.
    pub fn text(text: &str, style: TextStyle, origin: Pt, size: f64, color: [u8; 3]) -> Result<Self, String> {
        let s = size / fonts::EM;
        let empty = Kind::Text { text: String::new(), style: style.clone(), outlines: Vec::new(), ink: Ink::default(), scale: (s, s) };
        // An empty layout has its box at the layout origin (top of the first line).
        let mut o = Object::new(empty, (origin.0, origin.1 - fonts::EM * s, 0.0, 0.0), color);
        o.set_text(text, style)?;
        Ok(o)
    }

    /// Change a text object's content or style. The letters keep their size
    /// and the start of the first line stays where it is.
    pub fn set_text(&mut self, text: &str, style: TextStyle) -> Result<(), String> {
        self.fix_text();
        let Kind::Text { ink, scale: (sx, sy), .. } = self.kind else { return Err("not a text object".into()) };
        let layout = fonts::layout(text, &style.fonts())?;
        // Where the layout origin is now, in the box and on the bed.
        let local_origin = |ink: &Ink| (-ink.x0 * sx, -ink.y0 * sy);
        let before = self.to_world(local_origin(&ink));
        let (w, h) = (layout.ink.w * sx, layout.ink.h * sy);
        let (outlines, _) = normalise(layout.outlines);
        self.w = w;
        self.h = h;
        self.kind = Kind::Text { text: text.to_string(), style, outlines, ink: layout.ink, scale: (sx, sy) };
        let after = self.to_world(local_origin(&layout.ink));
        self.x += before.0 - after.0;
        self.y += before.1 - after.1;
        Ok(())
    }

    /// Text from version 1 files has no layout box or scale; work them out
    /// from its content (keeps the outlines it was saved with).
    pub fn fix_text(&mut self) {
        let Kind::Text { text, style, ink, scale, .. } = &mut self.kind else { return };
        if scale.0 > 0.0 {
            return;
        }
        match fonts::layout(text, &style.fonts()) {
            Ok(l) if l.ink.w > 0.0 && l.ink.h > 0.0 => {
                *ink = l.ink;
                *scale = (self.w / l.ink.w, self.h / l.ink.h);
            }
            _ => {
                let s = self.h.max(1.0) / fonts::EM;
                *scale = (s, s);
            }
        }
    }

    /// How engraving fills the shape: fonts and drawn shapes use the
    /// non-zero rule (overlapping parts of a letter stay filled).
    pub fn fill_rule(&self) -> FillRule {
        match self.kind {
            Kind::Path { even_odd: true, .. } => FillRule::EvenOdd,
            _ => FillRule::NonZero,
        }
    }

    /// Filled shapes suggest engraving rather than cutting.
    pub fn fill_hint(&self) -> bool {
        matches!(self.kind, Kind::Path { filled: true, .. } | Kind::Text { .. })
    }

    /// The shape as curves in mm relative to the box's top-left, before
    /// flipping and rotation.
    fn local_curves(&self) -> Vec<SubPath> {
        let (w, h) = (self.w, self.h);
        match &self.kind {
            Kind::Rect { radius } => vec![bezier::rect(w, h, *radius)],
            Kind::Ellipse => vec![bezier::ellipse(w, h)],
            Kind::Polygon { sides } => vec![bezier::polygon(w, h, *sides)],
            Kind::Path { subpaths, .. } | Kind::Text { outlines: subpaths, .. } => subpaths.iter().map(|s| s.map(&|(u, v)| (u * w, v * h))).collect(),
        }
    }

    /// Box-local mm (before flipping) → design mm.
    pub fn to_world(&self, (px, py): Pt) -> Pt {
        let (cx, cy) = (self.w / 2.0, self.h / 2.0);
        let px = if self.flip_x { self.w - px } else { px };
        let py = if self.flip_y { self.h - py } else { py };
        let (sin, cos) = self.rotation.to_radians().sin_cos();
        let (dx, dy) = (px - cx, py - cy);
        (self.x + cx + dx * cos - dy * sin, self.y + cy + dx * sin + dy * cos)
    }

    /// Design mm → box-local mm (before flipping); the inverse of `to_world`.
    pub fn to_local(&self, (x, y): Pt) -> Pt {
        let (cx, cy) = (self.w / 2.0, self.h / 2.0);
        let (sin, cos) = self.rotation.to_radians().sin_cos();
        let (dx, dy) = (x - self.x - cx, y - self.y - cy);
        let (px, py) = (cx + dx * cos + dy * sin, cy - dx * sin + dy * cos);
        (if self.flip_x { self.w - px } else { px }, if self.flip_y { self.h - py } else { py })
    }

    /// The shape as curves in design coordinates.
    pub fn curves(&self) -> Vec<SubPath> {
        self.local_curves().iter().map(|s| s.map(&|p| self.to_world(p))).collect()
    }

    /// Outline in design coordinates.
    pub fn outline(&self) -> Vec<Vec<Pt>> {
        bezier::flatten_all(&self.curves(), FLATTEN_TOLERANCE_MM)
    }

    pub fn bounds(&self) -> Bounds {
        if self.rotation.rem_euclid(360.0) == 0.0 {
            return ((self.x, self.y), (self.x + self.w, self.y + self.h));
        }
        points_bounds(self.outline().iter().flatten()).unwrap_or(((self.x, self.y), (self.x, self.y)))
    }

    /// Replace the shape with a path in design coordinates (no rotation or
    /// flipping left), for editing its points. Text loses its letters.
    pub fn make_path(&mut self) {
        if matches!(self.kind, Kind::Path { .. }) && self.rotation == 0.0 && !self.flip_x && !self.flip_y {
            return;
        }
        let filled = self.fill_hint();
        let even_odd = self.fill_rule() == FillRule::EvenOdd;
        let curves = self.curves();
        let name = if matches!(self.kind, Kind::Path { .. }) { self.name.clone() } else { t!("object-path") };
        let mut o = Object::path(curves, self.color);
        if let Kind::Path { filled: f, even_odd: e, .. } = &mut o.kind {
            *f = filled;
            *e = even_odd;
        }
        o.name = name;
        o.group = self.group;
        *self = o;
    }

    /// A path's curves in design coordinates (call `make_path` first).
    pub fn path_curves(&self) -> Option<Vec<SubPath>> {
        matches!(self.kind, Kind::Path { .. }).then(|| self.curves())
    }

    /// Set a path's curves from design coordinates; the box follows them.
    pub fn set_path_curves(&mut self, curves: Vec<SubPath>) {
        let Kind::Path { filled, even_odd, .. } = self.kind else { return };
        let (subpaths, (x, y, w, h)) = normalise(curves);
        (self.x, self.y, self.w, self.h) = (x, y, w, h);
        (self.rotation, self.flip_x, self.flip_y) = (0.0, false, false);
        self.kind = Kind::Path { subpaths, filled, even_odd };
    }
}

fn points_bounds<'a>(pts: impl Iterator<Item = &'a Pt>) -> Option<Bounds> {
    pts.fold(None, |b, &(x, y)| match b {
        None => Some(((x, y), (x, y))),
        Some(((x0, y0), (x1, y1))) => Some(((x0.min(x), y0.min(y)), (x1.max(x), y1.max(y)))),
    })
}

/// Scale curves (mm) into 0..1 of the bounds of the drawn curve; returns
/// them with the bounds as (x, y, w, h). A zero width or height stays 0.
fn normalise(curves: Vec<SubPath>) -> (Vec<SubPath>, (f64, f64, f64, f64)) {
    let ((x0, y0), (x1, y1)) = bezier::bounds(&curves, FLATTEN_TOLERANCE_MM).unwrap_or(((0.0, 0.0), (0.0, 0.0)));
    let (w, h) = (x1 - x0, y1 - y0);
    let n = |v: f64, lo: f64, size: f64| if size > 0.0 { (v - lo) / size } else { 0.0 };
    let out = curves.iter().map(|s| s.map(&|(x, y)| (n(x, x0, w), n(y, y0, h)))).collect();
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
        let all: Vec<SubPath> = shapes.iter().flat_map(|s| s.curves.iter().cloned()).collect();
        let ((_, y0), (x1, _)) = bezier::bounds(&all, FLATTEN_TOLERANCE_MM).ok_or_else(|| t!("err-svg-empty"))?;
        let (dx, dy) = (top_right.0 - x1, top_right.1 - y0);
        let group = self.new_group();
        let first = self.objects.len();
        for s in shapes {
            let moved = s.curves.iter().map(|c| c.map(&|(x, y)| (x + dx, y + dy))).collect();
            let (subpaths, rect) = normalise(moved);
            let mut o = Object::new(Kind::Path { subpaths, filled: s.filled, even_odd: s.even_odd }, rect, s.color);
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
        let copies: Vec<Object> = sel.iter().map(|&i| self.objects[i].clone()).collect();
        self.paste(&copies, (5.0, 5.0))
    }

    /// Add copies of `objects` moved by `offset`, in new groups of their
    /// own; returns their indices.
    pub fn paste(&mut self, objects: &[Object], offset: Pt) -> Vec<usize> {
        let mut groups = std::collections::HashMap::new();
        let first = self.objects.len();
        for o in objects {
            let mut o = o.clone();
            o.x += offset.0;
            o.y += offset.1;
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

    /// Move the selection to the top of the drawing order (drawn and picked
    /// last) or to the bottom; returns its new indices.
    pub fn reorder(&mut self, sel: &[usize], to_front: bool) -> Vec<usize> {
        let mut picked = Vec::new();
        let mut rest = Vec::new();
        for (i, o) in std::mem::take(&mut self.objects).into_iter().enumerate() {
            if sel.contains(&i) { picked.push(o) } else { rest.push(o) }
        }
        let n = picked.len();
        let start = if to_front { rest.len() } else { 0 };
        self.objects = if to_front { rest.into_iter().chain(picked).collect() } else { picked.into_iter().chain(rest).collect() };
        self.revision += 1;
        (start..start + n).collect()
    }

    /// Turn the selection by `deg` degrees (clockwise) around its centre.
    pub fn rotate(&mut self, sel: &[usize], deg: f64) {
        let Some(((x0, y0), (x1, y1))) = self.selection_bounds(sel) else { return };
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let (sin, cos) = deg.to_radians().sin_cos();
        for &i in sel {
            let o = &mut self.objects[i];
            let (dx, dy) = (o.x + o.w / 2.0 - cx, o.y + o.h / 2.0 - cy);
            o.x = cx + dx * cos - dy * sin - o.w / 2.0;
            o.y = cy + dx * sin + dy * cos - o.h / 2.0;
            o.rotation = (o.rotation + deg).rem_euclid(360.0);
        }
        self.touch();
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
            if let (Kind::Text { scale, .. }, Kind::Text { scale: old, .. }) = (&mut obj.kind, &o.kind) {
                // An empty text has no size to scale from: keep its letter size.
                let k = |new: f64, was: f64| if was > 1e-9 { new / was } else { 1.0 };
                *scale = (old.0 * k(obj.w, o.w), old.1 * k(obj.h, o.h));
            }
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

    /// A layer's outlines for filling, mapped by `f` (design mm → µm), with
    /// one fill shape per object (each with its own rule; shapes are united).
    pub fn layer_fill(&self, color: [u8; 3], f: impl Fn(Pt) -> (i32, i32)) -> (Vec<Vec<(i32, i32)>>, Vec<FillShape>) {
        let mut polys = Vec::new();
        let mut shapes = Vec::new();
        for o in self.objects.iter().filter(|o| o.color == color) {
            let outline: Vec<Vec<(i32, i32)>> = o.outline().into_iter().map(|pl| pl.into_iter().map(&f).collect()).collect();
            shapes.push(FillShape { len: outline.len(), rule: o.fill_rule() });
            polys.extend(outline);
        }
        (polys, shapes)
    }

    /// A layer's outlines in design µm, for filling.
    pub fn layer_fill_um(&self, color: [u8; 3]) -> (Vec<Vec<(i32, i32)>>, Vec<FillShape>) {
        let um = |v: f64| (v * 1000.0).round() as i32;
        self.layer_fill(color, |(x, y)| (um(x), um(y)))
    }

    /// Engraving geometry that drives the time: total width of all scan
    /// lines (from first to last span, gaps included) in mm, and line count.
    pub fn engrave_stats(&self, color: [u8; 3], interval_mm: f32) -> (f64, usize) {
        let interval = (interval_mm * 1000.0).round().max(1.0) as i32;
        let (polys, shapes) = self.layer_fill_um(color);
        let lines = ruida::fill::scan_shapes(&polys, &shapes, interval);
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
    /// is already cut free can shift); within a layer, holes before the
    /// shape around them and otherwise the nearest path next (see `paths`).
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
        // Each layer's paths are ordered from where the previous one ended
        // (the head, for the first), innermost shapes first.
        let mut pos = (0, 0);
        Ok(layers
            .into_iter()
            .map(|(l, op, s)| {
                // Engraving is filled line by line: the outline order doesn't matter.
                let (paths, shapes) = if op == Operation::Engrave {
                    self.layer_fill(l.color, to_ctrl)
                } else {
                    let paths: Vec<Vec<(i32, i32)>> = self.layer_paths(l.color).into_iter().map(|pl| pl.into_iter().map(to_ctrl).collect()).collect();
                    let ordered = paths::order(paths, pos);
                    if let Some(&last) = ordered.last().and_then(|p| p.last()) {
                        pos = last;
                    }
                    (ordered, Vec::new())
                };
                JobLayer {
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
                paths,
                shapes,
            }})
            .collect())
    }
}

impl Design {
    /// A job that moves the head once around the box of the whole design
    /// with the laser off ("Frame"), so you can check where it will go.
    pub fn frame_job(&self, origin_right: bool, anchor: Anchor) -> Result<Vec<JobLayer>, String> {
        let b @ ((x0, y0), (x1, y1)) = self.bounds().ok_or_else(|| t!("err-design-empty"))?;
        let (ax, ay) = anchor.point(b);
        let um = |v: f64| (v * 1000.0).round() as i32;
        let to_ctrl = |(x, y): Pt| (um(if origin_right { ax - x } else { x - ax }), um(y - ay));
        let rect = vec![to_ctrl((x0, y0)), to_ctrl((x1, y0)), to_ctrl((x1, y1)), to_ctrl((x0, y1)), to_ctrl((x0, y0))];
        Ok(vec![JobLayer {
            kind: LayerKind::Cut,
            color: [0, 0, 0],
            speed: FRAME_SPEED,
            min_power: 0.0,
            max_power: 0.0,
            passes: 1,
            air_assist: false,
            // Start at the corner nearest the head.
            paths: paths::order(vec![rect], (0, 0)),
            shapes: Vec::new(),
        }])
    }

    /// Expected time of [`Self::frame_job`] in seconds.
    pub fn frame_time_s(&self) -> f64 {
        self.bounds().map(|((x0, y0), (x1, y1))| 2.0 * ((x1 - x0) + (y1 - y0)) / FRAME_SPEED + 1.0).unwrap_or(0.0)
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

fn dist(a: Pt, b: Pt) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

// ---------------------------------------------------------------- SVG and text

struct Shape {
    color: [u8; 3],
    /// Any path of this colour has a fill (suggests engraving).
    filled: bool,
    /// A filled path asked for the even-odd rule.
    even_odd: bool,
    /// In mm.
    curves: Vec<SubPath>,
}

fn svg_shapes(data: &[u8]) -> Result<Vec<Shape>, String> {
    let tree = usvg::Tree::from_data(data, &fonts::svg_options()).map_err(|e| t!("err-svg-unreadable", error = e.to_string()))?;
    let mut shapes: Vec<Shape> = Vec::new();
    collect(tree.root(), &mut shapes);
    shapes.retain(|s| !s.curves.is_empty());
    if shapes.is_empty() {
        return Err(t!("err-svg-empty"));
    }
    Ok(shapes)
}

fn collect(group: &usvg::Group, shapes: &mut Vec<Shape>) {
    for node in group.children() {
        match node {
            usvg::Node::Group(g) => collect(g, shapes),
            usvg::Node::Text(t) => collect(t.flattened(), shapes),
            usvg::Node::Path(p) => {
                let color = path_color(p);
                let filled = p.fill().is_some();
                let even_odd = p.fill().is_some_and(|f| f.rule() == usvg::FillRule::EvenOdd);
                let ts = p.abs_transform().post_scale(PX_TO_MM as f32, PX_TO_MM as f32);
                let curves = bezier::from_usvg(p.data(), ts);
                match shapes.iter_mut().find(|s| s.color == color) {
                    Some(s) => {
                        s.curves.extend(curves);
                        s.filled |= filled;
                        s.even_odd |= even_odd;
                    }
                    None => shapes.push(Shape { color, filled, even_odd, curves }),
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
        let style = TextStyle { bold: true, ..TextStyle::default() };
        let t = Object::text("FOKUS", style, (0.0, 20.0), 10.0, [0, 0, 0]).unwrap();
        // Capitals are about 0.73 em high in Inter, sitting on the baseline.
        assert!(t.h > 6.5 && t.h < 8.0, "{}", t.h);
        // Round letters (O, S) dip slightly below the baseline.
        assert!((t.y + t.h - 20.0).abs() < 0.2, "{}", t.y + t.h);
        assert!(t.w > 25.0 && t.w < 60.0, "{}", t.w);
        assert!(t.outline().len() >= 5, "at least one outline per letter");
    }

    #[test]
    fn editing_text_keeps_its_start_and_size() {
        let mut t = Object::text("Hi", TextStyle::default(), (10.0, 20.0), 10.0, [0, 0, 0]).unwrap();
        t.rotation = 30.0;
        let start = t.to_world((-match t.kind { Kind::Text { ink, scale, .. } => ink.x0 * scale.0, _ => 0.0 }, 0.0));
        t.set_text("Hi there", TextStyle::default()).unwrap();
        let after = t.to_world((-match t.kind { Kind::Text { ink, scale, .. } => ink.x0 * scale.0, _ => 0.0 }, 0.0));
        assert!(close(start.0, after.0) && close(start.1, after.1), "{start:?} {after:?}");
        assert!(t.w > 30.0);
        t.set_text("", TextStyle::default()).unwrap();
        assert_eq!(t.outline().len(), 0);
        t.set_text("Hi", TextStyle::default()).unwrap();
        assert!(t.w > 5.0, "size survives empty text");
    }

    #[test]
    fn resizing_empty_text_keeps_its_letter_size() {
        let mut d = Design::default();
        d.add(Object::text("", TextStyle::default(), (10.0, 20.0), 12.0, [0, 0, 0]).unwrap());
        let o = d.objects[0].clone();
        let from = o.bounds();
        d.resize(&[(0, o)], from, (from.0, (from.0.0 + 0.1, from.0.1 + 0.1)));
        d.objects[0].set_text("Hi", TextStyle::default()).unwrap();
        assert!(d.objects[0].h > 7.0, "{}", d.objects[0].h);
    }

    #[test]
    fn overlapping_letter_parts_engrave_solid() {
        // Inter draws the bar of "t" over its stem: no gap where they cross.
        let mut d = Design::default();
        d.add(Object::text("t", TextStyle::default(), (0.0, 20.0), 20.0, [255, 0, 0]).unwrap());
        let (polys, shapes) = d.layer_fill_um([255, 0, 0]);
        let ((x0, y0), (x1, y1)) = d.objects[0].bounds();
        // Across the crossbar, a little above the middle of the letter.
        let mut found_bar = false;
        for k in 1..40 {
            let y = (y0 + (y1 - y0) * k as f64 / 40.0) * 1000.0;
            let spans = ruida::fill::spans_shapes(&polys, &shapes, y);
            if spans.len() == 1 && (spans[0].1 - spans[0].0) as f64 > (x1 - x0) * 1000.0 * 0.8 {
                found_bar = true;
            }
            let even_odd = ruida::fill::spans_at(&polys, y);
            assert!(spans.len() <= even_odd.len().max(1));
        }
        assert!(found_bar, "the crossbar is one solid span");
    }

    #[test]
    fn shapes_become_editable_paths() {
        let mut r = Object::new(Kind::Rect { radius: 0.0 }, (10.0, 10.0, 20.0, 10.0), [0, 0, 0]);
        r.rotation = 90.0;
        let before = r.bounds();
        r.make_path();
        assert!(matches!(r.kind, Kind::Path { .. }));
        assert_eq!(r.rotation, 0.0);
        let after = r.bounds();
        assert!(close(before.0.0, after.0.0) && close(before.1.1, after.1.1));
        let mut curves = r.path_curves().unwrap();
        for n in &mut curves[0].nodes {
            n.p.0 -= 5.0;
        }
        r.set_path_curves(curves);
        assert!(close(r.bounds().0.0, before.0.0 - 5.0));
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
    fn reorder_and_rotate() {
        let mut d = design();
        let n = d.objects.len();
        let moved = d.reorder(&[0], true);
        assert_eq!(moved, vec![n - 1]);
        let back = d.reorder(&[n - 1], false);
        assert_eq!(back, vec![0]);
        let before = d.selection_bounds(&[0]).unwrap();
        d.rotate(&[0], 90.0);
        let after = d.selection_bounds(&[0]).unwrap();
        assert!(close(after.1.0 - after.0.0, before.1.1 - before.0.1));
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
    fn frame_traces_the_design_box_with_the_laser_off() {
        let d = design();
        let frame = d.frame_job(true, Anchor::TOP_RIGHT).unwrap();
        assert_eq!(frame.len(), 1);
        assert_eq!((frame[0].min_power, frame[0].max_power, frame[0].air_assist), (0.0, 0.0, false));
        let pts = &frame[0].paths[0];
        assert_eq!(pts.len(), 5);
        assert_eq!(pts.first(), pts.last());
        assert_eq!(pts[0], (0, 0), "starts at the head (the top-right corner)");
        let job = d.to_job(Some(&plywood()), true, Anchor::TOP_RIGHT).unwrap();
        let bounds = |ps: &mut dyn Iterator<Item = (i32, i32)>| ps.fold((i32::MAX, i32::MAX, i32::MIN, i32::MIN), |b, p| (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1)));
        let frame_box = bounds(&mut pts.iter().copied());
        let job_box = bounds(&mut job.iter().flat_map(|l| l.paths.iter().flatten().copied()));
        assert_eq!(frame_box, job_box, "the frame is exactly the job's box");
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
