//! The material library. One material ("Pappelsperrholz 4 mm") carries the
//! settings for every operation it supports, so a job only needs the
//! material and each layer just says what to do: cut, mark or engrave.
//!
//! Stored as JSON in the user's config folder (e.g. ~/Library/Application
//! Support/fokus/materials.json) so it can be copied between computers.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::t;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Operation {
    /// Follow the lines, cutting through.
    Cut,
    /// Follow the lines, marking the surface only.
    Mark,
    /// Fill closed shapes line by line ("Scan" in LotusCut).
    Engrave,
    /// Bitmap images, line by line.
    Photo,
}

impl Operation {
    pub const ALL: [Operation; 4] = [Operation::Cut, Operation::Mark, Operation::Engrave, Operation::Photo];

    pub fn label(self) -> String {
        match self {
            Operation::Cut => t!("op-cut"),
            Operation::Mark => t!("op-mark"),
            Operation::Engrave => t!("op-engrave"),
            Operation::Photo => t!("op-photo"),
        }
    }

    pub fn explain(self) -> String {
        match self {
            Operation::Cut => t!("op-cut-explain"),
            Operation::Mark => t!("op-mark-explain"),
            Operation::Engrave => t!("op-engrave-explain"),
            Operation::Photo => t!("op-photo-explain"),
        }
    }

