//! Writing `.rd` job files: vector cut / score layers and fill engraving.
//!
//! The byte layout mirrors what LightBurn 1.7 writes for our 644XG
//! (tests/data/lightburn_square_text.rd), so the controller sees a structure
//! it already accepts. Coordinates are job-relative µm in controller axes:
//! the job runs from wherever the head is when it starts, so (0, 0) is the
//! head. Coordinates may be negative when the head is not at the job's
//! corner nearest the machine origin (e.g. a centred start point); LightBurn's
//! test file only shows the corner case, so that use needs a check on the
//! machine.
//!
//! Deliberately left out compared to LightBurn:
//! - the trailing `DA 01 06 20 ...` controller memory write (meaning unknown)
//!
//! Unclear but copied as-is:
//! - the 10-byte element name LightBurn writes (`F2 02`)
//!
//! Learned on the machine: each layer body starts with `CA 01 <mode>`,
//! 00 = cut, 01 = scan. In scan mode the laser only fires on X moves, so a
//! cut layer marked 01 only burns its horizontal lines, and an engrave layer
//! marked 00 loses the controller's scan handling (dark line ends).

use std::fmt;

use crate::codec::*;
use crate::fill;
use crate::scramble::{scramble, MAGIC_644X};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LayerKind {
    /// Follow the paths with the laser on.
    Cut,
    /// Fill the closed paths line by line ("Scan"), lines `interval` µm apart.
    Engrave { interval: i32 },
}

/// One layer of paths, all burned with the same settings.
#[derive(Debug, Clone, PartialEq)]
pub struct JobLayer {
    pub kind: LayerKind,
    pub color: [u8; 3],
    /// mm/s
    pub speed: f64,
    /// Percent, 0..=100.
    pub min_power: f32,
    pub max_power: f32,
    pub passes: u32,
    pub air_assist: bool,
    /// Polylines (Cut) or closed outlines (Engrave) in µm, relative to the head.
    pub paths: Vec<Vec<(i32, i32)>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WriteError {
    NoGeometry,
    TooManyLayers(usize),
    BadSpeed { layer: usize, speed: f64 },
    BadPower { layer: usize },
    BadPasses { layer: usize },
    BadInterval { layer: usize },
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::NoGeometry => write!(f, "nothing to cut"),
            WriteError::TooManyLayers(n) => write!(f, "{n} layers, the controller supports at most {MAX_LAYERS}"),
            WriteError::BadSpeed { layer, speed } => write!(f, "layer {layer}: speed {speed} mm/s is out of range"),
            WriteError::BadPower { layer } => write!(f, "layer {layer}: power must be 0–100 % with min <= max"),
            WriteError::BadPasses { layer } => write!(f, "layer {layer}: passes must be 1–{MAX_PASSES}"),
            WriteError::BadInterval { layer } => write!(f, "layer {layer}: line spacing must be 0.01–1 mm"),
        }
    }
}

impl std::error::Error for WriteError {}

pub const MAX_LAYERS: usize = 32;
pub const MAX_PASSES: u32 = 50;
const MAX_SPEED: f64 = 2000.0;
/// LightBurn writes laser 2 settings even for single-tube machines.
const LASER2_POWER: f32 = 20.0;
/// Element name bytes as written by LightBurn (meaning unknown).
const ELEMENT_NAME: [u8; 10] = [0x05, 0x2a, 0x39, 0x1c, 0x41, 0x04, 0x6a, 0x15, 0x08, 0x20];

type Bounds = ((i32, i32), (i32, i32));

fn bounds<'a>(points: impl Iterator<Item = &'a (i32, i32)>) -> Option<Bounds> {
    points.fold(None, |b, &(x, y)| match b {
        None => Some(((x, y), (x, y))),
        Some(((x0, y0), (x1, y1))) => Some(((x0.min(x), y0.min(y)), (x1.max(x), y1.max(y)))),
    })
}

/// Relative moves are 14-bit signed µm.
const MAX_REL: i32 = 8191;

fn has_geometry(l: &JobLayer) -> bool {
    match l.kind {
        LayerKind::Cut => l.paths.iter().any(|p| p.len() >= 2),
        LayerKind::Engrave { interval } => !fill::scan(&l.paths, interval).is_empty(),
    }
}

fn validate(layers: &[JobLayer]) -> Result<(), WriteError> {
    if layers.len() > MAX_LAYERS {
        return Err(WriteError::TooManyLayers(layers.len()));
    }
    let mut any = false;
    for (i, l) in layers.iter().enumerate() {
        if !(l.speed > 0.0 && l.speed <= MAX_SPEED) {
            return Err(WriteError::BadSpeed { layer: i, speed: l.speed });
        }
        let ok = |p: f32| (0.0..=100.0).contains(&p);
        if !(ok(l.min_power) && ok(l.max_power) && l.min_power <= l.max_power) {
            return Err(WriteError::BadPower { layer: i });
        }
        if !(1..=MAX_PASSES).contains(&l.passes) {
            return Err(WriteError::BadPasses { layer: i });
        }
        if let LayerKind::Engrave { interval } = l.kind {
            if !(10..=1000).contains(&interval) {
                return Err(WriteError::BadInterval { layer: i });
            }
        }
    }
    for l in layers {
        any |= has_geometry(l);
    }
    if any {
        Ok(())
    } else {
        Err(WriteError::NoGeometry)
    }
}

