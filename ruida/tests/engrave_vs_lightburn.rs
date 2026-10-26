//! Our scan fill against LightBurn's, for the same glyph outlines.
//!
//! tests/data/lightburn_square_text.lbrn2 stores the engraved "TEST" as
//! Bézier outlines; the .rd next to it holds LightBurn's scan lines for them.
//! We rebuild the outlines, fill them ourselves, and compare line by line.

use std::collections::BTreeMap;

use ruida::command::{parse, Command, Record};
use ruida::fill::spans_at;
use ruida::scramble::{unscramble, MAGIC_644X};
use ruida::writer::{write_plain, JobLayer, LayerKind};

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/lightburn_square_text");

/// Text position in job coordinates (mm): the shape's XForm translation
/// minus the square's corner, which is the job origin (570.5 - 12.5, 186.5 - 12.5).
const TEXT_OFFSET: (f64, f64) = (570.16992 - 558.0, 191.73912 - 174.0);

struct Vert {
    pos: (f64, f64),
    c0: Option<(f64, f64)>,
    c1: Option<(f64, f64)>,
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> &'a str {
    let a = s.find(open).unwrap() + open.len();
    let b = a + s[a..].find(close).unwrap();
    &s[a..b]
}

/// LightBurn VertList: `V<x> <y>[c0x<x>c0y<y>][c1x<x>c1y<y>]`; `c0x1` alone means no control point.
fn parse_verts(list: &str) -> Vec<Vert> {
    list.split('V')
        .filter(|c| !c.trim().is_empty())
        .map(|chunk| {
            let (head, rest) = chunk.split_at(chunk.find('c').unwrap_or(chunk.len()));
            let mut it = head.split_whitespace().map(|v| v.parse::<f64>().unwrap());
            let pos = (it.next().unwrap(), it.next().unwrap());
            let mut c = BTreeMap::new();
            for part in rest.split('c').filter(|p| !p.is_empty()) {
                c.insert(&part[..2], part[2..].parse::<f64>().unwrap());
            }
            let ctrl = |k: &str| c.get(format!("{k}y").as_str()).map(|&y| (c[format!("{k}x").as_str()], y));
            Vert { pos, c0: ctrl("0"), c1: ctrl("1") }
        })
        .collect()
}

/// Closed outlines in job µm, Béziers flattened finely.
fn glyph_outlines() -> Vec<Vec<(i32, i32)>> {
    let project = std::fs::read_to_string(format!("{DATA}.lbrn2")).unwrap();
    let verts = parse_verts(between(&project, "<VertList>", "</VertList>"));
    let prims = between(&project, "<PrimList>", "</PrimList>").replace('B', " B ").replace('L', " L ");
    let tokens: Vec<&str> = prims.split_whitespace().collect();

    let um = |(x, y): (f64, f64)| (((x + TEXT_OFFSET.0) * 1000.0).round() as i32, ((y + TEXT_OFFSET.1) * 1000.0).round() as i32);
    let mut outlines: Vec<Vec<(i32, i32)>> = Vec::new();
    let mut last_end = None;
    for t in tokens.chunks(3) {
        let (a, b): (usize, usize) = (t[1].parse().unwrap(), t[2].parse().unwrap());
        if last_end != Some(a) {
            outlines.push(vec![um(verts[a].pos)]);
        }
        let (p0, p1) = (verts[a].pos, verts[b].pos);
        let poly = outlines.last_mut().unwrap();
        if t[0] == "B" {
            let c0 = verts[a].c0.unwrap_or(p0);
            let c1 = verts[b].c1.unwrap_or(p1);
            for i in 1..=64 {
                let s = i as f64 / 64.0;
                let u = 1.0 - s;
                let (k0, k1, k2, k3) = (u * u * u, 3.0 * u * u * s, 3.0 * u * s * s, s * s * s);
                poly.push(um((k0 * p0.0 + k1 * c0.0 + k2 * c1.0 + k3 * p1.0, k0 * p0.1 + k1 * c0.1 + k2 * c1.1 + k3 * p1.1)));
            }
        } else {
            poly.push(um(p1));
        }
        last_end = Some(b);
    }
    outlines
}

/// Scan lines in a decoded job, from the `Layer(part)` marker on:
/// y -> spans as sorted (min, max).
fn scan_lines(records: &[Record], part: u8) -> BTreeMap<i32, Vec<(i32, i32)>> {
    let start = records.iter().position(|r| r.cmd == Command::Layer(part)).unwrap();
    let mut lines: BTreeMap<i32, Vec<(i32, i32)>> = BTreeMap::new();
    let mut pos = (0, 0);
    for r in &records[start..] {
        match r.cmd {
            Command::MoveAbs { x, y } => pos = (x, y),
            Command::MoveRel { dx, dy } => pos = (pos.0 + dx, pos.1 + dy),
            Command::CutRel { dx, dy } => {
                assert_eq!(dy, 0, "scan cuts are horizontal");
                let to = pos.0 + dx;
                lines.entry(pos.1).or_default().push((pos.0.min(to), pos.0.max(to)));
                pos.0 = to;
            }
            Command::Layer(p) if p != part => break,
            _ => {}
        }
    }
    for spans in lines.values_mut() {
        spans.sort();
        // Join pieces of one span split into several steps.
        spans.dedup_by(|b, a| {
            if b.0 == a.1 {
                a.1 = b.1;
                true
            } else {
                false
            }
        });
    }
    lines
}

