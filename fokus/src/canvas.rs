//! The canvas: the laser bed with the design on it, the drawing tools,
//! selection, moving, resizing and rotating, the point editor and typing
//! text in place.
//!
//! Coordinates: design mm are bed mm as seen from the front (x right, y down,
//! origin top-left). Controller mm are what the Ruida reports; on our machine
//! its origin is top-right, so x is mirrored. Screen px = mm * zoom + offset.

use std::time::Instant;

use eframe::egui::{self, Align2, Color32, CursorIcon, Event, FontId, Key, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Vec2};

use crate::bezier::{self, Node, Side, SubPath};
use crate::design::{Action, Anchor, Bounds, Design, History, Kind, Object, Pt, TextStyle, MIN_SIZE, PREVIEW_LINE_INTERVAL_MM};
use crate::fonts;
use crate::icons::{self, Icon};
use crate::job::Job;
use crate::materials::{Material, Operation};
use crate::t;
use crate::textedit::{self, Caret};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Nodes,
    Rect,
    Ellipse,
    Polygon,
    Pen,
    Text,
}

impl Tool {
    pub const ALL: [Tool; 7] = [Tool::Select, Tool::Nodes, Tool::Rect, Tool::Ellipse, Tool::Polygon, Tool::Pen, Tool::Text];

    pub fn label(self) -> String {
        match self {
            Tool::Select => t!("tool-select"),
            Tool::Nodes => t!("tool-nodes"),
            Tool::Rect => t!("tool-rect"),
            Tool::Ellipse => t!("tool-ellipse"),
            Tool::Polygon => t!("tool-polygon"),
            Tool::Pen => t!("tool-pen"),
            Tool::Text => t!("tool-text"),
        }
    }

    pub fn key(self) -> Key {
        match self {
            Tool::Select => Key::V,
            Tool::Nodes => Key::A,
            Tool::Rect => Key::R,
            Tool::Ellipse => Key::E,
            Tool::Polygon => Key::Y,
            Tool::Pen => Key::P,
            Tool::Text => Key::T,
        }
    }

    pub fn hint(self) -> String {
        match self {
            Tool::Select => t!("tool-select-hint"),
            Tool::Nodes => t!("tool-nodes-hint"),
            Tool::Rect => t!("tool-rect-hint"),
            Tool::Ellipse => t!("tool-ellipse-hint"),
            Tool::Polygon => t!("tool-polygon-hint"),
            Tool::Pen => t!("tool-pen-hint"),
            Tool::Text => t!("tool-text-hint"),
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Tool::Select => Icon::Pointer,
            Tool::Nodes => Icon::PointerHollow,
            Tool::Rect => Icon::Square,
            Tool::Ellipse => Icon::Circle,
            Tool::Polygon => Icon::Hexagon,
            Tool::Pen => Icon::Pen,
            Tool::Text => Icon::Type,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ZoomTarget {
    Bed,
    /// The design (or opened job file), with some room around it.
    Design,
}

pub struct Scene<'a> {
    pub bed: (f64, f64),
    pub origin_right: bool,
    /// Live head position, controller mm.
    pub head: Option<Pt>,
    /// An opened job file, drawn starting at `job_anchor` (controller mm).
    pub job: Option<&'a Job>,
    pub job_anchor: Pt,
    pub show_travel: bool,
    pub material: Option<&'a Material>,
    /// Colour new shapes get.
    pub draw_color: [u8; 3],
    /// Start point on the design (shown as a green square).
    pub anchor: Anchor,
    /// Colour new text gets: an engraving layer, so text isn't cut out.
    pub text_color: [u8; 3],
    /// Font and size (mm) new text gets.
    pub text_style: &'a TextStyle,
    pub text_size: f64,
    pub polygon_sides: u32,
    /// Corner handles keep the aspect ratio.
    pub lock_aspect: bool,
}

impl Scene<'_> {
    /// Controller mm → design mm (the mirroring is its own inverse).
    pub fn to_view(&self, (x, y): Pt) -> Pt {
        if self.origin_right {
            (self.bed.0 - x, y)
        } else {
            (x, y)
        }
    }
}

/// What the canvas edits.
pub struct Edit<'a> {
    pub design: &'a mut Design,
    pub selection: &'a mut Vec<usize>,
    pub history: &'a mut History,
    pub tool: &'a mut Tool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Handle {
    /// -1 = left/top edge, 0 = middle, 1 = right/bottom edge.
    hx: i8,
    hy: i8,
}

/// A node of a path: object, sub-path, node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeRef {
    pub obj: usize,
    pub sub: usize,
    pub node: usize,
}

enum Drag {
    None,
    Pan,
    Move { started: bool },
    Resize { handle: Handle, from: Bounds, originals: Vec<(usize, Object)> },
    Rotate { center: Pt, start: f64, originals: Vec<(usize, Object)>, angle: f64 },
    Marquee { start: Pt },
    Create { start: Pt },
    Nodes { started: bool },
    NodeHandle { at: NodeRef, side: Side, started: bool },
    NodeMarquee { start: Pt },
    /// Pulling the handles out of the node just placed with the pen.
    PenHandle,
    /// Selecting text with the mouse while editing it.
    TextSelect,
}

/// Text being typed or edited on the canvas.
struct TextSession {
    obj: usize,
    caret: Caret,
    /// Layout of the current text and style, for caret positions.
    layout: Option<(String, fonts::Style, fonts::Layout)>,
    /// When the caret last moved (it blinks from then on).
    moved: Instant,
    /// Take the keyboard on the next frame.
    want_focus: bool,
}

pub struct Canvas {
    /// Pixels per mm.
    zoom: f32,
    offset: Vec2,
    pending_zoom: Option<ZoomTarget>,
    last_rect: Option<Rect>,
    drag: Drag,
    /// Nodes of the path being drawn with the pen.
    pen: Vec<Node>,
    /// Pointer position in controller mm, if over the bed.
    pub hover: Option<Pt>,
    /// Selected nodes in the point editor.
    pub nodes: Vec<NodeRef>,
    text: Option<TextSession>,
    /// The pointer is where dragging rotates the selection.
    rotate_hover: bool,
    /// Object under the pointer (outlined as a hint).
    hovered: Option<usize>,
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas {
            zoom: 1.0,
            offset: Vec2::ZERO,
            pending_zoom: Some(ZoomTarget::Bed),
            last_rect: None,
            drag: Drag::None,
            pen: Vec::new(),
            hover: None,
            nodes: Vec::new(),
            text: None,
            rotate_hover: false,
            hovered: None,
        }
    }
}

const HANDLE_PX: f32 = 8.0;
const NODE_PX: f32 = 7.0;
/// Distance of the rotation handle above the selection.
const ROTATE_STEM_PX: f32 = 26.0;

fn rect_mm(a: Pt, b: Pt) -> Bounds {
    ((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1)))
}

fn rotate_point((x, y): Pt, (cx, cy): Pt, deg: f64) -> Pt {
    let (sin, cos) = deg.to_radians().sin_cos();
    let (dx, dy) = (x - cx, y - cy);
    (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos)
}

impl Canvas {
    pub fn zoom_to(&mut self, target: ZoomTarget) {
        self.pending_zoom = Some(target);
    }

    /// Zoom in or out around the canvas centre.
    pub fn zoom_by(&mut self, factor: f32) {
        if let Some(rect) = self.last_rect {
            let c = rect.size() / 2.0;
            let new_zoom = (self.zoom * factor).clamp(0.05, 200.0);
            self.offset = c - (c - self.offset) * (new_zoom / self.zoom);
            self.zoom = new_zoom;
        }
    }

    /// Pixels per mm, for the zoom readout.
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn cancel_gesture(&mut self) {
        self.pen.clear();
    }

    pub fn editing_text(&self) -> Option<usize> {
        self.text.as_ref().map(|s| s.obj)
    }

    /// Start editing a text object's content, caret at the end.
    pub fn edit_text(&mut self, design: &Design, i: usize) {
        if let Some(Kind::Text { text, .. }) = design.objects.get(i).map(|o| &o.kind) {
            let n = textedit::char_count(text);
            self.text = Some(TextSession { obj: i, caret: Caret { cursor: n, anchor: 0 }, layout: None, moved: Instant::now(), want_focus: true });
        }
    }

