//! Material test grids: a field for each combination of speed (columns) and
//! power (rows), to find the settings for a material on a scrap piece.
//!
//! A grid is an ordinary part of the design. Every field is an object on a
//! layer of its own, with the field's settings as that layer's adjusted
//! values, so framing, the checklist, the time estimate and exporting all
//! work as usual. The best field's settings are saved to the material from
//! its layer card.

use ruida::writer::MAX_LAYERS;

use crate::design::{Action, Design, Kind, LayerSettings, Object, Pt, TextStyle, DEFAULT_PHOTO_INTERVAL_MM};
use crate::materials::{Material, OpSettings, Operation};
use crate::raster::{self, ImageLook};
use crate::t;

/// Letter size of the speed and power labels, mm.
const LABEL_SIZE: f64 = 3.0;
/// Room for the power labels on the left and the speed labels on top, mm.
const LABEL_W: f64 = 12.0;
const LABEL_H: f64 = 6.0;
/// Labels on a material without engrave settings: light, so they don't cut.
const LABEL_FALLBACK: OpSettings = OpSettings { speed: 300.0, min_power: 20.0, max_power: 20.0, passes: 1, air_assist: true, kerf_mm: 0.0, line_interval_mm: 0.15 };

/// What to burn: speeds across the columns, powers down the rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub op: Operation,
    /// mm/s, first and last column.
    pub speed: (f64, f64),
    /// Percent, first and last row.
    pub power: (f32, f32),
    pub cols: u32,
    pub rows: u32,
    pub cell_mm: f64,
    pub gap_mm: f64,
    pub passes: u32,
    pub air_assist: bool,
    pub line_interval_mm: f32,
    /// Kept from the material, so saving a field doesn't lose it.
    pub kerf_mm: f32,
    pub labels: bool,
}

impl Spec {
    /// A 5 × 5 grid around the material's settings for `op`, or around a
    /// careful start if it has none.
    pub fn new(material: &Material, op: Operation) -> Self {
        let (speed, power, interval) = match op {
            Operation::Cut => (20.0, 50.0, 0.0),
            Operation::Mark => (100.0, 20.0, 0.0),
            Operation::Engrave => (300.0, 30.0, 0.1),
            Operation::Photo => (500.0, 40.0, DEFAULT_PHOTO_INTERVAL_MM),
        };
        let base = material.settings(op).cloned().unwrap_or(OpSettings { speed, min_power: power, max_power: power, passes: 1, air_assist: true, kerf_mm: 0.0, line_interval_mm: interval });
        Spec {
            op,
            speed: ((base.speed * 0.5).round().max(1.0), (base.speed * 1.5).round().min(2000.0)),
            power: ((base.max_power * 0.5).round().max(1.0), (base.max_power * 1.5).round().min(100.0)),
            cols: 5,
            rows: 5,
            cell_mm: 10.0,
            gap_mm: 3.0,
            passes: base.passes,
            air_assist: base.air_assist,
            line_interval_mm: if base.line_interval_mm > 0.0 { base.line_interval_mm } else { interval },
            kerf_mm: base.kerf_mm,
            labels: true,
        }
    }

    /// Size of the whole grid with its labels, mm.
    pub fn size(&self) -> Pt {
        let extent = |n: u32| n as f64 * self.cell_mm + n.saturating_sub(1) as f64 * self.gap_mm;
        let (w, h) = if self.labels { (LABEL_W, LABEL_H) } else { (0.0, 0.0) };
        (w + extent(self.cols), h + extent(self.rows))
    }

    /// Why this grid can't be added to `design`, if so: every field needs a
    /// layer, and the controller has a limited number of them.
    pub fn problem(&self, design: &Design) -> Option<String> {
        let total = design.layers.len() + (self.cols * self.rows) as usize + usize::from(self.labels);
        (total > MAX_LAYERS).then(|| t!("err-grid-too-many", layers = total.to_string(), max = MAX_LAYERS.to_string()))
    }

