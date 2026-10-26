//! rd-send: send a .rd job to the laser over USB, or stop the running job.
//!
//! THIS MOVES THE MACHINE AND FIRES THE LASER. The job runs from wherever
//! the head is. Stay at the machine with a hand near the emergency stop.

use std::io::{self, BufRead, Write};
use std::{env, fs, process::ExitCode, thread, time::Duration};

use ruida::command::parse;
use ruida::job::Toolpath;
use ruida::link::StatusLink;
use ruida::scramble::{detect_magic, unscramble, MAGIC_644X};
use ruida::sender::JobSender;
use ruida::serial::{list_ports, SerialTransport};

const USAGE: &str = "\
usage: rd-send <job.rd> [--port <name>]   send a job (asks for confirmation)
       rd-send --stop [--port <name>]     stop the running job";

fn port_arg(args: &[String]) -> Result<String, String> {
    if let Some(i) = args.iter().position(|a| a == "--port") {
        return args.get(i + 1).cloned().ok_or("--port needs a name".into());
    }
    list_ports()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|p| p.looks_like_ruida)
        .map(|p| p.name)
        .ok_or("no laser (FTDI 0403:6001) found on USB".into())
}

fn run(args: Vec<String>) -> Result<(), String> {
    let port = port_arg(&args)?;
    if args.iter().any(|a| a == "--stop") {
        let t = SerialTransport::open(&port, true).map_err(|e| format!("cannot open {port}: {e}"))?;
        JobSender::new(t, MAGIC_644X).stop().map_err(|e| e.to_string())?;
        println!("stop sent");
        return Ok(());
    }

    let port_value = args.iter().position(|a| a == "--port").map(|i| i + 1);
    let path = args.iter().enumerate().find(|(i, a)| !a.starts_with("--") && Some(*i) != port_value).map(|(_, a)| a).ok_or("missing job file")?;
    let raw = fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let magic = detect_magic(&raw).ok_or("empty file")?;
    if magic != MAGIC_644X {
        return Err(format!("file uses scramble magic 0x{magic:02x}, expected 0x88 for the 644XG"));
    }
    let plain = unscramble(&raw, magic);
    let records = parse(&plain);
    let tp = Toolpath::from_records(&records);
    if tp.unknown_commands > 0 {
        return Err(format!("{} unknown commands in the file; not sending", tp.unknown_commands));
    }

    // Show what will happen.
    println!("job:   {path} ({} bytes)", plain.len());
    if let Some(((x0, y0), (x1, y1))) = tp.cut_bounds() {
        let mm = |v: i64| v as f64 / 1000.0;
        println!("area:  x {:.1}..{:.1}  y {:.1}..{:.1} mm relative to the head  ({:.1} x {:.1} mm)", mm(x0), mm(x1), mm(y0), mm(y1), mm(x1 - x0), mm(y1 - y0));
    }
    let mut max_power = 0.0f32;
    for (id, l) in &tp.layers {
        let p = l.max_power.unwrap_or(0.0);
        max_power = max_power.max(p);
        println!("layer {id}: {:.0} mm/s, {:.0}-{:.0} %, {:.0} mm", l.speed.unwrap_or(0.0), l.min_power.unwrap_or(0.0), p, l.cut_mm);
    }
    println!("port:  {port}");
    if max_power == 0.0 {
        println!("\nDRY RUN: all power is 0 %, the laser should not fire.");
    } else {
        println!("\nTHE LASER WILL FIRE (up to {max_power:.0} %).");
    }

    // Where the head is before we start.
    let transport = SerialTransport::open(&port, true).map_err(|e| format!("cannot open {port}: {e}"))?;
    let mut status = StatusLink::new(transport, MAGIC_644X);
    let before = status.status().map_err(|e| format!("no answer from the controller: {e}"))?;
    println!("head:  X {:.1}  Y {:.1} mm, {}", before.position.0, before.position.1, before.state());
    if before.state() != "idle" {
        return Err("the machine is not idle; not sending".into());
    }

    print!("\nType 'send' and Enter to start, anything else cancels: ");
    io::stdout().flush().ok();
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer).map_err(|e| e.to_string())?;
    if answer.trim() != "send" {
        println!("cancelled");
        return Ok(());
    }

    let transport = status.into_transport();
    let mut sender = JobSender::new(transport, MAGIC_644X);
    sender
        .send_job(&plain, |sent, total| {
            print!("\rsending {sent}/{total} bytes");
            io::stdout().flush().ok();
            true
        })
        .map_err(|e| format!("\nsend failed: {e}"))?;
    println!("\nsent. watching the machine (Ctrl+C stops watching, not the job; use rd-send --stop)");

    // Watch until the job is done.
    let mut status = StatusLink::new(sender.into_transport(), MAGIC_644X);
    let mut seen_busy = false;
    let mut idle_polls = 0;
    for _ in 0..(60 * 60 * 4) {
        thread::sleep(Duration::from_millis(250));
        let Ok(s) = status.status() else { continue };
        print!("\r{:<12} X {:>8.1}  Y {:>8.1} mm   ", s.state(), s.position.0, s.position.1);
        io::stdout().flush().ok();
        if s.state() == "idle" {
            idle_polls += 1;
            // Done once it was busy and has been idle for a second, or it
            // never got busy within 5 seconds (e.g. rejected the job).
            if (seen_busy && idle_polls >= 4) || (!seen_busy && idle_polls >= 20) {
                println!("\n{}", if seen_busy { "job finished" } else { "the machine never started (job rejected?)" });
                break;
            }
        } else {
            seen_busy = true;
            idle_polls = 0;
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
