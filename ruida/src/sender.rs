//! Sending jobs to a controller. Unlike [`crate::link::StatusLink`], this
//! moves the machine and fires the laser: a job starts running as soon as
//! its `D8 00` (start process) command arrives.
//!
//! Over USB there is no checksum and no ACK. Data goes out in chunks of
//! about 1000 bytes, split between commands (as MeerK40t does); RTS/CTS
//! flow control on the serial port holds us back when the controller's
//! buffer is full.

use std::io;

use crate::link::Transport;
use crate::scramble::scramble;

pub const STOP: [u8; 2] = [0xD8, 0x01];
pub const PAUSE: [u8; 2] = [0xD8, 0x02];
pub const RESUME: [u8; 2] = [0xD8, 0x03];

const CHUNK: usize = 1000;

pub struct JobSender<T> {
    transport: T,
    magic: u8,
}

impl<T: Transport> JobSender<T> {
    pub fn new(transport: T, magic: u8) -> Self {
        JobSender { transport, magic }
    }

    /// Send a plain (unscrambled) job; `progress` gets (sent, total) bytes
    /// after each chunk and returns false to abort. Returns whether the
    /// whole job was sent. After an abort, call [`Self::stop`]: the part
    /// already sent may be running.
    pub fn send_job(&mut self, plain: &[u8], mut progress: impl FnMut(usize, usize) -> bool) -> io::Result<bool> {
        let total = plain.len();
        let mut sent = 0;
        for chunk in chunks(plain) {
            self.transport.write_all(&scramble(chunk, self.magic))?;
            sent += chunk.len();
            if !progress(sent, total) && sent < total {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn stop(&mut self) -> io::Result<()> {
        self.command(&STOP)
    }

    pub fn pause(&mut self) -> io::Result<()> {
        self.command(&PAUSE)
    }

    pub fn resume(&mut self) -> io::Result<()> {
        self.command(&RESUME)
    }

    fn command(&mut self, plain: &[u8]) -> io::Result<()> {
        self.transport.write_all(&scramble(plain, self.magic))
    }

    pub fn into_transport(self) -> T {
        self.transport
    }
}

/// Split a plain job into pieces of about `CHUNK` bytes, each starting at
/// a command byte (>= 0x80) so no command is cut in half.
fn chunks(plain: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..plain.len() {
        if i - start >= CHUNK && plain[i] >= 0x80 {
            out.push(&plain[start..i]);
            start = i;
        }
    }
    if start < plain.len() {
        out.push(&plain[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scramble::{unscramble, MAGIC_644X};

    struct Capture(Vec<Vec<u8>>);

    impl Transport for Capture {
        fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
            self.0.push(data.to_vec());
            Ok(())
        }
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    #[test]
    fn chunks_split_between_commands_and_reassemble() {
        // 300 four-byte commands.
        let plain: Vec<u8> = (0..300).flat_map(|i| [0xA8, (i % 100) as u8, 1, 2]).collect();
        let mut s = JobSender::new(Capture(Vec::new()), MAGIC_644X);
        assert!(s.send_job(&plain, |_, _| true).unwrap());
        let sent = s.into_transport().0;
        assert!(sent.len() > 1);
        for c in &sent {
            assert!(c.len() <= CHUNK + 4);
            assert_eq!(unscramble(&c[..1], MAGIC_644X)[0], 0xA8, "each chunk starts with a command");
        }
        assert_eq!(unscramble(&sent.concat(), MAGIC_644X), plain);
    }

    #[test]
    fn sending_can_be_aborted_between_chunks() {
        let plain: Vec<u8> = (0..3000).flat_map(|i| [0xA8, (i % 100) as u8, 1, 2]).collect();
        let mut s = JobSender::new(Capture(Vec::new()), MAGIC_644X);
        let done = s.send_job(&plain, |sent, _| sent < 2000).unwrap();
        assert!(!done);
        assert_eq!(s.into_transport().0.len(), 2, "stopped after the chunk that crossed 2000 bytes");
    }

    #[test]
    fn stop_is_scrambled_d8_01() {
        let mut s = JobSender::new(Capture(Vec::new()), MAGIC_644X);
        s.stop().unwrap();
        assert_eq!(unscramble(&s.into_transport().0[0], MAGIC_644X), STOP);
    }
}
