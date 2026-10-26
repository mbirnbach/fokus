# Fokus

Desktop app for Ruida-controlled CO2 lasers (Windows, macOS, Linux), built
with egui on top of the `ruida` crate next door. Made for a shared workshop
laser: people without much laser experience are guided step by step.

```bash
cargo run -p fokus                          # or: cargo run -p fokus -- fokus/examples/tag.svg
cargo run -p fokus -- fokus/examples/tag.svg --export tag.rd --material "Pappelsperrholz 4 mm"
```

## Layout (like LightBurn / xTool Studio)

- **Top bar**: New, Open, Undo/Redo; machine status, Materials, Settings.
- **Properties bar**: X/Y/W/H in mm (X/Y = corner nearest the machine
  origin), aspect lock, rotation, corner radius / polygon sides / text, font,
  Duplicate, Group/Ungroup, mirror, Align, Delete.
- **Tools** (left): Select (V), Rectangle (R), Ellipse (E), Polygon (P),
  Line (L, click points, double-click or Enter to finish), Text (T).
  Shift keeps squares / circles; drag handles to resize; arrows nudge 1 mm
  (Shift: 10 mm); ⌘Z/⇧⌘Z, ⌘D, ⌘G/⇧⌘G, ⌘A; Space+drag pans, wheel zooms.
- **Colour palette** (bottom): each colour is a layer; click a colour to use
  it for new shapes and to move the selection to that layer.
- **Right panel**: one material for the job, the layer list (Cut / Mark /
  Engrave / Skip, settings from the material), and **Start job**, which opens
  a checklist (material, focus, head at start corner, lid/exhaust) before the
  job can be started (or saved as a file).

Jobs start where the laser head is, like in LotusCut: a 3×3 picker above
Start job chooses which point of the box around all objects sits under the
head (green square on the canvas). The position on the canvas doesn't matter
for the job; "Show the design at the head" only lines up the view. Only the
top-right point has been checked against a real file so far.
Jobs run engrave → mark → cut, inner shapes before outer ones. Outlines from
SVGs default to Cut, filled shapes and new text to Engrave.

## Projects

File → New / Open / Save / Save as (⌘N, ⌘O, ⌘S, ⇧⌘S) work with `.fokus`
project files (JSON): the design, what each colour does, the material (by
name) and the start point. Text keeps its outlines too, so a project looks the
same on a computer without that font. The window title shows the project and a
dot while there are unsaved changes; New, Open and closing the window ask
before discarding them. File → Export job file saves a `.rd` for a USB stick.

## Languages

Fokus is fully translated into English and German and follows the system
language (Settings → Display → Language to choose). Texts live in
[Project Fluent](https://projectfluent.org) files, `locales/en.ftl` and
`locales/de.ftl`: `key = text`, with `{ $placeholders }`. To add a language
without rebuilding, copy `en.ftl` to `<config folder>/fokus/locales/<code>.ftl`
(e.g. `fr.ftl`) and translate it; it then appears in Settings. A file there
with the code `en` or `de` overrides the built-in texts. Missing keys fall back
to English. In code, texts are `t!("key")` or `t!("key", name = value)`; a test
checks that every key used in the code exists in both built-in languages.

## Other

- **Materials** (top bar): the library, seeded from the Lasermaxx Plott 100
  reference sheet, saved as JSON in the config folder.
- **Machine**: connects over USB by itself, shows position and status, and starts jobs after the checklist (Pause / Resume / Stop and progress in the right panel).
- **Settings**: machine connection (USB), origin corner,
  appearance (system / light / dark; `FOKUS_THEME=light|dark` at startup).
- Opening an `.rd` file shows it as a finished job (preview only).

Linux builds need the usual egui/winit system libraries plus `libudev-dev`.
