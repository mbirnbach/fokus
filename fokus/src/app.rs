//! The main window, laid out like common laser software:
//! top bar (file, undo, machine), properties bar for the selection,
//! tool rail on the left, the bed in the middle, material and layers on
//! the right with the start button, and a colour palette at the bottom.
//! Materials and settings open in windows of their own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Event, Frame, Key, Layout, Margin, Modifiers, PopupCloseBehavior, Rect, RichText, Sense, Stroke, Vec2};
use ruida::serial::{list_ports, PortInfo};

use crate::canvas::{Canvas, Edit, Scene, Tool, ZoomTarget};
use crate::design::{Action, Anchor, Design, History, Kind, LayerProblem, Object, Pt, TextStyle, DEFAULT_TEXT_SIZE, MIN_SIZE};
use crate::fonts;
use crate::icons::{self, Icon};
use crate::job::Job;
use crate::machine::{Conn, JobKind, JobPhase, Machine, Snapshot};
use crate::materials::{Library, Material, OpSettings, Operation};
use crate::project::{self, Project};
use crate::t;
use crate::theme::{self, Weight};
use crate::widgets::{self, Btn};

const DEFAULT_BED: (f64, f64) = (1000.0, 600.0);
const PORT_SCAN_INTERVAL: Duration = Duration::from_secs(2);

/// Layer colours offered in the palette (LightBurn-like).
const PALETTE: [[u8; 3]; 10] = [
    [0, 0, 0],
    [224, 49, 49],
    [25, 113, 194],
    [47, 158, 68],
    [240, 140, 0],
    [156, 54, 181],
    [12, 133, 153],
    [230, 73, 128],
    [102, 117, 127],
    [130, 90, 50],
];

/// The preparation checklist shown before a job can start.
const CHECKLIST: usize = 4;

/// Something that would throw away unsaved changes, held while we ask.
#[derive(Debug, Clone)]
enum Pending {
    New,
    Open(PathBuf),
    Quit,
}

/// What was last saved (or opened), to tell whether there are unsaved changes.
struct Saved {
    design: Design,
    material: Option<String>,
    anchor: Anchor,
}

pub struct FokusApp {
    machine: Machine,
    ports: Vec<PortInfo>,
    ports_checked: Instant,
    port: Option<String>,
    auto_connect: bool,
    /// The port auto-connect last tried, so a failure isn't retried in a loop.
    auto_tried: Option<String>,

    canvas: Canvas,
    tool: Tool,
    design: Design,
    selection: Vec<usize>,
    history: History,
    /// Objects copied with Copy / Cut.
    clipboard: Vec<Object>,
    draw_color: [u8; 3],
    polygon_sides: u32,
    lock_aspect: bool,
    /// Font and size (one em, mm) for new text.
    text_style: TextStyle,
    text_size: f64,
    font_filter: String,
    /// Where on the design the laser head is when the job starts.
    anchor: Anchor,
    /// A finished .rd job opened for preview.
    job: Option<Job>,
    file_error: Option<String>,

    library: Library,
    /// Name of the material chosen for this job.
    material: Option<String>,

    origin_right: bool,
    show_travel: bool,
    show_settings: bool,
    show_library: bool,
    library_selected: usize,
    library_filter: String,

    /// Engraving stats (line width total, line count) per (layer colour,
    /// line spacing µm, design revision); filling is too slow for every frame.
    length_cache: HashMap<([u8; 3], i32, u64), (f64, usize)>,
    prepare_open: bool,
    checklist: [bool; CHECKLIST],
    /// Result of the last save: (ok, message).
    save_msg: Option<(bool, String)>,

    /// The open project file, if the design has been saved or opened.
    project_path: Option<PathBuf>,
    saved: Saved,
    pending: Option<Pending>,
    /// The user confirmed closing with unsaved changes.
    allow_close: bool,
    window_title: String,
    settings: crate::settings::Settings,
}

impl FokusApp {
    pub fn new(open: Option<PathBuf>) -> Self {
        let mut app = FokusApp {
            machine: Machine::new(),
            ports: Vec::new(),
            ports_checked: Instant::now(),
            port: None,
            auto_connect: true,
            auto_tried: None,
            canvas: Canvas::default(),
            tool: Tool::Select,
            design: Design::default(),
            selection: Vec::new(),
            history: History::default(),
            clipboard: Vec::new(),
            draw_color: PALETTE[0],
            polygon_sides: 6,
            lock_aspect: true,
            text_style: TextStyle::default(),
            text_size: DEFAULT_TEXT_SIZE,
            font_filter: String::new(),
            anchor: Anchor::TOP_RIGHT,
            job: None,
            file_error: None,
            library: Library::load(),
            material: None,
            // Our machine: origin top-right (LightBurn "MirrorX").
            origin_right: true,
            show_travel: false,
            show_settings: false,
            show_library: false,
            library_selected: 0,
            library_filter: String::new(),
            length_cache: HashMap::new(),
            prepare_open: false,
            checklist: [false; CHECKLIST],
            save_msg: None,
            project_path: None,
            saved: Saved { design: Design::default(), material: None, anchor: Anchor::TOP_RIGHT },
            pending: None,
            allow_close: false,
            window_title: String::new(),
            settings: crate::settings::Settings::load(),
        };
        app.refresh_ports();
        if let Some(path) = open {
            app.open_file(&path, None);
        }
        app
    }

    fn refresh_ports(&mut self) {
        self.ports_checked = Instant::now();
        self.ports = list_ports().unwrap_or_default();
        let still_there = self.port.as_ref().is_some_and(|p| self.ports.iter().any(|i| &i.name == p));
        if !still_there {
            self.port = self.ports.iter().find(|p| p.looks_like_ruida).map(|p| p.name.clone());
        }
    }

    fn material(&self) -> Option<&Material> {
        let name = self.material.as_ref()?;
        self.library.materials.iter().find(|m| &m.name == name)
    }

    fn bed(&self, snap: &Snapshot) -> (f64, f64) {
        snap.status.as_ref().map(|s| s.bed).filter(|b| b.0 > 0.0 && b.1 > 0.0).unwrap_or(DEFAULT_BED)
    }

    /// Controller mm → design mm.
    fn to_view(&self, bed: (f64, f64), (x, y): Pt) -> Pt {
        if self.origin_right {
            (bed.0 - x, y)
        } else {
            (x, y)
        }
    }

    /// Open an SVG (added to the design) or an .rd job (preview only). New
    /// designs go to the head if known, else near the origin corner.
    fn open_file(&mut self, path: &Path, snap: Option<&Snapshot>) {
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        self.file_error = None;
        match ext.as_str() {
            "svg" => {
                let bed = snap.map(|s| self.bed(s)).unwrap_or(DEFAULT_BED);
                let head = snap.and_then(|s| s.status.as_ref()).map(|s| s.position);
                let corner = match (head, self.design.bounds()) {
                    // Next to what's already there.
                    (_, Some(((x0, y0), _))) => (x0 - 5.0, y0),
                    (Some(h), None) => self.to_view(bed, h),
                    (None, None) => self.to_view(bed, (10.0, 10.0)),
                };
                self.history.checkpoint(&self.design);
                match self.design.import_svg(path, corner) {
                    Ok(new) => {
                        self.job = None;
                        self.selection = new;
                        self.canvas.zoom_to(ZoomTarget::Design);
                    }
                    Err(e) => self.file_error = Some(e),
                }
            }
            "fokus" => self.request(Pending::Open(path.to_path_buf())),
            "rd" if !self.allow_discard() => self.request(Pending::Open(path.to_path_buf())),
            "rd" => match Job::load(path) {
                Ok(job) => {
                    self.job = Some(job);
                    self.history.checkpoint(&self.design);
                    self.design = Design::default();
                    self.selection.clear();
                    self.canvas.zoom_to(ZoomTarget::Design);
                }
                Err(e) => self.file_error = Some(e),
            },
            _ => self.file_error = Some(t!("err-unknown-file")),
        }
    }

    fn pick_file(&mut self, snap: Option<&Snapshot>) {
        if let Some(path) = rfd::FileDialog::new().add_filter(t!("filter-all"), &[project::EXTENSION, "svg", "rd"]).pick_file() {
            self.open_file(&path, snap);
        }
    }

    // ------------------------------------------------------------ projects

    fn project_name(&self) -> String {
        self.project_path.as_ref().and_then(|p| p.file_stem()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| t!("untitled"))
    }

    fn is_dirty(&self) -> bool {
        !self.design.same_content(&self.saved.design) || self.material != self.saved.material || self.anchor != self.saved.anchor
    }

    /// True when nothing unsaved would be lost.
    fn allow_discard(&self) -> bool {
        !self.is_dirty() || self.design.is_empty()
    }

    fn mark_saved(&mut self) {
        self.saved = Saved { design: self.design.clone(), material: self.material.clone(), anchor: self.anchor };
    }

    /// Do `action` now, or ask first if it would lose unsaved changes.
    fn request(&mut self, action: Pending) {
        if self.allow_discard() {
            self.perform(action);
        } else {
            self.pending = Some(action);
        }
    }

