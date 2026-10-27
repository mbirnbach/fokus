//! Colours and egui style for light and dark mode. The app follows the
//! system setting; `p()` returns the palette of the mode shown this frame.

use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, RichText, Stroke, TextStyle, Theme, ThemePreference};

use crate::t;

pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub panel: Color32,
    pub card: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    /// Text drawn on an accent-coloured button.
    pub on_accent: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub bed: Color32,
    pub grid_fine: Color32,
    pub grid_major: Color32,
    pub hover: Color32,
    pub pressed: Color32,
}

pub const LIGHT: Palette = Palette {
    dark: false,
    bg: Color32::from_rgb(0xEE, 0xF0, 0xF3),
    panel: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    card: Color32::from_rgb(0xF6, 0xF7, 0xF9),
    border: Color32::from_rgb(0xE1, 0xE4, 0xE9),
    text: Color32::from_rgb(0x1B, 0x21, 0x2C),
    muted: Color32::from_rgb(0x6A, 0x72, 0x80),
    accent: Color32::from_rgb(0xE8, 0x57, 0x2A),
    accent_hover: Color32::from_rgb(0xF0, 0x66, 0x3A),
    on_accent: Color32::WHITE,
    ok: Color32::from_rgb(0x1F, 0x9D, 0x5B),
    warn: Color32::from_rgb(0xC2, 0x7A, 0x0A),
    err: Color32::from_rgb(0xD6, 0x33, 0x33),
    bed: Color32::from_rgb(0xFB, 0xFB, 0xFC),
    grid_fine: Color32::from_rgb(0xEE, 0xF0, 0xF3),
    grid_major: Color32::from_rgb(0xD7, 0xDB, 0xE1),
    hover: Color32::from_rgb(0xEC, 0xEE, 0xF2),
    pressed: Color32::from_rgb(0xE2, 0xE5, 0xEA),
};

pub const DARK: Palette = Palette {
    dark: true,
    bg: Color32::from_rgb(0x12, 0x14, 0x18),
    panel: Color32::from_rgb(0x1A, 0x1D, 0x23),
    card: Color32::from_rgb(0x23, 0x27, 0x2F),
    border: Color32::from_rgb(0x2E, 0x33, 0x3D),
    text: Color32::from_rgb(0xE6, 0xE8, 0xEC),
    muted: Color32::from_rgb(0x8B, 0x93, 0xA1),
    accent: Color32::from_rgb(0xFF, 0x74, 0x45),
    accent_hover: Color32::from_rgb(0xFF, 0x86, 0x5C),
    on_accent: Color32::from_rgb(0x1A, 0x10, 0x0C),
    ok: Color32::from_rgb(0x3D, 0xD6, 0x8C),
    warn: Color32::from_rgb(0xFF, 0xC1, 0x4D),
    err: Color32::from_rgb(0xFF, 0x5C, 0x5C),
    bed: Color32::from_rgb(0x1E, 0x22, 0x29),
    grid_fine: Color32::from_rgb(0x26, 0x2B, 0x33),
    grid_major: Color32::from_rgb(0x34, 0x3A, 0x45),
    hover: Color32::from_rgb(0x2C, 0x31, 0x3B),
    pressed: Color32::from_rgb(0x33, 0x39, 0x44),
};

static DARK_MODE: AtomicBool = AtomicBool::new(false);

/// Call once per frame with the mode egui is showing.
pub fn set_dark(dark: bool) {
    DARK_MODE.store(dark, Ordering::Relaxed);
}

pub fn p() -> &'static Palette {
    if DARK_MODE.load(Ordering::Relaxed) {
        &DARK
    } else {
        &LIGHT
    }
}

/// Fallback layer colours, used when a design colour would be invisible on
/// the bed (black in dark mode, white in light mode).
const LAYER_COLORS_LIGHT: [Color32; 6] = [
    Color32::from_rgb(0x1B, 0x21, 0x2C),
    Color32::from_rgb(0xD6, 0x33, 0x33),
    Color32::from_rgb(0x1F, 0x6F, 0xC9),
    Color32::from_rgb(0x1F, 0x9D, 0x5B),
    Color32::from_rgb(0xC2, 0x7A, 0x0A),
    Color32::from_rgb(0x8B, 0x4C, 0xD6),
];
const LAYER_COLORS_DARK: [Color32; 6] = [
    Color32::from_rgb(0xE8, 0xEA, 0xEE),
    Color32::from_rgb(0xFF, 0x5C, 0x5C),
    Color32::from_rgb(0x4D, 0xAB, 0xF7),
    Color32::from_rgb(0x3D, 0xD6, 0x8C),
    Color32::from_rgb(0xFF, 0xC1, 0x4D),
    Color32::from_rgb(0xC0, 0x84, 0xFC),
];

fn luminance(c: Color32) -> f32 {
    (0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32) / 255.0
}

pub fn layer_color(id: u8, rgb: Option<[u8; 3]>) -> Color32 {
    let dark = DARK_MODE.load(Ordering::Relaxed);
    let fallback = if dark { LAYER_COLORS_DARK } else { LAYER_COLORS_LIGHT };
    match rgb {
        Some([r, g, b]) => {
            let c = Color32::from_rgb(r, g, b);
            if (luminance(c) - luminance(p().bed)).abs() > 0.25 {
                c
            } else {
                fallback[id as usize % fallback.len()]
            }
        }
        None => fallback[id as usize % fallback.len()],
    }
}

