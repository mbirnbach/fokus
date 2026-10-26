//! StatusLink against a simulated controller that answers memory reads the
//! way a 644x does over USB: scrambled `DA 01 <addr> <value>`, no ACK.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::time::Duration;

use ruida::codec::encode_u35;
use ruida::link::{LinkError, StatusLink, Transport};
use ruida::memory;
use ruida::scramble::{scramble, unscramble, MAGIC_644X};

struct SimController {
    memory: HashMap<u16, u64>,
    outgoing: VecDeque<u8>,
    /// Every plain request received, to check we only ever read.
    requests: Vec<Vec<u8>>,
    /// Deliver replies a few bytes at a time, like a real serial port.
    chunk: usize,
}

impl SimController {
    fn new() -> Self {
        let memory = HashMap::from([
            (memory::CARD_ID, 0x6510_6510),
            (memory::MACHINE_STATUS, 0),
            (memory::BED_SIZE_X, 600_000),
            (memory::BED_SIZE_Y, 400_000),
            (memory::CURRENT_X, 123_450),
            (memory::CURRENT_Y, (1u64 << 35) - 2_000), // -2 mm
        ]);
        SimController { memory, outgoing: VecDeque::new(), requests: Vec::new(), chunk: 3 }
    }
}

impl Transport for SimController {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        let plain = unscramble(data, MAGIC_644X);
        self.requests.push(plain.clone());
        if let [0xDA, 0x00, hi, lo] = plain[..] {
            let addr = u16::from_be_bytes([hi, lo]);
            if let Some(&v) = self.memory.get(&addr) {
                let mut reply = vec![0xDA, 0x01, hi, lo];
                reply.extend(encode_u35(v));
                self.outgoing.extend(scramble(&reply, MAGIC_644X));
            }
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.chunk.min(buf.len()).min(self.outgoing.len());
        if n == 0 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        for b in buf.iter_mut().take(n) {
            *b = self.outgoing.pop_front().unwrap();
        }
        Ok(n)
    }
}

#[test]
fn reads_full_status() {
    let mut link = StatusLink::new(SimController::new(), MAGIC_644X);
    let s = link.status().unwrap();
    assert_eq!(s.card_id, 0x6510_6510);
    assert_eq!(s.state(), "idle");
    assert_eq!(s.bed, (600.0, 400.0));
    assert_eq!(s.position, (123.45, -2.0));
}

#[test]
fn reports_moving() {
    let mut sim = SimController::new();
    sim.memory.insert(memory::MACHINE_STATUS, memory::STATUS_MOVING as u64);
    let mut link = StatusLink::new(sim, MAGIC_644X);
    assert_eq!(link.status().unwrap().state(), "moving");
}

#[test]
fn times_out_without_reply() {
    let mut link = StatusLink::new(SimController::new(), MAGIC_644X);
    link.timeout = Duration::from_millis(50);
    match link.read_memory(0x0999) {
        Err(LinkError::Timeout { addr: 0x0999, received: 0 }) => {}
        other => panic!("expected timeout, got {other:?}"),
    }
}

#[test]
fn only_sends_memory_reads() {
    let mut sim = SimController::new();
    {
        let mut link = StatusLink::new(&mut sim, MAGIC_644X);
        link.status().unwrap();
    }
    assert!(!sim.requests.is_empty());
    for r in &sim.requests {
        assert_eq!(r.len(), 4);
        assert_eq!(&r[..2], &[0xDA, 0x00], "non-read request sent: {r:02x?}");
    }
}