    fn perform(&mut self, action: Pending) {
        match action {
            Pending::New => {
                self.design = Design::default();
                self.history = History::default();
                self.selection.clear();
                self.job = None;
                self.project_path = None;
                self.material = None;
                self.anchor = Anchor::TOP_RIGHT;
                self.mark_saved();
            }
            Pending::Open(path) => {
                if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rd")) {
                    // A job file replaces the design without being a project.
                    self.saved = Saved { design: Design::default(), material: self.material.clone(), anchor: self.anchor };
                    self.design = Design::default();
                    self.open_file(&path, None);
                    return;
                }
                self.open_project(&path);
            }
            Pending::Quit => {
                self.allow_close = true;
            }
        }
    }

    fn open_project(&mut self, path: &Path) {
        match Project::load(path) {
            Ok(p) => {
                self.design = p.design;
                self.history = History::default();
                self.selection.clear();
                self.job = None;
                self.anchor = p.anchor;
                self.file_error = None;
                // The material is stored by name; it may not exist on this computer.
                self.material = p.material.filter(|name| {
                    let known = self.library.materials.iter().any(|m| &m.name == name);
                    if !known {
                        self.file_error = Some(t!("err-material-missing", name = name.clone()));
                    }
                    known
                });
                self.project_path = Some(path.to_path_buf());
                self.mark_saved();
                self.canvas.zoom_to(ZoomTarget::Design);
            }
            Err(e) => self.file_error = Some(e),
        }
    }

    /// Save the project; asks for a file name the first time or with `save_as`.
    /// Returns whether it was saved.
    fn save_project(&mut self, save_as: bool) -> bool {
        let path = match (&self.project_path, save_as) {
            (Some(p), false) => p.clone(),
            _ => {
                let name = format!("{}.{}", self.project_name(), project::EXTENSION);
                match rfd::FileDialog::new().add_filter(t!("filter-project"), &[project::EXTENSION]).set_file_name(name).save_file() {
                    Some(p) => p.with_extension(project::EXTENSION),
                    None => return false,
                }
            }
        };
        let p = Project::new(self.design.clone(), self.material.clone(), self.anchor);
        match p.save(&path) {
            Ok(()) => {
                self.project_path = Some(path);
                self.mark_saved();
                self.file_error = None;
                true
            }
            Err(e) => {
                self.file_error = Some(e);
                false
            }
        }
    }

    fn unsaved_modal(&mut self, ctx: &egui::Context) {
        let Some(action) = self.pending.clone() else { return };
        let pal = theme::p();
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::unique("unsaved")).frame(modal_frame()).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(RichText::new(t!("unsaved-title", name = self.project_name())).size(18.0).semibold());
            ui.add_space(2.0);
            ui.label(RichText::new(t!("unsaved-text")).color(pal.muted));
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if Btn::new(t!("dont-save")).ghost().show(ui).clicked() {
                    choice = Some(false);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if Btn::new(t!("save")).icon(Icon::Save).primary().show(ui).clicked() {
                        choice = Some(true);
                    }
                    if Btn::new(t!("cancel")).show(ui).clicked() {
                        self.pending = None;
                    }
                });
            });
        });
        if modal.should_close() {
            self.pending = None;
        }
        match choice {
            Some(true) => {
                if self.save_project(false) {
                    self.pending = None;
                    self.perform(action);
                }
            }
            Some(false) => {
                self.pending = None;
                self.perform(action);
            }
            None => {}
        }
    }

    /// Keep the window title in sync: "name (unsaved) — Fokus" while unsaved.
    fn update_title(&mut self, ctx: &egui::Context) {
        let title = if self.is_dirty() && !self.design.is_empty() {
            t!("title-unsaved", name = self.project_name())
        } else {
            t!("title-saved", name = self.project_name())
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    /// Ask before closing the window with unsaved changes.
    fn guard_close(&mut self, ctx: &egui::Context) {
        if self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_discard() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending = Some(Pending::Quit);
            // Show the question (bring a hidden or minimised window back).
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
        }
    }

    /// Send a job to the laser; it starts at the head right away. A dry run
    /// sends the same moves with the laser off; a frame only goes around
    /// the design, laser off, and needs no material.
    fn start_job(&mut self, kind: JobKind) {
        let estimate: f64 = match kind {
            JobKind::Frame => self.design.frame_time_s(),
            _ => self.layer_estimates().iter().flatten().map(|(_, _, t)| *t).sum(),
        };
        let job = match kind {
            JobKind::Frame => self.design.frame_job(self.origin_right, self.anchor),
            _ => self.design.to_job(self.material(), self.origin_right, self.anchor),
        };
        let result = job
            .map(|mut job| {
                if kind == JobKind::DryRun {
                    for l in &mut job {
                        l.min_power = 0.0;
                        l.max_power = 0.0;
                        l.air_assist = false;
                    }
                }
                job
            })
            .and_then(|job| ruida::writer::write_plain(&job).map_err(|e| e.to_string()))
            .and_then(|plain| self.machine.start_job(plain, estimate, kind));
        match result {
            Ok(()) => {
                // Framing from the checklist keeps it open: it's part of checking.
                if kind != JobKind::Frame {
                    self.prepare_open = false;
                }
                self.save_msg = None;
            }
            Err(e) => self.save_msg = Some((false, e)),
        }
    }

    fn save_job(&mut self) {
        let bytes = match self.design.to_job(self.material(), self.origin_right, self.anchor).and_then(|job| ruida::writer::write_file(&job).map_err(|e| e.to_string())) {
            Ok(b) => b,
            Err(e) => {
                self.save_msg = Some((false, e));
                return;
            }
        };
        let name = self.design.objects.first().map(|o| o.name.clone()).unwrap_or_else(|| t!("job-file-default-name"));
        let Some(path) = rfd::FileDialog::new().add_filter(t!("filter-rd"), &["rd"]).set_file_name(format!("{name}.rd")).save_file() else {
            return;
        };
        self.save_msg = Some(match std::fs::write(&path, &bytes) {
            Ok(()) => (true, t!("saved-file", file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())),
            Err(e) => (false, t!("err-save", error = e.to_string())),
        });
        self.prepare_open = false;
    }

    /// Text goes to an engraving layer: the current colour if it engraves
    /// (or is new), else an existing engrave layer, else red.
    fn text_color(&self) -> [u8; 3] {
        let engraves = |c: [u8; 3]| self.design.layers.iter().find(|l| l.color == c).is_none_or(|l| l.action == Action::Do(Operation::Engrave));
        if engraves(self.draw_color) && self.draw_color != PALETTE[0] {
            return self.draw_color;
        }
        self.design.layers.iter().find(|l| l.action == Action::Do(Operation::Engrave)).map(|l| l.color).unwrap_or(PALETTE[1])
    }

    /// Move the design on screen so its start point is where the head is.
    /// (The job itself always starts at the head; this only lines up the view.)
    fn place_at_head(&mut self, bed: (f64, f64), head: Pt) {
        let Some(b) = self.design.bounds() else { return };
        let (ax, ay) = self.anchor.point(b);
        let (hx, hy) = self.to_view(bed, head);
        self.history.checkpoint(&self.design);
        let all: Vec<usize> = (0..self.design.objects.len()).collect();
        self.design.translate(&all, (hx - ax, hy - ay));
        self.design.touch();
    }

    /// Distance between the head and the design's start point on screen, mm.
    fn head_offset(&self, bed: (f64, f64), head: Pt) -> Option<f64> {
        let (ax, ay) = self.anchor.point(self.design.bounds()?);
        let (hx, hy) = self.to_view(bed, head);
        Some((hx - ax).hypot(hy - ay))
    }

    /// Room the design needs on the bed around the head, as (left, right,
    /// up, down) in mm, for the "does it fit from here" check.
    fn extent_from_anchor(&self) -> Option<(f64, f64, f64, f64)> {
        let b @ ((x0, y0), (x1, y1)) = self.design.bounds()?;
        let (ax, ay) = self.anchor.point(b);
        Some((ax - x0, x1 - ax, ay - y0, y1 - ay))
    }

    /// Expected time per layer (all passes, seconds) with the settings it would use.
    fn layer_estimates(&mut self) -> Vec<Option<(Operation, OpSettings, f64)>> {
        let material = self.material.as_ref().and_then(|n| self.library.materials.iter().find(|m| &m.name == n));
        let design = &self.design;
        let cache = &mut self.length_cache;
        cache.retain(|k, _| k.2 == design.revision);
        design
            .layers
            .iter()
            .map(|l| {
                let (op, s) = l.resolve(material).ok().flatten()?;
                let time = match op {
                    Operation::Engrave => {
                        let key = (l.color, (s.line_interval_mm * 1000.0).round() as i32, design.revision);
                        let (extent, lines) = *cache.entry(key).or_insert_with(|| design.engrave_stats(l.color, s.line_interval_mm));
                        Design::engrave_time_s(extent, lines, s.speed) * s.passes as f64
                    }
                    _ => design.layer_time_s(l.color, op, &s),
                };
                Some((op, s, time))
            })
            .collect()
    }

    // ------------------------------------------------------------ editing

    fn edit(&mut self, f: impl FnOnce(&mut Design, &mut Vec<usize>)) {
        self.history.checkpoint(&self.design);
        f(&mut self.design, &mut self.selection);
        self.design.touch();
    }

    fn set_tool(&mut self, tool: Tool) {
        self.canvas.finish_text(&mut self.design, &mut self.selection);
        self.canvas.cancel_gesture();
        self.tool = tool;
    }

    fn delete_selection(&mut self) {
        if self.tool == Tool::Nodes && self.canvas.delete_nodes(&mut self.design, &mut self.selection, &mut self.history) {
            return;
        }
        if self.selection.is_empty() {
            return;
        }
        let sel = std::mem::take(&mut self.selection);
        self.edit(|d, _| d.remove(&sel));
    }

    fn copy(&mut self) {
        if !self.selection.is_empty() {
            self.clipboard = self.selection.iter().map(|&i| self.design.objects[i].clone()).collect();
        }
    }

    fn cut(&mut self) {
        self.copy();
        self.delete_selection();
    }

    fn paste(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        self.history.checkpoint(&self.design);
        let new = self.design.paste(&self.clipboard, (5.0, 5.0));
        // Pasting again steps further.
        for o in &mut self.clipboard {
            o.x += 5.0;
            o.y += 5.0;
        }
        self.selection = new;
        self.set_tool(Tool::Select);
    }

    fn duplicate(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let sel = self.selection.clone();
        self.history.checkpoint(&self.design);
        self.selection = self.design.duplicate(&sel);
    }

    fn reorder(&mut self, to_front: bool) {
        if self.selection.is_empty() {
            return;
        }
        let sel = self.selection.clone();
        self.history.checkpoint(&self.design);
        self.selection = self.design.reorder(&sel, to_front);
    }

    fn rotate_selection(&mut self, deg: f64) {
        let sel = self.selection.clone();
        self.edit(|d, _| d.rotate(&sel, deg));
    }

    fn flip(&mut self, horizontal: bool) {
        let sel = self.selection.clone();
        self.edit(|d, _| {
            for &i in &sel {
                let o = &mut d.objects[i];
                if horizontal {
                    o.flip_x = !o.flip_x;
                } else {
                    o.flip_y = !o.flip_y;
                }
            }
        });
    }

    fn convert_to_path(&mut self) {
        let sel = self.selection.clone();
        self.edit(|d, _| {
            for &i in &sel {
                d.objects[i].make_path();
            }
        });
    }

    fn set_color(&mut self, c: [u8; 3]) {
        self.draw_color = c;
        if !self.selection.is_empty() {
            let sel = self.selection.clone();
            self.edit(|d, _| {
                for &i in &sel {
                    d.objects[i].color = c;
                }
            });
        }
    }

    fn group_selection(&mut self, group: bool) {
        let sel = self.selection.clone();
        if group {
            self.edit(|d, _| d.group(&sel));
        } else {
            self.edit(|d, _| d.ungroup(&sel));
        }
    }

    fn is_grouped(&self) -> bool {
        let sel = &self.selection;
        sel.len() > 1 && sel.iter().all(|&i| self.design.objects[i].group.is_some() && self.design.objects[i].group == self.design.objects[sel[0]].group)
    }

    fn undo(&mut self) {
        self.canvas.finish_text(&mut self.design, &mut self.selection);
        if self.history.undo(&mut self.design) {
            self.selection.clear();
        }
    }

    fn redo(&mut self) {
        if self.history.redo(&mut self.design) {
            self.selection.clear();
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.prepare_open {
            return;
        }
        // While text is typed on the canvas it has the keyboard, but undo
        // still works (it ends the typing and undoes it).
        if self.canvas.editing_text().is_some() {
            let (redo, undo) = ctx.input_mut(|i| (i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z), i.consume_key(Modifiers::COMMAND, Key::Z)));
            if redo {
                self.redo();
            } else if undo {
                self.undo();
            }
            return;
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        let (save_as, save, open, new) = ctx.input_mut(|i| {
            (
                i.consume_key(cmd_shift, Key::S),
                i.consume_key(Modifiers::COMMAND, Key::S),
                i.consume_key(Modifiers::COMMAND, Key::O),
                i.consume_key(Modifiers::COMMAND, Key::N),
            )
        });
        if save_as || save {
            self.save_project(save_as);
        }
        if open {
            self.pick_file(None);
        }
        if new {
            self.request(Pending::New);
        }
        let (redo, undo, dup, ungroup, group, all, front, back) = ctx.input_mut(|i| {
            (
                i.consume_key(cmd_shift, Key::Z),
                i.consume_key(Modifiers::COMMAND, Key::Z),
                i.consume_key(Modifiers::COMMAND, Key::D),
                i.consume_key(cmd_shift, Key::G),
                i.consume_key(Modifiers::COMMAND, Key::G),
                i.consume_key(Modifiers::COMMAND, Key::A),
                i.consume_key(Modifiers::COMMAND, Key::CloseBracket),
                i.consume_key(Modifiers::COMMAND, Key::OpenBracket),
            )
        });
        if redo {
            self.redo();
        } else if undo {
            self.undo();
        }
        if dup {
            self.duplicate();
        }
        if ungroup {
            self.group_selection(false);
        } else if group {
            self.group_selection(true);
        }
        if all {
            self.selection = (0..self.design.objects.len()).collect();
        }
        if front {
            self.reorder(true);
        } else if back {
            self.reorder(false);
        }
        // Copy, cut and paste arrive as events (the system turns the keys into them).
        let (copy, cut, paste) = ctx.input(|i| {
            (
                i.events.iter().any(|e| matches!(e, Event::Copy)),
                i.events.iter().any(|e| matches!(e, Event::Cut)),
                i.events.iter().any(|e| matches!(e, Event::Paste(_))),
            )
        });
        if copy {
            self.copy();
        }
        if cut {
            self.cut();
        }
        if paste {
            self.paste();
        }

        // The pen uses Enter, Escape and Backspace itself.
        if self.tool == Tool::Pen {
            return;
        }
        let (plain, delete, escape, nudge) = ctx.input(|i| {
            let plain = !i.modifiers.command && !i.modifiers.alt;
            let step = if i.modifiers.shift { 10.0 } else { 1.0 };
            let mut nudge = (0.0, 0.0);
            if i.key_pressed(Key::ArrowLeft) {
                nudge.0 -= step;
            }
            if i.key_pressed(Key::ArrowRight) {
                nudge.0 += step;
            }
            if i.key_pressed(Key::ArrowUp) {
                nudge.1 -= step;
            }
            if i.key_pressed(Key::ArrowDown) {
                nudge.1 += step;
            }
            (plain, i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace), i.key_pressed(Key::Escape), nudge)
        });
        if plain {
            for tool in Tool::ALL {
                if ctx.input(|i| i.key_pressed(tool.key())) {
                    self.set_tool(tool);
                }
            }
        }
        if delete {
            self.delete_selection();
        }
        if escape {
            if self.tool == Tool::Nodes && !self.canvas.nodes.is_empty() {
                self.canvas.nodes.clear();
            } else if self.tool != Tool::Select {
                self.set_tool(Tool::Select);
            } else {
                self.selection.clear();
            }
        }
        if nudge != (0.0, 0.0) && !self.selection.is_empty() {
            let sel = self.selection.clone();
            self.edit(|d, _| d.translate(&sel, nudge));
        }
    }
}