    /// Stop editing text; text left empty is removed.
    pub fn finish_text(&mut self, design: &mut Design, selection: &mut Vec<usize>) {
        let Some(session) = self.text.take() else { return };
        if let Some(Kind::Text { text, .. }) = design.objects.get(session.obj).map(|o| &o.kind) {
            if text.trim().is_empty() {
                design.remove(&[session.obj]);
                selection.clear();
            }
        }
    }

    /// Fit a rectangle (design mm) into the canvas with `margin` px around it.
    fn fit(&mut self, rect: Rect, ((x0, y0), (x1, y1)): Bounds, margin: f32) {
        let (w, h) = ((x1 - x0).max(1.0) as f32, (y1 - y0).max(1.0) as f32);
        self.zoom = ((rect.width() - 2.0 * margin) / w).min((rect.height() - 2.0 * margin) / h).clamp(0.05, 200.0);
        let center = Vec2::new((x0 + x1) as f32 / 2.0, (y0 + y1) as f32 / 2.0) * self.zoom;
        self.offset = rect.size() / 2.0 - center;
    }

    fn content_bounds(scene: &Scene, design: &Design) -> Option<Bounds> {
        if let Some(b) = design.bounds() {
            return Some(b);
        }
        let ((x0, y0), (x1, y1)) = scene.job?.bounds?;
        let (ax, ay) = scene.job_anchor;
        Some(rect_mm(scene.to_view((ax + x0, ay + y0)), scene.to_view((ax + x1, ay + y1))))
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, scene: &Scene, ed: Edit) -> egui::Response {
        let pal = theme::p();
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = response.rect;
        let bed: Bounds = ((0.0, 0.0), scene.bed);

        if self.last_rect.is_some_and(|r| r.size() != rect.size()) && self.pending_zoom.is_none() {
            self.pending_zoom = Some(ZoomTarget::Bed);
        }
        self.last_rect = Some(rect);
        match self.pending_zoom.take() {
            Some(ZoomTarget::Bed) => self.fit(rect, bed, 40.0),
            Some(ZoomTarget::Design) => match Self::content_bounds(scene, ed.design) {
                Some(b) => self.fit(rect, b, 90.0),
                None => self.fit(rect, bed, 40.0),
            },
            None => {}
        }

        // Zoom with the wheel / pinch around the pointer.
        if let Some(pointer) = response.hover_pos() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = (scroll * 0.0015).exp() * pinch;
            if factor != 1.0 {
                let p = pointer - rect.min;
                let new_zoom = (self.zoom * factor).clamp(0.05, 200.0);
                self.offset = p - (p - self.offset) * (new_zoom / self.zoom);
                self.zoom = new_zoom;
            }
        }

        let (zoom, origin) = (self.zoom, rect.min + self.offset);
        let to_screen = move |(x, y): Pt| -> Pos2 { origin + Vec2::new(x as f32, y as f32) * zoom };
        let to_mm = move |p: Pos2| -> Pt {
            let v = (p - origin) / zoom;
            (v.x as f64, v.y as f64)
        };

        // Pointer readout in machine coordinates.
        self.hover = response.hover_pos().map(to_mm).filter(|&(x, y)| x >= 0.0 && y >= 0.0 && x <= scene.bed.0 && y <= scene.bed.1).map(|p| scene.to_view(p));

        let painter = painter.with_clip_rect(rect);
        painter.rect_filled(rect, 0.0, pal.bg);
        let bed_rect = Rect::from_two_pos(to_screen(bed.0), to_screen(bed.1));
        // A soft shadow under the bed, like a sheet on a table.
        painter.add(egui::Shadow { offset: [0, 2], blur: 18, spread: 0, color: Color32::from_black_alpha(if pal.dark { 90 } else { 22 }) }.as_shape(bed_rect, 6.0));
        painter.rect_filled(bed_rect, 6.0, pal.bed);
        self.draw_grid(&painter, scene, bed_rect);
        painter.rect_stroke(bed_rect, 6.0, Stroke::new(1.0, pal.grid_major), StrokeKind::Outside);
        let home = to_screen(scene.to_view((0.0, 0.0)));
        painter.circle_filled(home, 4.0, pal.muted);

        if let Some(job) = scene.job {
            draw_job(&painter, scene, job, &to_screen);
        }

