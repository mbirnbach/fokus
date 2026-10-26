//! The canvas: the laser bed with the design on it, drawing tools,
//! selection, moving and resizing.
//!
//! Coordinates: design mm are bed mm as seen from the front (x right, y down,
//! origin top-left). Controller mm are what the Ruida reports; on our machine
//! its origin is top-right, so x is mirrored. Screen px = mm * zoom + offset.

use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Vec2};

use crate::design::{Action, Anchor, Bounds, Design, History, Kind, Object, Pt, DEFAULT_TEXT_HEIGHT, MIN_SIZE, PREVIEW_LINE_INTERVAL_MM};
use crate::job::Job;
use crate::materials::{Material, Operation};
use crate::t;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Rect,
    Ellipse,
    Polygon,
    Line,
    Text,
}

impl Tool {
    pub const ALL: [Tool; 6] = [Tool::Select, Tool::Rect, Tool::Ellipse, Tool::Polygon, Tool::Line, Tool::Text];

    pub fn label(self) -> String {
        match self {
            Tool::Select => t!("tool-select"),
            Tool::Rect => t!("tool-rect"),
            Tool::Ellipse => t!("tool-ellipse"),
            Tool::Polygon => t!("tool-polygon"),
            Tool::Line => t!("tool-line"),
            Tool::Text => t!("tool-text"),
        }
    }

    pub fn key(self) -> Key {
        match self {
            Tool::Select => Key::V,
            Tool::Rect => Key::R,
            Tool::Ellipse => Key::E,
            Tool::Polygon => Key::P,
            Tool::Line => Key::L,
            Tool::Text => Key::T,
        }
    }

    pub fn hint(self) -> String {
        match self {
            Tool::Select => t!("tool-select-hint"),
            Tool::Rect => t!("tool-rect-hint"),
            Tool::Ellipse => t!("tool-ellipse-hint"),
            Tool::Polygon => t!("tool-polygon-hint"),
            Tool::Line => t!("tool-line-hint"),
            Tool::Text => t!("tool-text-hint"),
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

enum Drag {
    None,
    Pan,
    Move { started: bool },
    Resize { handle: Handle, from: Bounds, originals: Vec<(usize, Object)> },
    Marquee { start: Pt },
    Create { start: Pt },
}

pub struct Canvas {
    /// Pixels per mm.
    zoom: f32,
    offset: Vec2,
    pending_zoom: Option<ZoomTarget>,
    last_rect: Option<Rect>,
    drag: Drag,
    /// Points of a line being drawn.
    line: Vec<Pt>,
    /// Pointer position in controller mm, if over the bed.
    pub hover: Option<Pt>,
    /// A text object was just placed; the app focuses its text field.
    pub new_text: Option<usize>,
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas { zoom: 1.0, offset: Vec2::ZERO, pending_zoom: Some(ZoomTarget::Bed), last_rect: None, drag: Drag::None, line: Vec::new(), hover: None, new_text: None }
    }
}

const HANDLE_PX: f32 = 7.0;

fn rect_mm(a: Pt, b: Pt) -> Bounds {
    ((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1)))
}

impl Canvas {
    pub fn zoom_to(&mut self, target: ZoomTarget) {
        self.pending_zoom = Some(target);
    }