impl eframe::App for FokusApp {
    /// Runs every frame, also while the window is hidden or covered (when
    /// `ui` is skipped), so a close request is always caught here.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.guard_close(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        theme::set_dark(ui.visuals().dark_mode);
        let pal = theme::p();
        let snap = self.machine.snapshot();
        self.watch_ports(ui.ctx(), &snap);

        let dropped = ui.ctx().input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(path) = dropped {
            self.open_file(&path, Some(&snap));
        }
        self.shortcuts(ui.ctx());
        self.update_title(ui.ctx());
        self.selection.retain(|&i| i < self.design.objects.len());

        egui::Panel::top("top")
            .exact_size(54.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::symmetric(14, 8)).stroke(Stroke::new(1.0, pal.border)))
            .show(ui, |ui| self.top_bar(ui, &snap));

        egui::Panel::top("properties")
            .exact_size(48.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::symmetric(14, 6)).stroke(Stroke::new(1.0, pal.border)))
            .show(ui, |ui| self.properties_bar(ui, &snap));

        egui::Panel::bottom("palette")
            .exact_size(44.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::symmetric(14, 6)).stroke(Stroke::new(1.0, pal.border)))
            .show(ui, |ui| self.palette_bar(ui));

        egui::Panel::left("tools")
            .exact_size(56.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::symmetric(7, 10)).stroke(Stroke::new(1.0, pal.border)))
            .show(ui, |ui| self.tool_rail(ui));

        egui::Panel::right("job")
            .exact_size(344.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::same(16)).stroke(Stroke::new(1.0, pal.border)))
            .show(ui, |ui| self.job_panel(ui, &snap));

        let connected = snap.conn == Conn::Connected;
        let bed = self.bed(&snap);
        let material = self.material().cloned();
        let head = snap.status.as_ref().filter(|_| connected).map(|s| s.position);
        let text_style = self.text_style.clone();
        let scene = Scene {
            bed,
            origin_right: self.origin_right,
            head,
            job: self.job.as_ref(),
            job_anchor: snap.status.as_ref().map(|s| s.position).unwrap_or((0.0, 0.0)),
            show_travel: self.show_travel,
            material: material.as_ref(),
            draw_color: self.draw_color,
            anchor: self.anchor,
            text_color: self.text_color(),
            text_style: &text_style,
            text_size: self.text_size,
            polygon_sides: self.polygon_sides,
            lock_aspect: self.lock_aspect,
        };
        let has_content = !self.design.is_empty() || self.job.is_some();
        let canvas = egui::CentralPanel::no_frame().show(ui, |ui| {
            let rect = ui.max_rect();
            (self.canvas.ui(ui, &scene, Edit { design: &mut self.design, selection: &mut self.selection, history: &mut self.history, tool: &mut self.tool }), rect)
        });
        let (response, rect) = canvas.inner;
        response.context_menu(|ui| self.canvas_menu(ui));
        self.canvas_overlays(ui, rect, has_content);

        self.settings_window(ui.ctx(), &snap);
        self.library_window(ui.ctx());
        self.prepare_modal(ui.ctx(), &snap);
        self.unsaved_modal(ui.ctx());

        if connected {
            let every = if snap.job_active() { 200 } else { 500 };
            ui.ctx().request_repaint_after(Duration::from_millis(every));
        }
    }
}

// ---------------------------------------------------------------- bars

