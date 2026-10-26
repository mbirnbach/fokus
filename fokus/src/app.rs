//! The main window, laid out like common laser software:
//! top bar (file, undo, machine), properties bar for the selection,
//! tool palette on the left, the bed in the middle, material and layers on
//! the right with the start button, and a colour palette at the bottom.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Frame, Key, Layout, Margin, Modifiers, RichText, Sense, Stroke, Vec2};
use ruida::serial::{list_ports, PortInfo};

use crate::canvas::{Canvas, Edit, Scene, Tool, ZoomTarget};
use crate::design::{Action, Anchor, Design, Font, History, Kind, LayerProblem, Object, Pt, MIN_SIZE};
use crate::job::Job;
use crate::machine::{Conn, JobPhase, Machine, Snapshot};
use crate::materials::{Library, Material, OpSettings, Operation};
use crate::project::{self, Project};
use crate::t;
use crate::theme;

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
    draw_color: [u8; 3],
    polygon_sides: u32,
    lock_aspect: bool,
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
            draw_color: PALETTE[0],
            polygon_sides: 6,
            lock_aspect: true,
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

    fn pick_file(&mut self, snap: &Snapshot) {
        if let Some(path) = rfd::FileDialog::new().add_filter(&t!("filter-all"), &[project::EXTENSION, "svg", "rd"]).pick_file() {
            self.open_file(&path, Some(snap));
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
        let modal = egui::Modal::new(egui::Id::unique("unsaved")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new(t!("unsaved-title", name = self.project_name())).size(18.0).strong());
            ui.label(RichText::new(t!("unsaved-text")).color(pal.muted));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(t!("dont-save")).clicked() {
                    choice = Some(false);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(primary_button(&t!("save"))).clicked() {
                        choice = Some(true);
                    }
                    if ui.button(t!("cancel")).clicked() {
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

    /// Send the job to the laser; it starts at the head right away. A dry
    /// run sends the same moves with the laser off.
    fn start_job(&mut self, dry_run: bool) {
        let estimate: f64 = self.layer_estimates().iter().flatten().map(|(_, _, t)| *t).sum();
        let result = self
            .design
            .to_job(self.material(), self.origin_right, self.anchor)
            .map(|mut job| {
                if dry_run {
                    for l in &mut job {
                        l.min_power = 0.0;
                        l.max_power = 0.0;
                        l.air_assist = false;
                    }
                }
                job
            })
            .and_then(|job| ruida::writer::write_plain(&job).map_err(|e| e.to_string()))
            .and_then(|plain| self.machine.start_job(plain, estimate, dry_run));
        match result {
            Ok(()) => {
                self.prepare_open = false;
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

    fn delete_selection(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let sel = std::mem::take(&mut self.selection);
        self.edit(|d, _| d.remove(&sel));
    }

    fn undo(&mut self) {
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
        if ctx.egui_wants_keyboard_input() || self.prepare_open {
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
            if let Some(path) = rfd::FileDialog::new().add_filter(&t!("filter-all"), &[project::EXTENSION, "svg", "rd"]).pick_file() {
                self.open_file(&path, None);
            }
        }
        if new {
            self.request(Pending::New);
        }
        let (redo, undo, dup, ungroup, group, all) = ctx.input_mut(|i| {
            (
                i.consume_key(cmd_shift, Key::Z),
                i.consume_key(Modifiers::COMMAND, Key::Z),
                i.consume_key(Modifiers::COMMAND, Key::D),
                i.consume_key(cmd_shift, Key::G),
                i.consume_key(Modifiers::COMMAND, Key::G),
                i.consume_key(Modifiers::COMMAND, Key::A),
            )
        });
        if redo {
            self.redo();
        } else if undo {
            self.undo();
        }
        if dup && !self.selection.is_empty() {
            let sel = self.selection.clone();
            self.history.checkpoint(&self.design);
            self.selection = self.design.duplicate(&sel);
        }
        if ungroup {
            let sel = self.selection.clone();
            self.edit(|d, _| d.ungroup(&sel));
        } else if group {
            let sel = self.selection.clone();
            self.edit(|d, _| d.group(&sel));
        }
        if all {
            self.selection = (0..self.design.objects.len()).collect();
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
                    self.tool = tool;
                    self.canvas.cancel_line();
                }
            }
        }
        if delete {
            self.delete_selection();
        }
        if escape && self.tool == Tool::Select {
            self.selection.clear();
        } else if escape {
            self.tool = Tool::Select;
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
            .exact_size(52.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::symmetric(14, 8)).stroke(Stroke::new(1.0, pal.border)))
            .show(ui, |ui| self.top_bar(ui, &snap));

        egui::Panel::top("properties")
            .exact_size(46.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::symmetric(14, 6)).stroke(Stroke::new(1.0, pal.border)))
            .show(ui, |ui| self.properties_bar(ui, &snap));

        egui::Panel::bottom("palette")
            .exact_size(44.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::symmetric(14, 6)).stroke(Stroke::new(1.0, pal.border)))
            .show(ui, |ui| self.palette_bar(ui));

        egui::Panel::left("tools")
            .exact_size(56.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::symmetric(6, 10)).stroke(Stroke::new(1.0, pal.border)))
            .show(ui, |ui| self.tool_palette(ui));

        egui::Panel::right("job")
            .exact_size(340.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::same(14)).stroke(Stroke::new(1.0, pal.border)))
            .show(ui, |ui| self.job_panel(ui, &snap));

        let connected = snap.conn == Conn::Connected;
        let bed = self.bed(&snap);
        let material = self.material().cloned();
        let head = snap.status.as_ref().filter(|_| connected).map(|s| s.position);
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
            polygon_sides: self.polygon_sides,
            lock_aspect: self.lock_aspect,
        };
        let has_content = !self.design.is_empty() || self.job.is_some();
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let rect = ui.max_rect();
            self.canvas.ui(ui, &scene, Edit { design: &mut self.design, selection: &mut self.selection, history: &mut self.history, tool: &mut self.tool });
            egui::Area::new(egui::Id::unique("zoom")).fixed_pos(rect.right_bottom() - Vec2::new(214.0, 50.0)).order(egui::Order::Middle).show(ui.ctx(), |ui| {
                Frame::new().fill(pal.panel).stroke(Stroke::new(1.0, pal.border)).corner_radius(10).inner_margin(Margin::same(4)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.add_enabled(has_content, egui::Button::new(t!("zoom-design")).frame(false)).clicked() {
                            self.canvas.zoom_to(ZoomTarget::Design);
                        }
                        if ui.add(egui::Button::new(t!("zoom-bed")).frame(false)).clicked() {
                            self.canvas.zoom_to(ZoomTarget::Bed);
                        }
                    });
                });
            });
        });

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
            logo(ui);
            ui.label(RichText::new("Fokus").size(19.0).strong());
            ui.add_space(18.0);
            ui.menu_button(t!("menu-file"), |ui| {
                ui.set_min_width(220.0);
                let ctx = ui.ctx().clone();
                let item = |ui: &mut egui::Ui, label: String, key: String, enabled: bool| ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(key)).clicked();
                let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
                if item(ui, t!("menu-new"), shortcut(&ctx, Modifiers::COMMAND, Key::N), true) {
                    self.request(Pending::New);
                }
                if item(ui, t!("menu-open"), shortcut(&ctx, Modifiers::COMMAND, Key::O), true) {
                    self.pick_file(snap);
                }
                ui.separator();
                if item(ui, t!("save"), shortcut(&ctx, Modifiers::COMMAND, Key::S), !self.design.is_empty()) {
                    self.save_project(false);
                }
                if item(ui, t!("menu-save-as"), shortcut(&ctx, cmd_shift, Key::S), !self.design.is_empty()) {
                    self.save_project(true);
                }
                ui.separator();
                let can_export = !self.design.is_empty() && self.material().is_some() && self.design.problems(self.material()).is_empty();
                if item(ui, t!("menu-export"), String::new(), can_export) {
                    self.save_job();
                }
            });
            if ui.button(t!("save")).on_hover_text(t!("save-hint", key = shortcut(ui.ctx(), Modifiers::COMMAND, Key::S))).clicked() {
                self.save_project(false);
            }
            ui.add_space(8.0);
            if ui.add_enabled(self.history.can_undo(), egui::Button::new(t!("undo"))).on_hover_text(shortcut(ui.ctx(), Modifiers::COMMAND, Key::Z)).clicked() {
                self.undo();
            }
            if ui.add_enabled(self.history.can_redo(), egui::Button::new(t!("redo"))).on_hover_text(shortcut(ui.ctx(), Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)).clicked() {
                self.redo();
            }
            if let Some(e) = &self.file_error {
                ui.label(RichText::new(e).color(pal.err).size(12.0));
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(t!("settings")).clicked() {
                    self.show_settings = !self.show_settings;
                }
                if ui.button(t!("materials")).clicked() {
                    self.show_library = true;
                }
                let connected = snap.conn == Conn::Connected;
                let job_phase = snap.job.as_ref().filter(|j| j.phase.active()).map(|j| j.phase.clone());
                let (color, text) = match (&snap.conn, snap.status.as_ref()) {
                    (Conn::Connected, _) if job_phase == Some(JobPhase::Paused) => (pal.warn, t!("status-paused")),
                    (Conn::Connected, _) if job_phase.is_some() => {
                        let dry = snap.job.as_ref().is_some_and(|j| j.dry_run);
                        (pal.accent, if dry { t!("status-dry-run") } else { t!("status-lasering") })
                    }
                    (Conn::Connected, Some(s)) => (if s.state() == "idle" { pal.ok } else { pal.warn }, format!("{}  ·  X {:.1}  Y {:.1}", state_label(s.state()), s.position.0, s.position.1)),
                    (Conn::Connecting, _) => (pal.warn, t!("status-connecting")),
                    (Conn::Failed(_), _) => (pal.err, t!("status-problem")),
                    _ => (pal.muted, t!("status-offline")),
                };
                let tip = if connected { t!("status-tip-connected") } else { t!("status-tip") };
                if pill(ui, color, &text).on_hover_text(tip).clicked() {
                    self.show_settings = true;
                }
            });
        });
    }

    fn tool_palette(&mut self, ui: &mut egui::Ui) {
        let pal = theme::p();
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            for tool in Tool::ALL {
                let on = self.tool == tool;
                let (rect, response) = ui.allocate_exact_size(Vec2::splat(42.0), Sense::click());
                let fill = if on {
                    pal.accent.gamma_multiply(0.16)
                } else if response.hovered() {
                    pal.hover
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(rect, 9.0, fill);
                tool_icon(ui.painter(), rect, tool, if on { pal.accent } else { pal.text });
                let key = shortcut(ui.ctx(), Modifiers::NONE, tool.key());
                if response.on_hover_text(format!("{}  ({key})", tool.label())).clicked() {
                    self.tool = tool;
                    self.canvas.cancel_line();
                }
                if tool == Tool::Select {
                    ui.add_space(4.0);
                    let (r, _) = ui.allocate_exact_size(Vec2::new(30.0, 1.0), Sense::hover());
                    ui.painter().rect_filled(r, 0.0, pal.border);
                    ui.add_space(4.0);
                }
            }
        });
    }

    fn palette_bar(&mut self, ui: &mut egui::Ui) {
        let pal = theme::p();
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new(t!("layer-colour")).color(pal.muted).size(12.0));
            for (i, &c) in PALETTE.iter().enumerate() {
                let (rect, response) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                let color = Color32::from_rgb(c[0], c[1], c[2]);
                let current = self.draw_color == c;
                ui.painter().rect_filled(rect.shrink(2.0), 5.0, color);
                if current {
                    ui.painter().rect_stroke(rect, 6.0, Stroke::new(2.0, pal.accent), egui::StrokeKind::Inside);
                } else if response.hovered() {
                    ui.painter().rect_stroke(rect, 6.0, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
                }
                let tip = if self.selection.is_empty() { t!("palette-tip", colour = theme::color_name(c), layer = i + 1) } else { t!("palette-tip-selection", colour = theme::color_name(c), layer = i + 1) };
                if response.on_hover_text(tip).clicked() {
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
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some((x, y)) = self.canvas.hover {
                    ui.label(RichText::new(format!("X {x:7.1}   Y {y:7.1} mm")).monospace().color(pal.muted));
                }
                ui.label(RichText::new(t!("canvas-hint")).color(pal.muted).size(12.0));
            });
        });
    }

    fn properties_bar(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let pal = theme::p();
        let bed = self.bed(snap);
        ui.horizontal_centered(|ui| {
            let Some(((x0, y0), (x1, y1))) = self.design.selection_bounds(&self.selection) else {
                ui.label(RichText::new(self.tool.label()).strong());
                ui.label(RichText::new(self.tool.hint()).color(pal.muted));
                if self.tool == Tool::Polygon {
                    ui.add_space(12.0);
                    ui.label(RichText::new(t!("sides")).color(pal.muted));
                    ui.add(egui::DragValue::new(&mut self.polygon_sides).range(3..=24));
                }
                return;
            };
            let sel = self.selection.clone();

            // Position of the corner nearest the machine origin, in machine coordinates.
            let corner_x = if self.origin_right { bed.0 - x1 } else { x0 };
            let (w, h) = (x1 - x0, y1 - y0);
            let mut vals = [corner_x, y0, w, h];
            let labels = ["X", "Y", "W", "H"];
            let mut edited = None;
            for (k, label) in labels.iter().enumerate() {
                ui.label(RichText::new(*label).color(pal.muted));
                let r = ui.add(egui::DragValue::new(&mut vals[k]).speed(0.1).max_decimals(2).suffix(" mm").range(if k >= 2 { MIN_SIZE..=2000.0 } else { -2000.0..=2000.0 }));
                if r.drag_started() || r.gained_focus() {
                    self.history.checkpoint(&self.design);
                }
                if r.changed() {
                    edited = Some(k);
                }
                if k == 1 {
                    ui.add_space(6.0);
                }
            }
            let lock = if self.lock_aspect { "🔒" } else { "🔓" };
            if ui.selectable_label(self.lock_aspect, lock).on_hover_text(t!("keep-proportions")).clicked() {
                self.lock_aspect = !self.lock_aspect;
            }
            if let Some(k) = edited {
                let [nx, ny, mut nw, mut nh] = vals;
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

            ui.separator();
            // Per-object settings for a single object.
            if let [i] = sel[..] {
                let mut rot = self.design.objects[i].rotation;
                ui.label(RichText::new("↻").color(pal.muted)).on_hover_text(t!("rotation"));
                let r = ui.add(egui::DragValue::new(&mut rot).speed(1.0).suffix("°").range(-360.0..=360.0));
                if r.drag_started() || r.gained_focus() {
                    self.history.checkpoint(&self.design);
                }
                if r.changed() {
                    self.design.objects[i].rotation = rot;
                    self.design.touch();
                }
                self.object_fields(ui, i);
                ui.separator();
            }

            if ui.button(t!("duplicate")).on_hover_text(shortcut(ui.ctx(), Modifiers::COMMAND, Key::D)).clicked() {
                self.history.checkpoint(&self.design);
                self.selection = self.design.duplicate(&sel);
            }
            let grouped = sel.len() > 1 && sel.iter().all(|&i| self.design.objects[i].group.is_some() && self.design.objects[i].group == self.design.objects[sel[0]].group);
            if grouped {
                if ui.button(t!("ungroup")).on_hover_text(shortcut(ui.ctx(), Modifiers::COMMAND | Modifiers::SHIFT, Key::G)).clicked() {
                    self.edit(|d, _| d.ungroup(&sel));
                }
            } else if ui.add_enabled(sel.len() > 1, egui::Button::new(t!("group"))).on_hover_text(shortcut(ui.ctx(), Modifiers::COMMAND, Key::G)).clicked() {
                self.edit(|d, _| d.group(&sel));
            }
            if ui.button("⇋").on_hover_text(t!("mirror-h")).clicked() {
                self.edit(|d, _| {
                    for &i in &sel {
                        d.objects[i].flip_x = !d.objects[i].flip_x;
                    }
                });
            }
            if ui.button("⇵").on_hover_text(t!("mirror-v")).clicked() {
                self.edit(|d, _| {
                    for &i in &sel {
                        d.objects[i].flip_y = !d.objects[i].flip_y;
                    }
                });
            }
            if sel.len() > 1 {
                ui.menu_button(t!("align"), |ui| {
                    let mut align = |f: &dyn Fn((f64, f64, f64, f64), (f64, f64, f64, f64)) -> (f64, f64)| {
                        self.edit(|d, _| {
                            for &i in &sel {
                                let ((a0, b0), (a1, b1)) = d.objects[i].bounds();
                                let delta = f((x0, y0, x1, y1), (a0, b0, a1, b1));
                                d.objects[i].x += delta.0;
                                d.objects[i].y += delta.1;
                            }
                        });
                    };
                    if ui.button(t!("align-left")).clicked() {
                        align(&|s, o| (s.0 - o.0, 0.0));
                    }
                    if ui.button(t!("align-hcentre")).clicked() {
                        align(&|s, o| ((s.0 + s.2) / 2.0 - (o.0 + o.2) / 2.0, 0.0));
                    }
                    if ui.button(t!("align-right")).clicked() {
                        align(&|s, o| (s.2 - o.2, 0.0));
                    }
                    ui.separator();
                    if ui.button(t!("align-top")).clicked() {
                        align(&|s, o| (0.0, s.1 - o.1));
                    }
                    if ui.button(t!("align-vcentre")).clicked() {
                        align(&|s, o| (0.0, (s.1 + s.3) / 2.0 - (o.1 + o.3) / 2.0));
                    }
                    if ui.button(t!("align-bottom")).clicked() {
                        align(&|s, o| (0.0, s.3 - o.3));
                    }
                });
            }
            if ui.button(t!("delete")).on_hover_text(t!("delete-hint")).clicked() {
                self.delete_selection();
            }
        });
    }

    /// Settings specific to one object: corner radius, sides, text.
    fn object_fields(&mut self, ui: &mut egui::Ui, i: usize) {
        let pal = theme::p();
        let focus_text = self.canvas.new_text == Some(i);
        match self.design.objects[i].kind.clone() {
            Kind::Rect { mut radius } => {
                ui.label(RichText::new(t!("corners")).color(pal.muted));
                let r = ui.add(egui::DragValue::new(&mut radius).speed(0.1).suffix(" mm").range(0.0..=500.0));
                if r.drag_started() || r.gained_focus() {
                    self.history.checkpoint(&self.design);
                }
                if r.changed() {
                    self.design.objects[i].kind = Kind::Rect { radius };
                    self.design.touch();
                }
            }
            Kind::Polygon { mut sides } => {
                ui.label(RichText::new(t!("sides")).color(pal.muted));
                let r = ui.add(egui::DragValue::new(&mut sides).range(3..=24));
                if r.drag_started() || r.gained_focus() {
                    self.history.checkpoint(&self.design);
                }
                if r.changed() {
                    self.design.objects[i].kind = Kind::Polygon { sides };
                    self.design.touch();
                }
            }
            Kind::Text { mut text, mut font, mut bold, .. } => {
                let mut changed = false;
                let id = ui.make_persistent_id(("text_field", i));
                let out = egui::TextEdit::singleline(&mut text).id(id).desired_width(180.0).hint_text(t!("type-text")).show(ui);
                if out.response.gained_focus() {
                    self.history.checkpoint(&self.design);
                }
                changed |= out.response.changed();
                if focus_text {
                    out.response.request_focus();
                    let mut state = out.state;
                    state.cursor.set_char_range(Some(egui::text_selection::CCursorRange::select_all_str(&text)));
                    state.store(ui.ctx(), out.response.id);
                    self.canvas.new_text = None;
                }
                egui::ComboBox::from_id_salt(("font", i)).width(70.0).selected_text(font.label()).show_ui(ui, |ui| {
                    for f in Font::ALL {
                        changed |= ui.selectable_value(&mut font, f, f.label()).changed();
                    }
                });
                if ui.selectable_label(bold, RichText::new(t!("bold-letter")).strong()).on_hover_text(t!("bold")).clicked() {
                    bold = !bold;
                    changed = true;
                }
                if changed {
                    if !out.response.has_focus() {
                        self.history.checkpoint(&self.design);
                    }
                    if self.design.objects[i].set_text(&text, font, bold).is_ok() {
                        self.design.touch();
                    }
                }
            }
            _ => {}
        }
    }
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
        egui::Panel::bottom("start").frame(Frame::new().fill(pal.panel).inner_margin(Margin { top: 12, ..Margin::ZERO })).show(ui, |ui| self.start_footer(ui, snap, ready));

        egui::ScrollArea::vertical().show(ui, |ui| {
            section(ui, &t!("section-material"));
            self.material_picker(ui);
            ui.add_space(14.0);
            section(ui, &t!("section-layers"));
            if self.design.is_empty() {
                ui.label(RichText::new(t!("layers-empty")).color(pal.muted).size(12.5));
            } else {
                self.layer_list(ui);
            }
        });
    }

    fn material_picker(&mut self, ui: &mut egui::Ui) {
        let pal = theme::p();
        let current = self.material.clone().unwrap_or_else(|| t!("choose-material"));
        let text = if self.material.is_some() { RichText::new(current).strong() } else { RichText::new(current).color(pal.accent).strong() };
        egui::ComboBox::from_id_salt("material").width(ui.available_width()).height(460.0).selected_text(text).show_ui(ui, |ui| {
            for m in &self.library.materials {
                let ops: Vec<String> = Operation::ALL.iter().filter(|op| m.settings(**op).is_some()).map(|op| op.label()).collect();
                if ui.selectable_label(self.material.as_ref() == Some(&m.name), format!("{}   ·   {}", m.name, ops.join(", "))).clicked() {
                    self.material = Some(m.name.clone());
                }
            }
        });
        if let Some(m) = self.material() {
            ui.horizontal_wrapped(|ui| {
                for op in Operation::ALL {
                    chip(ui, &op.label(), m.settings(op).is_some());
                }
            });
            if !m.verified {
                ui.label(RichText::new(t!("untested-warning")).color(pal.warn).size(12.0));
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
            Frame::new().fill(pal.card).stroke(Stroke::new(1.0, pal.border)).corner_radius(10).inner_margin(Margin::same(10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (rect, swatch) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                    ui.painter().rect_filled(rect, 4.0, Color32::from_rgb(layer.color[0], layer.color[1], layer.color[2]));
                    if swatch.on_hover_text(t!("select-layer")).clicked() {
                        select_color = Some(layer.color);
                    }
                    ui.label(RichText::new(theme::color_name(layer.color)).strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let time = estimates[idx].as_ref().map(|(_, _, t)| *t);
                        if let Some(t) = time {
                            ui.label(RichText::new(format!("≈ {}", fmt_duration(t))).color(pal.muted).size(12.0));
                        }
                    });
                });
                let before = layer.action;
                segmented(ui, &mut layer.action, &[(Action::Do(Operation::Cut), Operation::Cut.label()), (Action::Do(Operation::Mark), Operation::Mark.label()), (Action::Do(Operation::Engrave), Operation::Engrave.label()), (Action::Skip, t!("action-skip"))]);
                if layer.action != before {
                    layer.adjusted = None;
                }
                match layer.resolve(material.as_ref()) {
                    Err(LayerProblem::NoMaterial) => {
                        if let Action::Do(op) = layer.action {
                            ui.label(RichText::new(op.explain()).color(pal.muted).size(12.0));
                        }
                    }
                    Err(p) => {
                        ui.label(RichText::new(p.message(material.as_ref())).color(pal.err).size(12.0));
                    }
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
                            if layer.adjusted.is_some() && ui.small_button(t!("back-to-material")).clicked() {
                                layer.adjusted = None;
                            }
                        });
                    }
                }
            });
            ui.add_space(6.0);
        }
        if let Some(c) = select_color {
            self.selection = (0..self.design.objects.len()).filter(|&i| self.design.objects[i].color == c).collect();
            self.tool = Tool::Select;
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
                ui.label(RichText::new(format!("{:.0} × {:.0} mm", x1 - x0, y1 - y0)).color(pal.muted));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(time).color(pal.muted));
                });
            });
            if let Some(head) = snap.status.as_ref().filter(|_| snap.conn == Conn::Connected).map(|s| s.position) {
                let off = self.head_offset(bed, head).unwrap_or(0.0);
                if off > 0.5 && ui.add_sized([ui.available_width(), 30.0], egui::Button::new(t!("place-at-head"))).on_hover_text(t!("place-at-head-hint")).clicked() {
                    self.place_at_head(bed, head);
                }
            }
        }
        ui.horizontal(|ui| {
            anchor_picker(ui, &mut self.anchor);
            ui.vertical(|ui| {
                ui.label(RichText::new(t!("head-position")).strong());
                ui.label(RichText::new(t!("head-position-hint", point = self.anchor.label())).color(pal.muted).size(12.0));
            });
        });
        ui.add_space(6.0);
        if snap.job.is_some() {
            self.job_card(ui, snap);
            if snap.job_active() {
                return;
            }
        }
        let start = primary_button(&t!("start-job-dots")).corner_radius(10);
        if ui.add_enabled_ui(ready, |ui| ui.add_sized([ui.available_width(), 44.0], start)).inner.clicked() {
            self.checklist = [false; CHECKLIST];
            self.prepare_open = true;
        }
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
            ui.label(RichText::new(msg).color(if *ok { pal.ok } else { pal.err }).size(12.0));
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
                let title = match (&job.phase, job.dry_run) {
                    (JobPhase::Paused, _) => t!("status-paused"),
                    (_, true) => t!("dry-run-running"),
                    _ => t!("status-lasering"),
                };
                (title, if job.phase == JobPhase::Paused { pal.warn } else { pal.accent }, f, t!("time-of", elapsed = clock(elapsed), total = clock(job.estimate_s)))
            }
            JobPhase::Finished => (if job.dry_run { t!("dry-run-finished") } else { t!("job-finished") }, pal.ok, 1.0, t!("took", time = clock(elapsed))),
            JobPhase::Stopped => (t!("job-stopped"), pal.warn, 0.0, t!("after", time = clock(elapsed))),
            JobPhase::Failed(e) => (t!("job-failed"), pal.err, 0.0, e.clone()),
        };
        Frame::new().fill(color.gamma_multiply(0.08)).stroke(Stroke::new(1.0, color.gamma_multiply(0.5))).corner_radius(10).inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).strong().color(color));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(detail).color(pal.muted).size(12.0));
                });
            });
            ui.add(egui::ProgressBar::new(fraction).fill(color).desired_height(8.0));
            if job.phase.active() {
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 8.0) / 2.0;
                    match job.phase {
                        JobPhase::Paused => {
                            if ui.add_sized([w, 36.0], egui::Button::new(t!("resume"))).clicked() {
                                self.machine.resume_job();
                            }
                        }
                        JobPhase::Running => {
                            if ui.add_sized([w, 36.0], egui::Button::new(t!("pause"))).clicked() {
                                self.machine.pause_job();
                            }
                        }
                        _ => {
                            ui.add_enabled_ui(false, |ui| ui.add_sized([w, 36.0], egui::Button::new(t!("pause"))));
                        }
                    }
                    let stop = egui::Button::new(RichText::new(t!("stop")).color(Color32::WHITE).strong()).fill(pal.err).stroke(Stroke::NONE);
                    if ui.add_sized([w, 36.0], stop).on_hover_text(t!("stop-hint")).clicked() {
                        self.machine.stop_job();
                    }
                });
            } else if ui.small_button(t!("dismiss")).clicked() {
                self.machine.clear_job();
            }
        });
        ui.add_space(8.0);
    }

    fn job_file_panel(&mut self, ui: &mut egui::Ui) {
        let pal = theme::p();
        let Some(job) = &self.job else { return };
        section(ui, &t!("section-job-file"));
        card(ui, |ui| {
            ui.label(RichText::new(&job.name).size(16.0).strong());
            ui.label(RichText::new(t!("job-file-hint")).color(pal.muted).size(12.0));
            if let Some(((x0, y0), (x1, y1))) = job.bounds {
                kv(ui, &t!("size"), &format!("{:.1} × {:.1} mm", x1 - x0, y1 - y0));
            }
            let total: f64 = job.toolpath.layers.values().filter_map(Job::layer_seconds).sum();
            kv(ui, &t!("laser-time-label"), &format!("≈ {}", fmt_duration(total)));
        });
        for (id, layer) in &job.toolpath.layers {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    dot(ui, theme::layer_color(*id, layer.color));
                    ui.label(RichText::new(t!("layer-n", n = id + 1)).strong());
                });
                let speed = layer.speed.map(|s| format!("{s:.0} mm/s")).unwrap_or_else(|| "—".into());
                let power = match (layer.min_power, layer.max_power) {
                    (Some(a), Some(b)) if (a - b).abs() < 0.05 => format!("{b:.0} %"),
                    (Some(a), Some(b)) => format!("{a:.0}–{b:.0} %"),
                    _ => "—".into(),
                };
                kv(ui, &t!("speed"), &speed);
                kv(ui, &t!("power"), &power);
            });
        }
        if ui.button(t!("close-job-file")).clicked() {
            self.job = None;
        }
    }
}

