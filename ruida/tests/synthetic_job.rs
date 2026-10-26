//! A hand-built job: one layer, a 20 x 20 mm square. Not a valid file for a
//! real machine (no header), only for exercising the decoder.

use ruida::codec::*;
use ruida::command::parse;
use ruida::job::Toolpath;
use ruida::scramble::{detect_magic, scramble, unscramble, MAGIC_644X};

fn square_job() -> Vec<u8> {
    let mut d = Vec::new();
    d.extend([0xC9, 0x04, 0x00]);
    d.extend(encode_speed(20.0));
    d.extend([0xC6, 0x31, 0x00]);
    d.extend(encode_power(30.0));
    d.extend([0xC6, 0x32, 0x00]);
    d.extend(encode_power(50.0));
    d.extend([0xCA, 0x02, 0x00]);
    d.push(0x88);
    d.extend(encode_abs(10_000));
    d.extend(encode_abs(10_000));
    for (x, y) in [(30_000, 10_000), (30_000, 30_000), (10_000, 30_000), (10_000, 10_000)] {
        d.push(0xA8);
        d.extend(encode_abs(x));
        d.extend(encode_abs(y));
    }
    d.push(0xD7);
    d
}

#[test]
fn detects_magic_and_decodes_square() {
    let plain = square_job();
    let scrambled = scramble(&plain, MAGIC_644X);

    assert_eq!(detect_magic(&scrambled), Some(MAGIC_644X));
    assert_eq!(unscramble(&scrambled, MAGIC_644X), plain);

    let tp = Toolpath::from_records(&parse(&plain));
    assert_eq!(tp.unknown_commands, 0);
    assert_eq!(tp.segments.iter().filter(|s| s.cut).count(), 4);
    assert_eq!(tp.cut_bounds(), Some(((10_000, 10_000), (30_000, 30_000))));

    let layer = &tp.layers[&0];
    assert_eq!(layer.speed, Some(20.0));
    assert!((layer.min_power.unwrap() - 30.0).abs() < 0.01);
    assert!((layer.max_power.unwrap() - 50.0).abs() < 0.01);
    assert!((layer.cut_mm - 80.0).abs() < 1e-9);
}