        // Interaction last: it edits the design and then draws it on top.
        self.interact(ui, &response, scene, ed, &to_mm, &to_screen);
        response
    }

    #[allow(clippy::too_many_lines)]
    fn interact(&mut self, ui: &egui::Ui, response: &egui::Response, scene: &Scene, ed: Edit, to_mm: &impl Fn(Pos2) -> Pt, to_screen: &impl Fn(Pt) -> Pos2) {
        let Edit { design, selection, history, tool } = ed;
        let (shift, alt, panning_key) = ui.input(|i| (i.modifiers.shift, i.modifiers.alt, i.key_down(Key::Space) && self.text.is_none()));
        let other_button = ui.input(|i| i.pointer.middle_down() || i.pointer.secondary_down());
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        let tol_mm = 6.0 / self.zoom as f64;

        if self.text.as_ref().is_some_and(|s| s.obj >= design.objects.len() || !matches!(design.objects[s.obj].kind, Kind::Text { .. })) {
            self.text = None;
        }
        if *tool != Tool::Pen {
            self.pen.clear();
        }
        self.nodes.retain(|n| n.obj < design.objects.len() && selection.contains(&n.obj));

        // Panning: space + drag, or the middle / right mouse button.
        if response.drag_started() && (panning_key || other_button) {
            self.drag = Drag::Pan;
        }
        if let Drag::Pan = self.drag {
            self.offset += response.drag_delta();
            if response.drag_stopped() {
                self.drag = Drag::None;
            }
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            self.paint_overlay(ui, response, scene, design, selection, *tool, to_screen, to_mm);
            return;
        }
        if panning_key && response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }

        let sel_bounds = design.selection_bounds(selection);
        let sel_screen = sel_bounds.map(|((x0, y0), (x1, y1))| Rect::from_two_pos(to_screen((x0, y0)), to_screen((x1, y1))));
        let handle_at = |p: Pos2| -> Option<Handle> {
            let r = sel_screen?;
            for hx in [-1i8, 0, 1] {
                for hy in [-1i8, 0, 1] {
                    if hx == 0 && hy == 0 {
                        continue;
                    }
                    // Middle handles of a thin selection would sit on the corners.
                    if (hx == 0 && r.width() < 3.0 * HANDLE_PX) || (hy == 0 && r.height() < 3.0 * HANDLE_PX) {
                        continue;
                    }
                    let hp = Pos2::new(lerp3(r.min.x, r.max.x, hx), lerp3(r.min.y, r.max.y, hy));
                    if (hp - p).length() <= HANDLE_PX {
                        return Some(Handle { hx, hy });
                    }
                }
            }
            None
        };
        // The round handle above the box, or just outside a corner.
        let rotate_at = |p: Pos2| -> bool {
            let Some(r) = sel_screen else { return false };
            if (p - (r.center_top() - Vec2::new(0.0, ROTATE_STEM_PX))).length() <= 9.0 {
                return true;
            }
            !r.expand(HANDLE_PX * 0.6).contains(p) && [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()].iter().any(|c| (*c - p).length() <= 22.0)
        };
        let hit = |design: &Design, p: Pt| hit_test(design, p, tol_mm);
        let select = |selection: &mut Vec<usize>, design: &Design, i: usize, add: bool| {
            let members = design.with_group(i);
            if add {
                if selection.contains(&i) {
                    selection.retain(|j| !members.contains(j));
                } else {
                    selection.extend(members);
                }
            } else {
                *selection = members;
            }
        };

        // ---- text being edited: keys go to it
        if self.text.is_some() {
            self.text_keys(ui, response, design, selection);
        } else if response.has_focus() {
            // Give the keyboard back to the app's shortcuts.
            response.surrender_focus();
        }

        // ---- right click: select what's under the pointer for the context menu
        if response.secondary_clicked() {
            if let Some(p) = pointer {
                match hit(design, to_mm(p)) {
                    Some(i) if !selection.contains(&i) => select(selection, design, i, false),
                    None if *tool != Tool::Nodes => selection.clear(),
                    _ => {}
                }
            }
        }

        // ---- drag start
        if response.drag_started() && !ui.input(|i| i.pointer.secondary_down()) {
            // Where the button went down, not where the drag was recognised.
            let p = ui.input(|i| i.pointer.press_origin()).or(pointer).unwrap_or_default();
            let mm = to_mm(p);
            self.drag = match *tool {
                _ if self.text.as_ref().is_some_and(|s| point_in_object(design, s.obj, mm, tol_mm)) => {
                    self.place_caret(design, mm, false);
                    Drag::TextSelect
                }
                Tool::Select => {
                    self.finish_text(design, selection);
                    if let (Some(handle), Some(from)) = (handle_at(p), sel_bounds) {
                        history.checkpoint(design);
                        let originals = selection.iter().map(|&i| (i, design.objects[i].clone())).collect();
                        Drag::Resize { handle, from, originals }
                    } else if let (true, Some(((x0, y0), (x1, y1)))) = (rotate_at(p), sel_bounds) {
                        history.checkpoint(design);
                        let center = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                        let originals = selection.iter().map(|&i| (i, design.objects[i].clone())).collect();
                        Drag::Rotate { center, start: (mm.1 - center.1).atan2(mm.0 - center.0).to_degrees(), originals, angle: 0.0 }
                    } else if let Some(i) = hit(design, mm) {
                        if !selection.contains(&i) {
                            select(selection, design, i, shift);
                        }
                        Drag::Move { started: false }
                    } else {
                        if !shift {
                            selection.clear();
                        }
                        Drag::Marquee { start: mm }
                    }
                }
                Tool::Nodes => match self.node_hit(design, selection, p, to_screen) {
                    Some(NodeHit::Handle(at, side)) => Drag::NodeHandle { at, side, started: false },
                    Some(NodeHit::Node(at)) => {
                        if !self.nodes.contains(&at) {
                            if !shift {
                                self.nodes.clear();
                            }
                            self.nodes.push(at);
                        }
                        Drag::Nodes { started: false }
                    }
                    None => match hit(design, mm) {
                        Some(i) if !selection.contains(&i) => {
                            *selection = vec![i];
                            self.nodes.clear();
                            Drag::None
                        }
                        _ => {
                            if !shift {
                                self.nodes.clear();
                            }
                            Drag::NodeMarquee { start: mm }
                        }
                    },
                },
                Tool::Rect | Tool::Ellipse | Tool::Polygon => Drag::Create { start: mm },
                Tool::Pen => {
                    self.pen_add(design, selection, history, tool, scene, mm, p, to_screen);
                    if self.pen.is_empty() { Drag::None } else { Drag::PenHandle }
                }
                Tool::Text => Drag::None,
            };
        }

        // ---- dragging
        if response.dragged() && !matches!(self.drag, Drag::None) {
            let p = pointer.unwrap_or_default();
            let mm = to_mm(p);
            let d = response.drag_delta() / self.zoom;
            let d = (d.x as f64, d.y as f64);
            match &mut self.drag {
                Drag::Move { started } => {
                    if d != (0.0, 0.0) {
                        if !*started {
                            history.checkpoint(design);
                            *started = true;
                        }
                        design.translate(selection, d);
                    }
                }
                Drag::Resize { handle, from, originals } => {
                    let to = resize_bounds(*from, *handle, mm, shift || scene.lock_aspect, alt);
                    design.resize(originals, *from, to);
                }
                Drag::Rotate { center, start, originals, angle } => {
                    let mut a = (mm.1 - center.1).atan2(mm.0 - center.0).to_degrees() - *start;
                    if shift {
                        a = (a / 15.0).round() * 15.0;
                    }
                    *angle = a;
                    for (i, o) in originals.iter() {
                        let mut r = o.clone();
                        r.rotation = (o.rotation + a).rem_euclid(360.0);
                        let c = rotate_point((o.x + o.w / 2.0, o.y + o.h / 2.0), *center, a);
                        r.x = c.0 - o.w / 2.0;
                        r.y = c.1 - o.h / 2.0;
                        design.objects[*i] = r;
                    }
                    design.revision += 1;
                }
                Drag::Nodes { started } => {
                    if d != (0.0, 0.0) {
                        if !*started {
                            history.checkpoint(design);
                            *started = true;
                        }
                        let nodes = self.nodes.clone();
                        edit_nodes(design, &nodes, |sub, i| {
                            let n = &mut sub.nodes[i];
                            let mv = |q: Pt| (q.0 + d.0, q.1 + d.1);
                            n.p = mv(n.p);
                            n.c_in = n.c_in.map(mv);
                            n.c_out = n.c_out.map(mv);
                        });
                    }
                }
                Drag::NodeHandle { at, side, started } => {
                    if !*started {
                        history.checkpoint(design);
                        *started = true;
                    }
                    let (at, side) = (*at, *side);
                    edit_nodes(design, &[at], |sub, i| sub.move_handle(i, side, mm, alt));
                }
                Drag::PenHandle => {
                    if let Some(n) = self.pen.last_mut() {
                        if bezier::dist(n.p, mm) * self.zoom as f64 > 3.0 {
                            n.c_out = Some(mm);
                            n.c_in = Some((2.0 * n.p.0 - mm.0, 2.0 * n.p.1 - mm.1));
                            n.smooth = true;
                        }
                    }
                }
                Drag::TextSelect => self.place_caret(design, mm, true),
                _ => {}
            }
        }

        // ---- drag end
        if response.drag_stopped() {
            let end = to_mm(pointer.unwrap_or_default());
            match std::mem::replace(&mut self.drag, Drag::None) {
                Drag::Marquee { start } => {
                    let ((x0, y0), (x1, y1)) = rect_mm(start, end);
                    for (i, o) in design.objects.iter().enumerate() {
                        let ((a0, b0), (a1, b1)) = o.bounds();
                        if a0 >= x0 && b0 >= y0 && a1 <= x1 && b1 <= y1 && !selection.contains(&i) {
                            selection.extend(design.with_group(i).into_iter().filter(|j| !selection.contains(j)).collect::<Vec<_>>());
                        }
                    }
                }
                Drag::NodeMarquee { start } => {
                    let ((x0, y0), (x1, y1)) = rect_mm(start, end);
                    for &obj in selection.iter() {
                        for (sub, s) in design.objects[obj].curves().iter().enumerate() {
                            for (node, n) in s.nodes.iter().enumerate() {
                                let r = NodeRef { obj, sub, node };
                                if n.p.0 >= x0 && n.p.0 <= x1 && n.p.1 >= y0 && n.p.1 <= y1 && !self.nodes.contains(&r) && editable(design, obj) {
                                    self.nodes.push(r);
                                }
                            }
                        }
                    }
                }
                Drag::Create { start } => {
                    let b = creation_box(start, end, shift);
                    history.checkpoint(design);
                    let i = design.add(new_shape(*tool, b, scene));
                    *selection = vec![i];
                    *tool = Tool::Select;
                }
                Drag::Move { .. } | Drag::Resize { .. } | Drag::Rotate { .. } | Drag::Nodes { .. } | Drag::NodeHandle { .. } => design.touch(),
                _ => {}
            }
        }

        // ---- clicks
        // Only real mouse clicks: egui also "clicks" the focused canvas when
        // Space or Enter is pressed, which happens while typing text.
        if response.clicked_by(egui::PointerButton::Primary) {
            let p = pointer.unwrap_or_default();
            let mm = to_mm(p);
            let in_text = self.text.as_ref().is_some_and(|s| point_in_object(design, s.obj, mm, tol_mm));
            if in_text {
                self.place_caret(design, mm, shift);
            } else {
                if self.text.is_some() {
                    self.finish_text(design, selection);
                    if *tool == Tool::Text {
                        // The click that ends editing doesn't start new text.
                        *tool = Tool::Select;
                        return self.paint_overlay(ui, response, scene, design, selection, *tool, to_screen, to_mm);
                    }
                }
                match *tool {
                    Tool::Select => match hit(design, mm) {
                        Some(i) => select(selection, design, i, shift),
                        None if !shift => selection.clear(),
                        None => {}
                    },
                    Tool::Nodes => match self.node_hit(design, selection, p, to_screen) {
                        Some(NodeHit::Node(at)) => {
                            if shift {
                                if let Some(k) = self.nodes.iter().position(|n| *n == at) {
                                    self.nodes.remove(k);
                                } else {
                                    self.nodes.push(at);
                                }
                            } else {
                                self.nodes = vec![at];
                            }
                        }
                        Some(NodeHit::Handle(..)) => {}
                        None => match hit(design, mm) {
                            Some(i) if !selection.contains(&i) => {
                                *selection = vec![i];
                                self.nodes.clear();
                            }
                            Some(_) => self.nodes.clear(),
                            None if self.nodes.is_empty() => selection.clear(),
                            None => self.nodes.clear(),
                        },
                    },
                    Tool::Rect | Tool::Ellipse | Tool::Polygon => {
                        // A click without dragging: a 20 mm shape.
                        history.checkpoint(design);
                        let i = design.add(new_shape(*tool, ((mm.0, mm.1), (mm.0 + 20.0, mm.1 + 20.0)), scene));
                        *selection = vec![i];
                        *tool = Tool::Select;
                    }
                    Tool::Pen => self.pen_add(design, selection, history, tool, scene, mm, p, to_screen),
                    Tool::Text => match hit(design, mm).filter(|&i| matches!(design.objects[i].kind, Kind::Text { .. })) {
                        Some(i) => {
                            history.checkpoint(design);
                            *selection = vec![i];
                            self.edit_text(design, i);
                            self.place_caret(design, mm, false);
                        }
                        None => {
                            history.checkpoint(design);
                            // The click marks the top-left of the first line.
                            let baseline = (mm.0, mm.1 + scene.text_size * 0.8);
                            if let Ok(o) = Object::text("", scene.text_style.clone(), baseline, scene.text_size, scene.text_color) {
                                let i = design.add(o);
                                *selection = vec![i];
                                self.edit_text(design, i);
                            }
                        }
                    },
                }
            }
        }
        if response.double_clicked() {
            let p = pointer.unwrap_or_default();
            let mm = to_mm(p);
            match *tool {
                Tool::Pen => self.finish_pen(design, selection, history, tool, scene, false),
                Tool::Select => match hit(design, mm) {
                    Some(i) if matches!(design.objects[i].kind, Kind::Text { .. }) => {
                        history.checkpoint(design);
                        *selection = vec![i];
                        self.edit_text(design, i);
                        self.place_caret(design, mm, false);
                        self.select_word();
                    }
                    Some(i) => {
                        // Into the point editor.
                        *selection = vec![i];
                        self.nodes.clear();
                        *tool = Tool::Nodes;
                    }
                    None if design.is_empty() || selection.is_empty() => self.pending_zoom = Some(ZoomTarget::Bed),
                    None => {}
                },
                Tool::Nodes => match self.node_hit(design, selection, p, to_screen) {
                    Some(NodeHit::Node(at)) => {
                        history.checkpoint(design);
                        edit_nodes(design, &[at], |sub, i| sub.toggle_smooth(i));
                        design.touch();
                    }
                    Some(NodeHit::Handle(..)) => {}
                    None => {
                        // On a segment: a new node there.
                        if let Some((obj, sub, seg, t)) = self.segment_hit(design, selection, mm) {
                            history.checkpoint(design);
                            design.objects[obj].make_path();
                            if let Some(mut curves) = design.objects[obj].path_curves() {
                                let node = curves[sub].split(seg, t);
                                design.objects[obj].set_path_curves(curves);
                                design.touch();
                                self.nodes = vec![NodeRef { obj, sub, node }];
                            }
                        }
                    }
                },
                _ => {}
            }
        }
        if *tool == Tool::Pen && !self.pen.is_empty() {
            let (enter, escape, back) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape), i.key_pressed(Key::Backspace)));
            if enter || escape {
                self.finish_pen(design, selection, history, tool, scene, false);
            } else if back {
                self.pen.pop();
            }
        }

        // ---- cursor and hover
        self.rotate_hover = false;
        self.hovered = None;
        if let Some(p) = response.hover_pos() {
            let mm = to_mm(p);
            let over_text = self.text.as_ref().is_some_and(|s| point_in_object(design, s.obj, mm, tol_mm));
            let icon = match (&self.drag, *tool) {
                _ if over_text => CursorIcon::Text,
                (Drag::Move { .. }, _) => CursorIcon::Grabbing,
                (Drag::Resize { handle, .. }, _) => handle_cursor(*handle),
                (Drag::Rotate { .. }, _) => {
                    self.rotate_hover = true;
                    CursorIcon::None
                }
                (_, Tool::Select) => match handle_at(p) {
                    Some(h) => handle_cursor(h),
                    None if rotate_at(p) => {
                        self.rotate_hover = true;
                        CursorIcon::None
                    }
                    None => match hit(design, mm) {
                        Some(i) => {
                            self.hovered = Some(i);
                            CursorIcon::Grab
                        }
                        None => CursorIcon::Default,
                    },
                },
                (_, Tool::Nodes) => match self.node_hit(design, selection, p, to_screen) {
                    Some(_) => CursorIcon::PointingHand,
                    None => {
                        self.hovered = hit(design, mm);
                        CursorIcon::Default
                    }
                },
                (_, Tool::Text) => CursorIcon::Text,
                _ => CursorIcon::Crosshair,
            };
            if !(panning_key || matches!(self.drag, Drag::Pan)) {
                ui.ctx().set_cursor_icon(icon);
            }
        }
        if self.text.is_some() {
            // Keep the caret blinking.
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }

        self.paint_overlay(ui, response, scene, design, selection, *tool, to_screen, to_mm);
    }

    // ------------------------------------------------------------ pen

    #[allow(clippy::too_many_arguments)]
    fn pen_add(&mut self, design: &mut Design, selection: &mut Vec<usize>, history: &mut History, tool: &mut Tool, scene: &Scene, mm: Pt, p: Pos2, to_screen: &impl Fn(Pt) -> Pos2) {
        if self.pen.len() >= 2 && (to_screen(self.pen[0].p) - p).length() < 9.0 {
            self.finish_pen(design, selection, history, tool, scene, true);
            return;
        }
        if self.pen.last().is_none_or(|n| bezier::dist(n.p, mm) > 0.01) {
            self.pen.push(Node::corner(mm));
        }
    }

    fn finish_pen(&mut self, design: &mut Design, selection: &mut Vec<usize>, history: &mut History, tool: &mut Tool, scene: &Scene, close: bool) {
        let nodes = std::mem::take(&mut self.pen);
        if nodes.len() >= 2 {
            history.checkpoint(design);
            let mut o = Object::path(vec![SubPath { nodes, closed: close }], scene.draw_color);
            o.name = t!("object-line");
            let i = design.add(o);
            *selection = vec![i];
        }
        *tool = Tool::Select;
    }

    // ------------------------------------------------------------ nodes

    fn node_hit(&self, design: &Design, selection: &[usize], p: Pos2, to_screen: &impl Fn(Pt) -> Pos2) -> Option<NodeHit> {
        let near = |q: Pt| (to_screen(q) - p).length() <= NODE_PX;
        // Handles of selected nodes first: they sit on top.
        for &r in &self.nodes {
            let n = design.objects.get(r.obj)?.curves().get(r.sub)?.nodes.get(r.node).copied()?;
            if n.c_out.is_some_and(near) {
                return Some(NodeHit::Handle(r, Side::Out));
            }
            if n.c_in.is_some_and(near) {
                return Some(NodeHit::Handle(r, Side::In));
            }
        }
        for &obj in selection {
            if !editable(design, obj) {
                continue;
            }
            for (sub, s) in design.objects[obj].curves().iter().enumerate() {
                for (node, n) in s.nodes.iter().enumerate() {
                    if near(n.p) {
                        return Some(NodeHit::Node(NodeRef { obj, sub, node }));
                    }
                }
            }
        }
        None
    }

    /// The nearest point on a selected path's outline: (object, sub-path, segment, t).
    fn segment_hit(&self, design: &Design, selection: &[usize], mm: Pt) -> Option<(usize, usize, usize, f64)> {
        let tol = 6.0 / self.zoom as f64;
        let mut best: Option<(usize, usize, usize, f64, f64)> = None;
        for &obj in selection {
            if !editable(design, obj) {
                continue;
            }
            for (sub, s) in design.objects[obj].curves().iter().enumerate() {
                if let Some((seg, t, d)) = s.nearest(mm) {
                    if d <= tol && best.is_none_or(|b| d < b.4) {
                        best = Some((obj, sub, seg, t, d));
                    }
                }
            }
        }
        best.map(|(o, s, g, t, _)| (o, s, g, t))
    }

    /// Delete the selected nodes; paths left too short go, and so do
    /// objects with nothing left. Returns whether anything was selected.
    pub fn delete_nodes(&mut self, design: &mut Design, selection: &mut Vec<usize>, history: &mut History) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        history.checkpoint(design);
        let nodes = std::mem::take(&mut self.nodes);
        let mut empty = Vec::new();
        let mut objs: Vec<usize> = nodes.iter().map(|n| n.obj).collect();
        objs.sort_unstable();
        objs.dedup();
        for obj in objs {
            design.objects[obj].make_path();
            let Some(mut curves) = design.objects[obj].path_curves() else { continue };
            let mut keep = Vec::new();
            for (si, mut s) in curves.drain(..).enumerate() {
                let gone: Vec<usize> = nodes.iter().filter(|n| n.obj == obj && n.sub == si).map(|n| n.node).collect();
                if s.remove_nodes(&gone) {
                    keep.push(s);
                }
            }
            if keep.is_empty() {
                empty.push(obj);
            } else {
                design.objects[obj].set_path_curves(keep);
            }
        }
        if !empty.is_empty() {
            selection.retain(|i| !empty.contains(i));
            design.remove(&empty);
            // Indices after removed objects moved down.
            selection.clear();
        }
        design.touch();
        true
    }

    /// Make the selected nodes smooth or sharp corners.
    pub fn set_nodes_smooth(&mut self, design: &mut Design, history: &mut History, smooth: bool) {
        if self.nodes.is_empty() {
            return;
        }
        history.checkpoint(design);
        let nodes = self.nodes.clone();
        edit_nodes(design, &nodes, |sub, i| {
            let n = sub.nodes[i];
            let has = n.c_in.is_some() || n.c_out.is_some();
            if has != smooth {
                sub.toggle_smooth(i);
            }
        });
        design.touch();
    }

    // ------------------------------------------------------------ text

    /// Caret positions of the edited text, refreshed when it changed.
    fn text_layout(&mut self, design: &Design) -> Option<(fonts::Layout, Object)> {
        let s = self.text.as_mut()?;
        let o = design.objects.get(s.obj)?;
        let Kind::Text { text, style, .. } = &o.kind else { return None };
        let fs = style.fonts();
        if !s.layout.as_ref().is_some_and(|(t, st, _)| t == text && *st == fs) {
            s.layout = fonts::layout(text, &fs).ok().map(|l| (text.clone(), fs, l));
        }
        s.layout.as_ref().map(|(_, _, l)| (l.clone(), o.clone()))
    }

    /// Put the caret at the character nearest `mm` (design mm).
    fn place_caret(&mut self, design: &Design, mm: Pt, extend: bool) {
        let Some((layout, o)) = self.text_layout(design) else { return };
        let Kind::Text { text, ink, scale, .. } = &o.kind else { return };
        let (lx, ly) = o.to_local(mm);
        let (ux, uy) = (lx / scale.0.max(1e-9) + ink.x0, ly / scale.1.max(1e-9) + ink.y0);
        let line = layout
            .lines
            .iter()
            .enumerate()
            .min_by(|a, b| (a.1.baseline - fonts::EM * 0.35 - uy).abs().total_cmp(&(b.1.baseline - fonts::EM * 0.35 - uy).abs()))
            .map_or(0, |(i, _)| i);
        let col = layout.lines.get(line).map_or(0, |l| l.carets.iter().enumerate().min_by(|a, b| (a.1 - ux).abs().total_cmp(&(b.1 - ux).abs())).map_or(0, |(i, _)| i));
        if let Some(s) = self.text.as_mut() {
            s.caret.move_to(textedit::index(text, line, col), extend);
            s.moved = Instant::now();
            s.want_focus = true;
        }
    }

    fn select_word(&mut self) {
        let Some(s) = self.text.as_mut() else { return };
        let Some((text, _, _)) = &s.layout else { return };
        let i = s.caret.cursor;
        s.caret = Caret { anchor: textedit::word_boundary(text, i, false), cursor: textedit::word_boundary(text, i, true) };
    }

    /// Keyboard input while text is edited.
    fn text_keys(&mut self, ui: &egui::Ui, response: &egui::Response, design: &mut Design, selection: &mut Vec<usize>) {
        let Some(s) = self.text.as_mut() else { return };
        let obj = s.obj;
        // Take the keyboard when editing starts or when nothing else has it,
        // but leave it to a field the user clicked (the font size, say).
        let focused = ui.memory(|m| m.focused());
        if s.want_focus {
            s.want_focus = false;
            ui.memory_mut(|m| m.request_focus(response.id));
        } else if focused.is_none() {
            // A field just let go (its Enter is in this frame's keys): take the
            // keyboard back from the next frame on.
            ui.memory_mut(|m| m.request_focus(response.id));
            return;
        } else if focused != Some(response.id) {
            return;
        }
        ui.memory_mut(|m| m.set_focus_lock_filter(response.id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }));
        let Kind::Text { text, style, .. } = &design.objects[obj].kind else { return };
        let (mut text, style) = (text.clone(), style.clone());
        let before = text.clone();
        let mut caret = s.caret;
        let mut finish = false;
        let lines: Vec<usize> = text.split('\n').map(textedit::char_count).collect();
        for event in ui.input(|i| i.events.clone()) {
            match event {
                Event::Text(t) => {
                    let t: String = t.chars().filter(|c| !c.is_control()).collect();
                    if !t.is_empty() {
                        textedit::insert(&mut text, &mut caret, &t);
                    }
                }
                Event::Paste(t) => textedit::insert(&mut text, &mut caret, &t.replace("\r\n", "\n")),
                Event::Copy if caret.has_selection() => ui.ctx().copy_text(textedit::selected(&text, &caret)),
                Event::Cut if caret.has_selection() => {
                    ui.ctx().copy_text(textedit::selected(&text, &caret));
                    textedit::insert(&mut text, &mut caret, "");
                }
                Event::Key { key, pressed: true, modifiers, .. } => {
                    let n = textedit::char_count(&text);
                    let (line, col) = textedit::line_col(&text, caret.cursor);
                    let word = modifiers.alt || (modifiers.ctrl && !cfg!(target_os = "macos"));
                    let whole_line = modifiers.mac_cmd;
                    match key {
                        Key::Escape => finish = true,
                        Key::Enter => textedit::insert(&mut text, &mut caret, "\n"),
                        Key::Backspace if word && !caret.has_selection() => {
                            caret.move_to(textedit::word_boundary(&text, caret.cursor, false), true);
                            textedit::insert(&mut text, &mut caret, "");
                        }
                        Key::Backspace => textedit::delete(&mut text, &mut caret, false),
                        Key::Delete => textedit::delete(&mut text, &mut caret, true),
                        Key::A if modifiers.command => caret = Caret { anchor: 0, cursor: n },
                        Key::ArrowLeft => {
                            let to = if whole_line {
                                textedit::index(&text, line, 0)
                            } else if word {
                                textedit::word_boundary(&text, caret.cursor, false)
                            } else if caret.has_selection() && !modifiers.shift {
                                caret.range().0
                            } else {
                                caret.cursor.saturating_sub(1)
                            };
                            caret.move_to(to, modifiers.shift);
                        }
                        Key::ArrowRight => {
                            let to = if whole_line {
                                textedit::index(&text, line, usize::MAX)
                            } else if word {
                                textedit::word_boundary(&text, caret.cursor, true)
                            } else if caret.has_selection() && !modifiers.shift {
                                caret.range().1
                            } else {
                                (caret.cursor + 1).min(n)
                            };
                            caret.move_to(to, modifiers.shift);
                        }
                        Key::ArrowUp if line > 0 => caret.move_to(textedit::index(&text, line - 1, col), modifiers.shift),
                        Key::ArrowUp => caret.move_to(0, modifiers.shift),
                        Key::ArrowDown if line + 1 < lines.len() => caret.move_to(textedit::index(&text, line + 1, col), modifiers.shift),
                        Key::ArrowDown => caret.move_to(n, modifiers.shift),
                        Key::Home => caret.move_to(textedit::index(&text, line, 0), modifiers.shift),
                        Key::End => caret.move_to(textedit::index(&text, line, usize::MAX), modifiers.shift),
                        _ => continue,
                    }
                }
                _ => continue,
            }
            s.moved = Instant::now();
        }
        s.caret = caret;
        if text != before {
            let _ = design.objects[obj].set_text(&text, style);
            design.touch();
        }
        if finish {
            ui.memory_mut(|m| m.surrender_focus(response.id));
            self.finish_text(design, selection);
        }
    }

    // ------------------------------------------------------------ drawing

    /// Everything that depends on the (possibly just edited) design.
    #[allow(clippy::too_many_arguments)]
    fn paint_overlay(&mut self, ui: &egui::Ui, response: &egui::Response, scene: &Scene, design: &Design, selection: &[usize], tool: Tool, to_screen: &impl Fn(Pt) -> Pos2, to_mm: &impl Fn(Pos2) -> Pt) {
        let pal = theme::p();
        let painter = ui.painter().with_clip_rect(response.rect);

        draw_design(&painter, scene, design, self.zoom, to_screen);

        // The start point: where the laser head will be, on the box around
        // all objects (LotusCut's green square).
        if let Some(b @ ((x0, y0), (x1, y1))) = design.bounds() {
            let r = Rect::from_two_pos(to_screen((x0, y0)), to_screen((x1, y1)));
            dashed_rect(&painter, r, pal.ok.gamma_multiply(0.35));
            let c = to_screen(scene.anchor.point(b));
            let sq = Rect::from_center_size(c, Vec2::splat(10.0));
            painter.rect_filled(sq, 2.0, pal.ok);
            painter.rect_stroke(sq, 2.0, Stroke::new(1.5, pal.panel), StrokeKind::Outside);
        }

        // Hover hint: the outline of the object under the pointer.
        if let Some(i) = self.hovered.filter(|i| !selection.contains(i)) {
            if let Some(o) = design.objects.get(i) {
                for pl in o.outline() {
                    painter.add(Shape::line(pl.into_iter().map(to_screen).collect(), Stroke::new(1.5, pal.accent.gamma_multiply(0.6))));
                }
            }
        }

        let editing = self.text.as_ref().map(|s| s.obj);
        if tool == Tool::Nodes {
            self.paint_nodes(&painter, design, selection, to_screen);
        } else if let Some(((x0, y0), (x1, y1))) = design.selection_bounds(selection).filter(|_| editing.is_none()) {
            // Selection: box, size label and handles.
            let r = Rect::from_two_pos(to_screen((x0, y0)), to_screen((x1, y1)));
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, pal.accent), StrokeKind::Outside);
            if selection.len() > 1 {
                for &i in selection {
                    if let Some(o) = design.objects.get(i) {
                        let ((a0, b0), (a1, b1)) = o.bounds();
                        dashed_rect(&painter, Rect::from_two_pos(to_screen((a0, b0)), to_screen((a1, b1))), pal.accent.gamma_multiply(0.5));
                    }
                }
            }
            if tool == Tool::Select {
                let top = r.center_top();
                let knob = top - Vec2::new(0.0, ROTATE_STEM_PX);
                painter.line_segment([top, knob + Vec2::new(0.0, 6.0)], Stroke::new(1.0, pal.accent));
                painter.circle_filled(knob, 6.0, pal.panel);
                painter.circle_stroke(knob, 6.0, Stroke::new(1.5, pal.accent));
                for hx in [-1i8, 0, 1] {
                    for hy in [-1i8, 0, 1] {
                        if (hx == 0 && hy == 0) || (hx == 0 && r.width() < 3.0 * HANDLE_PX) || (hy == 0 && r.height() < 3.0 * HANDLE_PX) {
                            continue;
                        }
                        let c = Pos2::new(lerp3(r.min.x, r.max.x, hx), lerp3(r.min.y, r.max.y, hy));
                        let hr = Rect::from_center_size(c, Vec2::splat(HANDLE_PX));
                        painter.rect_filled(hr, 2.0, pal.panel);
                        painter.rect_stroke(hr, 2.0, Stroke::new(1.5, pal.accent), StrokeKind::Middle);
                    }
                }
            }
            let label = format!("{:.1} × {:.1} mm", x1 - x0, y1 - y0);
            badge(&painter, r.center_bottom() + Vec2::new(0.0, 16.0), &label, pal.accent, pal.on_accent);
        }

        // In-progress gestures.
        let pointer = response.hover_pos().or(response.interact_pointer_pos());
        match &self.drag {
            Drag::Marquee { start } | Drag::NodeMarquee { start } => {
                if let Some(p) = pointer {
                    let r = Rect::from_two_pos(to_screen(*start), p);
                    painter.rect_filled(r, 0.0, pal.accent.gamma_multiply(0.08));
                    dashed_rect(&painter, r, pal.accent);
                }
            }
            Drag::Create { start } => {
                if let Some(p) = pointer {
                    let shift = ui.input(|i| i.modifiers.shift);
                    let b = creation_box(*start, to_mm(p), shift);
                    let preview = new_shape(tool, b, scene);
                    for pl in preview.outline() {
                        painter.add(Shape::line(pl.into_iter().map(to_screen).collect(), Stroke::new(1.5, pal.accent)));
                    }
                    let ((x0, y0), (x1, y1)) = b;
                    badge(&painter, to_screen((x1, y1)) + Vec2::new(40.0, 16.0), &format!("{:.1} × {:.1} mm", x1 - x0, y1 - y0), pal.accent, pal.on_accent);
                }
            }
            Drag::Rotate { angle, .. } => {
                if let Some(p) = pointer {
                    badge(&painter, p + Vec2::new(28.0, 22.0), &format!("{angle:.0}°"), pal.accent, pal.on_accent);
                }
            }
            _ => {}
        }
        if self.rotate_hover {
            if let Some(p) = response.hover_pos() {
                let r = Rect::from_center_size(p, Vec2::splat(18.0));
                painter.circle_filled(p, 12.0, pal.panel.gamma_multiply(0.85));
                icons::paint(&painter, r, Icon::RotateCw, pal.text);
            }
        }
        if !self.pen.is_empty() {
            self.paint_pen(&painter, pointer.map(to_mm), to_screen);
        }
        if editing.is_some() {
            self.paint_text_edit(&painter, design, to_screen);
        }

        // The head, and where the job will start relative to the design.
        if let Some(head) = scene.head {
            let hp = to_screen(scene.to_view(head));
            let bed_rect = Rect::from_two_pos(to_screen((0.0, 0.0)), to_screen(scene.bed));
            draw_head(&painter, bed_rect, hp, head);
        }
    }

    fn paint_nodes(&self, painter: &egui::Painter, design: &Design, selection: &[usize], to_screen: &impl Fn(Pt) -> Pos2) {
        let pal = theme::p();
        for &obj in selection {
            let Some(o) = design.objects.get(obj) else { continue };
            if !editable(design, obj) {
                let ((x0, y0), (x1, y1)) = o.bounds();
                dashed_rect(painter, Rect::from_two_pos(to_screen((x0, y0)), to_screen((x1, y1))), pal.muted);
                continue;
            }
            for pl in o.outline() {
                painter.add(Shape::line(pl.into_iter().map(to_screen).collect(), Stroke::new(1.2, pal.accent)));
            }
            for (sub, s) in o.curves().iter().enumerate() {
                for (node, n) in s.nodes.iter().enumerate() {
                    let selected = self.nodes.contains(&NodeRef { obj, sub, node });
                    let c = to_screen(n.p);
                    if selected {
                        for h in [n.c_in, n.c_out].into_iter().flatten() {
                            let hp = to_screen(h);
                            painter.line_segment([c, hp], Stroke::new(1.0, pal.accent));
                            painter.circle_filled(hp, 3.5, pal.accent);
                        }
                    }
                    let fill = if selected { pal.accent } else { pal.panel };
                    if n.smooth {
                        painter.circle_filled(c, NODE_PX / 2.0 + 0.5, fill);
                        painter.circle_stroke(c, NODE_PX / 2.0 + 0.5, Stroke::new(1.5, pal.accent));
                    } else {
                        let r = Rect::from_center_size(c, Vec2::splat(NODE_PX));
                        painter.rect_filled(r, 1.0, fill);
                        painter.rect_stroke(r, 1.0, Stroke::new(1.5, pal.accent), StrokeKind::Middle);
                    }
                }
            }
        }
    }

    fn paint_pen(&self, painter: &egui::Painter, pointer: Option<Pt>, to_screen: &impl Fn(Pt) -> Pos2) {
        let pal = theme::p();
        let mut nodes = self.pen.clone();
        let dragging = matches!(self.drag, Drag::PenHandle);
        if let (Some(p), false) = (pointer, dragging) {
            nodes.push(Node::corner(p));
        }
        let s = SubPath { nodes, closed: false };
        let pts: Vec<Pos2> = s.flatten(0.05).into_iter().map(to_screen).collect();
        painter.add(Shape::line(pts, Stroke::new(1.5, pal.accent)));
        for (k, n) in self.pen.iter().enumerate() {
            let c = to_screen(n.p);
            if k + 1 == self.pen.len() {
                for h in [n.c_in, n.c_out].into_iter().flatten() {
                    painter.line_segment([c, to_screen(h)], Stroke::new(1.0, pal.accent));
                    painter.circle_filled(to_screen(h), 3.0, pal.accent);
                }
            }
            let r = Rect::from_center_size(c, Vec2::splat(NODE_PX));
            painter.rect_filled(r, 1.0, if k == 0 { pal.accent } else { pal.panel });
            painter.rect_stroke(r, 1.0, Stroke::new(1.5, pal.accent), StrokeKind::Middle);
        }
    }

    fn paint_text_edit(&mut self, painter: &egui::Painter, design: &Design, to_screen: &impl Fn(Pt) -> Pos2) {
        let pal = theme::p();
        let Some((layout, o)) = self.text_layout(design) else { return };
        let Some(s) = &self.text else { return };
        let Kind::Text { text, ink, scale, .. } = &o.kind else { return };
        let screen = |ux: f64, uy: f64| to_screen(o.to_world(((ux - ink.x0) * scale.0, (uy - ink.y0) * scale.1)));
        let (top, bottom) = (-fonts::EM * 0.95, fonts::EM * 0.25);
        // The box being edited.
        let ((x0, y0), (x1, y1)) = o.bounds();
        let r = Rect::from_two_pos(to_screen((x0, y0)), to_screen((x1, y1))).expand(4.0);
        painter.rect_stroke(r, 3.0, Stroke::new(1.0, pal.accent.gamma_multiply(0.4)), StrokeKind::Outside);
        // Selection.
        let (a, b) = s.caret.range();
        if a != b {
            for (li, line) in layout.lines.iter().enumerate() {
                let start = textedit::index(text, li, 0);
                let len = line.carets.len() - 1;
                let (from, to) = (a.max(start), b.min(start + len));
                if from > to || (from == to && !(a <= start + len && b > start + len)) {
                    continue;
                }
                let (xa, xb) = (line.carets[from - start], line.carets[(to - start).min(len)] + if b > start + len { fonts::EM * 0.25 } else { 0.0 });
                let quad = vec![screen(xa, line.baseline + top), screen(xb, line.baseline + top), screen(xb, line.baseline + bottom), screen(xa, line.baseline + bottom)];
                painter.add(Shape::convex_polygon(quad, pal.accent.gamma_multiply(0.22), Stroke::NONE));
            }
        }
        // Caret, blinking.
        let blink_on = (s.moved.elapsed().as_millis() / 530) % 2 == 0;
        if blink_on {
            let (line, col) = textedit::line_col(text, s.caret.cursor);
            if let Some(l) = layout.lines.get(line) {
                let x = l.carets.get(col).copied().unwrap_or(0.0);
                painter.line_segment([screen(x, l.baseline + top), screen(x, l.baseline + bottom)], Stroke::new(1.6, pal.accent));
            }
        }
    }

    fn draw_grid(&self, painter: &egui::Painter, scene: &Scene, bed_rect: Rect) {
        let pal = theme::p();
        let (bw, bh) = scene.bed;
        let zoom = self.zoom as f64;
        let fine = if zoom * 10.0 >= 6.0 { Some(10.0) } else { None };
        let major = [10.0, 50.0, 100.0].into_iter().find(|s| zoom * s >= 45.0).unwrap_or(200.0);
        let line = |step: f64, color: Color32, labels: bool| {
            let mut v = step;
            while v < bw {
                let x = bed_rect.min.x + (v * zoom) as f32;
                painter.line_segment([Pos2::new(x, bed_rect.min.y), Pos2::new(x, bed_rect.max.y)], Stroke::new(1.0, color));
                if labels {
                    let ctrl_x = if scene.origin_right { bw - v } else { v };
                    painter.text(Pos2::new(x, bed_rect.min.y - 6.0), Align2::CENTER_BOTTOM, format!("{ctrl_x:.0}"), FontId::proportional(10.0), pal.muted);
                }
                v += step;
            }
            let mut v = step;
            while v < bh {
                let y = bed_rect.min.y + (v * zoom) as f32;
                painter.line_segment([Pos2::new(bed_rect.min.x, y), Pos2::new(bed_rect.max.x, y)], Stroke::new(1.0, color));
                if labels {
                    painter.text(Pos2::new(bed_rect.min.x - 6.0, y), Align2::RIGHT_CENTER, format!("{v:.0}"), FontId::proportional(10.0), pal.muted);
                }
                v += step;
            }
        };
        if let Some(step) = fine.filter(|&f| f < major) {
            line(step, pal.grid_fine, false);
        }
        line(major, pal.grid_major, true);
    }
}

