//! A real job saved by LightBurn 1.7 for our Ruida 644XG
//! (see tests/data/lightburn_square_text.lbrn2 for the source project):
//! - layer C00, cut, 25 mm/s, 100 % / 100 %: a 25 x 25 mm square
//! - layer C02, scan, 220 mm/s, 23 % / 23 %: the text "TEST", 5 mm high

use ruida::command::parse;
use ruida::job::Toolpath;
use ruida::scramble::{detect_magic, unscramble, MAGIC_644X};

fn decode() -> Toolpath {
    let raw = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/lightburn_square_text.rd")).unwrap();
    assert_eq!(detect_magic(&raw), Some(MAGIC_644X));
    Toolpath::from_records(&parse(&unscramble(&raw, MAGIC_644X)))
}

#[test]
fn every_command_is_recognised() {
    assert_eq!(decode().unknown_commands, 0);
}

#[test]
fn layer_settings_match_lightburn() {
    let tp = decode();
    assert_eq!(tp.layers.len(), 2);

    let cut = &tp.layers[&0];
    assert_eq!(cut.speed, Some(25.0));
    assert_eq!(cut.min_power, Some(100.0));
    assert_eq!(cut.max_power, Some(100.0));
    // Perimeter of the 25 mm square.
    assert!((cut.cut_mm - 100.0).abs() < 1e-6);

    let scan = &tp.layers[&1];
    assert_eq!(scan.speed, Some(220.0));
    assert!((scan.min_power.unwrap() - 23.0).abs() < 0.01);
    assert!((scan.max_power.unwrap() - 23.0).abs() < 0.01);
}

#[test]
fn geometry_fits_the_square() {
    let tp = decode();
    assert_eq!(tp.cut_bounds(), Some(((0, 0), (25_000, 25_000))));

    // The text is 5 mm high LightBurn units, centred horizontally in the square.
    let text = Toolpath { segments: tp.segments.iter().filter(|s| s.cut && s.layer == Some(1)).copied().collect(), ..Default::default() };
    let ((x0, y0), (x1, y1)) = text.bounds().unwrap();
    assert!((x0 + x1 - 25_000).abs() < 1_000, "text not centred: x {x0}..{x1}");
    assert!((3_000..=4_000).contains(&(y1 - y0)), "text height {}", y1 - y0);
}
