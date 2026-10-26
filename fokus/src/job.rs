//! A loaded `.rd` job for preview.

use std::path::Path;

use ruida::command;
use ruida::job::{LayerInfo, Toolpath};
use ruida::scramble;

use crate::t;

pub struct Job {
    pub name: String,
    pub toolpath: Toolpath,
    /// Cut bounds in mm, job coordinates: (min, max).
    pub bounds: Option<((f64, f64), (f64, f64))>,
}

impl Job {
    pub fn load(path: &Path) -> Result<Job, String> {
        let raw = std::fs::read(path).map_err(|e| t!("err-could-not-read", file = path.display().to_string(), error = e.to_string()))?;
        let magic = scramble::detect_magic(&raw).ok_or_else(|| t!("err-file-empty"))?;
        let records = command::parse(&scramble::unscramble(&raw, magic));
        let toolpath = Toolpath::from_records(&records);
        if toolpath.unknown_commands * 5 > toolpath.total_commands {
            return Err(t!("err-not-rd"));
        }
        let mm = |v: i64| v as f64 / 1000.0;
        let bounds = toolpath.cut_bounds().map(|((x0, y0), (x1, y1))| ((mm(x0), mm(y0)), (mm(x1), mm(y1))));
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Job { name, toolpath, bounds })
    }

    /// Rough burn time: cut length / speed, ignoring travel and acceleration.
    pub fn layer_seconds(layer: &LayerInfo) -> Option<f64> {
        layer.speed.filter(|s| *s > 0.0).map(|s| layer.cut_mm / s)
    }
}