enum NodeHit {
    Node(NodeRef),
    Handle(NodeRef, Side),
}

/// Text keeps its letters: its points can't be edited (convert it first).
fn editable(design: &Design, obj: usize) -> bool {
    design.objects.get(obj).is_some_and(|o| !matches!(o.kind, Kind::Text { .. }))
}

/// Change nodes (in design coordinates) of paths; shapes become paths first.
fn edit_nodes(design: &mut Design, nodes: &[NodeRef], f: impl Fn(&mut SubPath, usize)) {
    let mut objs: Vec<usize> = nodes.iter().map(|n| n.obj).collect();
    objs.sort_unstable();
    objs.dedup();
    for obj in objs {
        if !editable(design, obj) {
            continue;
        }
        design.objects[obj].make_path();
        let Some(mut curves) = design.objects[obj].path_curves() else { continue };
        for n in nodes.iter().filter(|n| n.obj == obj) {
            if let Some(s) = curves.get_mut(n.sub) {
                if n.node < s.nodes.len() {
                    f(s, n.node);
                }
            }
        }
        design.objects[obj].set_path_curves(curves);
    }
    design.revision += 1;
}

fn point_in_object(design: &Design, i: usize, p: Pt, tol: f64) -> bool {
    design.objects.get(i).is_some_and(|o| {
        let ((x0, y0), (x1, y1)) = o.bounds();
        p.0 >= x0 - tol && p.0 <= x1 + tol && p.1 >= y0 - tol && p.1 <= y1 + tol
    })
}