impl FokusApp {
    /// Notice the laser being plugged in or out while offline, and connect
    /// by itself when it appears (until the user disconnects).
    fn watch_ports(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        if matches!(snap.conn, Conn::Connected | Conn::Connecting) {
            return;
        }
        if self.ports_checked.elapsed() >= PORT_SCAN_INTERVAL {
            self.refresh_ports();
        }
        let laser_port = self.ports.iter().find(|p| p.looks_like_ruida).map(|p| p.name.clone());
        if let Some(port) = &laser_port {
            if self.auto_connect && snap.conn == Conn::Disconnected && self.auto_tried.as_ref() != Some(port) {
                self.auto_tried = Some(port.clone());
                self.port = Some(port.clone());
                self.machine.connect(port.clone(), ctx.clone());
            }
        }
        if laser_port.is_none() {
            self.auto_tried = None;
        }
        ctx.request_repaint_after(PORT_SCAN_INTERVAL);
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let pal = theme::p();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            widgets::logo(ui, 28.0);
            ui.add_space(4.0);
            ui.label(RichText::new("Fokus").size(16.0).semibold());
            ui.add_space(14.0);
            let ctx = ui.ctx().clone();
            if widgets::icon_button(ui, Icon::FilePlus, format!("{}  {}", t!("menu-new"), shortcut(&ctx, Modifiers::COMMAND, Key::N))).clicked() {
                self.request(Pending::New);
            }
            if widgets::icon_button(ui, Icon::FolderOpen, format!("{}  {}", t!("menu-open"), shortcut(&ctx, Modifiers::COMMAND, Key::O))).clicked() {
                self.pick_file(Some(snap));
            }
            let can_save = !self.design.is_empty();
            if ui.add_enabled_ui(can_save, |ui| widgets::icon_button(ui, Icon::Save, t!("save-hint", key = shortcut(&ctx, Modifiers::COMMAND, Key::S)))).inner.clicked() {
                self.save_project(false);
            }
            let more = widgets::icon_button(ui, Icon::Ellipsis, t!("more"));
            egui::Popup::menu(&more).width(240.0).show(|ui| {
                let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
                ui.add_enabled_ui(can_save, |ui| {
                    if widgets::menu_item(ui, Some(Icon::Save), &t!("menu-save-as"), &shortcut(&ctx, cmd_shift, Key::S)).clicked() {
                        self.save_project(true);
                    }
                });
                let can_export = can_save && self.material().is_some() && self.design.problems(self.material()).is_empty();
                ui.add_enabled_ui(can_export, |ui| {
                    if widgets::menu_item(ui, Some(Icon::FileDown), &t!("menu-export"), "").clicked() {
                        self.save_job();
                    }
                });
            });
            widgets::vdivider(ui);
            if ui.add_enabled_ui(self.history.can_undo(), |ui| widgets::icon_button(ui, Icon::Undo, format!("{}  {}", t!("undo"), shortcut(&ctx, Modifiers::COMMAND, Key::Z)))).inner.clicked() {
                self.undo();
            }
            if ui.add_enabled_ui(self.history.can_redo(), |ui| widgets::icon_button(ui, Icon::Redo, format!("{}  {}", t!("redo"), shortcut(&ctx, Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)))).inner.clicked() {
                self.redo();
            }
            widgets::vdivider(ui);
            ui.add_space(6.0);
            // The document: its name, and a dot while there are unsaved changes.
            ui.label(RichText::new(self.project_name()).medium());
            if self.is_dirty() && !self.design.is_empty() {
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 3.5, pal.muted);
                resp.on_hover_text(t!("unsaved-changes"));
            }
            if let Some(e) = self.file_error.clone() {
                ui.add_space(10.0);
                Frame::new().fill(pal.err.gamma_multiply(0.1)).corner_radius(8).inner_margin(Margin::symmetric(8, 4)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        icons::paint(ui.painter(), r, Icon::Warning, pal.err);
                        ui.label(RichText::new(e).color(pal.err).size(12.0));
                        if widgets::icon_button_sized(ui, Icon::X, false, 18.0, "").clicked() {
                            self.file_error = None;
                        }
                    });
                });
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if widgets::icon_toggle(ui, Icon::Settings, self.show_settings, t!("settings")).clicked() {
                    self.show_settings = !self.show_settings;
                }
                if Btn::new(t!("materials")).icon(Icon::Package).ghost().show(ui).clicked() {
                    self.show_library = true;
                }
                ui.add_space(4.0);
                let connected = snap.conn == Conn::Connected;
                let job_phase = snap.job.as_ref().filter(|j| j.phase.active()).map(|j| j.phase.clone());
                let (color, text) = match (&snap.conn, snap.status.as_ref()) {
                    (Conn::Connected, _) if job_phase == Some(JobPhase::Paused) => (pal.warn, t!("status-paused")),
                    (Conn::Connected, _) if job_phase.is_some() => {
                        let label = match snap.job.as_ref().map(|j| j.kind) {
                            Some(JobKind::DryRun) => t!("status-dry-run"),
                            Some(JobKind::Frame) => t!("status-framing"),
                            _ => t!("status-lasering"),
                        };
                        (pal.accent, label)
                    }
                    (Conn::Connected, Some(s)) => {
                        // The controller calls any run a job; name it after what we last sent.
                        let state = match (s.state(), snap.job.as_ref().map(|j| j.kind)) {
                            ("job running", Some(JobKind::Frame)) => t!("state-frame-running"),
                            (state, _) => state_label(state),
                        };
                        (if s.state() == "idle" { pal.ok } else { pal.warn }, format!("{state}  ·  X {:.1}  Y {:.1}", s.position.0, s.position.1))
                    }
                    (Conn::Connecting, _) => (pal.warn, t!("status-connecting")),
                    (Conn::Failed(_), _) => (pal.err, t!("status-problem")),
                    _ => (pal.muted, t!("status-offline")),
                };
                let tip = if connected { t!("status-tip-connected") } else { t!("status-tip") };
                if widgets::pill(ui, color, &text).on_hover_text(tip).clicked() {
                    self.show_settings = true;
                }
            });
        });
    }

    fn tool_rail(&mut self, ui: &mut egui::Ui) {
        let pal = theme::p();
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for tool in Tool::ALL {
                let key = shortcut(ui.ctx(), Modifiers::NONE, tool.key());
                let tip = format!("{}  ({key})\n{}", tool.label(), tool.hint());
                if widgets::icon_button_sized(ui, tool.icon(), self.tool == tool, 40.0, tip).clicked() {
                    self.set_tool(tool);
                }
                if tool == Tool::Nodes {
                    ui.add_space(4.0);
                    let (r, _) = ui.allocate_exact_size(Vec2::new(28.0, 1.0), Sense::hover());
                    ui.painter().rect_filled(r, 0.0, pal.border);
                    ui.add_space(4.0);
                }
            }
        });
    }

    fn palette_bar(&mut self, ui: &mut egui::Ui) {
        let pal = theme::p();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            widgets::field_label(ui, &t!("layer-colour"));
            ui.add_space(4.0);
            for (i, &c) in PALETTE.iter().enumerate() {
                let (rect, response) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                let color = Color32::from_rgb(c[0], c[1], c[2]);
                let current = self.draw_color == c;
                let used = self.design.layers.iter().any(|l| l.color == c);
                ui.painter().circle_filled(rect.center(), if response.hovered() { 9.5 } else { 8.5 }, color);
                if current {
                    ui.painter().circle_stroke(rect.center(), 11.5, Stroke::new(2.0, pal.accent));
                } else if color.r() as u16 + color.g() as u16 + color.b() as u16 > 600 || (pal.dark && c == [0, 0, 0]) {
                    ui.painter().circle_stroke(rect.center(), 8.5, Stroke::new(1.0, pal.border));
                }
                if used {
                    // A small dot under colours the design uses.
                    ui.painter().circle_filled(rect.center_bottom() + Vec2::new(0.0, 3.0), 1.6, pal.muted);
                }
                let tip = if self.selection.is_empty() { t!("palette-tip", colour = theme::color_name(c), layer = i + 1) } else { t!("palette-tip-selection", colour = theme::color_name(c), layer = i + 1) };
                if response.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.set_color(c);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(t!("canvas-hint")).color(pal.muted).size(12.0));
            });
        });
    }

    /// Zoom buttons and the pointer position, floating over the canvas.
    fn canvas_overlays(&mut self, ui: &mut egui::Ui, rect: Rect, has_content: bool) {
        let pal = theme::p();
        let floating = || Frame::new().fill(pal.panel).stroke(Stroke::new(1.0, pal.border)).corner_radius(10).inner_margin(Margin::same(3)).shadow(egui::Shadow { offset: [0, 2], blur: 10, spread: 0, color: Color32::from_black_alpha(if pal.dark { 80 } else { 18 }) });
        egui::Area::new(egui::Id::unique("zoom")).fixed_pos(rect.right_bottom() - Vec2::new(222.0, 52.0)).order(egui::Order::Middle).show(ui.ctx(), |ui| {
            floating().show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.horizontal(|ui| {
                    if widgets::icon_button(ui, Icon::Minus, t!("zoom-out")).clicked() {
                        self.canvas.zoom_by(1.0 / 1.25);
                    }
                    // 100 % = real size on a 96 dpi screen.
                    let percent = self.canvas.zoom() as f64 * 25.4 / 96.0 * 100.0;
                    ui.add_sized([46.0, 30.0], egui::Label::new(RichText::new(format!("{percent:.0} %")).medium().size(12.0)));
                    if widgets::icon_button(ui, Icon::Plus, t!("zoom-in")).clicked() {
                        self.canvas.zoom_by(1.25);
                    }
                    widgets::vdivider(ui);
                    if ui.add_enabled_ui(has_content, |ui| widgets::icon_button(ui, Icon::Focus, t!("zoom-design"))).inner.clicked() {
                        self.canvas.zoom_to(ZoomTarget::Design);
                    }
                    if widgets::icon_button(ui, Icon::Fullscreen, t!("zoom-bed")).clicked() {
                        self.canvas.zoom_to(ZoomTarget::Bed);
                    }
                });
            });
        });
        if let Some((x, y)) = self.canvas.hover {
            egui::Area::new(egui::Id::unique("coords")).fixed_pos(rect.left_bottom() + Vec2::new(14.0, -46.0)).order(egui::Order::Middle).interactable(false).show(ui.ctx(), |ui| {
                floating().inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
                    // Never wrap: the area is narrow near the window's edge.
                    ui.add(egui::Label::new(RichText::new(format!("X {x:.1}   Y {y:.1} mm")).color(pal.muted).size(12.0).medium()).extend());
                });
            });
        }
    }

    fn properties_bar(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let pal = theme::p();
        let bed = self.bed(snap);
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let Some(((x0, y0), (x1, y1))) = self.design.selection_bounds(&self.selection) else {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                icons::paint(ui.painter(), r, self.tool.icon(), pal.accent);
                ui.label(RichText::new(self.tool.label()).semibold());
                ui.label(RichText::new(self.tool.hint()).color(pal.muted));
                match self.tool {
                    Tool::Polygon => {
                        widgets::vdivider(ui);
                        widgets::field_label(ui, &t!("sides"));
                        ui.add_sized([56.0, 26.0], egui::DragValue::new(&mut self.polygon_sides).range(3..=24));
                    }
                    Tool::Text => {
                        widgets::vdivider(ui);
                        let mut style = self.text_style.clone();
                        let mut size = self.text_size;
                        if text_controls(ui, &mut style, &mut size, &mut self.font_filter) {
                            self.text_style = style;
                            self.text_size = size;
                        }
                    }
                    _ => {}
                }
                return;
            };
            let sel = self.selection.clone();

            // Position of the corner nearest the machine origin, in machine coordinates.
            let corner_x = if self.origin_right { bed.0 - x1 } else { x0 };
            let (w, h) = (x1 - x0, y1 - y0);
            let mut vals = [corner_x, y0, w, h];
            let mut edited = None;
            for (k, label) in ["X", "Y", "W", "H"].iter().enumerate() {
                if k == 3 {
                    let lock = if self.lock_aspect { Icon::Lock } else { Icon::LockOpen };
                    if widgets::icon_button_sized(ui, lock, self.lock_aspect, 26.0, t!("keep-proportions")).clicked() {
                        self.lock_aspect = !self.lock_aspect;
                    }
                }
                widgets::field_label(ui, label);
                let r = ui.add_sized([84.0, 26.0], egui::DragValue::new(&mut vals[k]).speed(0.1).max_decimals(2).suffix(" mm").range(if k >= 2 { 0.0..=2000.0 } else { -2000.0..=2000.0 }));
                if r.drag_started() || r.gained_focus() {
                    self.history.checkpoint(&self.design);
                }
                if r.changed() {
                    edited = Some(k);
                }
                if k == 1 {
                    ui.add_space(8.0);
                }
            }
            if let Some(k) = edited {
                let [nx, ny, mut nw, mut nh] = vals;
                // Zero sizes (empty text, straight lines) stay zero unless typed.
                if k == 2 {
                    nw = nw.max(MIN_SIZE);
                } else if k == 3 {
                    nh = nh.max(MIN_SIZE);
                }
                if self.lock_aspect && k == 2 {
                    nh = h * nw / w.max(1e-9);
                } else if self.lock_aspect && k == 3 {
                    nw = w * nh / h.max(1e-9);
                }
                // Keep the origin-side corner where it is when resizing.
                let nx1 = if self.origin_right { bed.0 - nx } else { nx + nw };
                let to = ((nx1 - nw, ny), (nx1, ny + nh));
                let originals: Vec<(usize, Object)> = sel.iter().map(|&i| (i, self.design.objects[i].clone())).collect();
                self.design.resize(&originals, ((x0, y0), (x1, y1)), to);
                self.design.touch();
            }

            widgets::vdivider(ui);
            // Per-object settings for a single object.
            if let [i] = sel[..] {
                let mut rot = self.design.objects[i].rotation;
                let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                icons::paint(ui.painter(), r, Icon::RotateCw, pal.muted);
                let r = ui.add_sized([62.0, 26.0], egui::DragValue::new(&mut rot).speed(1.0).suffix("°").range(-360.0..=360.0)).on_hover_text(t!("rotation"));
                if r.drag_started() || r.gained_focus() {
                    self.history.checkpoint(&self.design);
                }
                if r.changed() {
                    self.design.objects[i].rotation = rot;
                    self.design.touch();
                }
                self.object_fields(ui, i);
                widgets::vdivider(ui);
            }

            if widgets::icon_button(ui, Icon::FlipH, t!("mirror-h")).clicked() {
                self.flip(true);
            }
            if widgets::icon_button(ui, Icon::FlipV, t!("mirror-v")).clicked() {
                self.flip(false);
            }
            if widgets::icon_button(ui, Icon::RotateCw, t!("rotate-90")).clicked() {
                self.rotate_selection(90.0);
            }
            if sel.len() > 1 {
                widgets::vdivider(ui);
                let aligns: [(Icon, String, fn((f64, f64, f64, f64), (f64, f64, f64, f64)) -> (f64, f64)); 6] = [
                    (Icon::AlignLeft, t!("align-left"), |s, o| (s.0 - o.0, 0.0)),
                    (Icon::AlignHCentre, t!("align-hcentre"), |s, o| ((s.0 + s.2) / 2.0 - (o.0 + o.2) / 2.0, 0.0)),
                    (Icon::AlignRight, t!("align-right"), |s, o| (s.2 - o.2, 0.0)),
                    (Icon::AlignTop, t!("align-top"), |s, o| (0.0, s.1 - o.1)),
                    (Icon::AlignVCentre, t!("align-vcentre"), |s, o| (0.0, (s.1 + s.3) / 2.0 - (o.1 + o.3) / 2.0)),
                    (Icon::AlignBottom, t!("align-bottom"), |s, o| (0.0, s.3 - o.3)),
                ];
                for (icon, tip, f) in aligns {
                    if widgets::icon_button(ui, icon, tip).clicked() {
                        self.edit(|d, _| {
                            for &i in &sel {
                                let ((a0, b0), (a1, b1)) = d.objects[i].bounds();
                                let delta = f((x0, y0, x1, y1), (a0, b0, a1, b1));
                                d.objects[i].x += delta.0;
                                d.objects[i].y += delta.1;
                            }
                        });
                    }
                }
            }
            widgets::vdivider(ui);
            let ctx = ui.ctx().clone();
            if self.is_grouped() {
                if widgets::icon_button(ui, Icon::Ungroup, format!("{}  {}", t!("ungroup"), shortcut(&ctx, Modifiers::COMMAND | Modifiers::SHIFT, Key::G))).clicked() {
                    self.group_selection(false);
                }
            } else if ui.add_enabled_ui(sel.len() > 1, |ui| widgets::icon_button(ui, Icon::Group, format!("{}  {}", t!("group"), shortcut(&ctx, Modifiers::COMMAND, Key::G)))).inner.clicked() {
                self.group_selection(true);
            }
            if widgets::icon_button(ui, Icon::Copy, format!("{}  {}", t!("duplicate"), shortcut(&ctx, Modifiers::COMMAND, Key::D))).clicked() {
                self.duplicate();
            }
            if widgets::icon_button(ui, Icon::Trash, t!("delete-hint")).clicked() {
                self.delete_selection();
            }
        });
    }

    /// Settings specific to one object: corner radius, sides, font.
    fn object_fields(&mut self, ui: &mut egui::Ui, i: usize) {
        match self.design.objects[i].kind.clone() {
            Kind::Rect { mut radius } => {
                widgets::field_label(ui, &t!("corners"));
                let r = ui.add_sized([70.0, 26.0], egui::DragValue::new(&mut radius).speed(0.1).suffix(" mm").range(0.0..=500.0));
                if r.drag_started() || r.gained_focus() {
                    self.history.checkpoint(&self.design);
                }
                if r.changed() {
                    self.design.objects[i].kind = Kind::Rect { radius };
                    self.design.touch();
                }
            }
            Kind::Polygon { mut sides } => {
                widgets::field_label(ui, &t!("sides"));
                let r = ui.add_sized([56.0, 26.0], egui::DragValue::new(&mut sides).range(3..=24));
                if r.drag_started() || r.gained_focus() {
                    self.history.checkpoint(&self.design);
                }
                if r.changed() {
                    self.design.objects[i].kind = Kind::Polygon { sides };
                    self.design.touch();
                }
            }
            Kind::Text { text, mut style, scale, .. } => {
                let before_size = scale.1 * fonts::EM;
                let mut size = before_size;
                widgets::vdivider(ui);
                if text_controls(ui, &mut style, &mut size, &mut self.font_filter) {
                    self.history.checkpoint(&self.design);
                    if (size - before_size).abs() > 1e-6 {
                        // Scale around the top-left corner.
                        let o = self.design.objects[i].clone();
                        let k = size / before_size.max(1e-9);
                        let from = ((o.x, o.y), (o.x + o.w, o.y + o.h));
                        self.design.resize(&[(i, o.clone())], from, ((o.x, o.y), (o.x + o.w * k, o.y + o.h * k)));
                    }
                    let _ = self.design.objects[i].set_text(&text, style.clone());
                    self.design.touch();
                    // New text follows the last font used.
                    self.text_style = style;
                    self.text_size = size;
                }
                if widgets::icon_button(ui, Icon::TypeOutline, t!("convert-to-path-hint")).clicked() {
                    self.canvas.finish_text(&mut self.design, &mut self.selection);
                    self.convert_to_path();
                }
            }
            _ => {}
        }
    }

    /// Right-click menu on the canvas.
    fn canvas_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_width(250.0);
        let ctx = ui.ctx().clone();
        let cmd = |k: Key| shortcut(&ctx, Modifiers::COMMAND, k);
        let has = !self.selection.is_empty();
        if self.tool == Tool::Nodes && !self.canvas.nodes.is_empty() {
            if widgets::menu_item(ui, Some(Icon::Diamond), &t!("node-corner"), "").clicked() {
                self.canvas.set_nodes_smooth(&mut self.design, &mut self.history, false);
            }
            if widgets::menu_item(ui, Some(Icon::CircleDot), &t!("node-smooth"), "").clicked() {
                self.canvas.set_nodes_smooth(&mut self.design, &mut self.history, true);
            }
            if widgets::menu_item(ui, Some(Icon::Trash), &t!("node-delete"), &delete_key()).clicked() {
                self.delete_selection();
            }
            ui.separator();
        }
        ui.add_enabled_ui(has, |ui| {
            if widgets::menu_item(ui, Some(Icon::Scissors), &t!("cut"), &cmd(Key::X)).clicked() {
                self.cut();
            }
            if widgets::menu_item(ui, Some(Icon::Copy), &t!("copy"), &cmd(Key::C)).clicked() {
                self.copy();
            }
        });
        ui.add_enabled_ui(!self.clipboard.is_empty(), |ui| {
            if widgets::menu_item(ui, Some(Icon::Paste), &t!("paste"), &cmd(Key::V)).clicked() {
                self.paste();
            }
        });
        if !has {
            ui.separator();
            ui.add_enabled_ui(!self.design.is_empty(), |ui| {
                if widgets::menu_item(ui, Some(Icon::Pointer), &t!("select-all"), &cmd(Key::A)).clicked() {
                    self.selection = (0..self.design.objects.len()).collect();
                }
                if widgets::menu_item(ui, Some(Icon::Focus), &t!("zoom-design"), "").clicked() {
                    self.canvas.zoom_to(ZoomTarget::Design);
                }
            });
            if widgets::menu_item(ui, Some(Icon::Fullscreen), &t!("zoom-bed"), "").clicked() {
                self.canvas.zoom_to(ZoomTarget::Bed);
            }
            return;
        }
        if widgets::menu_item(ui, Some(Icon::Copy), &t!("duplicate"), &cmd(Key::D)).clicked() {
            self.duplicate();
        }
        if widgets::menu_item(ui, Some(Icon::Trash), &t!("delete"), &delete_key()).clicked() {
            self.delete_selection();
        }
        ui.separator();
        if self.is_grouped() {
            if widgets::menu_item(ui, Some(Icon::Ungroup), &t!("ungroup"), &shortcut(&ctx, Modifiers::COMMAND | Modifiers::SHIFT, Key::G)).clicked() {
                self.group_selection(false);
            }
        } else if self.selection.len() > 1 && widgets::menu_item(ui, Some(Icon::Group), &t!("group"), &cmd(Key::G)).clicked() {
            self.group_selection(true);
        }
        if widgets::menu_item(ui, Some(Icon::BringToFront), &t!("bring-to-front"), &cmd(Key::CloseBracket)).clicked() {
            self.reorder(true);
        }
        if widgets::menu_item(ui, Some(Icon::SendToBack), &t!("send-to-back"), &cmd(Key::OpenBracket)).clicked() {
            self.reorder(false);
        }
        ui.separator();
        if widgets::menu_item(ui, Some(Icon::RotateCw), &t!("rotate-90"), "").clicked() {
            self.rotate_selection(90.0);
        }
        if widgets::menu_item(ui, Some(Icon::RotateCcw), &t!("rotate-90-ccw"), "").clicked() {
            self.rotate_selection(-90.0);
        }
        if widgets::menu_item(ui, Some(Icon::FlipH), &t!("mirror-h"), "").clicked() {
            self.flip(true);
        }
        if widgets::menu_item(ui, Some(Icon::FlipV), &t!("mirror-v"), "").clicked() {
            self.flip(false);
        }
        ui.separator();
        let all_paths = self.selection.iter().all(|&i| matches!(self.design.objects[i].kind, Kind::Path { .. }));
        if !all_paths && widgets::menu_item(ui, Some(Icon::TypeOutline), &t!("convert-to-path"), "").clicked() {
            self.canvas.finish_text(&mut self.design, &mut self.selection);
            self.convert_to_path();
        }
        if self.tool != Tool::Nodes && widgets::menu_item(ui, Some(Icon::PointerHollow), &t!("edit-points"), "A").clicked() {
            self.set_tool(Tool::Nodes);
        }
        ui.separator();
        // The layer colours as a row of swatches.
        let pal = theme::p();
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            for &c in &PALETTE {
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                let current = self.selection.iter().all(|&i| self.design.objects[i].color == c);
                ui.painter().circle_filled(r.center(), if resp.hovered() { 9.0 } else { 8.0 }, Color32::from_rgb(c[0], c[1], c[2]));
                if current {
                    ui.painter().circle_stroke(r.center(), 10.5, Stroke::new(1.5, pal.accent));
                }
                if resp.on_hover_text(theme::color_name(c)).clicked() {
                    self.set_color(c);
                    ui.close();
                }
            }
        });
    }
}

