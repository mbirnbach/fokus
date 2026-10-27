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
/// 2: paths as Bézier curves, text with any font family.
const VERSION: u32 = 2;

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
        for o in &mut project.design.objects {
            o.fix_text();
        }
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
    use crate::design::{Action, Kind, Object, TextStyle};
    use crate::materials::Operation;

    fn sample() -> Project {
        let mut d = Design::default();
        d.add(Object::new(Kind::Rect { radius: 4.0 }, (10.0, 20.0, 80.0, 40.0), [0, 0, 0]));
        let mut circle = Object::new(Kind::Ellipse, (15.0, 35.0, 6.0, 6.0), [0, 0, 0]);
        circle.rotation = 30.0;
        d.add(circle);
        d.add(Object::text("Hallo", TextStyle { bold: true, ..TextStyle::default() }, (30.0, 40.0), 12.0, [224, 49, 49]).unwrap());
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
        let future = sample().to_json().replace("\"version\":2", "\"version\":99");
        assert_eq!(Project::from_json(&future).unwrap_err(), t!("err-project-newer", version = 99));
    }

    #[test]
    fn version_1_files_still_open() {
        // Points instead of curves, and one of three fixed fonts.
        let json = r#"{"format":"fokus-project","version":1,"material":null,"anchor":{"h":1,"v":-1},
            "design":{"next_group":0,"layers":[],"objects":[
              {"name":"Line","kind":{"Path":{"polylines":[[[0.0,0.0],[1.0,1.0]]],"filled":false}},"x":5.0,"y":5.0,"w":10.0,"h":10.0,"rotation":0.0,"flip_x":false,"flip_y":false,"color":[0,0,0],"group":null},
              {"name":"Text","kind":{"Text":{"text":"Hi","font":"Serif","bold":false,"outlines":[[[0.0,0.0],[1.0,0.0],[1.0,1.0],[0.0,0.0]]],"aspect":1.5}},"x":0.0,"y":0.0,"w":15.0,"h":10.0,"rotation":0.0,"flip_x":false,"flip_y":false,"color":[224,49,49],"group":null}]}}"#;
        let p = Project::from_json(json).unwrap();
        assert_eq!(p.design.objects[0].outline(), vec![vec![(5.0, 5.0), (15.0, 15.0)]]);
        let Kind::Text { style, scale, .. } = &p.design.objects[1].kind else { panic!() };
        assert!(crate::fonts::families().contains(&style.family));
        assert!(scale.0 > 0.0 && scale.1 > 0.0);
        // Saved outlines are kept until the text is edited.
        assert_eq!(p.design.objects[1].outline()[0].len(), 4);
        assert_eq!(p.design.layers.len(), 2);
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
