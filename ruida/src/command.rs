//! Splitting an unscrambled byte stream into commands and decoding them.
//!
//! Every byte >= 0x80 starts a new command; argument bytes are always < 0x80.
//! That makes splitting robust even for commands we don't understand yet.

use std::fmt;

use crate::codec::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerKind {
    Immediate,
    End,
    Min,
    Max,
    Through,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Travel move, laser off. Coordinates in µm.
    MoveAbs { x: i32, y: i32 },
    MoveRel { dx: i32, dy: i32 },
    /// Cutting move, laser on.
    CutAbs { x: i32, y: i32 },
    CutRel { dx: i32, dy: i32 },
    Power { kind: PowerKind, laser: u8, part: Option<u8>, percent: f32 },
    Speed { what: &'static str, part: Option<u8>, mm_per_s: f64 },
    /// Selects the layer ("part") the following moves belong to.
    Layer(u8),
    LayerColor { part: Option<u8>, rgb: [u8; 3] },
    AirAssist(bool),
    /// Declared bounding box corner.
    Bound { what: &'static str, part: Option<u8>, x: i32, y: i32 },
    /// Recognised but not interpreted; arguments are shown as hex.
    Known(&'static str),
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// Byte offset in the (unscrambled) stream.
    pub offset: usize,
    pub bytes: Vec<u8>,
    pub cmd: Command,
}

pub fn parse(data: &[u8]) -> Vec<Record> {
    let mut records = Vec::new();
    let mut start = None;
    let mut push = |s: usize, end: usize| {
        records.push(Record { offset: s, bytes: data[s..end].to_vec(), cmd: decode(&data[s..end]) });
    };
    for (i, &b) in data.iter().enumerate() {
        match start {
            None => start = Some(i),
            Some(s) if b >= 0x80 => {
                push(s, i);
                start = Some(i);
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        push(s, data.len());
    }
    records
}

pub fn decode(c: &[u8]) -> Command {
    use Command::*;
    let Some((&op, a)) = c.split_first() else { return Unknown };
    if op < 0x80 {
        return Unknown;
    }
    let rel = |b: &[u8]| decode_i14(b) as i32;
    match (op, a.len()) {
        (0x88, 10) => MoveAbs { x: decode_abs(&a[0..5]), y: decode_abs(&a[5..10]) },
        (0x89, 4) => MoveRel { dx: rel(&a[0..2]), dy: rel(&a[2..4]) },
        (0x8A, 2) => MoveRel { dx: rel(a), dy: 0 },
        (0x8B, 2) => MoveRel { dx: 0, dy: rel(a) },
        (0xA8, 10) => CutAbs { x: decode_abs(&a[0..5]), y: decode_abs(&a[5..10]) },
        (0xA9, 4) => CutRel { dx: rel(&a[0..2]), dy: rel(&a[2..4]) },
        (0xAA, 2) => CutRel { dx: rel(a), dy: 0 },
        (0xAB, 2) => CutRel { dx: 0, dy: rel(a) },
        (0x80, 6) | (0xA0, 6) => Known("axis move"),
        (0xC7, 2) => power(PowerKind::Immediate, 1, None, a),
        (0xC0, 2) => power(PowerKind::Immediate, 2, None, a),
        (0xC2, 2) => power(PowerKind::Immediate, 3, None, a),
        (0xC3, 2) => power(PowerKind::Immediate, 4, None, a),
        (0xC8, 2) => power(PowerKind::End, 1, None, a),
        (0xC1, 2) => power(PowerKind::End, 2, None, a),
        (0xC4, 2) => power(PowerKind::End, 3, None, a),
        (0xC5, 2) => power(PowerKind::End, 4, None, a),
        (0xC6, n) if n >= 1 => decode_c6(a[0], &a[1..]),
        (0xC9, n) if n >= 1 => decode_c9(a[0], &a[1..]),
        (0xCA, n) if n >= 1 => decode_ca(a[0], &a[1..]),
        (0xCC, 0) => Known("ACK"),
        (0xCD, 0) => Known("ERR"),
        (0xCE, 0) => Known("keep alive"),
        (0xD0, 1) => Known("inhale zone"),
        (0xD7, 0) => Known("end of file"),
        (0xD8, 1) => Known(match a[0] {
            0x00 => "start process",
            0x10..=0x12 => "reference point mode",
            0x2A => "home XY",
            0x2C => "home Z",
            0x2D => "home U",
            0x2E => "focus Z",
            0x20..=0x27 => "key down",
            0x30..=0x37 => "key up",
            _ => "control",
        }),
        (0xD9, _) => Known("rapid move"),
        (0xDA, n) if n >= 1 => Known(match a[0] {
            0x00 => "get setting",
            0x01 => "set setting",
            _ => "setting",
        }),
        (0xE5, n) if n >= 1 => Known(match a[0] {
            0x00 => "document page",
            0x02 => "document data end",
            0x05 => "file checksum",
            _ => "document",
        }),
        (0xE6, 1) if a[0] == 0x01 => Known("set absolute"),
        (0xE7, n) if n >= 1 => decode_e7(a[0], &a[1..]),
        (0xE8, _) => Known("realtime"),
        (0xEA, 1) => Known("array start"),
        (0xEB, 0) => Known("array end"),
        (0xF0, 0) => Known("ref point set"),
        (0xF1, _) => Known("element info"),
        (0xF2, _) => Known("element"),
        _ => Unknown,
    }
}

fn power(kind: PowerKind, laser: u8, part: Option<u8>, b: &[u8]) -> Command {
    Command::Power { kind, laser, part, percent: decode_power(b) }
}

fn decode_c6(sub: u8, s: &[u8]) -> Command {
    use PowerKind::*;
    let global = |kind, laser| if s.len() == 2 { power(kind, laser, None, s) } else { Command::Unknown };
    let per_part = |kind, laser| if s.len() == 3 { power(kind, laser, Some(s[0]), &s[1..]) } else { Command::Unknown };
    match sub {
        0x01 => global(Min, 1),
        0x02 => global(Max, 1),
        0x21 => global(Min, 2),
        0x22 => global(Max, 2),
        0x05 => global(Min, 3),
        0x06 => global(Max, 3),
        0x07 => global(Min, 4),
        0x08 => global(Max, 4),
        0x31 => per_part(Min, 1),
        0x32 => per_part(Max, 1),
        0x41 => per_part(Min, 2),
        0x42 => per_part(Max, 2),
        0x35 => per_part(Min, 3),
        0x36 => per_part(Max, 3),
        0x37 => per_part(Min, 4),
        0x38 => per_part(Max, 4),
        0x50 => global(Through, 1),
        0x51 => global(Through, 2),
        0x55 => global(Through, 3),
        0x56 => global(Through, 4),
        0x10 => Command::Known("laser interval"),
        0x11 => Command::Known("add delay"),
        0x12 => Command::Known("laser on delay"),
        0x13 => Command::Known("laser off delay"),
        0x15 => Command::Known("laser on delay 2"),
        0x16 => Command::Known("laser off delay 2"),
        0x60 => Command::Known("laser frequency"),
        _ => Command::Unknown,
    }
}

fn decode_c9(sub: u8, s: &[u8]) -> Command {
    let speed = |what, part: Option<u8>, b: &[u8]| {
        if b.len() == 5 {
            Command::Speed { what, part, mm_per_s: decode_speed(b) }
        } else {
            Command::Unknown
        }
    };
    match sub {
        0x02 => speed("laser", None, s),
        0x03 => speed("axis", None, s),
        0x04 if s.len() == 6 => speed("layer", Some(s[0]), &s[1..]),
        0x05 => speed("force engrave", None, s),
        0x06 => speed("axis move", None, s),
        _ => Command::Unknown,
    }
}

fn decode_ca(sub: u8, s: &[u8]) -> Command {
    match (sub, s.len()) {
        (0x01, 1) => match s[0] {
            0x00 => Command::Known("layer end"),
            0x12 => Command::AirAssist(false),
            0x13 => Command::AirAssist(true),
            0x10 | 0x11 => Command::Known("laser device"),
            _ => Command::Known("work mode / flag"),
        },
        (0x02, 1) => Command::Layer(s[0]),
        // LightBurn sends this with one argument (seen: 01); MeerK40t lists none.
        (0x03, 0 | 1) => Command::Known("enable laser tube start"),
        (0x04, 1) => Command::Known("X sign map"),
        (0x05, 5) => Command::LayerColor { part: None, rgb: decode_color(s) },
        (0x06, 6) => Command::LayerColor { part: Some(s[0]), rgb: decode_color(&s[1..]) },
        (0x10, 1) => Command::Known("enable external IO"),
        (0x22, 1) => Command::Known("max layer"),
        (0x30, 2) => Command::Known("U file id"),
        (0x40, 1) => Command::Known("ZU map"),
        (0x41, 2) => Command::Known("layer work mode"),
        _ => Command::Unknown,
    }
}

fn decode_e7(sub: u8, s: &[u8]) -> Command {
    let bound = |what| {
        if s.len() == 10 {
            Command::Bound { what, part: None, x: decode_abs(&s[0..5]), y: decode_abs(&s[5..10]) }
        } else {
            Command::Unknown
        }
    };
    let part_bound = |what| {
        if s.len() == 11 {
            Command::Bound { what, part: Some(s[0]), x: decode_abs(&s[1..6]), y: decode_abs(&s[6..11]) }
        } else {
            Command::Unknown
        }
    };
    match sub {
        0x00 => Command::Known("block end"),
        0x01 => Command::Known("filename"),
        0x03 => bound("process top-left"),
        0x07 => bound("process bottom-right"),
        0x13 => bound("array min"),
        0x17 => bound("array max"),
        0x50 => bound("document min"),
        0x51 => bound("document max"),
        0x52 => part_bound("layer min"),
        0x53 => part_bound("layer max"),
        0x61 => part_bound("layer min ex"),
        0x62 => part_bound("layer max ex"),
        _ => Command::Known("job info"),
    }
}

fn mm(um: i32) -> f64 {
    um as f64 / 1000.0
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Command::*;
        match self {
            MoveAbs { x, y } => write!(f, "move to   x={:.3} y={:.3} mm", mm(*x), mm(*y)),
            MoveRel { dx, dy } => write!(f, "move by   dx={:.3} dy={:.3} mm", mm(*dx), mm(*dy)),
            CutAbs { x, y } => write!(f, "CUT to    x={:.3} y={:.3} mm", mm(*x), mm(*y)),
            CutRel { dx, dy } => write!(f, "CUT by    dx={:.3} dy={:.3} mm", mm(*dx), mm(*dy)),
            Power { kind, laser, part, percent } => {
                write!(f, "power {kind:?} laser {laser}")?;
                if let Some(p) = part {
                    write!(f, " layer {p}")?;
                }
                write!(f, " = {percent:.1}%")
            }
            Speed { what, part, mm_per_s } => {
                write!(f, "speed {what}")?;
                if let Some(p) = part {
                    write!(f, " {p}")?;
                }
                write!(f, " = {mm_per_s:.1} mm/s")
            }
            Layer(p) => write!(f, "--- layer {p} ---"),
            LayerColor { part, rgb: [r, g, b] } => {
                write!(f, "layer color")?;
                if let Some(p) = part {
                    write!(f, " {p}")?;
                }
                write!(f, " = #{r:02x}{g:02x}{b:02x}")
            }
            AirAssist(on) => write!(f, "air assist {}", if *on { "on" } else { "off" }),
            Bound { what, part, x, y } => {
                write!(f, "{what}")?;
                if let Some(p) = part {
                    write!(f, " {p}")?;
                }
                write!(f, " x={:.3} y={:.3} mm", mm(*x), mm(*y))
            }
            Known(name) => write!(f, "{name}"),
            Unknown => write!(f, "??"),
        }
    }
}

impl fmt::Display for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex: Vec<String> = self.bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut hex = hex.join(" ");
        if hex.len() > 41 {
            hex.truncate(38);
            hex.push_str("...");
        }
        write!(f, "{:06x}  {hex:<41}  {}", self.offset, self.cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_decodes() {
        let mut data = vec![0xCA, 0x02, 0x03];
        data.push(0xA8);
        data.extend(encode_abs(20_000));
        data.extend(encode_abs(-5_000));
        data.push(0xAA);
        data.extend(encode_i14(-1_500));
        data.push(0xD7);

        let cmds: Vec<Command> = parse(&data).into_iter().map(|r| r.cmd).collect();
        assert_eq!(
            cmds,
            vec![
                Command::Layer(3),
                Command::CutAbs { x: 20_000, y: -5_000 },
                Command::CutRel { dx: -1_500, dy: 0 },
                Command::Known("end of file"),
            ]
        );
    }

    #[test]
    fn wrong_length_is_unknown() {
        assert_eq!(decode(&[0x88, 0x00, 0x00]), Command::Unknown);
    }
}