/// Font, size, bold and italic for text. Returns whether anything changed.
fn text_controls(ui: &mut egui::Ui, style: &mut TextStyle, size: &mut f64, filter: &mut String) -> bool {
    let mut changed = font_picker(ui, &mut style.family, filter);
    widgets::field_label(ui, &t!("text-size"));
    let r = ui.add_sized([70.0, 26.0], egui::DragValue::new(size).speed(0.1).max_decimals(1).suffix(" mm").range(1.0..=500.0));
    changed |= r.changed();
    if widgets::icon_toggle(ui, Icon::Bold, style.bold, t!("bold")).clicked() {
        style.bold = !style.bold;
        changed = true;
    }
    if widgets::icon_toggle(ui, Icon::Italic, style.italic, t!("italic")).clicked() {
        style.italic = !style.italic;
        changed = true;
    }
    changed
}

/// A drop-down of the installed fonts, each shown in its own letters.
fn font_picker(ui: &mut egui::Ui, family: &mut String, filter: &mut String) -> bool {
    let pal = theme::p();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(190.0, 28.0), Sense::click());
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
    ui.painter().rect(rect, 7.0, if response.hovered() || open { pal.hover } else { pal.card }, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
    let p = ui.painter().with_clip_rect(rect.shrink(4.0));
    p.text(rect.left_center() + Vec2::new(10.0, 0.0), egui::Align2::LEFT_CENTER, family.as_str(), egui::FontId::proportional(13.0), pal.text);
    icons::paint(ui.painter(), Rect::from_center_size(rect.right_center() - Vec2::new(14.0, 0.0), Vec2::splat(14.0)), Icon::ChevronDown, pal.muted);
    let response = response.on_hover_text(t!("font"));
    let mut changed = false;
    // Keyboard highlight: (search it belongs to, row). Reset when the list opens.
    let hl_id = response.id.with("highlight");
    if response.clicked() {
        filter.clear();
        ui.data_mut(|d| d.remove::<(String, usize)>(hl_id));
    }
    egui::Popup::from_toggle_button_response(&response).close_behavior(PopupCloseBehavior::CloseOnClickOutside).width(300.0).show(|ui| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
            icons::paint(ui.painter(), r, Icon::Search, pal.muted);
            ui.add(egui::TextEdit::singleline(filter).hint_text(t!("search-fonts")).desired_width(f32::INFINITY).frame(Frame::NONE)).request_focus();
        });
        ui.separator();
        let needle = filter.to_lowercase();
        let names: Vec<&String> = fonts::families().iter().filter(|f| needle.is_empty() || f.to_lowercase().contains(&needle)).collect();
        let row_h = 30.0;
        let list_h = 360.0;
        let (mut hl, mut scroll) = match ui.data(|d| d.get_temp::<(String, usize)>(hl_id)) {
            Some((n, i)) if n == needle => (i, false),
            _ => (if needle.is_empty() { names.iter().position(|n| n.as_str() == family.as_str()).unwrap_or(0) } else { 0 }, true),
        };
        let (down, up, enter) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp), i.key_pressed(egui::Key::Enter)));
        if down && hl + 1 < names.len() {
            hl += 1;
            scroll = true;
        }
        if up && hl > 0 {
            hl -= 1;
            scroll = true;
        }
        if enter && let Some(name) = names.get(hl) {
            *family = name.to_string();
            changed = true;
            ui.close();
        }
        ui.data_mut(|d| d.insert_temp(hl_id, (needle.clone(), hl)));
        let mut area = egui::ScrollArea::vertical().max_height(list_h);
        if scroll {
            // show_rows spaces rows by their height plus the item spacing.
            let pitch = row_h + ui.spacing().item_spacing.y;
            area = area.vertical_scroll_offset((hl as f32 * pitch - (list_h - row_h) / 2.0).max(0.0));
        }
        area.show_rows(ui, row_h, names.len(), |ui, range| {
            for i in range {
                let name = names[i];
                let selected = name.as_str() == family.as_str();
                let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
                if selected || resp.hovered() || i == hl {
                    ui.painter().rect_filled(r, 6.0, if selected { pal.accent.gamma_multiply(0.12) } else { pal.hover });
                }
                let color = if selected { pal.accent } else { pal.text };
                let ppp = ui.ctx().pixels_per_point();
                let px = (20.0 * ppp).round() as u32;
                match fonts::preview(ui.ctx(), name, px) {
                    Some(tex) => {
                        let size = tex.size_vec2() / ppp;
                        let img = Rect::from_min_size(r.left_center() + Vec2::new(10.0, -size.y / 2.0), size);
                        ui.painter().with_clip_rect(r.shrink(2.0)).image(tex.id(), img, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), color);
                    }
                    None => {
                        ui.painter().text(r.left_center() + Vec2::new(10.0, 0.0), egui::Align2::LEFT_CENTER, name.as_str(), egui::FontId::proportional(13.0), color);
                    }
                }
                if resp.on_hover_text(name.as_str()).clicked() {
                    *family = name.to_string();
                    changed = true;
                    ui.close();
                }
            }
        });
    });
    changed
}

// ---------------------------------------------------------------- right panel

impl FokusApp {
    fn job_panel(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let pal = theme::p();
        if self.job.is_some() {
            self.job_file_panel(ui);
            return;
        }
        let ready = !self.design.is_empty() && self.material().is_some() && self.design.problems(self.material()).is_empty();
        egui::Panel::bottom("start").frame(Frame::new().fill(pal.panel).inner_margin(Margin { top: 14, ..Margin::ZERO })).show(ui, |ui| self.start_footer(ui, snap, ready));

        egui::ScrollArea::vertical().show(ui, |ui| {
            widgets::section(ui, &t!("section-material"));
            self.material_picker(ui);
            ui.add_space(18.0);
            widgets::section(ui, &t!("section-layers"));
            if self.design.is_empty() {
                empty_state(ui, Icon::Layers, &t!("layers-empty"));
            } else {
                self.layer_list(ui);
            }
        });
    }

