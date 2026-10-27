//! Small drawn widgets shared by the windows: icon buttons, the main
//! buttons, segmented choices, cards, menu rows and status pills.

use eframe::egui::{self, Align, Align2, Color32, CursorIcon, Frame, Layout, Margin, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::icons::{self, Icon};
use crate::theme::{self, Weight};

/// A square, frameless button with just an icon; the tooltip names it.
pub fn icon_button(ui: &mut Ui, icon: Icon, tip: impl Into<String>) -> Response {
    icon_toggle(ui, icon, false, tip)
}

/// An icon button that shows an on state (tinted background, accent icon).
pub fn icon_toggle(ui: &mut Ui, icon: Icon, on: bool, tip: impl Into<String>) -> Response {
    icon_button_sized(ui, icon, on, 30.0, tip)
}

pub fn icon_button_sized(ui: &mut Ui, icon: Icon, on: bool, size: f32, tip: impl Into<String>) -> Response {
    let pal = theme::p();
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let enabled = ui.is_enabled();
    let fill = if on {
        pal.accent.gamma_multiply(0.14)
    } else if response.is_pointer_button_down_on() && enabled {
        pal.pressed
    } else if response.hovered() && enabled {
        pal.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 7.0, fill);
    let color = match (enabled, on) {
        (false, _) => pal.muted.gamma_multiply(0.5),
        (_, true) => pal.accent,
        _ => pal.text,
    };
    let s = (size * 0.56).round();
    icons::paint(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(s)), icon, color);
    let tip: String = tip.into();
    let response = if tip.is_empty() { response } else { response.on_hover_text(tip) };
    if enabled { response.on_hover_cursor(CursorIcon::PointingHand) } else { response }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Style {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// A button with an optional icon before the label, drawn in one of the
/// app's button styles.
pub struct Btn {
    text: String,
    icon: Option<Icon>,
    style: Style,
    height: f32,
    width: Option<f32>,
}

impl Btn {
    pub fn new(text: impl Into<String>) -> Self {
        Btn { text: text.into(), icon: None, style: Style::Secondary, height: 30.0, width: None }
    }
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn primary(mut self) -> Self {
        self.style = Style::Primary;
        self
    }
    pub fn ghost(mut self) -> Self {
        self.style = Style::Ghost;
        self
    }
    pub fn danger(mut self) -> Self {
        self.style = Style::Danger;
        self
    }
    pub fn height(mut self, h: f32) -> Self {
        self.height = h;
        self
    }
    /// Fixed width; `f32::INFINITY` fills the row.
    pub fn width(mut self, w: f32) -> Self {
        self.width = Some(w);
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let pal = theme::p();
        let enabled = ui.is_enabled();
        let size = if self.height >= 40.0 { 14.0 } else { 13.0 };
        let font = theme::medium(size);
        let text_color = match self.style {
            Style::Primary => pal.on_accent,
            Style::Danger => Color32::WHITE,
            _ => pal.text,
        };
        let galley = ui.painter().layout_no_wrap(self.text.clone(), font, text_color);
        let icon_size = if self.height >= 40.0 { 18.0 } else { 16.0 };
        let gap = if self.icon.is_some() && !self.text.is_empty() { 8.0 } else { 0.0 };
        let content = galley.size().x + self.icon.map_or(0.0, |_| icon_size) + gap;
        let width = match self.width {
            Some(w) if w.is_infinite() => ui.available_width(),
            Some(w) => w,
            None => content + 2.0 * if self.height >= 40.0 { 18.0 } else { 12.0 },
        };
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, self.height), Sense::click());
        let hovered = response.hovered() && enabled;
        let pressed = response.is_pointer_button_down_on() && enabled;
        let (fill, stroke) = match self.style {
            Style::Primary => (if pressed { pal.accent_fill.gamma_multiply(0.85) } else if hovered { pal.accent_fill_hover } else { pal.accent_fill }, Stroke::NONE),
            Style::Danger => (if hovered { pal.err.gamma_multiply(0.9) } else { pal.err }, Stroke::NONE),
            Style::Secondary => (if pressed { pal.pressed } else if hovered { pal.hover } else { pal.panel }, Stroke::new(1.0, pal.border)),
            Style::Ghost => (if pressed { pal.pressed } else if hovered { pal.hover } else { Color32::TRANSPARENT }, Stroke::NONE),
        };
        let radius = if self.height >= 40.0 { 10.0 } else { 8.0 };
        let p = ui.painter();
        let fade = if enabled { 1.0 } else { 0.45 };
        p.rect(rect, radius, fill.gamma_multiply(fade), stroke, StrokeKind::Inside);
        let mut x = rect.center().x - content / 2.0;
        if let Some(icon) = self.icon {
            icons::paint(p, Rect::from_min_size(egui::pos2(x, rect.center().y - icon_size / 2.0), Vec2::splat(icon_size)), icon, text_color.gamma_multiply(fade));
            x += icon_size + gap;
        }
        p.galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, text_color.gamma_multiply(fade));
        if enabled { response.on_hover_cursor(CursorIcon::PointingHand) } else { response }
    }
}

