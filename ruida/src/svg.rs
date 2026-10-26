//! Render a toolpath as SVG: cuts as solid lines in the layer colour,
//! travel moves as thin dashed grey lines. Units are millimetres.
//! Shown in controller coordinates; the machine's axes may be mirrored.

use std::fmt::Write;

use crate::job::Toolpath;

const FALLBACK_COLORS: [&str; 6] = ["#e03131", "#1971c2", "#2f9e44", "#f08c00", "#9c36b5", "#0c8599"];

/// Flip the preview to match how LightBurn shows the job. Machines with the
/// origin on the right (LightBurn's MirrorX) need `x: true`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mirror {
    pub x: bool,
    pub y: bool,
}

pub fn render(tp: &Toolpath, mirror: Mirror) -> String {
    let Some(((x0, y0), (x1, y1))) = tp.bounds() else {
        return r#"<svg xmlns="http://www.w3.org/2000/svg" width="10mm" height="10mm"/>"#.to_string();
    };
    let mm = |v: i64| v as f64 / 1000.0;
    let pad = 2.0;
    let (vx, vy) = (mm(x0) - pad, mm(y0) - pad);
    let (w, h) = (mm(x1 - x0) + 2.0 * pad, mm(y1 - y0) + 2.0 * pad);

    let mut out = String::new();
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w:.3}mm" height="{h:.3}mm" viewBox="{vx:.3} {vy:.3} {w:.3} {h:.3}">"#
    );
    let _ = writeln!(out, r#"<rect x="{vx:.3}" y="{vy:.3}" width="{w:.3}" height="{h:.3}" fill="white"/>"#);
    // Mirror about the centre of the drawing: x' = -x + 2 * cx.
    let (sx, tx) = if mirror.x { (-1.0, mm(x0 + x1)) } else { (1.0, 0.0) };
    let (sy, ty) = if mirror.y { (-1.0, mm(y0 + y1)) } else { (1.0, 0.0) };
    let _ = writeln!(out, r#"<g transform="matrix({sx} 0 0 {sy} {tx:.3} {ty:.3})">"#);

    // Merge consecutive connected segments of the same kind into one path.
    let mut current: Option<(bool, Option<u8>, String, (i64, i64))> = None;
    let flush = |out: &mut String, cur: &mut Option<(bool, Option<u8>, String, (i64, i64))>| {
        if let Some((cut, layer, d, _)) = cur.take() {
            let style = if cut {
                let color = layer
                    .and_then(|l| tp.layers.get(&l).and_then(|i| i.color))
                    .map(|[r, g, b]| format!("#{r:02x}{g:02x}{b:02x}"))
                    .unwrap_or_else(|| FALLBACK_COLORS[layer.unwrap_or(0) as usize % FALLBACK_COLORS.len()].to_string());
                format!(r#"stroke="{color}" stroke-width="0.25""#)
            } else {
                r##"stroke="#aaa" stroke-width="0.12" stroke-dasharray="0.6 0.6""##.to_string()
            };
            let _ = writeln!(out, r#"<path d="{d}" fill="none" {style} stroke-linecap="round" stroke-linejoin="round"/>"#);
        }
    };

    for s in &tp.segments {
        let continues = matches!(&current, Some((cut, layer, _, end)) if *cut == s.cut && *layer == s.layer && *end == s.from);
        if !continues {
            flush(&mut out, &mut current);
            current = Some((s.cut, s.layer, format!("M{:.3} {:.3}", mm(s.from.0), mm(s.from.1)), s.from));
        }
        if let Some((_, _, d, end)) = current.as_mut() {
            let _ = write!(d, " L{:.3} {:.3}", mm(s.to.0), mm(s.to.1));
            *end = s.to;
        }
    }
    flush(&mut out, &mut current);
    out.push_str("</g>\n</svg>\n");
    out
}