    fn material_picker(&mut self, ui: &mut egui::Ui) {
        let pal = theme::p();
        let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.0), Sense::click());
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
        ui.painter().rect(rect, 10.0, if response.hovered() || open { pal.hover } else { pal.card }, Stroke::new(1.0, if self.material.is_none() { pal.accent.gamma_multiply(0.6) } else { pal.border }), egui::StrokeKind::Inside);
        let icon_rect = Rect::from_center_size(rect.left_center() + Vec2::new(24.0, 0.0), Vec2::splat(30.0));
        ui.painter().rect_filled(icon_rect, 8.0, pal.accent.gamma_multiply(0.12));
        icons::paint(ui.painter(), icon_rect.shrink(7.0), Icon::Package, pal.accent);
        let p = ui.painter().with_clip_rect(rect.shrink(4.0));
        let x = rect.min.x + 48.0;
        match self.material() {
            Some(m) => {
                p.text(egui::pos2(x, rect.min.y + 17.0), egui::Align2::LEFT_CENTER, &m.name, theme::semibold(13.5), pal.text);
                let ops: Vec<String> = Operation::ALL.iter().filter(|op| m.settings(**op).is_some()).map(|op| op.label()).collect();
                p.text(egui::pos2(x, rect.min.y + 35.0), egui::Align2::LEFT_CENTER, format!("{} mm  ·  {}", m.thickness_mm, ops.join(", ")), egui::FontId::proportional(11.5), pal.muted);
            }
            None => {
                p.text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, t!("choose-material"), theme::semibold(13.5), pal.accent);
            }
        }
        icons::paint(ui.painter(), Rect::from_center_size(rect.right_center() - Vec2::new(18.0, 0.0), Vec2::splat(16.0)), Icon::ChevronDown, pal.muted);
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        let mut pick = None;
        egui::Popup::from_toggle_button_response(&response).width(rect.width()).show(|ui| {
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                let untested = t!("untested");
                for m in &self.library.materials {
                    let ops: Vec<String> = Operation::ALL.iter().filter(|op| m.settings(**op).is_some()).map(|op| op.label()).collect();
                    let badge = (!m.verified).then_some((untested.as_str(), pal.warn));
                    if widgets::list_row(ui, self.material.as_ref() == Some(&m.name), &m.name, &format!("{} mm  ·  {}", m.thickness_mm, ops.join(", ")), badge).clicked() {
                        pick = Some(m.name.clone());
                        ui.close();
                    }
                }
            });
            ui.separator();
            if widgets::menu_item(ui, Some(Icon::Settings), &t!("manage-materials"), "").clicked() {
                self.show_library = true;
            }
        });
        if let Some(name) = pick {
            self.material = Some(name);
        }
        ui.add_space(4.0);
        if let Some(m) = self.material() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
                for op in Operation::ALL {
                    widgets::chip(ui, &op.label(), m.settings(op).is_some());
                }
            });
            if !m.verified {
                notice(ui, Icon::Warning, pal.warn, &t!("untested-warning"));
            }
        } else {
            ui.label(RichText::new(t!("material-hint")).color(pal.muted).size(12.0));
        }
    }

    fn layer_list(&mut self, ui: &mut egui::Ui) {
        let pal = theme::p();
        let estimates = self.layer_estimates();
        let material = self.material().cloned();
        let mut select_color = None;
        for (idx, layer) in self.design.layers.iter_mut().enumerate() {
            widgets::card(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, swatch) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                    ui.painter().circle_filled(rect.center(), 8.0, Color32::from_rgb(layer.color[0], layer.color[1], layer.color[2]));
                    ui.painter().circle_stroke(rect.center(), 8.0, Stroke::new(1.0, pal.border));
                    if swatch.on_hover_text(t!("select-layer")).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        select_color = Some(layer.color);
                    }
                    ui.label(RichText::new(theme::color_name(layer.color)).semibold());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(t) = estimates[idx].as_ref().map(|(_, _, t)| *t) {
                            ui.label(RichText::new(fmt_duration(t)).color(pal.muted).size(12.0).medium());
                        }
                    });
                });
                let before = layer.action;
                widgets::segmented(ui, &mut layer.action, &[(Action::Do(Operation::Cut), Operation::Cut.label()), (Action::Do(Operation::Mark), Operation::Mark.label()), (Action::Do(Operation::Engrave), Operation::Engrave.label()), (Action::Skip, t!("action-skip"))]);
                if layer.action != before {
                    layer.adjusted = None;
                }
                match layer.resolve(material.as_ref()) {
                    Err(LayerProblem::NoMaterial) => {
                        if let Action::Do(op) = layer.action {
                            ui.label(RichText::new(op.explain()).color(pal.muted).size(12.0));
                        }
                    }
                    Err(p) => notice(ui, Icon::Warning, pal.err, &p.message(material.as_ref())),
                    Ok(None) => {
                        ui.label(RichText::new(t!("not-lasered")).color(pal.muted).size(12.0));
                    }
                    Ok(Some((op, s))) => {
                        let title = if layer.adjusted.is_some() { t!("adjusted", summary = s.summary(op)) } else { s.summary(op) };
                        egui::CollapsingHeader::new(RichText::new(title).size(12.0).color(pal.muted)).id_salt(("adjust", idx)).show(ui, |ui| {
                            let mut edit = s.clone();
                            if settings_editor(ui, op, &mut edit, ("layer", idx)) {
                                layer.adjusted = Some(edit);
                            }
                            if layer.adjusted.is_some() && Btn::new(t!("back-to-material")).icon(Icon::Undo).ghost().height(26.0).show(ui).clicked() {
                                layer.adjusted = None;
                            }
                        });
                    }
                }
            });
            ui.add_space(8.0);
        }
        if let Some(c) = select_color {
            self.selection = (0..self.design.objects.len()).filter(|&i| self.design.objects[i].color == c).collect();
            self.set_tool(Tool::Select);
        }
    }

    fn start_footer(&mut self, ui: &mut egui::Ui, snap: &Snapshot, ready: bool) {
        let pal = theme::p();
        let estimates = self.layer_estimates();
        let bed = self.bed(snap);
        if let Some(((x0, y0), (x1, y1))) = self.design.bounds() {
            let total: f64 = estimates.iter().flatten().map(|(_, _, t)| *t).sum();
            let time = if estimates.iter().any(Option::is_some) { t!("laser-time", time = fmt_duration(total)) } else { t!("laser-time-unknown") };
            ui.horizontal(|ui| {
                stat(ui, Icon::Scan, &format!("{:.0} × {:.0} mm", x1 - x0, y1 - y0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(time).color(pal.muted).medium());
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
                    icons::paint(ui.painter(), r, Icon::Flame, pal.muted);
                });
            });
            if let Some(head) = snap.status.as_ref().filter(|_| snap.conn == Conn::Connected).map(|s| s.position) {
                let off = self.head_offset(bed, head).unwrap_or(0.0);
                if off > 0.5 && Btn::new(t!("place-at-head")).icon(Icon::Crosshair).width(f32::INFINITY).show(ui).on_hover_text(t!("place-at-head-hint")).clicked() {
                    self.place_at_head(bed, head);
                }
            }
            ui.add_space(4.0);
        }
        ui.horizontal(|ui| {
            anchor_picker(ui, &mut self.anchor);
            ui.vertical(|ui| {
                ui.add_space(6.0);
                ui.label(RichText::new(t!("head-position")).semibold());
                ui.label(RichText::new(t!("head-position-hint", point = self.anchor.label())).color(pal.muted).size(12.0));
            });
        });
        ui.add_space(8.0);
        if snap.job.is_some() {
            self.job_card(ui, snap);
            if snap.job_active() {
                return;
            }
        }
        let can_frame = snap.conn == Conn::Connected && !snap.job_active() && !self.design.is_empty();
        ui.horizontal(|ui| {
            if ui.add_enabled_ui(can_frame, |ui| Btn::new(t!("frame")).icon(Icon::Scan).height(46.0).show(ui)).inner.on_hover_text(t!("frame-hint")).clicked() {
                self.start_job(JobKind::Frame);
            }
            if ui.add_enabled_ui(ready, |ui| Btn::new(t!("start-job-dots")).icon(Icon::Flame).primary().height(46.0).width(f32::INFINITY).show(ui)).inner.clicked() {
                self.checklist = [false; CHECKLIST];
                self.prepare_open = true;
            }
        });
        if !ready {
            let why = if self.design.is_empty() {
                t!("why-design")
            } else if self.material().is_none() {
                t!("why-material")
            } else {
                t!("why-layers")
            };
            ui.label(RichText::new(why).color(pal.muted).size(12.0));
        }
        if let Some((ok, msg)) = &self.save_msg {
            notice(ui, if *ok { Icon::Check } else { Icon::Warning }, if *ok { pal.ok } else { pal.err }, msg);
        }
    }

    /// Progress of the job on the laser, with Pause / Resume / Stop.
    fn job_card(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let pal = theme::p();
        let Some(job) = &snap.job else { return };
        let elapsed = job.elapsed().as_secs_f64();
        let clock = |s: f64| format!("{}:{:02}", (s as u64) / 60, (s as u64) % 60);
        let (title, color, fraction, detail) = match &job.phase {
            JobPhase::Sending => {
                let f = job.sent as f32 / job.total.max(1) as f32;
                (t!("sending"), pal.accent, f, t!("sent-kb", sent = job.sent / 1000, total = job.total.div_ceil(1000)))
            }
            JobPhase::Running | JobPhase::Paused => {
                let f = if job.estimate_s > 0.0 { (elapsed / job.estimate_s).min(0.99) as f32 } else { 0.5 };
                let title = match (&job.phase, job.kind) {
                    (JobPhase::Paused, _) => t!("status-paused"),
                    (_, JobKind::DryRun) => t!("dry-run-running"),
                    (_, JobKind::Frame) => t!("status-framing"),
                    _ => t!("status-lasering"),
                };
                (title, if job.phase == JobPhase::Paused { pal.warn } else { pal.accent }, f, t!("time-of", elapsed = clock(elapsed), total = clock(job.estimate_s)))
            }
            JobPhase::Finished => {
                let title = match job.kind {
                    JobKind::DryRun => t!("dry-run-finished"),
                    JobKind::Frame => t!("frame-finished"),
                    JobKind::Job => t!("job-finished"),
                };
                (title, pal.ok, 1.0, t!("took", time = clock(elapsed)))
            }
            JobPhase::Stopped => (if job.kind == JobKind::Frame { t!("frame-stopped") } else { t!("job-stopped") }, pal.warn, 0.0, t!("after", time = clock(elapsed))),
            JobPhase::Failed(_) => (t!("job-failed"), pal.err, 0.0, t!("after", time = clock(elapsed))),
        };
        // Longer text goes on its own line under the title.
        let message = match &job.phase {
            JobPhase::Failed(e) => Some(e.clone()),
            phase if phase.active() && snap.conn != Conn::Connected => Some(t!("job-reconnecting")),
            _ => job.note.clone(),
        };
        Frame::new().fill(color.gamma_multiply(0.08)).stroke(Stroke::new(1.0, color.gamma_multiply(0.4))).corner_radius(12).inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).semibold().color(color));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !job.phase.active() && widgets::icon_button_sized(ui, Icon::X, false, 22.0, t!("dismiss")).clicked() {
                        self.machine.clear_job();
                    }
                    ui.label(RichText::new(detail).color(pal.muted).size(12.0));
                });
            });
            if let Some(message) = message {
                ui.label(RichText::new(message).color(pal.text).size(12.0));
                ui.add_space(2.0);
            }
            // A slim progress bar.
            let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 6.0), Sense::hover());
            ui.painter().rect_filled(r, 3.0, color.gamma_multiply(0.15));
            ui.painter().rect_filled(Rect::from_min_size(r.min, Vec2::new(r.width() * fraction, r.height())), 3.0, color);
            if job.phase.active() {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 8.0) / 2.0;
                    match job.phase {
                        JobPhase::Paused => {
                            if Btn::new(t!("resume")).icon(Icon::Play).height(38.0).width(w).show(ui).clicked() {
                                self.machine.resume_job();
                            }
                        }
                        JobPhase::Running => {
                            if Btn::new(t!("pause")).icon(Icon::Pause).height(38.0).width(w).show(ui).clicked() {
                                self.machine.pause_job();
                            }
                        }
                        _ => {
                            ui.add_enabled_ui(false, |ui| Btn::new(t!("pause")).icon(Icon::Pause).height(38.0).width(w).show(ui));
                        }
                    }
                    if Btn::new(t!("stop")).icon(Icon::Stop).danger().height(38.0).width(w).show(ui).on_hover_text(t!("stop-hint")).clicked() {
                        self.machine.stop_job();
                    }
                });
            }
        });
        ui.add_space(8.0);
    }

    fn job_file_panel(&mut self, ui: &mut egui::Ui) {
        let pal = theme::p();
        let Some(job) = &self.job else { return };
        widgets::section(ui, &t!("section-job-file"));
        widgets::card(ui, |ui| {
            ui.label(RichText::new(&job.name).size(16.0).semibold());
            ui.label(RichText::new(t!("job-file-hint")).color(pal.muted).size(12.0));
            if let Some(((x0, y0), (x1, y1))) = job.bounds {
                widgets::kv(ui, &t!("size"), &format!("{:.1} × {:.1} mm", x1 - x0, y1 - y0));
            }
            let total: f64 = job.toolpath.layers.values().filter_map(Job::layer_seconds).sum();
            widgets::kv(ui, &t!("laser-time-label"), &format!("≈ {}", fmt_duration(total)));
        });
        ui.add_space(8.0);
        for (id, layer) in &job.toolpath.layers {
            widgets::card(ui, |ui| {
                ui.horizontal(|ui| {
                    widgets::dot(ui, theme::layer_color(*id, layer.color));
                    ui.label(RichText::new(t!("layer-n", n = id + 1)).semibold());
                });
                let speed = layer.speed.map(|s| format!("{s:.0} mm/s")).unwrap_or_else(|| "—".into());
                let power = match (layer.min_power, layer.max_power) {
                    (Some(a), Some(b)) if (a - b).abs() < 0.05 => format!("{b:.0} %"),
                    (Some(a), Some(b)) => format!("{a:.0}–{b:.0} %"),
                    _ => "—".into(),
                };
                widgets::kv(ui, &t!("speed"), &speed);
                widgets::kv(ui, &t!("power"), &power);
            });
            ui.add_space(8.0);
        }
        if Btn::new(t!("close-job-file")).icon(Icon::X).show(ui).clicked() {
            self.job = None;
        }
    }
}

// ---------------------------------------------------------------- windows

/// The frame of a window of its own (materials, settings).
fn window_panel() -> Frame {
    let pal = theme::p();
    Frame::new().fill(pal.panel).inner_margin(Margin::same(20))
}

