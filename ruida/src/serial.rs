//! USB serial transport.
//!
//! The 644x controller's USB port is an FTDI chip (VID 0403, PID 6001).
//! It shows up as a COM port on Windows (if the FTDI "VCP" driver is loaded),
//! /dev/cu.usbserial-* on macOS and /dev/ttyUSB* on Linux.
//! Settings follow MeerK40t: 115200 8N1, RTS/CTS flow control, DTR on.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use serialport::{ClearBuffer, FlowControl, SerialPort, SerialPortType};

use crate::link::Transport;

pub const FTDI_VID: u16 = 0x0403;
pub const FTDI_PID: u16 = 0x6001;
pub const BAUD: u32 = 115_200;
/// How long a write may wait for the controller to take more data (its
/// buffer fills while it is busy burning) before we give up.
const WRITE_PATIENCE: Duration = Duration::from_secs(60);

pub struct PortInfo {
    pub name: String,
    pub description: String,
    pub looks_like_ruida: bool,
}

pub fn list_ports() -> Result<Vec<PortInfo>, serialport::Error> {
    let mut ports: Vec<PortInfo> = serialport::available_ports()?
        .into_iter()
        // macOS lists every device twice; the tty.* twin blocks on open.
        .filter(|p| !p.port_name.starts_with("/dev/tty."))
        .map(|p| match p.port_type {
            SerialPortType::UsbPort(u) => PortInfo {
                description: format!(
                    "USB {:04x}:{:04x} {} {}",
                    u.vid,
                    u.pid,
                    u.manufacturer.unwrap_or_default(),
                    u.product.unwrap_or_default()
                )
                .trim_end()
                .to_string(),
                looks_like_ruida: u.vid == FTDI_VID && u.pid == FTDI_PID,
                name: p.port_name,
            },
            other => PortInfo { name: p.port_name, description: format!("{other:?}"), looks_like_ruida: false },
        })
        .collect();
    ports.sort_by_key(|p| !p.looks_like_ruida);
    Ok(ports)
}

pub struct SerialTransport {
    port: Box<dyn SerialPort>,
}

impl SerialTransport {
    pub fn open(name: &str, hardware_flow_control: bool) -> Result<Self, serialport::Error> {
        let flow = if hardware_flow_control { FlowControl::Hardware } else { FlowControl::None };
        let mut port = serialport::new(name, BAUD).flow_control(flow).timeout(Duration::from_millis(50)).open()?;
        port.write_data_terminal_ready(true)?;
        Ok(SerialTransport { port })
    }
}

impl Transport for SerialTransport {
    fn write_all(&mut self, mut data: &[u8]) -> io::Result<()> {
        // The port's own timeout is short (for status reads); while the
        // controller's buffer is full, flow control holds writes back, so
        // keep trying instead of failing halfway through a job.
        let deadline = Instant::now() + WRITE_PATIENCE;
        while !data.is_empty() {
            match self.port.write(data) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => data = &data[n..],
                Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                    if Instant::now() > deadline {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "the controller stopped accepting data"));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        self.port.flush()
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.port.read(buf)
    }

    fn clear_input(&mut self) -> io::Result<()> {
        self.port.clear(ClearBuffer::Input).map_err(io::Error::from)
    }
}
