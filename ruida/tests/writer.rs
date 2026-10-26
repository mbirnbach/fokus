//! The writer against LightBurn's real job (tests/data/lightburn_square_text.rd).
//! Asking for only its square layer, with the same settings, must reproduce
//! LightBurn's bytes for every part that doesn't depend on the text layer.

use ruida::command::{parse, Command};
use ruida::job::Toolpath;
use ruida::scramble::{unscramble, MAGIC_644X};
use ruida::writer::{checksums, write_file, write_plain, JobLayer, LayerKind, WriteError};

fn lightburn_plain() -> Vec<u8> {
    let raw = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/lightburn_square_text.rd")).unwrap();
    unscramble(&raw, MAGIC_644X)
}

/// LightBurn's layer C00: 25 mm square, cut at 25 mm/s, 100 %, starting at (25, 0).
fn square_layer() -> JobLayer {
    JobLayer {
        kind: LayerKind::Cut,
        color: [0, 0, 0],
        speed: 25.0,
        min_power: 100.0,
        max_power: 100.0,
        passes: 1,
        air_assist: true,
        paths: vec![vec![(25_000, 0), (0, 0), (0, 25_000), (25_000, 25_000), (25_000, 0)]],
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[test]
fn checksum_matches_lightburn_file() {
    let (stored, computed) = checksums(&lightburn_plain()).unwrap();
    assert_eq!(stored, computed);
}

#[test]
fn document_header_and_layer_table_match_lightburn() {
    let lb = lightburn_plain();
    let ours = write_plain(&[square_layer()]).unwrap();
    // Document header + layer 0 table: LightBurn bytes 0x00..0xb6.
    assert_eq!(&ours[..0xb6], &lb[..0xb6]);
}

#[test]
fn element_block_matches_lightburn() {
    let lb = lightburn_plain();
    let ours = write_plain(&[square_layer()]).unwrap();
    // From the pen offsets (E7 54) up to the first layer body (CA 01 00 CA 02 00).
    let lb_block = &lb[0x115..0x1d8];
    let start = find(&ours, &[0xE7, 0x54, 0x00]).unwrap();
    assert_eq!(&ours[start..start + lb_block.len()], lb_block);
}

#[test]
fn layer_body_matches_lightburn() {
    let lb = lightburn_plain();
    let ours = write_plain(&[square_layer()]).unwrap();
    let lb_body = &lb[0x1d8..0x24e]; // CA 01 00 ... last cut of the square
    let start = find(&ours, &[0xCA, 0x01, 0x00, 0xCA, 0x02, 0x00]).unwrap();
    assert_eq!(&ours[start..start + lb_body.len()], lb_body);
}

#[test]
fn own_checksum_is_consistent() {
    let ours = write_plain(&[square_layer()]).unwrap();
    let (stored, computed) = checksums(&ours).unwrap();
    assert_eq!(stored, computed);
}

#[test]
fn roundtrip_through_decoder() {
    let mut second = square_layer();
    second.color = [255, 0, 0];
    second.speed = 10.0;
    second.min_power = 30.0;
    second.max_power = 60.0;
    second.passes = 2;
    second.paths = vec![vec![(5_000, 5_000), (20_000, 20_000)]];

    let file = write_file(&[square_layer(), second]).unwrap();
    let records = parse(&unscramble(&file, MAGIC_644X));
    let tp = Toolpath::from_records(&records);

    assert_eq!(tp.unknown_commands, 0);
    assert_eq!(tp.layers.len(), 2);
    assert_eq!(tp.layers[&0].speed, Some(25.0));
    assert_eq!(tp.layers[&1].speed, Some(10.0));
    assert!((tp.layers[&1].min_power.unwrap() - 30.0).abs() < 0.01);
    assert!((tp.layers[&1].max_power.unwrap() - 60.0).abs() < 0.01);
    assert_eq!(tp.layers[&1].color, Some([255, 0, 0]));
    // Two passes over a 15 mm diagonal.
    assert!((tp.layers[&1].cut_mm - 2.0 * 15_000f64.hypot(15_000.0) / 1000.0).abs() < 1e-6);
    assert_eq!(tp.cut_bounds(), Some(((0, 0), (25_000, 25_000))));
    assert!(matches!(records.last().unwrap().cmd, Command::Known("end of file")));
}

#[test]
fn rejects_bad_settings() {
    let mut l = square_layer();
    l.max_power = 120.0;
    assert_eq!(write_plain(&[l]), Err(WriteError::BadPower { layer: 0 }));

    let mut l = square_layer();
    l.speed = 0.0;
    assert!(matches!(write_plain(&[l]), Err(WriteError::BadSpeed { .. })));

    let mut l = square_layer();
    l.paths.clear();
    assert_eq!(write_plain(&[l]), Err(WriteError::NoGeometry));
}

#[test]
fn long_engrave_lines_are_split_and_decode_back() {
    // A 30 x 2 mm bar: every line is longer than one relative step (8.191 mm).
    let bar = JobLayer {
        kind: LayerKind::Engrave { interval: 100 },
        color: [255, 0, 0],
        speed: 300.0,
        min_power: 30.0,
        max_power: 30.0,
        passes: 2,
        air_assist: true,
        paths: vec![vec![(0, 0), (30_000, 0), (30_000, 2_000), (0, 2_000)]],
    };
    let tp = Toolpath::from_records(&parse(&write_plain(&[bar]).unwrap()));
    assert_eq!(tp.unknown_commands, 0);
    // 20 lines of 30 mm, two passes.
    assert!((tp.layers[&0].cut_mm - 2.0 * 20.0 * 30.0).abs() < 1e-6, "{}", tp.layers[&0].cut_mm);
    assert_eq!(tp.cut_bounds(), Some(((0, 50), (30_000, 1_950))));
}

#[test]
fn coordinates_on_both_sides_of_the_head_roundtrip() {
    // A square centred on the head (a centre start point).
    let mut l = square_layer();
    l.paths = vec![vec![(-12_500, -12_500), (12_500, -12_500), (12_500, 12_500), (-12_500, 12_500), (-12_500, -12_500)]];
    let tp = Toolpath::from_records(&parse(&write_plain(&[l]).unwrap()));
    assert_eq!(tp.unknown_commands, 0);
    assert_eq!(tp.cut_bounds(), Some(((-12_500, -12_500), (12_500, 12_500))));
    assert!((tp.layers[&0].cut_mm - 100.0).abs() < 1e-6);
}

/// Regression from the first live job: with engraving first and cutting
/// second, the work modes were swapped (cut only burned horizontal lines).
#[test]
fn each_layer_body_gets_its_own_work_mode() {
    let engrave = JobLayer {
        kind: LayerKind::Engrave { interval: 150 },
        color: [255, 0, 0],
        speed: 300.0,
        min_power: 30.0,
        max_power: 30.0,
        passes: 1,
        air_assist: true,
        paths: vec![vec![(5_000, 5_000), (20_000, 5_000), (20_000, 10_000), (5_000, 10_000)]],
    };
    let plain = write_plain(&[engrave, square_layer()]).unwrap();
    let body = |part: u8| {
        let at = plain.windows(3).position(|w| w == [0xCA, 0x02, part]).unwrap();
        plain[at - 3..at].to_vec()
    };
    assert_eq!(body(0), vec![0xCA, 0x01, 0x01], "engrave layer: scan mode");
    assert_eq!(body(1), vec![0xCA, 0x01, 0x00], "cut layer: cut mode");
}
