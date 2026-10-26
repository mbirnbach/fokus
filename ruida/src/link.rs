//! A read-only connection to a controller.
//!
//! [`StatusLink`] can only send memory *read* requests (`DA 00`). There is no
//! method that sends anything else, so it cannot move the head or fire the
//! laser. Motion and job upload will get their own, separate type.
//!
//! Over USB there is no checksum and no ACK: we send the scrambled request
//! and wait for the 9-byte reply.

use std::fmt;
use std::io;
use std::time::{Duration, Instant};

use crate::memory::{self, MachineStatus};
use crate::scramble::{scramble, unscramble};

/// Byte transport (USB serial, or a simulator in tests).
pub trait Transport {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;
    /// Read what is available. May return `Ok(0)` or a `TimedOut` error
    /// when nothing arrived within the transport's own short timeout.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    /// Drop stale input before a new request.
    fn clear_input(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Lets a link borrow a transport instead of owning it.
impl<T: Transport + ?Sized> Transport for &mut T {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        (**self).write_all(data)
    }
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (**self).read(buf)
    }
    fn clear_input(&mut self) -> io::Result<()> {
        (**self).clear_input()
    }
}

#[derive(Debug)]
pub enum LinkError {
    Io(io::Error),
    /// No (complete) reply for `addr`; `received` bytes did arrive.
    Timeout { addr: u16, received: usize },
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::Io(e) => write!(f, "I/O error: {e}"),
            LinkError::Timeout { addr, received: 0 } => {
                write!(f, "no reply to memory read 0x{addr:04x} (nothing received)")
            }
            LinkError::Timeout { addr, received } => {
                write!(f, "no valid reply to memory read 0x{addr:04x} ({received} bytes received, wrong magic?)")
            }
        }
    }
}

impl std::error::Error for LinkError {}

impl From<io::Error> for LinkError {
    fn from(e: io::Error) -> Self {
        LinkError::Io(e)
    }
}

pub struct StatusLink<T> {
    transport: T,
    magic: u8,
    pub timeout: Duration,
}

impl<T: Transport> StatusLink<T> {
    pub fn new(transport: T, magic: u8) -> Self {
        StatusLink { transport, magic, timeout: Duration::from_millis(1000) }
    }

    /// Read one memory address; returns the raw 35-bit value.
    pub fn read_memory(&mut self, addr: u16) -> Result<u64, LinkError> {
        self.transport.clear_input()?;
        self.transport.write_all(&scramble(&memory::get_setting(addr), self.magic))?;

        let deadline = Instant::now() + self.timeout;
        let mut received = Vec::new();
        let mut chunk = [0u8; 64];
        while Instant::now() < deadline {
            match self.transport.read(&mut chunk) {
                Ok(n) => received.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => {}
                Err(e) => return Err(e.into()),
            }
            if let Some(v) = memory::parse_reply(&unscramble(&received, self.magic), addr) {
                return Ok(v);
            }
        }
        Err(LinkError::Timeout { addr, received: received.len() })
    }

    /// Give the transport back, e.g. to hand it to a job sender.
    pub fn into_transport(self) -> T {
        self.transport
    }

    pub fn card_id(&mut self) -> Result<u32, LinkError> {
        Ok(self.read_memory(memory::CARD_ID)? as u32)
    }

    pub fn status(&mut self) -> Result<MachineStatus, LinkError> {
        let um = |v: u64| memory::signed35(v) as f64 / 1000.0;
        Ok(MachineStatus {
            card_id: self.card_id()?,
            bits: self.read_memory(memory::MACHINE_STATUS)? as u32,
            bed: (um(self.read_memory(memory::BED_SIZE_X)?), um(self.read_memory(memory::BED_SIZE_Y)?)),
            position: (um(self.read_memory(memory::CURRENT_X)?), um(self.read_memory(memory::CURRENT_Y)?)),
        })
    }

    /// Only machine state and position: the part worth polling.
    pub fn poll(&mut self, last: &mut MachineStatus) -> Result<(), LinkError> {
        let um = |v: u64| memory::signed35(v) as f64 / 1000.0;
        last.bits = self.read_memory(memory::MACHINE_STATUS)? as u32;
        last.position = (um(self.read_memory(memory::CURRENT_X)?), um(self.read_memory(memory::CURRENT_Y)?));
        Ok(())
    }
}
