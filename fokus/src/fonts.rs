//! Fonts for text objects: the system's fonts plus Inter (built in, so a
//! design looks the same on every computer), laid out with usvg into
//! outlines, with caret positions for editing text on the canvas.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

use crate::bezier::{self, SubPath};
use crate::t;

/// Font size of the layout, in layout units (an em is this many units).
pub const EM: f64 = 100.0;
/// Distance between baselines, in em.
pub const LINE_HEIGHT: f64 = 1.25;
/// The family new text gets; built in, so always there.
pub const DEFAULT_FAMILY: &str = "Inter";

pub fn database() -> Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        db.load_font_data(crate::theme::INTER_REGULAR.to_vec());
        db.load_font_data(crate::theme::INTER_SEMIBOLD.to_vec());
        // `sans-serif` would mean Arial, which Linux usually lacks; text in an
        // SVG whose fonts are all missing would then vanish. Inter is built in.
        db.set_sans_serif_family(DEFAULT_FAMILY);
        Arc::new(db)
    })
    .clone()
}

pub fn svg_options() -> usvg::Options<'static> {
    let mut opt = usvg::Options::default();
    opt.fontdb = database();
    // For text that names no font at all.
    opt.font_family = DEFAULT_FAMILY.to_string();
    opt
}

/// Whether a face has vector outlines. Bitmap-only fonts have none, so
/// they could neither be previewed nor engraved.
fn has_outlines(db: &usvg::fontdb::Database, id: usvg::fontdb::ID) -> bool {
    use ttf_parser::Tag;
    db.with_face_data(id, |data, index| {
        ttf_parser::RawFace::parse(data, index)
            .map(|face| [b"glyf", b"CFF ", b"CFF2"].iter().any(|t| face.table(Tag::from_bytes(t)).is_some()))
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

/// Installed font families with outlines, sorted by name (hidden system
/// fonts left out).
pub fn families() -> &'static [String] {
    static FAMILIES: OnceLock<Vec<String>> = OnceLock::new();
    FAMILIES.get_or_init(|| {
        let db = database();
        let mut names: Vec<String> = db
            .faces()
            .filter(|f| has_outlines(&db, f.id))
            .filter_map(|f| f.families.first().map(|(n, _)| n.clone()))
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup();
        names
    })
}

/// The family to use for a font saved by an older version (Sans, Serif, Mono).
pub fn legacy_family(name: &str) -> String {
    let wanted: &[&str] = match name {
        "Serif" => &["Georgia", "Times New Roman", "Liberation Serif", "DejaVu Serif"],
        "Mono" => &["Menlo", "Consolas", "Liberation Mono", "DejaVu Sans Mono"],
        "Sans" => &["Helvetica", "Arial", "Liberation Sans", "DejaVu Sans"],
        other => return other.to_string(),
    };
    let have = families();
    wanted.iter().find(|w| have.iter().any(|h| h == *w)).map(|s| s.to_string()).unwrap_or_else(|| DEFAULT_FAMILY.to_string())
}

/// The box around the drawn letters, in layout units.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Ink {
    pub x0: f64,
    pub y0: f64,
    pub w: f64,
    pub h: f64,
}

/// One line of text: its baseline and the x of every caret position
/// (one more than the line has characters), in layout units.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub baseline: f64,
    pub carets: Vec<f64>,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub outlines: Vec<SubPath>,
    /// Zero-sized at the origin for empty text.
    pub ink: Ink,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Style {
    pub family: String,
    pub bold: bool,
    pub italic: bool,
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn text_svg(lines: &[&str], style: &Style) -> String {
    let spans: String = lines
        .iter()
        .enumerate()
        .map(|(i, l)| format!(r#"<tspan x="0" y="{}">{}</tspan>"#, EM + i as f64 * EM * LINE_HEIGHT, escape(l)))
        .collect();
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="100000" height="100000"><text font-family="'{}'" font-weight="{}" font-style="{}" font-size="{EM}" xml:space="preserve">{spans}</text></svg>"#,
        escape(&style.family.replace('\'', "")),
        if style.bold { "bold" } else { "normal" },
        if style.italic { "italic" } else { "normal" },
    )
}

fn find_text(group: &usvg::Group) -> Option<&usvg::Text> {
    group.children().iter().find_map(|n| match n {
        usvg::Node::Text(t) => Some(t.as_ref()),
        usvg::Node::Group(g) => find_text(g),
        _ => None,
    })
}

fn collect_paths(group: &usvg::Group, out: &mut Vec<SubPath>) {
    for n in group.children() {
        match n {
            usvg::Node::Group(g) => collect_paths(g, out),
            usvg::Node::Path(p) => out.extend(bezier::from_usvg(p.data(), p.abs_transform())),
            usvg::Node::Text(t) => collect_paths(t.flattened(), out),
            usvg::Node::Image(_) => {}
        }
    }
}

/// Caret positions for one line: lay it out with a sentinel after it, so
/// the last caret sits after the last character's advance.
fn carets(line: &str, style: &Style) -> Vec<f64> {
    let chars = line.chars().count();
    let fallback = || (0..=chars).map(|i| i as f64 * EM * 0.55).collect::<Vec<_>>();
    let svg = text_svg(&[&format!("{line}|")], style);
    let Ok(tree) = usvg::Tree::from_str(&svg, &svg_options()) else { return fallback() };
    let Some(text) = find_text(tree.root()) else { return fallback() };
    // (characters in the cluster, x where it starts)
    let mut clusters: Vec<(usize, f64)> = Vec::new();
    let mut last_x = f64::NAN;
    for g in text.layouted().iter().flat_map(|s| s.positioned_glyphs.iter()) {
        let x = g.transform().tx as f64;
        let n = g.text.chars().count();
        // Several glyphs of one cluster (accents) share its start.
        if n == 0 || (x == last_x && clusters.last().is_some_and(|c| c.0 == n)) {
            continue;
        }
        clusters.push((n, x));
        last_x = x;
    }
    let total: usize = clusters.iter().map(|c| c.0).sum();
    if total != chars + 1 {
        return fallback();
    }
    let mut out = Vec::with_capacity(chars + 1);
    for (k, &(n, x)) in clusters.iter().enumerate() {
        let next = clusters.get(k + 1).map_or(x, |c| c.1);
        for j in 0..n {
            out.push(x + (next - x) * j as f64 / n as f64);
        }
    }
    out.truncate(chars + 1);
    out
}

/// Lay out `text` (lines split at '\n').
pub fn layout(text: &str, style: &Style) -> Result<Layout, String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut outlines = Vec::new();
    if !text.trim().is_empty() {
        let tree = usvg::Tree::from_str(&text_svg(&lines, style), &svg_options()).map_err(|_| t!("err-no-font"))?;
        collect_paths(tree.root(), &mut outlines);
        if outlines.is_empty() {
            return Err(t!("err-no-font"));
        }
    }
    let ink = match bezier::bounds(&outlines, 0.05) {
        Some(((x0, y0), (x1, y1))) => Ink { x0, y0, w: x1 - x0, h: y1 - y0 },
        None => Ink { x0: 0.0, y0: 0.0, w: 0.0, h: 0.0 },
    };
    let lines = lines.iter().enumerate().map(|(i, l)| Line { baseline: EM + i as f64 * EM * LINE_HEIGHT, carets: carets(l, style) }).collect();
    Ok(Layout { outlines, ink, lines })
}

// ---------------------------------------------------------------- previews

/// The family's name written in that family, as a white texture `px` high
/// (for the font picker). Cached.
pub fn preview(ctx: &egui::Context, family: &str, px: u32) -> Option<TextureHandle> {
    static CACHE: OnceLock<Mutex<HashMap<(String, u32), Option<TextureHandle>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(t) = cache.lock().ok()?.get(&(family.to_string(), px)) {
        return t.clone();
    }
    let style = Style { family: family.to_string(), bold: false, italic: false };
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="4000" height="{h}"><text x="0" y="{base}" fill="#fff" font-family="'{f}'" font-size="{size}">{name}</text></svg>"##,
        h = px,
        base = px as f64 * 0.78,
        size = px as f64 * 0.8,
        f = escape(&style.family.replace('\'', "")),
        name = escape(family),
    );
    let texture = (|| {
        let tree = usvg::Tree::from_str(&svg, &svg_options()).ok()?;
        let width = (tree.root().abs_bounding_box().right().ceil() as u32 + 2).clamp(1, 4000);
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, px)?;
        resvg::render(&tree, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
        let image = ColorImage::from_rgba_premultiplied([width as usize, px as usize], pixmap.data());
        Some(ctx.load_texture(format!("font-{family}-{px}"), image, TextureOptions::LINEAR))
    })();
    cache.lock().ok()?.insert((family.to_string(), px), texture.clone());
    texture
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inter() -> Style {
        Style { family: DEFAULT_FAMILY.into(), bold: false, italic: false }
    }

    #[test]
    fn built_in_font_is_listed_and_lays_out() {
        assert!(families().iter().any(|f| f == DEFAULT_FAMILY));
        let l = layout("Fokus", &inter()).unwrap();
        assert!(!l.outlines.is_empty());
        assert!(l.ink.w > 100.0 && l.ink.h > 50.0);
    }

    #[test]
    fn carets_step_through_the_line() {
        let l = layout("Hi you", &inter()).unwrap();
        let c = &l.lines[0].carets;
        assert_eq!(c.len(), 7);
        assert!(c.windows(2).all(|w| w[1] > w[0]), "{c:?}");
        // The space has width too.
        assert!(c[3] - c[2] > 10.0);
    }

    #[test]
    fn several_lines_and_empty_text() {
        let l = layout("A\n\nB", &inter()).unwrap();
        assert_eq!(l.lines.len(), 3);
        assert_eq!(l.lines[1].carets.len(), 1);
        assert!(l.lines[2].baseline > l.lines[0].baseline);
        let e = layout("", &inter()).unwrap();
        assert!(e.outlines.is_empty());
        assert_eq!(e.ink.w, 0.0);
    }

    #[test]
    fn old_font_names_map_to_installed_families() {
        let f = legacy_family("Sans");
        assert!(families().contains(&f));
        assert_eq!(legacy_family("Inter"), "Inter");
    }

    #[test]
    fn every_listed_family_has_outlines() {
        let db = database();
        for name in families() {
            assert!(db.faces().any(|f| f.families.iter().any(|(n, _)| n == name) && has_outlines(&db, f.id)), "{name}");
        }
    }
}
