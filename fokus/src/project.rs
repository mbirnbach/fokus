//! Fokus project files (`.fokus`): the design, what each colour does, the
//! chosen material and the start point, as JSON.
//!
//! Text objects keep their outlines as well as their content and font, so a
//! project looks the same on a computer without that font. The material is
//! stored by name; settings stay in the material library.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::design::{Anchor, Design};
use crate::t;

pub const EXTENSION: &str = "fokus";
const FORMAT: &str = "fokus-project";
const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    format: String,
    version: u32,
    pub design: Design,
    pub material: Option<String>,
    pub anchor: Anchor,
}

impl Project {
    pub fn new(design: Design, material: Option<String>, anchor: Anchor) -> Self {
        Project { format: FORMAT.into(), version: VERSION, design, material, anchor }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("projects always serialise")
    }

    pub fn from_json(json: &str) -> Result<Project, String> {
        // Check the header first for a clear message on foreign files.
        #[derive(Deserialize)]
        struct Header {
            format: Option<String>,
            version: Option<u32>,
        }
        let header: Header = serde_json::from_str(json).map_err(|_| t!("err-not-a-project"))?;
        if header.format.as_deref() != Some(FORMAT) {
            return Err(t!("err-not-a-project"));
        }
        match header.version {
            Some(v) if v > VERSION => return Err(t!("err-project-newer", version = v)),
            Some(_) => {}
            None => return Err(t!("err-project-no-version")),
        }
        let mut project: Project = serde_json::from_str(json).map_err(|e| t!("err-project-damaged", error = e.to_string()))?;
        project.design.touch();
        Ok(project)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        // Write next to the target first so a failed save never leaves a
        // half-written project behind.
        let tmp = path.with_extension("fokus.tmp");
        std::fs::write(&tmp, self.to_json()).map_err(|e| t!("err-could-not-save", file = path.display().to_string(), error = e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| t!("err-could-not-save", file = path.display().to_string(), error = e.to_string()))
    }

    pub fn load(path: &Path) -> Result<Project, String> {
        let json = std::fs::read_to_string(path).map_err(|e| t!("err-could-not-open", file = path.display().to_string(), error = e.to_string()))?;
        Project::from_json(&json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::{Action, Font, Kind, Object};
    use crate::materials::Operation;

    fn sample() -> Project {
        let mut d = Design::default();
        d.add(Object::new(Kind::Rect { radius: 4.0 }, (10.0, 20.0, 80.0, 40.0), [0, 0, 0]));
        let mut circle = Object::new(Kind::Ellipse, (15.0, 35.0, 6.0, 6.0), [0, 0, 0]);
        circle.rotation = 30.0;
        d.add(circle);
        d.add(Object::text("Hallo", Font::Serif, true, (30.0, 30.0), 12.0, [224, 49, 49]).unwrap());
        d.group(&[0, 1]);
        d.layers[0].action = Action::Do(Operation::Cut);
        d.layers[1].action = Action::Skip;
        Project::new(d, Some("Pappelsperrholz 4 mm".into()), Anchor { h: 0, v: 0 })
    }

    #[test]
    fn roundtrip_keeps_everything() {
        let p = sample();
        let back = Project::from_json(&p.to_json()).unwrap();
        assert!(back.design.same_content(&p.design));
        assert_eq!(back.material, p.material);
        assert_eq!(back.anchor, p.anchor);
        // Groups still work after loading (next group id restored too).
        assert_eq!(back.design.with_group(0), vec![0, 1]);
        let mut d = back.design.clone();
        let copies = d.duplicate(&[0, 1]);
        assert_ne!(d.objects[copies[0]].group, d.objects[0].group);
    }

    #[test]
    fn text_keeps_its_outlines() {
        let p = sample();
        let back = Project::from_json(&p.to_json()).unwrap();
        assert_eq!(back.design.objects[2].outline(), p.design.objects[2].outline());
    }

    #[test]
    fn foreign_and_future_files_are_refused() {
        assert_eq!(Project::from_json("{\"objects\": []}").unwrap_err(), t!("err-not-a-project"));
        assert!(Project::from_json("not json").is_err());
        let future = sample().to_json().replace("\"version\":1", "\"version\":99");
        assert_eq!(Project::from_json(&future).unwrap_err(), t!("err-project-newer", version = 99));
    }

    #[test]
    fn save_and_load_a_file() {
        let dir = std::env::temp_dir().join(format!("fokus-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tag.fokus");
        sample().save(&path).unwrap();
        let back = Project::load(&path).unwrap();
        assert!(back.design.same_content(&sample().design));
        assert!(!dir.join("tag.fokus.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