struct Out(Vec<u8>);

impl Out {
    fn op(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.extend_from_slice(bytes);
        self
    }
    fn abs(&mut self, v: i32) -> &mut Self {
        self.op(&encode_abs(v))
    }
    fn point(&mut self, (x, y): (i32, i32)) -> &mut Self {
        self.abs(x).abs(y)
    }
    fn power(&mut self, p: f32) -> &mut Self {
        self.op(&encode_power(p))
    }
    fn speed(&mut self, s: f64) -> &mut Self {
        self.op(&encode_speed(s))
    }
    fn rel(&mut self, d: i32) -> &mut Self {
        self.op(&encode_i14(d as i16))
    }
    /// Relative X move (8A) or cut (AA), split into encodable steps.
    fn steps_x(&mut self, op: u8, mut d: i32) -> &mut Self {
        while d != 0 {
            let step = d.clamp(-MAX_REL, MAX_REL);
            self.op(&[op]).rel(step);
            d -= step;
        }
        self
    }
}

fn write_cut_body(o: &mut Out, l: &JobLayer) {
    o.op(&[0xC9, 0x02]).speed(l.speed);
    o.op(&[0xC6, 0x12]).op(&encode_u35(0));
    o.op(&[0xC6, 0x13]).op(&encode_u35(0));
    o.op(&[0xC6, 0x50, 0x00, 0x01, 0xC6, 0x51, 0x00, 0x01]);
    o.op(&[0xC6, 0x01]).power(l.min_power);
    o.op(&[0xC6, 0x02]).power(l.max_power);
    o.op(&[0xC6, 0x21]).power(LASER2_POWER);
    o.op(&[0xC6, 0x22]).power(LASER2_POWER);
    o.op(&[0xCA, 0x03, 0x01]);
    for _ in 0..l.passes {
        for path in l.paths.iter().filter(|p| p.len() >= 2) {
            o.op(&[0x88]).point(path[0]);
            for &pt in &path[1..] {
                o.op(&[0xA8]).point(pt);
            }
        }
    }
}

/// Scan layers as LightBurn writes them: an absolute move to the first
/// span, then relative cuts (AA) and gaps (8A) along each line, and a
/// relative step (8B, or 89 when X changes too) to the next line.
fn write_engrave_body(o: &mut Out, l: &JobLayer, interval: i32) {
    o.op(&[0xC9, 0x02]).speed(l.speed);
    o.op(&[0xC6, 0x01]).power(l.min_power);
    o.op(&[0xC6, 0x02]).power(l.max_power);
    o.op(&[0xC6, 0x21]).power(LASER2_POWER);
    o.op(&[0xC6, 0x22]).power(LASER2_POWER);
    o.op(&[0xC6, 0x12]).op(&encode_u35(0));
    o.op(&[0xC6, 0x13]).op(&encode_u35(0));
    o.op(&[0xCA, 0x03, 0x01]);

    let lines = fill::scan(&l.paths, interval);
    for _ in 0..l.passes {
        let mut pos: Option<(i32, i32)> = None;
        for line in &lines {
            for &(from, to) in &line.spans {
                let start = (from, line.y);
                match pos {
                    None => {
                        o.op(&[0x88]).point(start);
                    }
                    Some((px, py)) => {
                        let (dx, dy) = (from - px, line.y - py);
                        if dy == 0 {
                            o.steps_x(0x8A, dx);
                        } else if dx == 0 && dy.abs() <= MAX_REL {
                            o.op(&[0x8B]).rel(dy);
                        } else if dx.abs() <= MAX_REL && dy.abs() <= MAX_REL {
                            o.op(&[0x89]).rel(dx).rel(dy);
                        } else {
                            o.op(&[0x88]).point(start);
                        }
                    }
                }
                o.steps_x(0xAA, to - from);
                pos = Some((to, line.y));
            }
        }
    }
}