/// The smallest object whose box contains the point (within `tol` mm), so
/// inner shapes can be picked out of larger ones.
fn hit_test(design: &Design, p: Pt, tol: f64) -> Option<usize> {
    let area = |o: &Object| {
        let ((x0, y0), (x1, y1)) = o.bounds();
        (x1 - x0).max(tol) * (y1 - y0).max(tol)
    };
    (0..design.objects.len()).filter(|&i| point_in_object(design, i, p, tol)).min_by(|&a, &b| area(&design.objects[a]).total_cmp(&area(&design.objects[b])))
}

fn lerp3(a: f32, b: f32, t: i8) -> f32 {
    match t {
        -1 => a,
        0 => (a + b) / 2.0,
        _ => b,
    }
}

fn handle_cursor(h: Handle) -> CursorIcon {
    match (h.hx, h.hy) {
        (0, _) => CursorIcon::ResizeVertical,
        (_, 0) => CursorIcon::ResizeHorizontal,
        (a, b) if a == b => CursorIcon::ResizeNwSe,
        _ => CursorIcon::ResizeNeSw,
    }
}

/// New bounds when dragging `handle` of `from` to `p`. `from_centre`
/// (Alt) resizes around the middle.
fn resize_bounds(from: Bounds, h: Handle, p: Pt, keep_aspect: bool, from_centre: bool) -> Bounds {
    let ((fx0, fy0), (fx1, fy1)) = from;
    let (cx, cy) = ((fx0 + fx1) / 2.0, (fy0 + fy1) / 2.0);
    let ((mut x0, mut y0), (mut x1, mut y1)) = from;
    match h.hx {
        -1 => x0 = p.0.min(x1 - MIN_SIZE),
        1 => x1 = p.0.max(x0 + MIN_SIZE),
        _ => {}
    }
    match h.hy {
        -1 => y0 = p.1.min(y1 - MIN_SIZE),
        1 => y1 = p.1.max(y0 + MIN_SIZE),
        _ => {}
    }
    if from_centre {
        if h.hx == -1 {
            x1 = 2.0 * cx - x0;
        } else if h.hx == 1 {
            x0 = 2.0 * cx - x1;
        }
        if h.hy == -1 {
            y1 = 2.0 * cy - y0;
        } else if h.hy == 1 {
            y0 = 2.0 * cy - y1;
        }
    }
    if keep_aspect && h.hx != 0 && h.hy != 0 {
        let (fw, fh) = ((fx1 - fx0).max(1e-9), (fy1 - fy0).max(1e-9));
        let s = ((x1 - x0) / fw).max((y1 - y0) / fh);
        let (w, h2) = (fw * s, fh * s);
        if from_centre {
            (x0, x1, y0, y1) = (cx - w / 2.0, cx + w / 2.0, cy - h2 / 2.0, cy + h2 / 2.0);
        } else {
            // Keep the opposite corner fixed.
            if h.hx == -1 {
                x0 = x1 - w;
            } else {
                x1 = x0 + w;
            }
            if h.hy == -1 {
                y0 = y1 - h2;
            } else {
                y1 = y0 + h2;
            }
        }
    }
    ((x0, y0), (x1, y1))
}

