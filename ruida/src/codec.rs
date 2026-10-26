//! Number encodings.
//!
//! Command bytes have the high bit set; argument bytes never do. Numbers are
//! therefore packed 7 bits per byte, big-endian:
//! - 2 bytes = 14 bits: relative coordinates (signed), power, small values
//! - 5 bytes = 35 bits: absolute coordinates (signed 32-bit), speeds, times
//!
//! Coordinates are in micrometres, speeds in µm/s, power in 1/16383ths.

pub fn decode_u14(b: &[u8]) -> u16 {
    ((b[0] as u16 & 0x7F) << 7) | (b[1] as u16 & 0x7F)
}

pub fn decode_i14(b: &[u8]) -> i16 {
    let v = decode_u14(b) as i16;
    if v >= 0x2000 {
        v - 0x4000
    } else {
        v
    }
}

pub fn decode_u35(b: &[u8]) -> u64 {
    b[..5].iter().fold(0, |acc, &x| (acc << 7) | (x & 0x7F) as u64)
}

/// Absolute coordinate: 35 bits on the wire, interpreted as a wrapping i32.
pub fn decode_abs(b: &[u8]) -> i32 {
    decode_u35(b) as u32 as i32
}

pub fn encode_u14(v: u16) -> [u8; 2] {
    [((v >> 7) & 0x7F) as u8, (v & 0x7F) as u8]
}

pub fn encode_i14(v: i16) -> [u8; 2] {
    encode_u14(v as u16 & 0x3FFF)
}

pub fn encode_u35(v: u64) -> [u8; 5] {
    let mut out = [0u8; 5];
    for (i, byte) in out.iter_mut().rev().enumerate() {
        *byte = ((v >> (7 * i)) & 0x7F) as u8;
    }
    out
}

pub fn encode_abs(v: i32) -> [u8; 5] {
    encode_u35(v as u32 as u64)
}

const POWER_FULL_SCALE: f32 = 16383.0;

pub fn decode_power(b: &[u8]) -> f32 {
    decode_u14(b) as f32 * 100.0 / POWER_FULL_SCALE
}

pub fn encode_power(percent: f32) -> [u8; 2] {
    encode_u14((percent.clamp(0.0, 100.0) * POWER_FULL_SCALE / 100.0).round() as u16)
}

/// Speed in mm/s (stored as µm/s).
pub fn decode_speed(b: &[u8]) -> f64 {
    decode_u35(b) as f64 / 1000.0
}

pub fn encode_speed(mm_per_s: f64) -> [u8; 5] {
    encode_u35((mm_per_s.max(0.0) * 1000.0).round() as u64)
}

/// Layer colour, stored as 0x00BBGGRR.
pub fn decode_color(b: &[u8]) -> [u8; 3] {
    let v = decode_u35(b);
    [(v & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, ((v >> 16) & 0xFF) as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i14_roundtrip() {
        for v in [-8192i16, -1000, -1, 0, 1, 1000, 8191] {
            assert_eq!(decode_i14(&encode_i14(v)), v);
        }
    }

    #[test]
    fn abs_roundtrip() {
        for v in [i32::MIN, -20_000, 0, 1, 20_000, 1_300_000, i32::MAX] {
            assert_eq!(decode_abs(&encode_abs(v)), v);
        }
    }

    #[test]
    fn argument_bytes_never_have_high_bit() {
        assert!(encode_abs(-1).iter().all(|b| b & 0x80 == 0));
        assert!(encode_i14(-1).iter().all(|b| b & 0x80 == 0));
    }

    #[test]
    fn power_and_speed() {
        assert!((decode_power(&encode_power(30.0)) - 30.0).abs() < 0.01);
        assert_eq!(encode_power(100.0), [0x7F, 0x7F]);
        assert_eq!(decode_speed(&encode_speed(20.0)), 20.0);
    }
}