impl FokusApp {
    /// Settings, in a window of its own.
    fn settings_window(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        if !self.show_settings {
            return;
        }
        let builder = egui::ViewportBuilder::default().with_title(t!("settings")).with_inner_size([520.0, 600.0]).with_min_inner_size([440.0, 420.0]);
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("settings"), builder, |ui, _| {
            if ui.input(|i| i.viewport().close_requested()) {
                self.show_settings = false;
            }
            theme::set_dark(ui.visuals().dark_mode);
            let pal = theme::p();
            egui::CentralPanel::default().frame(window_panel()).show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(RichText::new(t!("settings")).size(20.0).semibold());
                    ui.add_space(12.0);
                    widgets::section(ui, &t!("section-machine"));
                    widgets::card(ui, |ui| {
                        let busy = matches!(snap.conn, Conn::Connected | Conn::Connecting);
                        let (color, text) = match &snap.conn {
                            Conn::Connected => (pal.ok, t!("conn-connected")),
                            Conn::Connecting => (pal.warn, t!("status-connecting")),
                            Conn::Failed(e) => (pal.err, e.clone()),
                            Conn::Disconnected => (pal.muted, t!("conn-disconnected")),
                        };
                        ui.horizontal_top(|ui| {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                            icons::paint(ui.painter(), r, if snap.conn == Conn::Connected { Icon::Plug } else { Icon::Unplug }, color);
                            ui.add(egui::Label::new(RichText::new(text).color(color).medium()).wrap());
                        });
                        ui.add_space(4.0);
                        ui.add_enabled_ui(!busy, |ui| {
                            ui.horizontal(|ui| {
                                let selected = self.port.clone().unwrap_or_else(|| t!("no-laser-found"));
                                egui::ComboBox::from_id_salt("port").width(ui.available_width() - 40.0).selected_text(short_port(&selected)).show_ui(ui, |ui| {
                                    for p in &self.ports {
                                        let label = if p.looks_like_ruida { t!("port-laser", port = short_port(&p.name)) } else { short_port(&p.name) };
                                        ui.selectable_value(&mut self.port, Some(p.name.clone()), label);
                                    }
                                });
                                if widgets::icon_button(ui, Icon::Repeat, t!("refresh")).clicked() {
                                    self.refresh_ports();
                                }
                            });
                        });
                        if busy {
                            if Btn::new(t!("disconnect")).icon(Icon::Unplug).show(ui).clicked() {
                                self.machine.disconnect();
                                self.auto_connect = false;
                            }
                        } else if ui.add_enabled_ui(self.port.is_some(), |ui| Btn::new(t!("connect")).icon(Icon::Plug).primary().show(ui)).inner.clicked() {
                            if let Some(port) = self.port.clone() {
                                self.auto_connect = true;
                                self.machine.connect(port, ui.ctx().clone());
                            }
                        }
                        if let Some(s) = &snap.status {
                            ui.add_space(4.0);
                            widgets::kv(ui, &t!("bed-size"), &format!("{:.0} × {:.0} mm", s.bed.0, s.bed.1));
                            widgets::kv(ui, &t!("controller"), &format!("Ruida · 0x{:08X}", s.card_id));
                        }
                        ui.add_space(4.0);
                        ui.checkbox(&mut self.origin_right, t!("origin-right"));
                    });

                    ui.add_space(16.0);
                    widgets::section(ui, &t!("section-display"));
                    widgets::card(ui, |ui| {
                        self.language_picker(ui);
                        ui.add_space(6.0);
                        widgets::field_label(ui, &t!("appearance"));
                        let mut pref = ui.ctx().options(|o| o.theme_preference);
                        if widgets::segmented(ui, &mut pref, &[(egui::ThemePreference::System, t!("theme-system")), (egui::ThemePreference::Light, t!("theme-light")), (egui::ThemePreference::Dark, t!("theme-dark"))]) {
                            ui.ctx().set_theme(pref);
                            self.settings.theme = match pref {
                                egui::ThemePreference::Light => Some("light".into()),
                                egui::ThemePreference::Dark => Some("dark".into()),
                                egui::ThemePreference::System => None,
                            };
                            self.settings.save();
                        }
                        ui.add_space(6.0);
                        ui.checkbox(&mut self.show_travel, t!("show-travel"));
                    });
                });
            });
        });
    }

    /// Language: follow the system, or a fixed one (built-in or a community file).
    fn language_picker(&mut self, ui: &mut egui::Ui) {
        let pal = theme::p();
        let languages = crate::i18n::available();
        let name_of = |code: &str| languages.iter().find(|l| l.code == code).map(|l| l.name.clone()).unwrap_or_else(|| code.to_string());
        let system = t!("language-system", language = name_of(&crate::i18n::system_language()));
        let selected = match &self.settings.language {
            Some(code) => name_of(code),
            None => system.clone(),
        };
        let mut choice = None;
        widgets::field_label(ui, &t!("language"));
        egui::ComboBox::from_id_salt("language").width(ui.available_width()).selected_text(selected).show_ui(ui, |ui| {
            if ui.selectable_label(self.settings.language.is_none(), system).clicked() {
                choice = Some(None);
            }
            for l in &languages {
                let label = if l.builtin { l.name.clone() } else { t!("language-community", language = l.name.clone()) };
                if ui.selectable_label(self.settings.language.as_ref() == Some(&l.code), label).clicked() {
                    choice = Some(Some(l.code.clone()));
                }
            }
        });
        if let Some(dir) = crate::i18n::user_locale_dir() {
            ui.label(RichText::new(t!("language-files-hint", folder = dir.display().to_string())).color(pal.muted).size(11.0));
        }
        if let Some(code) = choice {
            self.settings.language = code;
            crate::i18n::set_language(self.settings.language.as_deref());
            self.settings.save();
        }
    }

    /// The material library, in a window of its own: the list on the left,
    /// the chosen material's settings on the right.
    fn library_window(&mut self, ctx: &egui::Context) {
        if !self.show_library {
            return;
        }
        let builder = egui::ViewportBuilder::default().with_title(t!("materials")).with_inner_size([900.0, 640.0]).with_min_inner_size([720.0, 480.0]);
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("materials"), builder, |ui, _| {
            if ui.input(|i| i.viewport().close_requested()) {
                self.show_library = false;
            }
            theme::set_dark(ui.visuals().dark_mode);
            let pal = theme::p();
            let mut changed = false;
            egui::Panel::left("matlist").exact_size(290.0).frame(Frame::new().fill(pal.card).inner_margin(Margin::same(14)).stroke(Stroke::new(1.0, pal.border))).show(ui, |ui| {
                ui.label(RichText::new(t!("materials")).size(18.0).semibold());
                ui.add_space(8.0);
                Frame::new().fill(pal.panel).stroke(Stroke::new(1.0, pal.border)).corner_radius(8).inner_margin(Margin::symmetric(8, 5)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
                        icons::paint(ui.painter(), r, Icon::Search, pal.muted);
                        ui.add(egui::TextEdit::singleline(&mut self.library_filter).hint_text(t!("search")).desired_width(f32::INFINITY).frame(Frame::NONE));
                    });
                });
                ui.add_space(6.0);
                egui::Panel::bottom("matactions").frame(Frame::new().inner_margin(Margin { top: 8, ..Margin::ZERO })).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if Btn::new(t!("lib-new")).icon(Icon::Plus).show(ui).clicked() {
                            self.library.materials.push(Material::new_blank());
                            self.library_selected = self.library.materials.len() - 1;
                            changed = true;
                        }
                        if widgets::icon_button(ui, Icon::Copy, t!("duplicate")).clicked() && self.library_selected < self.library.materials.len() {
                            let mut copy = self.library.materials[self.library_selected].clone();
                            copy.name.push_str(&t!("copy-suffix"));
                            copy.verified = false;
                            self.library.materials.push(copy);
                            self.library_selected = self.library.materials.len() - 1;
                            changed = true;
                        }
                        if widgets::icon_button(ui, Icon::Trash, t!("delete")).clicked() && self.library_selected < self.library.materials.len() {
                            self.library.materials.remove(self.library_selected);
                            self.library_selected = self.library_selected.saturating_sub(1);
                            changed = true;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::icon_button(ui, Icon::RotateCcw, t!("restore-reference-hint")).clicked() {
                                changed |= self.library.restore_reference() > 0;
                            }
                        });
                    });
                });
                let filter = self.library_filter.to_lowercase();
                egui::ScrollArea::vertical().id_salt("matscroll").show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (i, m) in self.library.materials.iter().enumerate() {
                        if !filter.is_empty() && !m.name.to_lowercase().contains(&filter) {
                            continue;
                        }
                        let ops: Vec<String> = Operation::ALL.iter().filter(|op| m.settings(**op).is_some()).map(|op| op.label()).collect();
                        let untested = t!("untested");
                        let badge = (!m.verified).then_some((untested.as_str(), pal.warn));
                        if widgets::list_row(ui, self.library_selected == i, &m.name, &format!("{} mm  ·  {}", m.thickness_mm, ops.join(", ")), badge).clicked() {
                            self.library_selected = i;
                        }
                    }
                });
            });
            egui::CentralPanel::default().frame(window_panel()).show(ui, |ui| {
                if let Some(e) = &self.library.error {
                    notice(ui, Icon::Warning, pal.err, e);
                }
                let location = self.library.location();
                let Some(m) = self.library.materials.get_mut(self.library_selected) else {
                    empty_state(ui, Icon::Package, &t!("no-material-selected"));
                    return;
                };
                let old_name = m.name.clone();
                egui::ScrollArea::vertical().id_salt("matedit").show(ui, |ui| {
                    widgets::field_label(ui, &t!("name"));
                    changed |= ui.add(egui::TextEdit::singleline(&mut m.name).desired_width(f32::INFINITY).font(theme::semibold(16.0)).margin(Margin::symmetric(8, 6))).changed();
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        widgets::field_label(ui, &t!("thickness"));
                        changed |= ui.add_sized([90.0, 26.0], egui::DragValue::new(&mut m.thickness_mm).range(0.0..=50.0).suffix(" mm").speed(0.1)).changed();
                        ui.add_space(16.0);
                        changed |= ui.checkbox(&mut m.verified, t!("tested-hint")).changed();
                    });
                    ui.add_space(14.0);
                    widgets::section(ui, &t!("operations"));
                    for op in Operation::ALL {
                        let slot = m.settings_mut(op);
                        let mut on = slot.is_some();
                        widgets::card(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(op.label()).semibold());
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if toggle(ui, &mut on).changed() {
                                        *slot = on.then(|| OpSettings { speed: 100.0, min_power: 20.0, max_power: 20.0, passes: 1, air_assist: true, line_interval_mm: if op.uses_line_interval() { 0.1 } else { 0.0 } });
                                        changed = true;
                                    }
                                });
                            });
                            if let Some(s) = slot {
                                ui.add_space(4.0);
                                changed |= settings_editor(ui, op, s, ("lib", op.label()));
                            } else {
                                ui.label(RichText::new(t!("op-off")).color(pal.muted).size(12.0));
                            }
                        });
                        ui.add_space(8.0);
                    }
                    ui.add_space(6.0);
                    widgets::section(ui, &t!("notes"));
                    changed |= ui.add(egui::TextEdit::multiline(&mut m.notes).desired_rows(3).desired_width(f32::INFINITY).margin(Margin::same(8))).changed();
                    if let Some(loc) = &location {
                        ui.add_space(8.0);
                        ui.label(RichText::new(t!("saved-in", location = loc.clone())).color(pal.muted).size(11.0));
                    }
                });
                if self.material.as_ref() == Some(&old_name) {
                    if let Some(m) = self.library.materials.get(self.library_selected) {
                        if m.name != old_name {
                            self.material = Some(m.name.clone());
                        }
                    }
                }
            });
            if changed {
                self.library.save();
            }
        });
    }

    fn prepare_modal(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        if !self.prepare_open {
            return;
        }
        let pal = theme::p();
        let material = self.material().cloned();
        let Some(((x0, y0), (x1, y1))) = self.design.bounds() else { return };
        let size = (x1 - x0, y1 - y0);
        let bed = self.bed(snap);
        let head = snap.status.as_ref().filter(|_| snap.conn == Conn::Connected).map(|s| s.position);
        let head_off = head.and_then(|h| self.head_offset(bed, h));
        let extent = self.extent_from_anchor().unwrap_or_default();
        let mut close = false;
        let mut save = false;
        let mut start = false;
        let mut dry = false;
        let mut place = false;
        let mut frame = false;

        let modal = egui::Modal::new(egui::Id::unique("prepare")).frame(modal_frame()).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.label(RichText::new(t!("checklist-title")).size(20.0).semibold());
            if let Some(m) = &material {
                ui.label(RichText::new(format!("{}  ·  {:.0} × {:.0} mm", m.name, size.0, size.1)).color(pal.muted));
            }
            ui.add_space(10.0);
            let thickness = material.as_ref().map(|m| m.thickness_mm).unwrap_or(0.0);
            let focus_text = if thickness > 0.0 {
                t!("focus-thickness", thickness = thickness.to_string())
            } else {
                t!("focus-surface")
            };
            let items: [(String, String); CHECKLIST] = [
                (t!("check-material"), t!("check-material-text", width = format!("{:.0}", size.0 + 5.0), height = format!("{:.0}", size.1 + 5.0))),
                (t!("check-focus"), focus_text),
                (t!("check-head"), t!("check-head-text", point = self.anchor.label())),
                (t!("check-lid"), t!("check-lid-text")),
            ];
            for (i, (title, text)) in items.iter().enumerate() {
                let done = self.checklist[i];
                let response = Frame::new()
                    .fill(if done { pal.ok.gamma_multiply(0.07) } else { pal.card })
                    .stroke(Stroke::new(1.0, if done { pal.ok.gamma_multiply(0.4) } else { pal.border }))
                    .corner_radius(12)
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_top(|ui| {
                            // A round check mark, ticked by clicking anywhere on the card.
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
                            if done {
                                ui.painter().circle_filled(r.center(), 11.0, pal.ok);
                                icons::paint(ui.painter(), r.shrink(4.0), Icon::Check, Color32::WHITE);
                            } else {
                                ui.painter().circle_stroke(r.center(), 10.0, Stroke::new(1.5, pal.muted));
                            }
                            ui.vertical(|ui| {
                                ui.label(RichText::new(title).semibold());
                                ui.label(RichText::new(text).color(pal.muted).size(12.5));
                                if i == 2 {
                                    start_corner_diagram(ui, size, self.anchor);
                                    match (head, head_off) {
                                        (Some((hx, hy)), Some(off)) => {
                                            if off > 0.5 {
                                                ui.label(RichText::new(t!("head-elsewhere", x = format!("{hx:.1}"), y = format!("{hy:.1}"))).color(pal.muted).size(12.5));
                                            } else {
                                                ui.label(RichText::new(t!("head-matches", x = format!("{hx:.1}"), y = format!("{hy:.1}"))).color(pal.ok).size(12.5));
                                            }
                                            if let Some(problem) = fits_from_head(bed, self.to_view(bed, (hx, hy)), extent) {
                                                notice(ui, Icon::Warning, pal.err, &problem);
                                            }
                                            ui.horizontal(|ui| {
                                                if Btn::new(t!("frame")).icon(Icon::Scan).height(28.0).show(ui).on_hover_text(t!("frame-hint")).clicked() {
                                                    frame = true;
                                                }
                                                if off > 0.5 && Btn::new(t!("show-at-head")).icon(Icon::Crosshair).ghost().height(28.0).show(ui).clicked() {
                                                    place = true;
                                                }
                                            });
                                        }
                                        _ => {
                                            ui.label(RichText::new(t!("connect-to-check")).color(pal.muted).size(12.0));
                                        }
                                    }
                                }
                            });
                        });
                    })
                    .response;
                if ui.interact(response.rect, ui.scope_id().with(("check", i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.checklist[i] = !self.checklist[i];
                }
                ui.add_space(6.0);
            }
            ui.add_space(4.0);
            let connected = head.is_some();
            let (icon, hint) = if connected { (Icon::Warning, t!("start-warning")) } else { (Icon::Info, t!("not-connected-hint")) };
            notice(ui, icon, pal.muted, &hint);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if Btn::new(t!("cancel")).ghost().height(38.0).show(ui).clicked() {
                    close = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let all = self.checklist.iter().all(|&c| c);
                    let checked = t!("checked-count", done = self.checklist.iter().filter(|&&c| c).count(), total = CHECKLIST);
                    if connected {
                        let label = if all { t!("start-job") } else { checked };
                        if ui.add_enabled_ui(all, |ui| Btn::new(label).icon(Icon::Flame).primary().height(40.0).width(180.0).show(ui)).inner.clicked() {
                            start = true;
                        }
                        if Btn::new(t!("save-as-file")).icon(Icon::FileDown).height(40.0).show(ui).clicked() {
                            save = true;
                        }
                        if Btn::new(t!("dry-run")).icon(Icon::EyeOff).height(40.0).show(ui).on_hover_text(t!("dry-run-hint")).clicked() {
                            dry = true;
                        }
                    } else {
                        let label = if all { t!("save-job-file") } else { checked };
                        if ui.add_enabled_ui(all, |ui| Btn::new(label).icon(Icon::FileDown).primary().height(40.0).width(200.0).show(ui)).inner.clicked() {
                            save = true;
                        }
                    }
                });
            });
        });
        if modal.should_close() || close {
            self.prepare_open = false;
        }
        if place {
            if let Some(h) = head {
                self.place_at_head(bed, h);
            }
        }
        if save {
            self.save_job();
        }
        if frame {
            self.start_job(JobKind::Frame);
        }
        if start || dry {
            self.start_job(if dry { JobKind::DryRun } else { JobKind::Job });
        }
    }
}

