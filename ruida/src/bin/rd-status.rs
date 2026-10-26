//! rd-status: read status from a Ruida controller over USB.
//!
//! Read-only: it only sends memory read requests (see `ruida::link`), so it
//! cannot move the head or fire the laser.

use std::io::Write;
use std::{env, process::ExitCode, thread, time::Duration};

use ruida::link::{LinkError, StatusLink};
use ruida::memory;
use ruida::scramble::MAGIC_644X;
use ruida::serial::{list_ports, SerialTransport};

const USAGE: &str = "\
usage: rd-status [options]

options:
  --list              list serial ports and exit
  --port <name>       serial port (default: the first FTDI 0403:6001 port)
  --watch             keep polling state and position (Ctrl+C to stop)
  --raw               also print the raw value of every memory address read
  --no-flow-control   disable RTS/CTS (try this if nothing is received)
  --magic <hex>       scramble magic (default 88 for the 644x)

Close LightBurn / Lotus Laser first: only one program can use the port.";

struct Args {
    list: bool,
    port: Option<String>,
    watch: bool,
    raw: bool,
    flow_control: bool,
    magic: u8,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { list: false, port: None, watch: false, raw: false, flow_control: true, magic: MAGIC_644X };
    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--list" => args.list = true,
            "--port" => args.port = Some(it.next().ok_or("--port needs a name")?),
            "--watch" => args.watch = true,
            "--raw" => args.raw = true,
            "--no-flow-control" => args.flow_control = false,
            "--magic" => {
                let v = it.next().ok_or("--magic needs a value")?;
                let v = v.trim_start_matches("0x");
                args.magic = u8::from_str_radix(v, 16).map_err(|_| format!("bad magic: {v}"))?;
            }
            "-h" | "--help" => return Err(String::new()),
            s => return Err(format!("unknown argument {s}")),
        }
    }
    Ok(args)
}

fn print_ports() -> Result<(), String> {
    let ports = list_ports().map_err(|e| format!("cannot list serial ports: {e}"))?;
    if ports.is_empty() {
        println!("no serial ports found");
    }
    for p in ports {
        let mark = if p.looks_like_ruida { "  <- likely the laser" } else { "" };
        println!("{:<28} {}{mark}", p.name, p.description);
    }
    Ok(())
}

fn pick_port() -> Result<(String, String), String> {
    let ports = list_ports().map_err(|e| format!("cannot list serial ports: {e}"))?;
    ports
        .into_iter()
        .find(|p| p.looks_like_ruida)
        .map(|p| (p.name, p.description))
        .ok_or_else(|| {
            [
                "no FTDI (0403:6001) serial port found.",
                "  - Is the laser switched on and the USB cable plugged in?",
                "  - Windows: in Device Manager open \"USB Serial Converter\" -> Properties -> Advanced,",
                "    tick \"Load VCP\" and replug. A COM port should appear.",
                "  - Run with --list to see all ports and pick one with --port.",
            ]
            .join("\n")
        })
}

fn hint(e: &LinkError) -> &'static str {
    match e {
        LinkError::Timeout { received: 0, .. } => {
            "\nhint: nothing came back. Close LightBurn/Lotus Laser, check the port, or try --no-flow-control."
        }
        LinkError::Timeout { .. } => "\nhint: bytes arrived but didn't decode. Try --raw, or --magic 11.",
        LinkError::Io(_) => "",
    }
}

fn run(args: Args) -> Result<(), String> {
    if args.list {
        return print_ports();
    }
    let (port, desc) = match args.port {
        Some(p) => (p, String::new()),
        None => pick_port()?,
    };
    println!("port:     {port} {desc}");

    let transport = SerialTransport::open(&port, args.flow_control).map_err(|e| {
        format!("cannot open {port}: {e}\nhint: is LightBurn or Lotus Laser still running? Only one program can use the port.")
    })?;
    let mut link = StatusLink::new(transport, args.magic);
    let fail = |e: LinkError| format!("{e}{}", hint(&e));

    if args.raw {
        for (name, addr) in [
            ("card id", memory::CARD_ID),
            ("machine status", memory::MACHINE_STATUS),
            ("bed size x", memory::BED_SIZE_X),
            ("bed size y", memory::BED_SIZE_Y),
            ("current x", memory::CURRENT_X),
            ("current y", memory::CURRENT_Y),
            ("current z", memory::CURRENT_Z),
            ("current u", memory::CURRENT_U),
        ] {
            match link.read_memory(addr) {
                Ok(v) => println!("  0x{addr:04x} {name:<15} = 0x{v:09x} ({})", memory::signed35(v)),
                Err(e) => println!("  0x{addr:04x} {name:<15} : {e}"),
            }
        }
    }

    let mut status = link.status().map_err(fail)?;
    println!("card id:  0x{:08x}", status.card_id);
    println!("bed:      {:.1} x {:.1} mm", status.bed.0, status.bed.1);
    println!("state:    {}", status.state());
    println!("position: x={:.2} y={:.2} mm (controller coordinates)", status.position.0, status.position.1);

    if args.watch {
        println!();
        loop {
            thread::sleep(Duration::from_millis(250));
            link.poll(&mut status).map_err(fail)?;
            print!(
                "\r{:<12} x={:>8.2}  y={:>8.2} mm   ",
                status.state(),
                status.position.0,
                status.position.1
            );
            let _ = std::io::stdout().flush();
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("error: {e}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
