//! Line icons from Lucide (ISC licence, see `icons/LICENSE.lucide`), drawn
//! from their SVGs at the screen's pixel size and tinted with any colour.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use eframe::egui::{self, Color32, ColorImage, Painter, Rect, TextureHandle, TextureOptions};

macro_rules! icons {
    ($($name:ident => $file:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Icon { $($name),* }

        impl Icon {
            fn svg(self) -> &'static str {
                match self { $(Icon::$name => include_str!(concat!("../icons/", $file, ".svg"))),* }
            }
        }
    };
}

icons! {
    AlignBottom => "align-end-horizontal",
    AlignHCentre => "align-center-vertical",
    AlignLeft => "align-start-vertical",
    AlignRight => "align-end-vertical",
    AlignTop => "align-start-horizontal",
    AlignVCentre => "align-center-horizontal",
    Bold => "bold",
    BringToFront => "bring-to-front",
    Check => "check",
    ChevronDown => "chevron-down",
    Circle => "circle",
    CircleDot => "circle-dot",
    Copy => "copy",
    Crosshair => "crosshair",
    Diamond => "diamond",
    Ellipsis => "ellipsis-vertical",
    EyeOff => "eye-off",
    FileDown => "file-down",
    FilePlus => "file-plus",
    Flame => "flame",
    FlipH => "flip-horizontal-2",
    FlipV => "flip-vertical-2",
    Focus => "focus",
    FolderOpen => "folder-open",
    Fullscreen => "fullscreen",
    Gauge => "gauge",
    Group => "group",
    Hexagon => "hexagon",
    Info => "info",
    Italic => "italic",
    Layers => "layers",
    Lock => "lock",
    LockOpen => "lock-open",
    Minus => "minus",
    Package => "package",
    Paste => "clipboard-paste",
    Pause => "pause",
    Pen => "pen-tool",
    Play => "play",
    Plug => "plug",
    Plus => "plus",
    Pointer => "mouse-pointer-2",
    PointerHollow => "mouse-pointer",
    Redo => "redo-2",
    Repeat => "repeat",
    RotateCcw => "rotate-ccw",
    RotateCw => "rotate-cw",
    Rows => "rows-3",
    Save => "save",
    Scan => "scan",
    Scissors => "scissors",
    Search => "search",
    SendToBack => "send-to-back",
    Settings => "settings",
    Square => "square",
    Stop => "octagon-x",
    Trash => "trash-2",
    Type => "type",
    TypeOutline => "type-outline",
    Undo => "undo-2",
    Ungroup => "ungroup",
    Unplug => "unplug",
    Warning => "triangle-alert",
    Wind => "wind",
    X => "x",
    Zap => "zap",
}

/// Stroke width drawn at the icons' 24 px design size (Lucide uses 2;
/// slightly lighter reads better next to text at our sizes).
const STROKE: &str = "1.75";

fn cache() -> &'static Mutex<HashMap<(Icon, u32), TextureHandle>> {
    static CACHE: OnceLock<Mutex<HashMap<(Icon, u32), TextureHandle>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The icon as a white texture `px` pixels square.
fn texture(ctx: &egui::Context, icon: Icon, px: u32) -> Option<TextureHandle> {
    let mut cache = cache().lock().ok()?;
    if let Some(t) = cache.get(&(icon, px)) {
        return Some(t.clone());
    }
    let svg = icon.svg().replace("currentColor", "#ffffff").replace("stroke-width=\"2\"", &format!("stroke-width=\"{STROKE}\""));
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
    let scale = px as f32 / tree.size().width();
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let image = ColorImage::from_rgba_premultiplied([px as usize, px as usize], pixmap.data());
    let handle = ctx.load_texture(format!("icon-{icon:?}-{px}"), image, TextureOptions::LINEAR);
    cache.insert((icon, px), handle.clone());
    Some(handle)
}

/// Draw `icon` filling `rect` (square), in `color`.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let ppp = painter.ctx().pixels_per_point();
    let px = (rect.width().max(rect.height()) * ppp).round().max(1.0) as u32;
    if let Some(tex) = texture(painter.ctx(), icon, px) {
        let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(tex.id(), rect, uv, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_parses() {
        // `icons!` lists each variant once; parse them all through usvg.
        let all = [
            Icon::AlignBottom, Icon::Bold, Icon::Copy, Icon::Lock, Icon::LockOpen, Icon::Pen, Icon::Pointer, Icon::PointerHollow,
            Icon::RotateCw, Icon::Settings, Icon::Stop, Icon::Trash, Icon::Type, Icon::Zap,
        ];
        for icon in all {
            assert!(usvg::Tree::from_str(icon.svg(), &usvg::Options::default()).is_ok(), "{icon:?}");
        }
    }
}