fn creation_box(start: Pt, end: Pt, square: bool) -> Bounds {
    let (mut dx, mut dy) = (end.0 - start.0, end.1 - start.1);
    if square {
        let s = dx.abs().max(dy.abs());
        dx = s * dx.signum();
        dy = s * dy.signum();
    }
    let b = rect_mm(start, (start.0 + dx, start.1 + dy));
    if (b.1.0 - b.0.0) < 0.5 && (b.1.1 - b.0.1) < 0.5 {
        return (start, (start.0 + 20.0, start.1 + 20.0));
    }
    b
}

fn new_shape(tool: Tool, ((x0, y0), (x1, y1)): Bounds, scene: &Scene) -> Object {
    let kind = match tool {
        Tool::Ellipse => Kind::Ellipse,
        Tool::Polygon => Kind::Polygon { sides: scene.polygon_sides },
        _ => Kind::Rect { radius: 0.0 },
    };
    Object::new(kind, (x0, y0, (x1 - x0).max(MIN_SIZE), (y1 - y0).max(MIN_SIZE)), scene.draw_color)
}

fn dashed_rect(painter: &egui::Painter, r: Rect, color: Color32) {
    let pts = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    painter.extend(Shape::dashed_line(&pts, Stroke::new(1.0, color), 4.0, 3.0));
}