// ---------------------------------------------------------------- windows

impl FokusApp {
    fn settings_window(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let pal = theme::p();
        let mut open = self.show_settings;
        egui::Window::new(t!("settings")).open(&mut open).collapsible(false).resizable(false).default_width(420.0).show(ctx, |ui| {
            section(ui, &t!("section-machine"));
            let busy = matches!(snap.conn, Conn::Connected | Conn::Connecting);
            match &snap.conn {
                Conn::Connected => ui.label(RichText::new(t!("conn-connected")).color(pal.ok)),
                Conn::Connecting => ui.label(t!("status-connecting")),
                Conn::Failed(e) => ui.label(RichText::new(e).color(pal.err)),
                Conn::Disconnected => ui.label(RichText::new(t!("conn-disconnected")).color(pal.muted)),
            };
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    let selected = self.port.clone().unwrap_or_else(|| t!("no-laser-found"));
                    egui::ComboBox::from_id_salt("port").width(250.0).selected_text(short_port(&selected)).show_ui(ui, |ui| {
                        for p in &self.ports {
                            let label = if p.looks_like_ruida { t!("port-laser", port = short_port(&p.name)) } else { short_port(&p.name) };
                            ui.selectable_value(&mut self.port, Some(p.name.clone()), label);
                        }
                    });
                    if ui.button(t!("refresh")).clicked() {
                        self.refresh_ports();
                    }
                });
            });
            if busy {
                if ui.button(t!("disconnect")).clicked() {
                    self.machine.disconnect();
                    self.auto_connect = false;
                }
            } else if ui.add_enabled(self.port.is_some(), primary_button(&t!("connect"))).clicked() {
                if let Some(port) = self.port.clone() {
                    self.auto_connect = true;
                    self.machine.connect(port, ctx.clone());
                }
            }
            if let Some(s) = &snap.status {
                kv(ui, &t!("bed-size"), &format!("{:.0} × {:.0} mm", s.bed.0, s.bed.1));
                kv(ui, &t!("controller"), &format!("Ruida · 0x{:08X}", s.card_id));
            }
            ui.checkbox(&mut self.origin_right, t!("origin-right"));

            ui.add_space(12.0);
            section(ui, &t!("section-display"));
            self.language_picker(ui);
            let mut pref = ctx.options(|o| o.theme_preference);
            let before = pref;
            segmented(ui, &mut pref, &[(egui::ThemePreference::System, t!("theme-system")), (egui::ThemePreference::Light, t!("theme-light")), (egui::ThemePreference::Dark, t!("theme-dark"))]);
            if pref != before {
                ctx.set_theme(pref);
            }
            ui.checkbox(&mut self.show_travel, t!("show-travel"));
        });
        self.show_settings = open;
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
        ui.horizontal(|ui| {
            ui.label(RichText::new(t!("language")).color(pal.muted));
            egui::ComboBox::from_id_salt("language").width(220.0).selected_text(selected).show_ui(ui, |ui| {
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

        let modal = egui::Modal::new(egui::Id::unique("prepare")).show(ctx, |ui| {
            ui.set_width(540.0);
            ui.label(RichText::new(t!("checklist-title")).size(20.0).strong());
            if let Some(m) = &material {
                ui.label(RichText::new(format!("{}  ·  {:.0} × {:.0} mm", m.name, size.0, size.1)).color(pal.muted));
            }
            ui.add_space(8.0);
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
                Frame::new().fill(if self.checklist[i] { pal.ok.gamma_multiply(0.08) } else { pal.card }).corner_radius(10).inner_margin(Margin::same(12)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.checkbox(&mut self.checklist[i], RichText::new(title).strong());
                    ui.horizontal(|ui| {
                        ui.add_space(26.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(text).color(pal.muted).size(12.5));
                            if i == 2 {
                                start_corner_diagram(ui, size, self.anchor);
                                match (head, head_off) {
                                    (Some((hx, hy)), Some(off)) => {
                                        if off > 0.5 {
                                            ui.label(RichText::new(t!("head-elsewhere", x = format!("{hx:.1}"), y = format!("{hy:.1}"))).color(pal.muted).size(12.5));
                                            if ui.button(t!("show-at-head")).clicked() {
                                                place = true;
                                            }
                                        } else {
                                            ui.label(RichText::new(t!("head-matches", x = format!("{hx:.1}"), y = format!("{hy:.1}"))).color(pal.ok).size(12.5));
                                        }
                                        if let Some(problem) = fits_from_head(bed, self.to_view(bed, (hx, hy)), extent) {
                                            ui.label(RichText::new(problem).color(pal.err).size(12.5));
                                        }
                                    }
                                    _ => {
                                        ui.label(RichText::new(t!("connect-to-check")).color(pal.muted).size(12.0));
                                    }
                                }
                            }
                        });
                    });
                });
                ui.add_space(4.0);
            }
            ui.add_space(6.0);
            let connected = head.is_some();
            if connected {
                ui.label(RichText::new(t!("start-warning")).color(pal.muted).size(12.0));
            } else {
                ui.label(RichText::new(t!("not-connected-hint")).color(pal.muted).size(12.0));
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(t!("cancel")).clicked() {
                    close = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let all = self.checklist.iter().all(|&c| c);
                    let checked = t!("checked-count", done = self.checklist.iter().filter(|&&c| c).count(), total = CHECKLIST);
                    if connected {
                        let label = if all { t!("start-job") } else { checked };
                        if ui.add_enabled_ui(all, |ui| ui.add_sized([170.0, 38.0], primary_button(&label))).inner.clicked() {
                            start = true;
                        }
                        if ui.button(t!("save-as-file")).clicked() {
                            save = true;
                        }
                        if ui.button(t!("dry-run")).on_hover_text(t!("dry-run-hint")).clicked() {
                            dry = true;
                        }
                    } else {
                        let label = if all { t!("save-job-file") } else { checked };
                        if ui.add_enabled_ui(all, |ui| ui.add_sized([190.0, 38.0], primary_button(&label))).inner.clicked() {
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
        if start || dry {
            self.start_job(dry);
        }
    }

    fn library_window(&mut self, ctx: &egui::Context) {
        let pal = theme::p();
        let mut open = self.show_library;
        let mut changed = false;
        egui::Window::new(t!("materials")).open(&mut open).default_size([820.0, 560.0]).collapsible(false).show(ctx, |ui| {
            if let Some(e) = &self.library.error {
                ui.label(RichText::new(e).color(pal.err));
            }
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(250.0);
                    ui.add(egui::TextEdit::singleline(&mut self.library_filter).hint_text(t!("search")).desired_width(240.0));
                    let filter = self.library_filter.to_lowercase();
                    egui::ScrollArea::vertical().id_salt("matlist").max_height(400.0).show(ui, |ui| {
                        for (i, m) in self.library.materials.iter().enumerate() {
                            if !filter.is_empty() && !m.name.to_lowercase().contains(&filter) {
                                continue;
                            }
                            let text = if m.verified { RichText::new(&m.name) } else { RichText::new(t!("untested-suffix", name = m.name.clone())) };
                            if ui.add_sized([240.0, 26.0], egui::Button::selectable(self.library_selected == i, text)).clicked() {
                                self.library_selected = i;
                            }
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        if ui.button(t!("lib-new")).clicked() {
                            self.library.materials.push(Material::new_blank());
                            self.library_selected = self.library.materials.len() - 1;
                            changed = true;
                        }
                        if ui.button(t!("duplicate")).clicked() && self.library_selected < self.library.materials.len() {
                            let mut copy = self.library.materials[self.library_selected].clone();
                            copy.name.push_str(&t!("copy-suffix"));
                            copy.verified = false;
                            self.library.materials.push(copy);
                            self.library_selected = self.library.materials.len() - 1;
                            changed = true;
                        }
                        if ui.button(t!("delete")).clicked() && self.library_selected < self.library.materials.len() {
                            self.library.materials.remove(self.library_selected);
                            self.library_selected = self.library_selected.saturating_sub(1);
                            changed = true;
                        }
                    });
                    if ui.button(t!("restore-reference")).on_hover_text(t!("restore-reference-hint")).clicked() {
                        changed |= self.library.restore_reference() > 0;
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    let Some(m) = self.library.materials.get_mut(self.library_selected) else { return };
                    let old_name = m.name.clone();
                    egui::Grid::new("matinfo").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label(RichText::new(t!("name")).color(pal.muted));
                        changed |= ui.add_sized([300.0, 26.0], egui::TextEdit::singleline(&mut m.name)).changed();
                        ui.end_row();
                        ui.label(RichText::new(t!("thickness")).color(pal.muted));
                        changed |= ui.add(egui::DragValue::new(&mut m.thickness_mm).range(0.0..=50.0).suffix(" mm").speed(0.1)).changed();
                        ui.end_row();
                        ui.label(RichText::new(t!("tested")).color(pal.muted));
                        changed |= ui.checkbox(&mut m.verified, t!("tested-hint")).changed();
                        ui.end_row();
                    });
                    if self.material.as_ref() == Some(&old_name) && m.name != old_name {
                        self.material = Some(m.name.clone());
                    }
                    egui::ScrollArea::vertical().id_salt("matedit").max_height(330.0).show(ui, |ui| {
                        for op in Operation::ALL {
                            ui.add_space(6.0);
                            let slot = m.settings_mut(op);
                            let mut on = slot.is_some();
                            if ui.checkbox(&mut on, RichText::new(op.label()).strong()).changed() {
                                *slot = on.then(|| OpSettings { speed: 100.0, min_power: 20.0, max_power: 20.0, passes: 1, air_assist: true, line_interval_mm: if op.uses_line_interval() { 0.1 } else { 0.0 } });
                                changed = true;
                            }
                            if let Some(s) = slot {
                                ui.indent(("op", op.label()), |ui| {
                                    changed |= settings_editor(ui, op, s, ("lib", op.label()));
                                });
                            }
                        }
                        ui.add_space(6.0);
                        ui.label(RichText::new(t!("notes")).color(pal.muted));
                        changed |= ui.add(egui::TextEdit::multiline(&mut m.notes).desired_rows(2).desired_width(f32::INFINITY)).changed();
                    });
                });
            });
            if let Some(loc) = self.library.location() {
                ui.label(RichText::new(t!("saved-in", location = loc)).color(pal.muted).size(11.0));
            }
        });
        if changed {
            self.library.save();
        }
        self.show_library = open;
    }
}