/// A row in a context or drop-down menu: icon, label, shortcut on the right.
pub fn menu_item(ui: &mut Ui, icon: Option<Icon>, text: &str, shortcut: &str) -> Response {
    let pal = theme::p();
    let width = ui.available_width().max(200.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 28.0), Sense::click());
    let enabled = ui.is_enabled();
    if response.hovered() && enabled {
        ui.painter().rect_filled(rect, 6.0, pal.hover);
    }
    let color = if enabled { pal.text } else { pal.muted.gamma_multiply(0.6) };
    let mut x = rect.min.x + 8.0;
    if let Some(icon) = icon {
        icons::paint(ui.painter(), Rect::from_min_size(egui::pos2(x, rect.center().y - 8.0), Vec2::splat(16.0)), icon, color);
    }
    x += 26.0;
    ui.painter().text(egui::pos2(x, rect.center().y), Align2::LEFT_CENTER, text, egui::FontId::proportional(13.0), color);
    if !shortcut.is_empty() {
        ui.painter().text(rect.right_center() - Vec2::new(8.0, 0.0), Align2::RIGHT_CENTER, shortcut, egui::FontId::proportional(12.0), pal.muted);
    }
    let clicked = response.clicked() && enabled;
    if clicked {
        ui.close();
    }
    response
}

/// A row of mutually exclusive buttons.
pub fn segmented<T: PartialEq + Copy, S: AsRef<str>>(ui: &mut Ui, value: &mut T, options: &[(T, S)]) -> bool {
    let pal = theme::p();
    let mut changed = false;
    Frame::new().fill(pal.bg).corner_radius(9).inner_margin(Margin::same(3)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 3.0 * (options.len() as f32 - 1.0)) / options.len() as f32;
            for (opt, label) in options {
                let on = *value == *opt;
                let (rect, response) = ui.allocate_exact_size(Vec2::new(w, 26.0), Sense::click());
                let fill = if on {
                    pal.panel
                } else if response.hovered() {
                    pal.hover
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(rect, 7.0, fill);
                if on {
                    ui.painter().rect_stroke(rect, 7.0, Stroke::new(1.0, pal.border), StrokeKind::Inside);
                }
                // Truncate rather than grow: long translations must not widen the panel.
                let color = if on { pal.accent } else { pal.muted };
                let galley = fit_label(ui, label.as_ref(), on, color, w - 8.0);
                ui.painter().with_clip_rect(rect).galley(rect.center() - galley.size() / 2.0, galley, color);
                if response.on_hover_text(label.as_ref()).on_hover_cursor(CursorIcon::PointingHand).clicked() && !on {
                    *value = *opt;
                    changed = true;
                }
            }
        });
    });
    changed
}

/// A label that fits `width`: a smaller size first, then shortened.
fn fit_label(ui: &Ui, text: &str, strong: bool, color: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    let font = |size: f32| if strong { theme::semibold(size) } else { theme::medium(size) };
    for size in [12.5, 11.5, 10.5] {
        let g = ui.painter().layout_no_wrap(text.to_string(), font(size), color);
        if g.size().x <= width {
            return g;
        }
    }
    let mut chars: Vec<char> = text.chars().collect();
    while chars.len() > 1 {
        chars.pop();
        let g = ui.painter().layout_no_wrap(format!("{}…", chars.iter().collect::<String>()), font(10.5), color);
        if g.size().x <= width {
            return g;
        }
    }
    ui.painter().layout_no_wrap("…".into(), font(10.5), color)
}