/// Plain (unscrambled) job bytes.
pub fn write_plain(layers: &[JobLayer]) -> Result<Vec<u8>, WriteError> {
    validate(layers)?;
    // Layers without geometry are dropped; part numbers are the remaining order.
    let layers: Vec<&JobLayer> = layers.iter().filter(|l| has_geometry(l)).collect();
    let layer_bounds: Vec<Bounds> = layers.iter().map(|l| bounds(l.paths.iter().flatten()).unwrap()).collect();
    let (dmin, dmax) = bounds(layer_bounds.iter().flat_map(|(a, b)| [a, b])).unwrap();
    let size = (dmax.0 - dmin.0, dmax.1 - dmin.1);

    let mut o = Out(Vec::new());
    // Document header.
    o.op(&[0xD8, 0x12, 0xF0, 0xF1, 0x02, 0x00, 0xD8, 0x00]);
    o.op(&[0xE7, 0x06]).abs(0).abs(0);
    o.op(&[0xE7, 0x38, 0x00]);
    o.op(&[0xE7, 0x03]).point(dmin);
    o.op(&[0xE7, 0x07]).point(dmax);
    o.op(&[0xE7, 0x50]).point(dmin);
    o.op(&[0xE7, 0x51]).point(dmax);
    o.op(&[0xE7, 0x04, 0x00, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    o.op(&[0xE7, 0x05, 0x00]);

    // Layer table.
    for (p, (l, (lmin, lmax))) in layers.iter().zip(&layer_bounds).enumerate() {
        let p = p as u8;
        o.op(&[0xC9, 0x04, p]).speed(l.speed);
        o.op(&[0xC6, 0x31, p]).power(l.min_power);
        o.op(&[0xC6, 0x32, p]).power(l.max_power);
        o.op(&[0xC6, 0x41, p]).power(LASER2_POWER);
        o.op(&[0xC6, 0x42, p]).power(LASER2_POWER);
        let [r, g, b] = l.color;
        o.op(&[0xCA, 0x06, p]).op(&encode_u35(r as u64 | (g as u64) << 8 | (b as u64) << 16));
        let mode = match l.kind {
            LayerKind::Cut => 0x00,
            LayerKind::Engrave { .. } => 0x02,
        };
        o.op(&[0xCA, 0x41, p, mode]);
        o.op(&[0xE7, 0x52, p]).point(*lmin);
        o.op(&[0xE7, 0x53, p]).point(*lmax);
        o.op(&[0xE7, 0x61, p]).point(*lmin);
        o.op(&[0xE7, 0x62, p]).point(*lmax);
    }
    o.op(&[0xCA, 0x22, (layers.len() - 1) as u8]);
    for axis in [0x00, 0x01] {
        o.op(&[0xE7, 0x54, axis]).abs(0);
    }
    for axis in [0x00, 0x01] {
        o.op(&[0xE7, 0x55, axis]).abs(0);
    }

    // Element / array block wrapping all layers.
    o.op(&[0xF1, 0x03]).abs(0).abs(0);
    o.op(&[0xF1, 0x00, 0x00, 0xF1, 0x01, 0x00, 0xF2, 0x00, 0x00, 0xF2, 0x01, 0x00]);
    o.op(&[0xF2, 0x02]).op(&ELEMENT_NAME);
    o.op(&[0xF2, 0x03]).point(dmin);
    o.op(&[0xF2, 0x04]).point(dmax);
    o.op(&[0xF2, 0x05, 0x00, 0x01, 0x00, 0x01]).point(size);
    o.op(&[0xF2, 0x06]).abs(0).abs(0);
    o.op(&[0xF2, 0x07, 0x00]);
    o.op(&[0xEA, 0x00, 0xE7, 0x60, 0x00]);
    o.op(&[0xE7, 0x13]).point(dmin);
    o.op(&[0xE7, 0x17]).point(dmax);
    o.op(&[0xE7, 0x23]).abs(0).abs(0);
    o.op(&[0xE7, 0x24, 0x00]);
    o.op(&[0xE7, 0x37]).point(size);
    o.op(&[0xE7, 0x08, 0x00, 0x01, 0x00, 0x01]).point(size);

    // Layer bodies.
    for (p, l) in layers.iter().enumerate() {
        o.op(&[0xCA, 0x01, match l.kind {
            LayerKind::Cut => 0x00,
            LayerKind::Engrave { .. } => 0x01,
        }]);
        o.op(&[0xCA, 0x02, p as u8]);
        o.op(&[0xCA, 0x01, 0x30, 0xCA, 0x01, 0x10]);
        o.op(&[0xCA, 0x01, if l.air_assist { 0x13 } else { 0x12 }]);
        match l.kind {
            LayerKind::Cut => write_cut_body(&mut o, l),
            LayerKind::Engrave { interval } => write_engrave_body(&mut o, l, interval),
        }
    }
    o.op(&[0xE7, 0x00, 0xEB, 0xE7, 0x00]);

    // Checksum: sum of every byte in the file except this command itself.
    let sum: u64 = o.0.iter().map(|&b| b as u64).sum::<u64>() + 0xD7;
    o.op(&[0xE5, 0x05]).op(&encode_u35(sum));
    o.op(&[0xD7]);
    Ok(o.0)
}

/// Scrambled job file for the 644x.
pub fn write_file(layers: &[JobLayer]) -> Result<Vec<u8>, WriteError> {
    Ok(scramble(&write_plain(layers)?, MAGIC_644X))
}

/// The checksum a plain job carries in its `E5 05` command, and the one
/// recomputed from its bytes. Useful to validate files from other software.
pub fn checksums(plain: &[u8]) -> Option<(u64, u64)> {
    let at = plain.windows(2).rposition(|w| w == [0xE5, 0x05])?;
    let stored = decode_u35(plain.get(at + 2..at + 7)?);
    let total: u64 = plain.iter().map(|&b| b as u64).sum();
    let own: u64 = plain[at..at + 7].iter().map(|&b| b as u64).sum();
    Some((stored, total - own))
}