// ---------------------------------------------------------------- widgets

/// Speed / power / passes / air (and line spacing) for one operation.
fn settings_editor(ui: &mut egui::Ui, op: Operation, s: &mut OpSettings, id: impl std::hash::Hash + std::fmt::Debug) -> bool {
    let pal = theme::p();
    let mut changed = false;
    egui::Grid::new(id).num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label(RichText::new(t!("speed")).color(pal.muted));
        changed |= ui.add(egui::DragValue::new(&mut s.speed).range(1.0..=2000.0).suffix(" mm/s").speed(0.5)).changed();
        ui.end_row();
        ui.label(RichText::new(t!("power")).color(pal.muted));
        ui.horizontal(|ui| {
            changed |= ui.add(egui::DragValue::new(&mut s.min_power).range(0.0..=s.max_power).suffix(" %").speed(0.5)).changed();
            ui.label("–");
            changed |= ui.add(egui::DragValue::new(&mut s.max_power).range(s.min_power..=100.0).suffix(" %").speed(0.5)).changed();
        });
        ui.end_row();
        if op.uses_line_interval() {
            ui.label(RichText::new(t!("line-spacing")).color(pal.muted));
            changed |= ui.add(egui::DragValue::new(&mut s.line_interval_mm).range(0.01..=1.0).suffix(" mm").speed(0.005).max_decimals(3)).changed();
            ui.end_row();
        }
        ui.label(RichText::new(t!("passes")).color(pal.muted));
        changed |= ui.add(egui::DragValue::new(&mut s.passes).range(1..=10)).changed();
        ui.end_row();
        ui.label(RichText::new(t!("air-assist")).color(pal.muted));
        changed |= ui.checkbox(&mut s.air_assist, "").changed();
        ui.end_row();
    });
    changed
}