    pub fn cancel_line(&mut self) {
        self.line.clear();
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

    pub fn ui(&mut self, ui: &mut egui::Ui, scene: &Scene, ed: Edit) {
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
        painter.rect_filled(bed_rect, 4.0, pal.bed);
        self.draw_grid(&painter, scene, bed_rect);
        painter.rect_stroke(bed_rect, 4.0, Stroke::new(1.0, pal.grid_major), StrokeKind::Outside);
        let home = to_screen(scene.to_view((0.0, 0.0)));
        painter.circle_filled(home, 4.0, pal.muted);

        if let Some(job) = scene.job {
            draw_job(&painter, scene, job, &to_screen);
        }

        // Interaction last: it edits the design and then draws it on top.
        self.interact(ui, &response, scene, ed, &to_mm, &to_screen);
    }

    fn interact(&mut self, ui: &egui::Ui, response: &egui::Response, scene: &Scene, ed: Edit, to_mm: &impl Fn(Pos2) -> Pt, to_screen: &impl Fn(Pt) -> Pos2) {
        let Edit { design, selection, history, tool } = ed;
        let (shift, panning_key) = ui.input(|i| (i.modifiers.shift, i.key_down(Key::Space)));
        let other_button = ui.input(|i| i.pointer.middle_down() || i.pointer.secondary_down());
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        let tol_mm = 6.0 / self.zoom as f64;

        // Panning: space + drag, or the middle / right mouse button.
        if response.drag_started() && (panning_key || other_button) {
            self.drag = Drag::Pan;
        }
        if let Drag::Pan = self.drag {
            self.offset += response.drag_delta();
            if response.drag_stopped() {
                self.drag = Drag::None;
            }
            self.paint_overlay(ui, response, scene, design, selection, *tool, to_screen, to_mm);
            return;
        }

        let sel_bounds = design.selection_bounds(selection);
        let handle_at = |p: Pos2| -> Option<Handle> {
            let ((x0, y0), (x1, y1)) = sel_bounds?;
            let a = to_screen((x0, y0));
            let b = to_screen((x1, y1));
            for hx in [-1i8, 0, 1] {
                for hy in [-1i8, 0, 1] {
                    if hx == 0 && hy == 0 {
                        continue;
                    }
                    let hp = Pos2::new(lerp3(a.x, b.x, hx), lerp3(a.y, b.y, hy));
                    if (hp - p).length() <= HANDLE_PX + 2.0 {
                        return Some(Handle { hx, hy });
                    }
                }
            }
            None
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

        // ---- drag start
        if response.drag_started() {
            // Where the button went down, not where the drag was recognised.
            let p = ui.input(|i| i.pointer.press_origin()).or(pointer).unwrap_or_default();
            let mm = to_mm(p);
            self.drag = match *tool {
                Tool::Select => {
                    if let (Some(handle), Some(from)) = (handle_at(p), sel_bounds) {
                        history.checkpoint(design);
                        let originals = selection.iter().map(|&i| (i, design.objects[i].clone())).collect();
                        Drag::Resize { handle, from, originals }
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
                Tool::Rect | Tool::Ellipse | Tool::Polygon => Drag::Create { start: mm },
                Tool::Line | Tool::Text => Drag::None,
            };
        }

        // ---- dragging
        if response.dragged() {
            let p = pointer.unwrap_or_default();
            match &mut self.drag {
                Drag::Move { started } => {
                    let d = response.drag_delta() / self.zoom;
                    if d != Vec2::ZERO {
                        if !*started {
                            history.checkpoint(design);
                            *started = true;
                        }
                        design.translate(selection, (d.x as f64, d.y as f64));
                    }
                }
                Drag::Resize { handle, from, originals } => {
                    let to = resize_bounds(*from, *handle, to_mm(p), shift || scene.lock_aspect);
                    design.resize(originals, *from, to);
                }
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
                Drag::Create { start } => {
                    let b = creation_box(start, end, shift);
                    history.checkpoint(design);
                    let i = design.add(new_shape(*tool, b, scene));
                    *selection = vec![i];
                    *tool = Tool::Select;
                }
                Drag::Move { .. } | Drag::Resize { .. } => design.touch(),
                _ => {}
            }
        }

        // ---- clicks
        if response.clicked() {
            let p = pointer.unwrap_or_default();
            let mm = to_mm(p);
            match *tool {
                Tool::Select => match hit(design, mm) {
                    Some(i) => select(selection, design, i, shift),
                    None if !shift => selection.clear(),
                    None => {}
                },
                Tool::Rect | Tool::Ellipse | Tool::Polygon => {
                    // A click without dragging: a 20 mm shape.
                    history.checkpoint(design);
                    let i = design.add(new_shape(*tool, ((mm.0, mm.1), (mm.0 + 20.0, mm.1 + 20.0)), scene));
                    *selection = vec![i];
                    *tool = Tool::Select;
                }
                Tool::Line => {
                    let closes = self.line.len() >= 3 && (to_screen(self.line[0]) - p).length() < 8.0;
                    if closes {
                        let first = self.line[0];
                        self.line.push(first);
                        self.finish_line(design, selection, history, tool, scene);
                    } else if self.line.last().is_none_or(|&q| (q.0 - mm.0).hypot(q.1 - mm.1) > 0.01) {
                        self.line.push(mm);
                    }
                }
                Tool::Text => {
                    history.checkpoint(design);
                    if let Ok(o) = Object::text(&t!("default-text"), crate::design::Font::Sans, false, mm, DEFAULT_TEXT_HEIGHT, scene.text_color) {
                        let i = design.add(o);
                        *selection = vec![i];
                        self.new_text = Some(i);
                    }
                    *tool = Tool::Select;
                }
            }
        }
        if response.double_clicked() {
            match *tool {
                Tool::Line => self.finish_line(design, selection, history, tool, scene),
                Tool::Select if design.is_empty() || selection.is_empty() => {
                    if pointer.map(to_mm).and_then(|p| hit(design, p)).is_none() {
                        self.pending_zoom = Some(ZoomTarget::Bed);
                    }
                }
                _ => {}
            }
        }
        if *tool == Tool::Line && !self.line.is_empty() {
            let (enter, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
            if enter {
                self.finish_line(design, selection, history, tool, scene);
            } else if escape {
                self.line.clear();
            }
        }

        // ---- cursor
        if let Some(p) = response.hover_pos() {
            let icon = match (&self.drag, *tool) {
                (Drag::Move { .. }, _) => CursorIcon::Grabbing,
                (Drag::Resize { handle, .. }, _) => handle_cursor(*handle),
                (_, Tool::Select) => match handle_at(p) {
                    Some(h) => handle_cursor(h),
                    None if hit(design, to_mm(p)).is_some() => CursorIcon::Grab,
                    None => CursorIcon::Default,
                },
                (_, Tool::Text) => CursorIcon::Text,
                _ => CursorIcon::Crosshair,
            };
            ui.ctx().set_cursor_icon(icon);
        }

        self.paint_overlay(ui, response, scene, design, selection, *tool, to_screen, to_mm);
    }

    fn finish_line(&mut self, design: &mut Design, selection: &mut Vec<usize>, history: &mut History, tool: &mut Tool, scene: &Scene) {
        let mut points = std::mem::take(&mut self.line);
        points.dedup_by(|a, b| (a.0 - b.0).hypot(a.1 - b.1) < 0.01);
        if points.len() >= 2 {
            history.checkpoint(design);
            let i = design.add(Object::polyline(points, scene.draw_color));
            *selection = vec![i];
        }
        *tool = Tool::Select;
    }

    /// Everything that depends on the (possibly just edited) design.
    #[allow(clippy::too_many_arguments)]
    fn paint_overlay(&self, ui: &egui::Ui, response: &egui::Response, scene: &Scene, design: &Design, selection: &[usize], tool: Tool, to_screen: &impl Fn(Pt) -> Pos2, to_mm: &impl Fn(Pos2) -> Pt) {
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

        // Selection: box, size label and handles.
        if let Some(((x0, y0), (x1, y1))) = design.selection_bounds(selection) {
            let r = Rect::from_two_pos(to_screen((x0, y0)), to_screen((x1, y1)));
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, pal.accent), StrokeKind::Outside);
            for &i in selection {
                if let Some(o) = design.objects.get(i) {
                    let ((a0, b0), (a1, b1)) = o.bounds();
                    let ro = Rect::from_two_pos(to_screen((a0, b0)), to_screen((a1, b1)));
                    if selection.len() > 1 {
                        dashed_rect(&painter, ro, pal.accent.gamma_multiply(0.5));
                    }
                }
            }
            if tool == Tool::Select {
                for hx in [-1i8, 0, 1] {
                    for hy in [-1i8, 0, 1] {
                        if hx == 0 && hy == 0 {
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
            painter.text(r.center_bottom() + Vec2::new(0.0, 10.0), Align2::CENTER_TOP, label, FontId::monospace(11.0), pal.accent);
        }

        // In-progress gestures.
        let pointer = response.hover_pos().or(response.interact_pointer_pos());
        match &self.drag {
            Drag::Marquee { start } => {
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
                    painter.text(to_screen((x1, y1)) + Vec2::new(8.0, 8.0), Align2::LEFT_TOP, format!("{:.1} × {:.1} mm", x1 - x0, y1 - y0), FontId::monospace(11.0), pal.accent);
                }
            }
            _ => {}
        }
        if !self.line.is_empty() {
            let mut pts: Vec<Pos2> = self.line.iter().map(|&p| to_screen(p)).collect();
            if let Some(p) = pointer {
                pts.push(p);
            }
            painter.add(Shape::line(pts, Stroke::new(1.5, pal.accent)));
            for &p in &self.line {
                painter.circle_filled(to_screen(p), 3.0, pal.accent);
            }
        }

        // The head, and where the job will start relative to the design.
        if let Some(head) = scene.head {
            let hp = to_screen(scene.to_view(head));
            let bed_rect = Rect::from_two_pos(to_screen((0.0, 0.0)), to_screen(scene.bed));
            draw_head(&painter, bed_rect, hp, head);
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
                    painter.text(Pos2::new(x, bed_rect.min.y - 6.0), Align2::CENTER_BOTTOM, format!("{ctrl_x:.0}"), FontId::monospace(10.0), pal.muted);
                }
                v += step;
            }
            let mut v = step;
            while v < bh {
                let y = bed_rect.min.y + (v * zoom) as f32;
                painter.line_segment([Pos2::new(bed_rect.min.x, y), Pos2::new(bed_rect.max.x, y)], Stroke::new(1.0, color));
                if labels {
                    painter.text(Pos2::new(bed_rect.min.x - 6.0, y), Align2::RIGHT_CENTER, format!("{v:.0}"), FontId::monospace(10.0), pal.muted);
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

/// The smallest object whose box contains the point (within `tol` mm), so
/// inner shapes can be picked out of larger ones.
fn hit_test(design: &Design, p: Pt, tol: f64) -> Option<usize> {
    let area = |o: &Object| {
        let ((x0, y0), (x1, y1)) = o.bounds();
        (x1 - x0).max(tol) * (y1 - y0).max(tol)
    };
    design
        .objects
        .iter()
        .enumerate()
        .filter(|(_, o)| {
            let ((x0, y0), (x1, y1)) = o.bounds();
            p.0 >= x0 - tol && p.0 <= x1 + tol && p.1 >= y0 - tol && p.1 <= y1 + tol
        })
        .min_by(|(_, a), (_, b)| area(a).total_cmp(&area(b)))
        .map(|(i, _)| i)
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

/// New bounds when dragging `handle` of `from` to `p`.
fn resize_bounds(from: Bounds, h: Handle, p: Pt, keep_aspect: bool) -> Bounds {
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
    if keep_aspect && h.hx != 0 && h.hy != 0 {
        let ((fx0, fy0), (fx1, fy1)) = from;
        let (fw, fh) = ((fx1 - fx0).max(1e-9), (fy1 - fy0).max(1e-9));
        let s = ((x1 - x0) / fw).max((y1 - y0) / fh);
        let (w, h2) = (fw * s, fh * s);
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
        let outlines = design.layer_outlines_um(layer.color);
        let Some((y0, y1)) = outlines.iter().flatten().fold(None, |acc: Option<(i32, i32)>, p| Some(acc.map_or((p.1, p.1), |(lo, hi)| (lo.min(p.1), hi.max(p.1))))) else {
            continue;
        };
        let mut step = ((interval_mm as f64).max(2.5 / zoom as f64) * 1000.0) as i32;
        step = step.max((y1 - y0) / MAX_PREVIEW_LINES).max(1);
        let color = theme::layer_color(li as u8, Some(layer.color)).gamma_multiply(0.55);
        for line in ruida::fill::scan(&outlines, step) {
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
    let galley = painter.layout_no_wrap(label, FontId::monospace(11.0), pal.text);
    let pos = p + Vec2::new(14.0, -14.0 - galley.size().y);
    let bg = Rect::from_min_size(pos, galley.size()).expand2(Vec2::new(6.0, 3.0));
    painter.rect_filled(bg, 4.0, pal.panel);
    painter.rect_stroke(bg, 4.0, Stroke::new(1.0, pal.border), StrokeKind::Inside);
    painter.galley(pos, galley, pal.text);
}