    fn speed_at(&self, col: u32) -> f64 {
        step(self.speed.0, self.speed.1, col, self.cols)
    }

    fn power_at(&self, row: u32) -> f32 {
        step(self.power.0 as f64, self.power.1 as f64, row, self.rows) as f32
    }
}

/// Value `i` of `n` from `a` to `b`, in whole numbers that can be read off.
fn step(a: f64, b: f64, i: u32, n: u32) -> f64 {
    if n < 2 {
        return a.round();
    }
    (a + (b - a) * i as f64 / (n - 1) as f64).round()
}

/// `n` colours no layer of the design uses, as far apart as possible.
fn fresh_colors(design: &Design, n: usize) -> Vec<[u8; 3]> {
    (0u32..)
        .map(|i| {
            let hue = (i as f64 * 137.508).rem_euclid(360.0) / 60.0;
            let x = 1.0 - (hue.rem_euclid(2.0) - 1.0).abs();
            let (r, g, b) = match hue as u32 {
                0 => (1.0, x, 0.0),
                1 => (x, 1.0, 0.0),
                2 => (0.0, 1.0, x),
                3 => (0.0, x, 1.0),
                4 => (x, 0.0, 1.0),
                _ => (1.0, 0.0, x),
            };
            // Saturation and value change too, so close hues stay different colours.
            let (low, high) = (40.0 + (i % 3) as f64 * 15.0, 170.0 + (i % 4) as f64 * 20.0);
            let c = |v: f64| (low + (high - low) * v).round() as u8;
            [c(r), c(g), c(b)]
        })
        .filter(|c| !design.layers.iter().any(|l| l.color == *c))
        .scan(Vec::new(), |seen: &mut Vec<[u8; 3]>, c| {
            let new = !seen.contains(&c);
            if new {
                seen.push(c);
            }
            Some(new.then_some(c))
        })
        .flatten()
        .take(n)
        .collect()
}

/// Add the grid with its top-right corner at `top_right`, as one group.
/// Returns the new objects' indices.
pub fn add(design: &mut Design, spec: &Spec, top_right: Pt, material: &Material) -> Result<Vec<usize>, String> {
    if let Some(problem) = spec.problem(design) {
        return Err(problem);
    }
    let (w, _) = spec.size();
    let x0 = top_right.0 - w;
    let y0 = top_right.1;
    let (lw, lh) = if spec.labels { (LABEL_W, LABEL_H) } else { (0.0, 0.0) };
    let cell_at = |i: u32| i as f64 * (spec.cell_mm + spec.gap_mm);

    let mut colors = fresh_colors(design, (spec.cols * spec.rows + 1) as usize).into_iter();
    let label_color = colors.next().expect("colours never run out");

    // Everything that can fail comes first, so a failure adds nothing.
    let mut labels = Vec::new();
    if spec.labels {
        let text = |s: &str| Object::text(s, TextStyle::default(), (0.0, 0.0), LABEL_SIZE, label_color);
        let mut header = text("mm/s")?;
        header.x = x0 + lw - 2.0 - header.w;
        header.y = y0 + (lh - header.h) / 2.0;
        labels.push(header);
        for c in 0..spec.cols {
            let mut o = text(&format!("{:.0}", spec.speed_at(c)))?;
            o.x = x0 + lw + cell_at(c) + (spec.cell_mm - o.w) / 2.0;
            o.y = y0 + (lh - o.h) / 2.0;
            labels.push(o);
        }
        for r in 0..spec.rows {
            let mut o = text(&format!("{:.0} %", spec.power_at(r)))?;
            o.x = x0 + lw - 2.0 - o.w;
            o.y = y0 + lh + cell_at(r) + (spec.cell_mm - o.h) / 2.0;
            labels.push(o);
        }
    }

    let first = design.objects.len();
    let label_layer = (!labels.is_empty()).then_some(label_color);
    for o in labels {
        design.add(o);
    }
    if let Some(color) = label_layer {
        // Burned like the material's engraving; a normal layer that can be skipped or changed.
        let settings = material.settings(Operation::Engrave).cloned().unwrap_or(LABEL_FALLBACK);
        let layer = layer_mut(design, color);
        layer.action = Action::Do(Operation::Engrave);
        layer.adjusted = Some(settings);
        layer.label = Some(t!("grid-labels"));
    }

    let ramp = (spec.op == Operation::Photo).then(raster::grey_ramp);
    for r in 0..spec.rows {
        for c in 0..spec.cols {
            let color = colors.next().expect("colours never run out");
            let (speed, power) = (spec.speed_at(c), spec.power_at(r));
            let rect = (x0 + lw + cell_at(c), y0 + lh + cell_at(r), spec.cell_mm, spec.cell_mm);
            let kind = match &ramp {
                // Photos: a ramp from white to black shows how the greys come out.
                Some(image) => Kind::Image { image: image.clone(), look: ImageLook::default() },
                None => Kind::Rect { radius: 0.0 },
            };
            let label = format!("{speed:.0} mm/s · {power:.0} %");
            let mut o = Object::new(kind, rect, color);
            o.name = label.clone();
            design.add(o);
            let interval = if spec.op.uses_line_interval() { spec.line_interval_mm } else { 0.0 };
            let layer = layer_mut(design, color);
            layer.action = Action::Do(spec.op);
            layer.adjusted = Some(OpSettings { speed, min_power: power, max_power: power, passes: spec.passes, air_assist: spec.air_assist, kerf_mm: spec.kerf_mm, line_interval_mm: interval });
            layer.label = Some(label);
            layer.test_cell = Some(spec.op);
        }
    }
    let new: Vec<usize> = (first..design.objects.len()).collect();
    design.group(&new);
    Ok(new)
}

