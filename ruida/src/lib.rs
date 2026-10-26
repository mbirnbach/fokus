//! Ruida laser controller protocol.
//!
//! Ruida controllers (RDC6442/6445/6332...) speak an undocumented binary
//! protocol. Everything here is based on community reverse engineering,
//! mainly the MeerK40t project's Ruida driver.
//!
//! Layers:
//! - [`scramble`]: the byte obfuscation every byte on the wire / in `.rd` files goes through
//! - [`codec`]: 7-bit-per-byte number encodings (coordinates, power, speed)
//! - [`command`]: splitting a plain byte stream into commands and decoding them
//! - [`job`]: interpreting commands as a toolpath (layers, cuts, travel moves)
//! - [`svg`]: rendering a toolpath for visual inspection
//! - [`memory`], [`link`]: read-only status queries to a live controller
//! - [`sender`]: sending jobs and stop / pause / resume (moves the machine)
//! - [`serial`]: the USB serial transport (feature `serial`)
//! - [`fill`]: scanline fill for engraving
//! - [`writer`]: writing `.rd` job files

pub mod codec;
pub mod command;
pub mod fill;
pub mod job;
pub mod link;
pub mod memory;
pub mod scramble;
pub mod sender;
#[cfg(feature = "serial")]
pub mod serial;
pub mod svg;
pub mod writer;