/// Human name for a design colour, for "what happens to the red parts".
pub fn color_name([r, g, b]: [u8; 3]) -> String {
    let (r, g, b) = (r as f32, g as f32, b as f32);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max - min < 30.0 {
        return match max {
            m if m < 60.0 => t!("color-black"),
            m if m > 200.0 => t!("color-white"),
            _ => t!("color-grey"),
        };
    }
    let hue = if max == r {
        60.0 * ((g - b) / (max - min)).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / (max - min) + 2.0)
    } else {
        60.0 * ((r - g) / (max - min) + 4.0)
    };
    match hue as u32 {
        0..=14 | 345..=360 => t!("color-red"),
        15..=44 => t!("color-orange"),
        45..=69 => t!("color-yellow"),
        70..=164 => t!("color-green"),
        165..=194 => t!("color-cyan"),
        195..=254 => t!("color-blue"),
        255..=289 => t!("color-purple"),
        _ => t!("color-pink"),
    }
}

/// The UI font (Inter, SIL Open Font Licence, see `fonts/LICENSE.inter`)
/// in three weights; egui's own fonts stay as fallback for other scripts.
pub const INTER_REGULAR: &[u8] = include_bytes!("../fonts/inter-400.ttf");
const INTER_MEDIUM: &[u8] = include_bytes!("../fonts/inter-500.ttf");
pub const INTER_SEMIBOLD: &[u8] = include_bytes!("../fonts/inter-600.ttf");

fn medium_family() -> FontFamily {
    FontFamily::Name("medium".into())
}

fn semibold_family() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn medium(size: f32) -> FontId {
    FontId::new(size, medium_family())
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, semibold_family())
}

/// Weights for labels, instead of egui's `strong()` (which only changes colour).
pub trait Weight {
    fn medium(self) -> RichText;
    fn semibold(self) -> RichText;
}

impl Weight for RichText {
    fn medium(self) -> RichText {
        self.family(medium_family())
    }
    fn semibold(self) -> RichText {
        self.family(semibold_family())
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (name, data) in [("inter-400", INTER_REGULAR), ("inter-500", INTER_MEDIUM), ("inter-600", INTER_SEMIBOLD)] {
        fonts.font_data.insert(name.into(), Arc::new(FontData::from_static(data)));
    }
    let chain = |first: &str| std::iter::once(first.to_string()).chain(fallback.iter().cloned()).collect::<Vec<_>>();
    fonts.families.insert(FontFamily::Proportional, chain("inter-400"));
    fonts.families.insert(medium_family(), chain("inter-500"));
    fonts.families.insert(semibold_family(), chain("inter-600"));
    ctx.set_fonts(fonts);
}

fn style(style: &mut egui::Style, pal: &Palette) {
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(11.0)),
        (TextStyle::Body, FontId::proportional(13.0)),
        (TextStyle::Button, medium(13.0)),
        (TextStyle::Heading, semibold(18.0)),
        (TextStyle::Monospace, FontId::monospace(12.0)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 30.0;
    style.spacing.menu_margin = egui::Margin::same(6);
    style.interaction.selectable_labels = false;

    let v = &mut style.visuals;
    v.panel_fill = pal.panel;
    v.window_fill = pal.panel;
    v.window_stroke = Stroke::new(1.0, pal.border);
    v.extreme_bg_color = pal.card;
    v.faint_bg_color = pal.card;
    v.override_text_color = Some(pal.text);
    v.selection.bg_fill = pal.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, pal.accent);
    v.hyperlink_color = pal.accent;

    let radius = CornerRadius::same(8);
    v.window_corner_radius = CornerRadius::same(12);
    v.window_shadow = egui::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(if pal.dark { 110 } else { 38 }) };
    v.popup_shadow = egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(if pal.dark { 100 } else { 30 }) };
    v.menu_corner_radius = radius;
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = radius;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal.border);
    v.widgets.noninteractive.bg_fill = pal.panel;
    v.widgets.inactive.weak_bg_fill = pal.card;
    v.widgets.inactive.bg_fill = pal.card;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, pal.border);
    v.widgets.hovered.weak_bg_fill = pal.hover;
    v.widgets.hovered.bg_fill = pal.hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, pal.accent.gamma_multiply(0.7));
    v.widgets.active.weak_bg_fill = pal.pressed;
    v.widgets.active.bg_fill = pal.pressed;
    v.widgets.open.weak_bg_fill = pal.hover;
}

pub fn apply(ctx: &egui::Context) {
    // FOKUS_THEME=light|dark forces a theme at startup (handy for screenshots).
    let pref = match std::env::var("FOKUS_THEME").as_deref() {
        Ok("light") => ThemePreference::Light,
        Ok("dark") => ThemePreference::Dark,
        _ => ThemePreference::System,
    };
    ctx.set_theme(pref);
    install_fonts(ctx);
    ctx.style_mut_of(Theme::Light, |s| style(s, &LIGHT));
    ctx.style_mut_of(Theme::Dark, |s| style(s, &DARK));
}