/// A row of mutually exclusive buttons.
fn segmented<T: PartialEq + Copy, S: AsRef<str>>(ui: &mut egui::Ui, value: &mut T, options: &[(T, S)]) {
    let pal = theme::p();
    Frame::new().fill(pal.bg).corner_radius(9).inner_margin(Margin::same(3)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.spacing_mut().button_padding = Vec2::new(4.0, 4.0);
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 3.0 * (options.len() as f32 - 1.0)) / options.len() as f32;
            for (opt, label) in options {
                let on = *value == *opt;
                let text = RichText::new(label.as_ref()).size(13.0).color(if on { pal.on_accent } else { pal.text });
                // Truncate rather than grow: long translations must not widen the panel.
                let button = egui::Button::new(text).truncate().fill(if on { pal.accent } else { Color32::TRANSPARENT }).stroke(Stroke::NONE).corner_radius(7);
                if ui.add_sized([w, 26.0], button).on_hover_text(label.as_ref()).clicked() {
                    *value = *opt;
                }
            }
        });
    });
}

fn tool_icon(p: &egui::Painter, rect: egui::Rect, tool: Tool, color: Color32) {
    let c = rect.center();
    let s = Stroke::new(1.8, color);
    match tool {
        Tool::Select => {
            let pts = vec![c + Vec2::new(-6.0, -9.0), c + Vec2::new(7.0, 3.0), c + Vec2::new(1.0, 4.0), c + Vec2::new(4.0, 10.0), c + Vec2::new(1.5, 11.0), c + Vec2::new(-1.5, 5.0), c + Vec2::new(-6.0, 9.0)];
            p.add(egui::Shape::closed_line(pts, s));
        }
        Tool::Rect => {
            p.rect_stroke(egui::Rect::from_center_size(c, Vec2::new(20.0, 15.0)), 2.0, s, egui::StrokeKind::Middle);
        }
        Tool::Ellipse => {
            p.circle_stroke(c, 9.5, s);
        }
        Tool::Polygon => {
            let pts: Vec<egui::Pos2> = (0..6)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * i as f32 / 6.0;
                    c + Vec2::new(a.cos(), a.sin()) * 10.0
                })
                .collect();
            p.add(egui::Shape::closed_line(pts, s));
        }
        Tool::Line => {
            let pts = vec![c + Vec2::new(-9.0, 7.0), c + Vec2::new(-2.0, -6.0), c + Vec2::new(3.0, 4.0), c + Vec2::new(9.0, -8.0)];
            for &q in &pts {
                p.circle_filled(q, 2.2, color);
            }
            p.add(egui::Shape::line(pts, s));
        }
        Tool::Text => {
            p.text(c, egui::Align2::CENTER_CENTER, "T", egui::FontId::proportional(22.0), color);
        }
    }
}