/// A small rounded tag; with `on` green with a check mark.
pub fn chip(ui: &mut Ui, text: &str, on: bool) {
    let pal = theme::p();
    let (fill, color) = if on { (pal.ok.gamma_multiply(0.12), pal.ok) } else { (pal.card, pal.muted) };
    let galley = ui.painter().layout_no_wrap(text.to_string(), theme::medium(12.0), color);
    let icon = if on { 17.0 } else { 0.0 };
    let size = Vec2::new(galley.size().x + icon + 18.0, 24.0);
    // One allocation, so a wrapping row can move the chip to the next line.
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, 12.0, fill);
    if on {
        icons::paint(ui.painter(), Rect::from_center_size(rect.left_center() + Vec2::new(15.0, 0.0), Vec2::splat(13.0)), Icon::Check, color);
    }
    ui.painter().galley(egui::pos2(rect.min.x + 9.0 + icon, rect.center().y - galley.size().y / 2.0), galley, color);
}

/// The app icon (assets/brand), as in the Dock or taskbar.
pub fn logo(ui: &mut Ui, size: f32) {
    static TEXTURE: std::sync::OnceLock<egui::TextureHandle> = std::sync::OnceLock::new();
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let tex = TEXTURE.get_or_init(|| {
        let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../assets/brand/icon/fokus-256.png")).expect("icon is a PNG");
        let image = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
        ui.ctx().load_texture("fokus-logo", image, egui::TextureOptions::LINEAR)
    });
    // The PNG has the usual transparent margin around the rounded square.
    ui.painter().image(tex.id(), rect.expand(size * 0.12), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
}

/// A small upper-case section heading.
pub fn section(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).size(10.5).color(theme::p().muted).semibold());
}

pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let pal = theme::p();
    Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(12)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Key on the left, value on the right.
pub fn kv(ui: &mut Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).color(theme::p().muted));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).medium());
        });
    });
}

pub fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// A rounded status label with a dot; clickable.
pub fn pill(ui: &mut Ui, color: Color32, text: &str) -> Response {
    let response = Frame::new()
        .fill(color.gamma_multiply(0.12))
        .corner_radius(15)
        .inner_margin(Margin::symmetric(11, 5))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 7.0;
            // The pill sits in a right-to-left layout: add the label first so
            // the dot ends up on its left.
            ui.horizontal(|ui| {
                ui.label(RichText::new(text).color(color).medium());
                dot(ui, color);
            });
        })
        .response;
    response.interact(Sense::click()).on_hover_cursor(CursorIcon::PointingHand)
}

/// A thin vertical divider between groups in a toolbar.
pub fn vdivider(ui: &mut Ui) {
    let pal = theme::p();
    let (r, _) = ui.allocate_exact_size(Vec2::new(9.0, 22.0), Sense::hover());
    ui.painter().line_segment([r.center_top(), r.center_bottom()], Stroke::new(1.0, pal.border));
}

/// A muted label in front of a field ("X", "W", …).
pub fn field_label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::p().muted).size(12.0).medium());
}

/// A row in a list (materials, menus with descriptions): a title and an
/// optional muted line under it, left-aligned, highlighted when selected.
pub fn list_row(ui: &mut Ui, selected: bool, title: &str, subtitle: &str, badge: Option<(&str, Color32)>) -> Response {
    let pal = theme::p();
    let height = if subtitle.is_empty() { 32.0 } else { 46.0 };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    let fill = if selected {
        pal.accent.gamma_multiply(0.12)
    } else if response.hovered() {
        pal.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 8.0, fill);
    let p = ui.painter().with_clip_rect(rect.shrink(2.0));
    let left = rect.min.x + 10.0;
    let title_y = if subtitle.is_empty() { rect.center().y } else { rect.min.y + 15.0 };
    let title_color = if selected { pal.accent } else { pal.text };
    let title_galley = p.layout_no_wrap(title.to_string(), theme::medium(13.0), title_color);
    let tw = title_galley.size().x;
    p.galley(egui::pos2(left, title_y - title_galley.size().y / 2.0), title_galley, title_color);
    if let Some((text, color)) = badge {
        let g = p.layout_no_wrap(text.to_string(), theme::medium(10.5), color);
        let r = Rect::from_min_size(egui::pos2(left + tw + 6.0, title_y - g.size().y / 2.0 - 2.0), g.size() + Vec2::new(10.0, 4.0));
        p.rect_filled(r, 6.0, color.gamma_multiply(0.14));
        p.galley(r.min + Vec2::new(5.0, 2.0), g, color);
    }
    if !subtitle.is_empty() {
        p.text(egui::pos2(left, rect.min.y + 32.0), Align2::LEFT_CENTER, subtitle, egui::FontId::proportional(11.5), pal.muted);
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}
