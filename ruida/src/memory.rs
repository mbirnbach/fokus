//! Controller memory ("settings") reads: the read-only way to get status.
//!
//! A request is `DA 00 <addr hi> <addr lo>`; the controller answers with
//! `DA 01 <addr hi> <addr lo> <value: 5 bytes, 35 bits>`.

use crate::codec::decode_u35;

pub const CARD_ID: u16 = 0x057E;
pub const BED_SIZE_X: u16 = 0x0026;
pub const BED_SIZE_Y: u16 = 0x0036;
pub const MACHINE_STATUS: u16 = 0x0400;
pub const CURRENT_X: u16 = 0x0421;
pub const CURRENT_Y: u16 = 0x0431;
pub const CURRENT_Z: u16 = 0x0441;
pub const CURRENT_U: u16 = 0x0451;

pub const STATUS_JOB_RUNNING: u32 = 0x0000_0001;
pub const STATUS_PART_END: u32 = 0x0000_0002;
pub const STATUS_MOVING: u32 = 0x0100_0000;

const GET_SETTING: [u8; 2] = [0xDA, 0x00];
const SETTING_REPLY: [u8; 2] = [0xDA, 0x01];
pub const REPLY_LEN: usize = 9;

/// Plain (unscrambled) request for one memory address.
pub fn get_setting(addr: u16) -> [u8; 4] {
    let [hi, lo] = addr.to_be_bytes();
    [GET_SETTING[0], GET_SETTING[1], hi, lo]
}

/// Find the reply for `addr` in a plain byte stream and return its raw value.
/// Anything before it (stale replies, noise) is skipped.
pub fn parse_reply(plain: &[u8], addr: u16) -> Option<u64> {
    let [hi, lo] = addr.to_be_bytes();
    let header = [SETTING_REPLY[0], SETTING_REPLY[1], hi, lo];
    plain
        .windows(REPLY_LEN)
        .find(|w| w[..4] == header)
        .map(|w| decode_u35(&w[4..]))
}

/// Interpret a raw 35-bit value as signed (positions can be negative).
pub fn signed35(v: u64) -> i64 {
    let v = (v & 0x7_FFFF_FFFF) as i64;
    if v & (1 << 34) != 0 {
        v - (1 << 35)
    } else {
        v
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MachineStatus {
    pub card_id: u32,
    pub bits: u32,
    /// Bed size in mm.
    pub bed: (f64, f64),
    /// Head position in mm, controller coordinates (the origin may be on the right).
    pub position: (f64, f64),
}

impl MachineStatus {
    pub fn state(&self) -> &'static str {
        if self.bits & STATUS_MOVING != 0 {
            "moving"
        } else if self.bits & STATUS_PART_END != 0 {
            "part end"
        } else if self.bits & STATUS_JOB_RUNNING != 0 {
            "job running"
        } else {
            "idle"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::encode_u35;

    #[test]
    fn finds_reply_after_noise() {
        let mut stream = vec![0x12, 0xDA, 0x01, 0x04, 0x31];
        stream.extend(encode_u35(7));
        stream.extend([0xDA, 0x01, 0x04, 0x21]);
        stream.extend(encode_u35(123_456));
        assert_eq!(parse_reply(&stream, CURRENT_X), Some(123_456));
        assert_eq!(parse_reply(&stream, CURRENT_Y), Some(7));
        assert_eq!(parse_reply(&stream, CARD_ID), None);
    }

    #[test]
    fn incomplete_reply_is_none() {
        assert_eq!(parse_reply(&[0xDA, 0x01, 0x04, 0x21, 0x00, 0x00], CURRENT_X), None);
    }

    #[test]
    fn signed_values() {
        assert_eq!(signed35(5), 5);
        assert_eq!(signed35((1 << 35) - 1), -1);
    }
}