/// The 3×3 start point picker, drawn as a small design box with 9 dots.
fn anchor_picker(ui: &mut egui::Ui, anchor: &mut Anchor) {
    let pal = theme::p();
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(62.0), Sense::hover());
    let inner = rect.shrink(11.0);
    ui.painter().rect_filled(inner, 3.0, pal.ok.gamma_multiply(0.05));
    ui.painter().rect_stroke(inner, 3.0, Stroke::new(1.0, pal.muted.gamma_multiply(0.6)), egui::StrokeKind::Middle);
    for v in [-1i8, 0, 1] {
        for h in [-1i8, 0, 1] {
            let c = egui::pos2(
                inner.min.x + inner.width() * (h + 1) as f32 / 2.0,
                inner.min.y + inner.height() * (v + 1) as f32 / 2.0,
            );
            let a = Anchor { h, v };
            let r = egui::Rect::from_center_size(c, Vec2::splat(14.0));
            let resp = ui.interact(r, ui.scope_id().with(("anchor", h, v)), Sense::click()).on_hover_text(a.label());
            if resp.clicked() {
                *anchor = a;
            }
            let on = *anchor == a;
            let sq = egui::Rect::from_center_size(c, Vec2::splat(if on { 11.0 } else { 7.0 }));
            if on {
                ui.painter().rect_filled(sq, 2.0, pal.ok);
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
    let design = egui::Rect::from_min_size(rect.min + Vec2::new(20.0, 18.0), Vec2::new(w, h));
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

fn primary_button(text: &str) -> egui::Button<'static> {
    let pal = theme::p();
    egui::Button::new(RichText::new(text.to_string()).color(pal.on_accent).strong()).fill(pal.accent).stroke(Stroke::NONE)
}

fn chip(ui: &mut egui::Ui, text: &str, on: bool) {
    let pal = theme::p();
    let (fill, color) = if on { (pal.ok.gamma_multiply(0.14), pal.ok) } else { (pal.card, pal.muted) };
    Frame::new().fill(fill).corner_radius(10).inner_margin(Margin::symmetric(8, 2)).show(ui, |ui| {
        ui.label(RichText::new(if on { format!("✓ {text}") } else { text.to_string() }).color(color).size(12.0));
    });
}

fn logo(ui: &mut egui::Ui) {
    let pal = theme::p();
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
    let c = rect.center();
    // A focus reticle: ring, four ticks, centre dot.
    let p = ui.painter();
    p.circle_filled(c, 13.0, pal.accent);
    p.circle_stroke(c, 7.0, Stroke::new(1.8, pal.on_accent));
    for d in [Vec2::new(1.0, 0.0), Vec2::new(-1.0, 0.0), Vec2::new(0.0, 1.0), Vec2::new(0.0, -1.0)] {
        p.line_segment([c + d * 4.0, c + d * 10.0], Stroke::new(1.8, pal.on_accent));
    }
    p.circle_filled(c, 1.8, pal.on_accent);
}

fn section(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(theme::p().muted).strong());
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let pal = theme::p();
    Frame::new().fill(pal.card).stroke(Stroke::new(1.0, pal.border)).corner_radius(10).inner_margin(Margin::same(12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

fn kv(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).color(theme::p().muted));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(value);
        });
    });
}

fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

fn pill(ui: &mut egui::Ui, color: Color32, text: &str) -> egui::Response {
    let response = Frame::new()
        .fill(color.gamma_multiply(0.13))
        .corner_radius(14)
        .inner_margin(Margin::symmetric(12, 5))
        .show(ui, |ui| {
            // The pill sits in a right-to-left layout: add the label first so
            // the dot ends up on its left.
            ui.horizontal(|ui| {
                ui.label(RichText::new(text).color(color).strong());
                dot(ui, color);
            });
        })
        .response;
    response.interact(Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A shortcut as the platform writes it (⌘S on macOS, Ctrl+S elsewhere).
fn shortcut(ctx: &egui::Context, modifiers: Modifiers, key: Key) -> String {
    ctx.format_shortcut(&egui::KeyboardShortcut::new(modifiers, key))
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