    pub fn uses_line_interval(self) -> bool {
        matches!(self, Operation::Engrave | Operation::Photo)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpSettings {
    /// mm/s
    pub speed: f64,
    pub min_power: f32,
    pub max_power: f32,
    pub passes: u32,
    pub air_assist: bool,
    /// Engrave / photo line spacing in mm.
    #[serde(default)]
    pub line_interval_mm: f32,
}

impl OpSettings {
    pub fn summary(&self, op: Operation) -> String {
        let power = if (self.min_power - self.max_power).abs() < 0.05 {
            format!("{:.0} %", self.max_power)
        } else {
            format!("{:.0}–{:.0} %", self.min_power, self.max_power)
        };
        let mut s = t!("settings-summary", speed = format!("{:.0}", self.speed), power = power);
        if self.passes > 1 {
            s.push_str(&format!(" · {}×", self.passes));
        }
        if op.uses_line_interval() && self.line_interval_mm > 0.0 {
            s.push_str(&t!("settings-summary-lines", interval = self.line_interval_mm.to_string()));
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Material {
    pub name: String,
    /// 0 = not specified (surface-only materials, e.g. anodised aluminium).
    pub thickness_mm: f32,
    pub cut: Option<OpSettings>,
    pub mark: Option<OpSettings>,
    pub engrave: Option<OpSettings>,
    pub photo: Option<OpSettings>,
    /// Tested on our machine and known to work.
    pub verified: bool,
    #[serde(default)]
    pub notes: String,
}

impl Material {
    pub fn settings(&self, op: Operation) -> Option<&OpSettings> {
        match op {
            Operation::Cut => self.cut.as_ref(),
            Operation::Mark => self.mark.as_ref(),
            Operation::Engrave => self.engrave.as_ref(),
            Operation::Photo => self.photo.as_ref(),
        }
    }

    pub fn settings_mut(&mut self, op: Operation) -> &mut Option<OpSettings> {
        match op {
            Operation::Cut => &mut self.cut,
            Operation::Mark => &mut self.mark,
            Operation::Engrave => &mut self.engrave,
            Operation::Photo => &mut self.photo,
        }
    }

    pub fn new_blank() -> Self {
        Material {
            name: t!("material-new"),
            thickness_mm: 3.0,
            cut: Some(OpSettings { speed: 20.0, min_power: 50.0, max_power: 50.0, passes: 1, air_assist: true, line_interval_mm: 0.0 }),
            mark: None,
            engrave: None,
            photo: None,
            verified: false,
            notes: String::new(),
        }
    }
}

fn op(speed: f64, power: f32, air_assist: bool, line_interval_mm: f32) -> Option<OpSettings> {
    Some(OpSettings { speed, min_power: power, max_power: power, passes: 1, air_assist, line_interval_mm })
}

/// The vendor's reference sheet, grouped into materials. Each value is as
/// printed; the sheet entry it came from is named in the notes.
pub fn reference_materials() -> Vec<Material> {
    let wood_engrave = op(300.0, 30.0, true, 0.15); // Holz_Gravur
    let wood_photo = op(1000.0, 45.0, true, 0.0); // Foto auf Holz
    let acrylic_engrave = op(500.0, 30.0, false, 0.05); // Gravur Acryl
    let m = |name: &str, thickness: f32, cut, engrave, photo, notes: String| Material {
        name: name.into(),
        thickness_mm: thickness,
        cut,
        mark: None,
        engrave,
        photo,
        verified: true,
        notes: format!("{} {notes}", t!("ref-note")).trim().to_string(),
    };
    vec![
        m("Pappelsperrholz 4 mm", 4.0, op(25.0, 100.0, false, 0.0), wood_engrave.clone(), wood_photo.clone(),
          t!("ref-note-ply4")),
        m("Sperrholz 8 mm", 8.0, op(12.0, 100.0, true, 0.0), wood_engrave, wood_photo,
          t!("ref-note-ply8")),
        m("Acryl 5 mm", 5.0, op(20.0, 80.0, true, 0.0), acrylic_engrave.clone(), None,
          t!("ref-note-acrylic5")),
        m("Acryl 8 mm", 8.0, op(10.0, 100.0, true, 0.0), acrylic_engrave.clone(), None, t!("ref-note-acrylic")),
        m("Acryl 10 mm", 10.0, op(6.0, 100.0, true, 0.0), acrylic_engrave, None, t!("ref-note-acrylic")),
        m("Pappe 1,5 mm", 1.5, op(200.0, 40.0, true, 0.0), None, None, String::new()),
        m("Pappe", 0.0, op(100.0, 40.0, true, 0.0), None, None, t!("ref-note-pappe")),
        m("Papier", 0.0, op(500.0, 50.0, false, 0.0), None, None, t!("ref-note-papier")),
        m("Kunstfilz", 0.0, op(100.0, 35.0, true, 0.0), None, None, String::new()),
        m("PE-Schaum 20 mm", 20.0, op(5.0, 20.0, true, 0.0), None, None, String::new()),
        m("Aluminium eloxiert", 0.0, None, op(300.0, 35.0, true, 0.05), op(800.0, 35.0, true, 0.0),
          t!("ref-note-alu")),
        m("Schiefer", 0.0, None, op(500.0, 40.0, true, 0.1), op(500.0, 30.0, true, 0.0),
          t!("ref-note-schiefer")),
        m("Edelstahl mit LMM 114", 0.0, None, op(150.0, 100.0, true, 0.05), None, t!("ref-note-edelstahl")),
        m("Gravurkunststoff Innograv", 0.0, None, op(800.0, 30.0, true, 0.05), None, t!("ref-note-innograv")),
        m("Polypropylen (PP)", 0.0, None, op(500.0, 40.0, false, 0.1), None, t!("ref-note-pp")),
    ]
}

pub struct Library {
    pub materials: Vec<Material>,
    path: Option<PathBuf>,
    pub error: Option<String>,
}

impl Library {
    pub fn load() -> Self {
        let path = dirs::config_dir().map(|d| d.join("fokus").join("materials.json"));
        let mut lib = Library { materials: reference_materials(), path: path.clone(), error: None };
        if let Some(path) = path.filter(|p| p.exists()) {
            let read = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|s| serde_json::from_str::<Vec<Material>>(&s).map_err(|e| e.to_string()));
            match read {
                Ok(materials) => lib.materials = materials,
                Err(e) => lib.error = Some(t!("err-could-not-read", file = path.display().to_string(), error = e)),
            }
        }
        lib
    }

    /// Add reference materials that are missing (by name).
    pub fn restore_reference(&mut self) -> usize {
        let missing: Vec<Material> = reference_materials()
            .into_iter()
            .filter(|r| !self.materials.iter().any(|m| m.name == r.name))
            .collect();
        let n = missing.len();
        self.materials.extend(missing);
        n
    }

    pub fn save(&mut self) {
        let Some(path) = &self.path else { return };
        let result = path
            .parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .and_then(|_| std::fs::write(path, serde_json::to_string_pretty(&self.materials).unwrap()));
        self.error = result.err().map(|e| t!("err-could-not-save", file = path.display().to_string(), error = e.to_string()));
    }

    pub fn location(&self) -> Option<String> {
        self.path.as_ref().map(|p| p.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_values() {
        let lib = reference_materials();
        let ply = lib.iter().find(|m| m.name == "Pappelsperrholz 4 mm").unwrap();
        // The LightBurn test job's cut layer.
        let cut = ply.settings(Operation::Cut).unwrap();
        assert_eq!((cut.speed, cut.max_power), (25.0, 100.0));
        assert_eq!(ply.settings(Operation::Engrave).unwrap().line_interval_mm, 0.15);
        for m in &lib {
            assert!(m.cut.is_some() || m.engrave.is_some(), "{}", m.name);
            if let Some(e) = &m.engrave {
                assert!(e.line_interval_mm > 0.0, "{}", m.name);
            }
        }
    }

    #[test]
    fn json_roundtrip() {
        let lib = reference_materials();
        let json = serde_json::to_string(&lib).unwrap();
        assert_eq!(serde_json::from_str::<Vec<Material>>(&json).unwrap(), lib);
    }
}
