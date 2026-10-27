//! Fokus: a desktop app for Ruida-controlled CO2 lasers.

// No console window next to the app on Windows (release builds).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bezier;
mod canvas;
mod design;
mod fonts;
mod i18n;
mod icons;
mod job;
mod machine;
mod materials;
mod paths;
mod project;
mod settings;
mod textedit;
mod theme;
mod widgets;

use eframe::egui;

const USAGE: &str = "usage: fokus [design.svg | job.rd]\n       fokus design.svg --export job.rd --material \"Pappelsperrholz 4 mm\" [--anchor top-right|centre] [--dry-run]";

fn main() -> eframe::Result {
    let settings = settings::Settings::load();
    i18n::set_language(settings.language.as_deref());
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let open = args.first().map(std::path::PathBuf::from);

    // Headless export, for checking jobs without the window.
    if args.len() > 1 {
        if let Err(e) = export(&args) {
            eprintln!("error: {e}\n{USAGE}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Fokus")
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([1040.0, 600.0])
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../../assets/brand/icon/fokus-256.png")).expect("icon is a PNG")),
        ..Default::default()
    };
    eframe::run_native(
        "Fokus",
        options,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx, settings.theme.as_deref());
            Ok(Box::new(app::FokusApp::new(open)))
        }),
    )
}

fn export(args: &[std::ffi::OsString]) -> Result<(), String> {
    let arg = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).ok_or(format!("missing {flag}"));
    let out = arg("--export")?;
    let name = arg("--material")?.to_string_lossy().into_owned();
    let library = materials::Library::load();
    let material = library.materials.iter().find(|m| m.name == name).ok_or(format!("unknown material {name:?}"))?;
    let mut design = design::Design::default();
    design.import_svg(std::path::Path::new(&args[0]), (0.0, 0.0))?;
    let anchor = match args.iter().position(|a| a == "--anchor").and_then(|i| args.get(i + 1)).map(|a| a.to_string_lossy().into_owned()).as_deref() {
        None | Some("top-right") => design::Anchor::TOP_RIGHT,
        Some("centre" | "center") => design::Anchor { h: 0, v: 0 },
        Some(other) => return Err(format!("unknown --anchor {other:?} (top-right or centre)")),
    };
    let mut job = design.to_job(Some(material), true, anchor)?;
    // --dry-run: same moves with the laser off, to check position and size.
    if args.iter().any(|a| a == "--dry-run") {
        for l in &mut job {
            l.min_power = 0.0;
            l.max_power = 0.0;
            l.air_assist = false;
        }
    }
    let bytes = ruida::writer::write_file(&job).map_err(|e| e.to_string())?;
    std::fs::write(out, bytes).map_err(|e| e.to_string())
}