fn lightburn_records() -> Vec<Record> {
    parse(&unscramble(&std::fs::read(format!("{DATA}.rd")).unwrap(), MAGIC_644X))
}

#[test]
fn outlines_match_lightburn_layer_bounds() {
    let pts: Vec<(i32, i32)> = glyph_outlines().into_iter().flatten().collect();
    let x0 = pts.iter().map(|p| p.0).min().unwrap();
    let x1 = pts.iter().map(|p| p.0).max().unwrap();
    let y0 = pts.iter().map(|p| p.1).min().unwrap();
    let y1 = pts.iter().map(|p| p.1).max().unwrap();
    // LightBurn's layer 1 bounds: x 5.820..18.410, y 16.980..20.680 mm.
    for (ours, lb) in [(x0, 5_820), (x1, 18_410), (y0, 16_980), (y1, 20_680)] {
        assert!((ours - lb).abs() <= 10, "bounds {ours} vs {lb}");
    }
}

#[test]
fn fill_matches_lightburn_line_by_line() {
    let outlines = glyph_outlines();
    let lb = scan_lines(&lightburn_records(), 1);
    assert_eq!(lb.len(), 38);

    // The S (job x ≈ 9.1..11.9 mm) is the only curved glyph. Where its
    // outline runs almost horizontally (top and bottom of each bowl), a few
    // µm of difference in how finely LightBurn flattens Béziers moves the
    // span ends a lot, so it gets a looser bound than the straight letters.
    let in_s = |s: &(i32, i32)| s.0 >= 9_000 && s.1 <= 12_000;
    let (mut straight, mut curved) = (0, 0);
    for (&y, lb_spans) in &lb {
        let ours = spans_at(&outlines, y as f64);
        assert_eq!(ours.len(), lb_spans.len(), "line y={y}: ours {ours:?}, LightBurn {lb_spans:?}");
        for (a, b) in ours.iter().zip(lb_spans) {
            let d = (a.0 - b.0).abs().max((a.1 - b.1).abs());
            if in_s(b) {
                curved = curved.max(d);
            } else {
                straight = straight.max(d);
            }
        }
    }
    assert!(straight <= 10, "straight glyphs: largest span end difference {straight} µm");
    assert!(curved <= 200, "curved glyph: largest span end difference {curved} µm");
}

#[test]
fn engrave_layer_encoding_matches_lightburn() {
    let lb_records = lightburn_records();
    let raw = std::fs::read(format!("{DATA}.rd")).unwrap();
    let lb_plain = unscramble(&raw, MAGIC_644X);

    let square = JobLayer {
        kind: LayerKind::Cut,
        color: [0, 0, 0],
        speed: 25.0,
        min_power: 100.0,
        max_power: 100.0,
        passes: 1,
        air_assist: true,
        paths: vec![vec![(25_000, 0), (0, 0), (0, 25_000), (25_000, 25_000), (25_000, 0)]],
    };
    let text = JobLayer {
        kind: LayerKind::Engrave { interval: 100 },
        color: [255, 0, 0],
        speed: 220.0,
        min_power: 23.0,
        max_power: 23.0,
        passes: 1,
        air_assist: true,
        paths: glyph_outlines(),
    };
    let ours = write_plain(&[square, text]).unwrap();

    // Layer body settings: LightBurn bytes 0x24e..0x285 (CA 01 01 ... CA 03 01).
    let header = &lb_plain[0x24e..0x285];
    let at = ours.windows(6).position(|w| w == [0xCA, 0x01, 0x01, 0xCA, 0x02, 0x01]).unwrap();
    assert_eq!(&ours[at..at + header.len()], header);

    // Same commands in the layer table entry (work mode 02 = scan).
    assert!(ours.windows(4).any(|w| w == [0xCA, 0x41, 0x01, 0x02]));

    // Decoded: LightBurn puts its first line 3 µm inside the top (38 lines);
    // we centre the lines half an interval inside (37 lines). Burn length
    // stays within 2 %.
    let our_records = parse(&ours);
    let our_lines = scan_lines(&our_records, 1);
    assert_eq!(our_lines.len(), 37);
    let total = |m: &BTreeMap<i32, Vec<(i32, i32)>>| m.values().flatten().map(|s| (s.1 - s.0) as f64).sum::<f64>();
    let (a, b) = (total(&our_lines), total(&scan_lines(&lb_records, 1)));
    assert!((a - b).abs() / b < 0.02, "burn length ours {a} µm vs LightBurn {b} µm");
}
