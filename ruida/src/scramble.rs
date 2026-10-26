//! Byte (un)scrambling ("swizzling").
//!
//! Each byte has bits 0 and 7 swapped, is XORed with a model-specific magic
//! value and incremented by one. The magic is 0x88 for the RDC644x series
//! and 0x11 for the older 634x series.

pub const MAGIC_644X: u8 = 0x88;
pub const MAGIC_634X: u8 = 0x11;

/// Swap bit 0 and bit 7 (an XOR swap, so it is its own inverse).
fn swap_bits_0_7(mut b: u8) -> u8 {
    b ^= b >> 7;
    b ^= b << 7;
    b ^= b >> 7;
    b
}

pub fn scramble_byte(b: u8, magic: u8) -> u8 {
    (swap_bits_0_7(b) ^ magic).wrapping_add(1)
}

pub fn unscramble_byte(b: u8, magic: u8) -> u8 {
    swap_bits_0_7(b.wrapping_sub(1) ^ magic)
}

pub fn scramble(data: &[u8], magic: u8) -> Vec<u8> {
    data.iter().map(|&b| scramble_byte(b, magic)).collect()
}

pub fn unscramble(data: &[u8], magic: u8) -> Vec<u8> {
    data.iter().map(|&b| unscramble_byte(b, magic)).collect()
}

/// Guess the magic of a scrambled `.rd` file.
///
/// Job files are dominated by 0x00 bytes (high digits of coordinates), and
/// 0x00 scrambles to `magic + 1`. So the most frequent byte, minus one, is
/// the magic. Repeated bytes count extra because zeros come in runs.
pub fn detect_magic(data: &[u8]) -> Option<u8> {
    let mut counts = [0u32; 256];
    let mut prev = None;
    for &b in data {
        counts[b as usize] += if prev == Some(b) { 5 } else { 1 };
        prev = Some(b);
    }
    let (most_common, &count) = counts.iter().enumerate().max_by_key(|&(_, c)| *c)?;
    if count == 0 {
        return None;
    }
    Some((most_common as u8).wrapping_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_bytes() {
        for magic in [MAGIC_644X, MAGIC_634X] {
            for b in 0..=255u8 {
                assert_eq!(unscramble_byte(scramble_byte(b, magic), magic), b);
            }
        }
    }

    #[test]
    fn known_values() {
        // Zero maps to magic + 1, which is what detect_magic relies on.
        assert_eq!(scramble_byte(0x00, MAGIC_644X), 0x89);
        // The controller's ACK (0xCC) appears on the wire as 0xC6.
        assert_eq!(scramble_byte(0xCC, MAGIC_644X), 0xC6);
    }
}