// ---------------------------------------------------------------- widgets

fn modal_frame() -> Frame {
    let pal = theme::p();
    Frame::new().fill(pal.panel).corner_radius(16).inner_margin(Margin::same(22)).stroke(Stroke::new(1.0, pal.border)).shadow(egui::Shadow { offset: [0, 12], blur: 40, spread: 0, color: Color32::from_black_alpha(if pal.dark { 140 } else { 50 }) })
}

/// An iOS-style switch.
fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let pal = theme::p();
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(36.0, 20.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let fill = if *on { pal.accent_fill } else { pal.border };
    ui.painter().rect_filled(rect, 10.0, fill);
    let x = egui::lerp((rect.left() + 10.0)..=(rect.right() - 10.0), t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 8.0, Color32::WHITE);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A line of text with an icon in front, in a colour (warnings, results).
fn notice(ui: &mut egui::Ui, icon: Icon, color: Color32, text: &str) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (r, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
        icons::paint(ui.painter(), r.translate(Vec2::new(0.0, 1.0)), icon, color);
        ui.add(egui::Label::new(RichText::new(text).color(color).size(12.0)).wrap());
    });
}

/// A small icon and value (design size, time).
fn stat(ui: &mut egui::Ui, icon: Icon, text: &str) {
    let pal = theme::p();
    let (r, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
    icons::paint(ui.painter(), r, icon, pal.muted);
    ui.label(RichText::new(text).color(pal.muted).medium());
}

/// A centred icon and text where a list is still empty.
fn empty_state(ui: &mut egui::Ui, icon: Icon, text: &str) {
    let pal = theme::p();
    Frame::new().fill(pal.card).corner_radius(12).inner_margin(Margin::same(18)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.vertical_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
            icons::paint(ui.painter(), r, icon, pal.muted.gamma_multiply(0.7));
            ui.add_space(4.0);
            ui.label(RichText::new(text).color(pal.muted).size(12.5));
        });
    });
}

/// Speed / power / passes / air (and line spacing) for one operation.
fn settings_editor(ui: &mut egui::Ui, op: Operation, s: &mut OpSettings, id: impl std::hash::Hash + std::fmt::Debug) -> bool {
    let pal = theme::p();
    let mut changed = false;
    let row = |ui: &mut egui::Ui, icon: Icon, label: String| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
            icons::paint(ui.painter(), r, icon, pal.muted);
            ui.label(RichText::new(label).color(pal.muted));
        });
    };
    egui::Grid::new(id).num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
        row(ui, Icon::Gauge, t!("speed"));
        changed |= ui.add_sized([110.0, 24.0], egui::DragValue::new(&mut s.speed).range(1.0..=2000.0).suffix(" mm/s").speed(0.5)).changed();
        ui.end_row();
        row(ui, Icon::Zap, t!("power"));
        ui.horizontal(|ui| {
            changed |= ui.add_sized([62.0, 24.0], egui::DragValue::new(&mut s.min_power).range(0.0..=s.max_power).suffix(" %").speed(0.5)).changed();
            ui.label(RichText::new("–").color(pal.muted));
            changed |= ui.add_sized([62.0, 24.0], egui::DragValue::new(&mut s.max_power).range(s.min_power..=100.0).suffix(" %").speed(0.5)).changed();
        });
        ui.end_row();
        if op.uses_line_interval() {
            row(ui, Icon::Rows, t!("line-spacing"));
            changed |= ui.add_sized([110.0, 24.0], egui::DragValue::new(&mut s.line_interval_mm).range(0.01..=1.0).suffix(" mm").speed(0.005).max_decimals(3)).changed();
            ui.end_row();
        }
        row(ui, Icon::Repeat, t!("passes"));
        changed |= ui.add_sized([62.0, 24.0], egui::DragValue::new(&mut s.passes).range(1..=10)).changed();
        ui.end_row();
        row(ui, Icon::Wind, t!("air-assist"));
        changed |= toggle(ui, &mut s.air_assist).changed();
        ui.end_row();
    });
    changed
}

/// The 3×3 start point picker, drawn as a small design box with 9 dots.
fn anchor_picker(ui: &mut egui::Ui, anchor: &mut Anchor) {
    let pal = theme::p();
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
    let inner = rect.shrink(11.0);
    ui.painter().rect_filled(inner, 4.0, pal.ok.gamma_multiply(0.06));
    ui.painter().rect_stroke(inner, 4.0, Stroke::new(1.0, pal.muted.gamma_multiply(0.5)), egui::StrokeKind::Middle);
    for v in [-1i8, 0, 1] {
        for h in [-1i8, 0, 1] {
            let c = egui::pos2(inner.min.x + inner.width() * (h + 1) as f32 / 2.0, inner.min.y + inner.height() * (v + 1) as f32 / 2.0);
            let a = Anchor { h, v };
            let r = Rect::from_center_size(c, Vec2::splat(15.0));
            let resp = ui.interact(r, ui.scope_id().with(("anchor", h, v)), Sense::click()).on_hover_text(a.label()).on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                *anchor = a;
            }
            let on = *anchor == a;
            let sq = Rect::from_center_size(c, Vec2::splat(if on { 12.0 } else { 7.0 }));
            if on {
                ui.painter().rect_filled(sq, 3.0, pal.ok);
            } else {
                ui.painter().rect_filled(sq, 2.0, if resp.hovered() { pal.muted } else { pal.panel });
                ui.painter().rect_stroke(sq, 2.0, Stroke::new(1.0, pal.muted), egui::StrokeKind::Middle);
            }
        }
    }
}

/// Whether the design fits on the bed with the head at `head` (design mm,
/// as seen from the front). `extent` = room needed left, right, up, down.
fn fits_from_head(bed: (f64, f64), (hx, hy): Pt, (left, right, up, down): (f64, f64, f64, f64)) -> Option<String> {
    let mut sides = Vec::new();
    if hx - left < 0.0 {
        sides.push(t!("edge-left"));
    }
    if hx + right > bed.0 {
        sides.push(t!("edge-right"));
    }
    if hy - up < 0.0 {
        sides.push(t!("edge-top"));
    }
    if hy + down > bed.1 {
        sides.push(t!("edge-bottom"));
    }
    (!sides.is_empty()).then(|| t!("runs-past-edge", sides = sides.join(", ")))
}

/// Sketch of the design with the head at its start point.
fn start_corner_diagram(ui: &mut egui::Ui, size: (f64, f64), anchor: Anchor) {
    let pal = theme::p();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(220.0, 90.0), Sense::hover());
    let p = ui.painter();
    let aspect = (size.0 / size.1.max(1.0)).clamp(0.4, 4.0) as f32;
    let h = 60.0f32.min(150.0 / aspect);
    let w = h * aspect;
    let design = Rect::from_min_size(rect.min + Vec2::new(20.0, 18.0), Vec2::new(w, h));
    p.rect_filled(design, 4.0, pal.accent.gamma_multiply(0.08));
    p.rect_stroke(design, 4.0, Stroke::new(1.0, pal.accent.gamma_multiply(0.7)), egui::StrokeKind::Middle);
    p.text(design.center(), egui::Align2::CENTER_CENTER, t!("diagram-design"), egui::FontId::proportional(11.0), pal.muted);
    let pick = |a: f32, b: f32, t: i8| match t {
        -1 => a,
        0 => (a + b) / 2.0,
        _ => b,
    };
    let head = egui::pos2(pick(design.min.x, design.max.x, anchor.h), pick(design.min.y, design.max.y, anchor.v));
    p.circle_filled(head, 9.0, pal.accent.gamma_multiply(0.2));
    p.circle_stroke(head, 6.0, Stroke::new(1.5, pal.accent));
    p.circle_filled(head, 2.5, pal.accent);
    p.text(design.right_center() + Vec2::new(14.0, 0.0), egui::Align2::LEFT_CENTER, t!("diagram-head"), egui::FontId::proportional(11.5), pal.text);
}

/// A shortcut as the platform writes it (⌘S on macOS, Ctrl+S elsewhere).
fn shortcut(ctx: &egui::Context, modifiers: Modifiers, key: Key) -> String {
    ctx.format_shortcut(&egui::KeyboardShortcut::new(modifiers, key))
}

/// The key that deletes, as printed on it.
fn delete_key() -> String {
    if cfg!(target_os = "macos") { "⌫".into() } else { "Del".into() }
}

/// The machine's state ("idle", …) in the current language.
fn state_label(state: &str) -> String {
    match state {
        "idle" => t!("state-idle"),
        "moving" => t!("state-moving"),
        "job running" => t!("state-job-running"),
        "part end" => t!("state-part-end"),
        other => other.to_string(),
    }
}

fn short_port(name: &str) -> String {
    name.trim_start_matches("/dev/").to_string()
}

fn fmt_duration(secs: f64) -> String {
    let s = secs.round() as u64;
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => t!("duration-min", min = s / 60, sec = format!("{:02}", s % 60)),
        _ => t!("duration-h", h = s / 3600, min = format!("{:02}", (s % 3600) / 60)),
    }
}
