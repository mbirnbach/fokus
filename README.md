# Fokus

Laser cutter software for Ruida-controlled CO2 lasers, for Windows, macOS
and Linux. Built for a shared workshop laser (Lasermaxx Plott 100, Ruida
RDC6445G): people without much laser experience are guided from the design to
the finished job.

- Draw (rectangle, ellipse, polygon, line, text) or import SVG designs
- One material per job, e.g. *Pappelsperrholz 4 mm*: each colour is cut,
  marked, engraved or skipped with the material's settings
- Start point like LotusCut (3 × 3 points around the design, at the laser head)
- A checklist (material, focus, head position, lid / exhaust) before every job
- Starts jobs over USB with progress, pause, resume and stop; dry run with the
  laser off; or export a `.rd` job file for a USB stick
- Projects (`.fokus`), English and German, light and dark mode

## Repository

| Folder | What |
|---|---|
| [`fokus/`](fokus/) | The desktop app (egui). See [fokus/README.md](fokus/README.md). |
| [`ruida/`](ruida/) | The Ruida protocol: `.rd` files (read and write), USB connection, status and job sending, plus command-line tools. See [ruida/README.md](ruida/README.md). |
| `fokus/locales/` | Translations ([Fluent](https://projectfluent.org) files). |
| `packaging/`, `scripts/package.sh` | Packaging for the release downloads. |
| `.github/workflows/` | Tests on every push; release builds when a GitHub release is published. |

## Building

Needs [Rust](https://rustup.rs) (stable).

```bash
cargo run -p fokus                     # the app
cargo test --workspace                 # all tests
```

Linux also needs: `libudev-dev libgtk-3-dev libxkbcommon-dev pkg-config`.

Command-line tools from the `ruida` crate:

```bash
cargo run -p ruida --bin rd-dump -- job.rd --svg preview.svg   # decode a job file
cargo run -p ruida --bin rd-status -- --watch                  # read-only machine status
cargo run -p ruida --bin rd-send -- job.rd                     # send a job (asks first)
```

## Releases

Publishing a release on GitHub (with a tag such as `v0.1.0`) runs
`.github/workflows/release.yml`, which builds Fokus for Windows, Linux and
macOS (Apple Silicon and Intel) and attaches the downloads to the release.
The same workflow can be started by hand ("Run workflow") to get test builds
as workflow artifacts without a release. The macOS build is not signed:
the first start needs right-click → Open.

## Status

Tested on the real machine: job files for cutting and engraving, sending over
USB, start points, pause / resume. Not yet: Stop on the real machine, photo
engraving, Windows and Linux builds on real hardware.
