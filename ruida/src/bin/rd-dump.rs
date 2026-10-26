//! rd-dump: decode a Ruida `.rd` job file and print what's in it.
//!
//! Read-only: this never talks to a machine.

use std::{env, fs, process::ExitCode};

use ruida::{command, job::Toolpath, scramble, svg};

const USAGE: &str = "\
usage: rd-dump <file.rd> [options]

options:
  --magic <hex>   scramble magic (default: auto-detect; 644x = 88, 634x = 11)
  --plain         input is already unscrambled
  --svg <out.svg> write a toolpath preview
  --mirror-x      flip the preview horizontally (origin on the right)
  --mirror-y      flip the preview vertically
  --summary       skip the command listing";

struct Args {
    input: String,
    magic: Option<u8>,
    plain: bool,
    svg_out: Option<String>,
    mirror: svg::Mirror,
    summary_only: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut it = env::args().skip(1);
    let mut input = None;
    let mut args = Args {
        input: String::new(),
        magic: None,
        plain: false,
        svg_out: None,
        mirror: svg::Mirror::default(),
        summary_only: false,
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--magic" => {
                let v = it.next().ok_or("--magic needs a value")?;
                let v = v.trim_start_matches("0x");
                args.magic = Some(u8::from_str_radix(v, 16).map_err(|_| format!("bad magic: {v}"))?);
            }
            "--plain" => args.plain = true,
            "--svg" => args.svg_out = Some(it.next().ok_or("--svg needs a path")?),
            "--mirror-x" => args.mirror.x = true,
            "--mirror-y" => args.mirror.y = true,
            "--summary" => args.summary_only = true,
            "-h" | "--help" => return Err(String::new()),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s => input = Some(s.to_string()),
        }
    }
    args.input = input.ok_or("missing input file")?;
    Ok(args)
}

fn run(args: Args) -> Result<(), String> {
    let raw = fs::read(&args.input).map_err(|e| format!("{}: {e}", args.input))?;

    let data = if args.plain {
        println!("file:  {} ({} bytes, plain)", args.input, raw.len());
        raw
    } else {
        let magic = match args.magic {
            Some(m) => m,
            None => scramble::detect_magic(&raw).ok_or("empty file")?,
        };
        let how = if args.magic.is_some() { "given" } else { "detected" };
        println!("file:  {} ({} bytes, magic 0x{magic:02x} {how})", args.input, raw.len());
        scramble::unscramble(&raw, magic)
    };

    let records = command::parse(&data);
    if !args.summary_only {
        println!();
        for r in &records {
            println!("{r}");
        }
    }

    let tp = Toolpath::from_records(&records);
    print_summary(&tp);

    if let Some(path) = &args.svg_out {
        fs::write(path, svg::render(&tp, args.mirror)).map_err(|e| format!("{path}: {e}"))?;
        println!("\npreview written to {path}");
    }
    Ok(())
}

fn print_summary(tp: &Toolpath) {
    let mm = |v: i64| v as f64 / 1000.0;
    println!("\n== summary ==");
    println!(
        "commands: {} ({} unknown)",
        tp.total_commands, tp.unknown_commands
    );
    if tp.total_commands > 0 && tp.unknown_commands * 5 > tp.total_commands {
        println!("WARNING: many unknown commands, the magic is probably wrong (try --magic 88 or --magic 11)");
    }
    let cuts = tp.segments.iter().filter(|s| s.cut).count();
    println!("segments: {} cut, {} travel", cuts, tp.segments.len() - cuts);
    if let Some(((x0, y0), (x1, y1))) = tp.cut_bounds() {
        println!(
            "cut area: x {:.2}..{:.2}  y {:.2}..{:.2} mm  ({:.2} x {:.2} mm)",
            mm(x0), mm(x1), mm(y0), mm(y1), mm(x1 - x0), mm(y1 - y0)
        );
    }
    println!("travel:   {:.1} mm", tp.travel_mm);

    if !tp.layers.is_empty() {
        println!("\nlayer  color    speed mm/s  min %  max %  cut length mm");
        let opt = |v: Option<f32>| v.map(|v| format!("{v:.1}")).unwrap_or_else(|| "-".into());
        for (id, l) in &tp.layers {
            let color = l.color.map(|[r, g, b]| format!("#{r:02x}{g:02x}{b:02x}")).unwrap_or_else(|| "-".into());
            let speed = l.speed.map(|v| format!("{v:.1}")).unwrap_or_else(|| "-".into());
            println!(
                "{id:>5}  {color:<7}  {speed:>10}  {:>5}  {:>5}  {:>13.1}",
                opt(l.min_power), opt(l.max_power), l.cut_mm
            );
        }
    }
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
