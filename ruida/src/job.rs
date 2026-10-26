//! Interpreting decoded commands as a toolpath.

use std::collections::BTreeMap;

use crate::command::{Command, PowerKind, Record};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    /// Endpoints in µm.
    pub from: (i64, i64),
    pub to: (i64, i64),
    pub cut: bool,
    pub layer: Option<u8>,
}

impl Segment {
    pub fn length_mm(&self) -> f64 {
        let dx = (self.to.0 - self.from.0) as f64;
        let dy = (self.to.1 - self.from.1) as f64;
        dx.hypot(dy) / 1000.0
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerInfo {
    pub speed: Option<f64>,
    pub min_power: Option<f32>,
    pub max_power: Option<f32>,
    pub color: Option<[u8; 3]>,
    pub cut_mm: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Toolpath {
    pub segments: Vec<Segment>,
    pub layers: BTreeMap<u8, LayerInfo>,
    pub travel_mm: f64,
    pub unknown_commands: usize,
    pub total_commands: usize,
}

impl Toolpath {
    pub fn from_records(records: &[Record]) -> Self {
        let mut tp = Toolpath { total_commands: records.len(), ..Default::default() };
        let mut pos = (0i64, 0i64);
        let mut layer: Option<u8> = None;

        for r in records {
            let target = match r.cmd {
                Command::MoveAbs { x, y } => Some(((x as i64, y as i64), false)),
                Command::CutAbs { x, y } => Some(((x as i64, y as i64), true)),
                Command::MoveRel { dx, dy } => Some(((pos.0 + dx as i64, pos.1 + dy as i64), false)),
                Command::CutRel { dx, dy } => Some(((pos.0 + dx as i64, pos.1 + dy as i64), true)),
                _ => None,
            };
            if let Some((to, cut)) = target {
                let seg = Segment { from: pos, to, cut, layer };
                if cut {
                    tp.layers.entry(layer.unwrap_or(0)).or_default().cut_mm += seg.length_mm();
                } else {
                    tp.travel_mm += seg.length_mm();
                }
                tp.segments.push(seg);
                pos = to;
                continue;
            }

            match r.cmd {
                Command::Layer(p) => {
                    layer = Some(p);
                    tp.layers.entry(p).or_default();
                }
                Command::Speed { what: "layer", part: Some(p), mm_per_s } => {
                    tp.layers.entry(p).or_default().speed = Some(mm_per_s);
                }
                Command::Power { kind, laser: 1, part, percent } => {
                    let Some(p) = part.or(layer) else { continue };
                    let info = tp.layers.entry(p).or_default();
                    match kind {
                        PowerKind::Min => info.min_power = Some(percent),
                        PowerKind::Max => info.max_power = Some(percent),
                        _ => {}
                    }
                }
                Command::LayerColor { part, rgb } => {
                    if let Some(p) = part.or(layer) {
                        tp.layers.entry(p).or_default().color = Some(rgb);
                    }
                }
                Command::Unknown => tp.unknown_commands += 1,
                _ => {}
            }
        }
        tp
    }

    /// Bounding box of all segment endpoints in µm: (min, max).
    pub fn bounds(&self) -> Option<((i64, i64), (i64, i64))> {
        let mut pts = self.segments.iter().flat_map(|s| [s.from, s.to]);
        let first = pts.next()?;
        Some(pts.fold((first, first), |(lo, hi), p| {
            ((lo.0.min(p.0), lo.1.min(p.1)), (hi.0.max(p.0), hi.1.max(p.1)))
        }))
    }

    /// Bounding box of cutting segments only.
    pub fn cut_bounds(&self) -> Option<((i64, i64), (i64, i64))> {
        let cuts = Toolpath { segments: self.segments.iter().filter(|s| s.cut).copied().collect(), ..Default::default() };
        cuts.bounds()
    }
}