/// A small rounded label centred at `at` (sizes, angles).
fn badge(painter: &egui::Painter, at: Pos2, text: &str, fill: Color32, color: Color32) {
    let galley = painter.layout_no_wrap(text.to_string(), theme::medium(11.0), color);
    let r = Rect::from_center_size(at, galley.size() + Vec2::new(12.0, 6.0));
    painter.rect_filled(r, 5.0, fill);
    painter.galley(r.center() - galley.size() / 2.0, galley, color);
}

/// Most hatch lines drawn per engrave layer; finer spacing is thinned out.
const MAX_PREVIEW_LINES: i32 = 400;

fn draw_design(painter: &egui::Painter, scene: &Scene, design: &Design, zoom: f32, to_screen: &impl Fn(Pt) -> Pos2) {
    // Engrave layers: hatch lines at the real spacing when zoomed in, thinned
    // out so they stay a few pixels apart when zoomed out.
    for (li, layer) in design.layers.iter().enumerate().filter(|(_, l)| l.action == Action::Do(Operation::Engrave)) {
        let interval_mm = match layer.resolve(scene.material) {
            Ok(Some((_, s))) => s.line_interval_mm,
            _ => PREVIEW_LINE_INTERVAL_MM,
        };
        let (outlines, shapes) = design.layer_fill_um(layer.color);
        let Some((y0, y1)) = outlines.iter().flatten().fold(None, |acc: Option<(i32, i32)>, p| Some(acc.map_or((p.1, p.1), |(lo, hi)| (lo.min(p.1), hi.max(p.1))))) else {
            continue;
        };
        let mut step = ((interval_mm as f64).max(2.5 / zoom as f64) * 1000.0) as i32;
        step = step.max((y1 - y0) / MAX_PREVIEW_LINES).max(1);
        let color = theme::layer_color(li as u8, Some(layer.color)).gamma_multiply(0.55);
        for line in ruida::fill::scan_shapes(&outlines, &shapes, step) {
            let y = line.y as f64 / 1000.0;
            for (a, b) in line.spans {
                painter.line_segment([to_screen((a as f64 / 1000.0, y)), to_screen((b as f64 / 1000.0, y))], Stroke::new(1.0, color));
            }
        }
    }
    for o in &design.objects {
        let li = design.layers.iter().position(|l| l.color == o.color);
        let mut color = theme::layer_color(li.unwrap_or(0) as u8, Some(o.color));
        if li.is_some_and(|l| design.layers[l].action == Action::Skip) {
            color = color.gamma_multiply(0.25);
        }
        for pl in o.outline() {
            painter.add(Shape::line(pl.into_iter().map(to_screen).collect(), Stroke::new(1.4, color)));
        }
    }
}