fn layer_mut(design: &mut Design, color: [u8; 3]) -> &mut LayerSettings {
    design.layers.iter_mut().find(|l| l.color == color).expect("adding an object makes its layer")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials::reference_materials;
    use ruida::writer::LayerKind;

    fn plywood() -> Material {
        reference_materials().into_iter().find(|m| m.name == "Pappelsperrholz 4 mm").unwrap()
    }

    fn cells(d: &Design) -> Vec<&LayerSettings> {
        d.layers.iter().filter(|l| l.test_cell.is_some()).collect()
    }

    #[test]
    fn starts_around_the_material_settings() {
        let s = Spec::new(&plywood(), Operation::Cut);
        // 25 mm/s and 100 %: half to one and a half times, within what the laser can do.
        assert_eq!((s.speed, s.power), ((13.0, 38.0), (50.0, 100.0)));
        // No spacing on the sheet: the photo default.
        assert_eq!(Spec::new(&plywood(), Operation::Photo).line_interval_mm, DEFAULT_PHOTO_INTERVAL_MM);
        // Operations the material doesn't have still get a start.
        let paper = reference_materials().into_iter().find(|m| m.name == "Papier").unwrap();
        assert_eq!(Spec::new(&paper, Operation::Engrave).speed, (150.0, 450.0));
    }

    #[test]
    fn every_field_has_its_own_settings() {
        let mut d = Design::default();
        let spec = Spec { cols: 3, rows: 2, speed: (100.0, 300.0), power: (20.0, 40.0), ..Spec::new(&plywood(), Operation::Engrave) };
        add(&mut d, &spec, (100.0, 10.0), &plywood()).unwrap();
        let got: Vec<(f64, f32)> = cells(&d).iter().map(|l| l.adjusted.as_ref().map(|s| (s.speed, s.max_power)).unwrap()).collect();
        assert_eq!(got, vec![(100.0, 20.0), (200.0, 20.0), (300.0, 20.0), (100.0, 40.0), (200.0, 40.0), (300.0, 40.0)]);
        assert_eq!(cells(&d)[1].label.as_deref(), Some("200 mm/s · 20 %"));
        assert!(cells(&d).iter().all(|l| l.action == Action::Do(Operation::Engrave)));
        // Faster to the right, more power further down, in the given corner.
        let field = |i: usize| d.objects.iter().find(|o| o.color == cells(&d)[i].color).unwrap();
        assert!(field(1).x > field(0).x && field(3).y > field(0).y);
        let ((_, y0), (x1, _)) = d.bounds().unwrap();
        assert!((x1 - 100.0).abs() < 1e-9 && y0 >= 10.0, "{x1} {y0}");
    }

    #[test]
    fn it_stays_clear_of_the_design_and_is_one_group() {
        let mut d = Design::default();
        d.add(Object::new(Kind::Rect { radius: 0.0 }, (0.0, 0.0, 5.0, 5.0), [0, 0, 0]));
        let new = add(&mut d, &Spec::new(&plywood(), Operation::Cut), (100.0, 10.0), &plywood()).unwrap();
        // 25 fields + labels + the rectangle, all on different layers.
        assert_eq!(d.layers.len(), 27);
        let mut colors: Vec<[u8; 3]> = d.layers.iter().map(|l| l.color).collect();
        colors.sort_unstable();
        colors.dedup();
        assert_eq!(colors.len(), 27);
        assert_eq!(d.with_group(new[0]).len(), new.len());
        assert_eq!(d.objects[0].group, None);
    }

    #[test]
    fn too_many_fields_are_refused_and_add_nothing() {
        let mut d = Design::default();
        let spec = Spec { cols: 6, rows: 6, ..Spec::new(&plywood(), Operation::Cut) };
        assert!(spec.problem(&d).is_some());
        assert!(add(&mut d, &spec, (100.0, 10.0), &plywood()).is_err());
        assert!(d.is_empty());
        assert!(Spec { cols: 6, rows: 5, ..spec }.problem(&d).is_none());
    }

    /// Each operation makes a job the writer accepts, with the fields' speeds
    /// and powers, the labels burned first and cuts last.
    #[test]
    fn grids_make_jobs_for_every_operation() {
        for op in Operation::ALL {
            let mut d = Design::default();
            let spec = Spec { cols: 2, rows: 2, speed: (100.0, 200.0), power: (10.0, 30.0), ..Spec::new(&plywood(), op) };
            add(&mut d, &spec, (100.0, 10.0), &plywood()).unwrap();
            let job = d.to_job(Some(&plywood()), true, crate::design::Anchor::TOP_RIGHT).unwrap();
            assert_eq!(job.len(), 5, "{op:?}");
            assert!(matches!(job[0].kind, LayerKind::Engrave { .. }), "{op:?}: labels first");
            let burned: Vec<(f64, f32)> = job[1..].iter().map(|l| (l.speed, l.max_power)).collect();
            assert_eq!(burned, vec![(100.0, 10.0), (200.0, 10.0), (100.0, 30.0), (200.0, 30.0)], "{op:?}");
            let plain = ruida::writer::write_plain(&job).unwrap();
            let tp = ruida::job::Toolpath::from_records(&ruida::command::parse(&plain));
            assert_eq!(tp.unknown_commands, 0, "{op:?}");
            assert_eq!(tp.layers.len(), 5, "{op:?}");
        }
    }

    #[test]
    fn labels_can_be_left_out() {
        let mut d = Design::default();
        let spec = Spec { cols: 2, rows: 1, labels: false, ..Spec::new(&plywood(), Operation::Cut) };
        add(&mut d, &spec, (50.0, 0.0), &plywood()).unwrap();
        assert_eq!(d.objects.len(), 2);
        assert_eq!(d.bounds().unwrap().0.1, 0.0);
    }

    #[test]
    fn grids_survive_a_project_file() {
        let mut d = Design::default();
        add(&mut d, &Spec { cols: 2, rows: 1, ..Spec::new(&plywood(), Operation::Photo) }, (50.0, 0.0), &plywood()).unwrap();
        let p = crate::project::Project::new(d, Some(plywood().name), crate::design::Anchor::TOP_RIGHT);
        let back = crate::project::Project::from_json(&p.to_json()).unwrap();
        assert!(back.design.same_content(&p.design));
    }
}