/// An opened job file, starting at the job anchor.
fn draw_job(painter: &egui::Painter, scene: &Scene, job: &Job, to_screen: &impl Fn(Pt) -> Pos2) {
    let (ax, ay) = scene.job_anchor;
    let at = |(x, y): (i64, i64)| to_screen(scene.to_view((ax + x as f64 / 1000.0, ay + y as f64 / 1000.0)));
    for s in &job.toolpath.segments {
        let (a, b) = (at(s.from), at(s.to));
        if s.cut {
            let layer = s.layer.unwrap_or(0);
            let rgb = job.toolpath.layers.get(&layer).and_then(|l| l.color);
            painter.line_segment([a, b], Stroke::new(1.5, theme::layer_color(layer, rgb)));
        } else if scene.show_travel {
            painter.line_segment([a, b], Stroke::new(0.75, theme::p().muted.gamma_multiply(0.5)));
        }
    }
}

fn draw_head(painter: &egui::Painter, bed_rect: Rect, p: Pos2, head: Pt) {
    let pal = theme::p();
    let guide = Stroke::new(1.0, pal.accent.gamma_multiply(0.25));
    painter.extend(Shape::dashed_line(&[Pos2::new(bed_rect.min.x, p.y), Pos2::new(bed_rect.max.x, p.y)], guide, 6.0, 4.0));
    painter.extend(Shape::dashed_line(&[Pos2::new(p.x, bed_rect.min.y), Pos2::new(p.x, bed_rect.max.y)], guide, 6.0, 4.0));
    painter.circle_filled(p, 14.0, pal.accent.gamma_multiply(0.12));
    painter.circle_stroke(p, 9.0, Stroke::new(1.5, pal.accent));
    painter.circle_filled(p, 3.0, pal.accent);
    let label = t!("head-label", x = format!("{:.1}", head.0), y = format!("{:.1}", head.1));
    let galley = painter.layout_no_wrap(label, theme::medium(11.0), pal.text);
    let pos = p + Vec2::new(14.0, -14.0 - galley.size().y);
    let bg = Rect::from_min_size(pos, galley.size()).expand2(Vec2::new(7.0, 4.0));
    painter.rect_filled(bg, 6.0, pal.panel);
    painter.rect_stroke(bg, 6.0, Stroke::new(1.0, pal.border), StrokeKind::Inside);
    painter.galley(pos, galley, pal.text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_from_centre_keeps_the_middle() {
        let from = ((0.0, 0.0), (10.0, 10.0));
        let b = resize_bounds(from, Handle { hx: 1, hy: 0 }, (12.0, 5.0), false, true);
        assert_eq!(b, ((-2.0, 0.0), (12.0, 10.0)));
        let b = resize_bounds(from, Handle { hx: 1, hy: 1 }, (15.0, 12.0), true, false);
        assert_eq!(b, ((0.0, 0.0), (15.0, 15.0)));
    }

    #[test]
    fn rotating_around_a_point() {
        let p = rotate_point((10.0, 0.0), (0.0, 0.0), 90.0);
        assert!((p.0).abs() < 1e-9 && (p.1 - 10.0).abs() < 1e-9);
    }
}
