//! The log: what happened with the connection, jobs, files and updates, for
//! finding out afterwards what went wrong. It lives next to the settings as
//! `fokus.log`; when it gets large it moves to `fokus.log.1` and starts over,
//! so it never takes more than a few megabytes.
//!
//! Logging never gets in the way: if the file can't be written, the line is
//! lost and Fokus carries on. Before `init` (and in tests) nothing is written.
//! The home folder in a line becomes `~`, so a shared log doesn't show whose
//! computer it is from.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// The log starts over beyond this size.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

struct Log {
    path: PathBuf,
    file: File,
    size: u64,
    max: u64,
    home: Option<String>,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

impl Log {
    fn open(path: PathBuf, max: u64, home: Option<String>) -> Option<Log> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).ok()?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
        let size = file.metadata().ok()?.len();
        Some(Log { path, file, size, max, home })
    }

    fn write(&mut self, line: &str) {
        let text = match &self.home {
            Some(home) => line.replace(home.as_str(), "~"),
            None => line.to_string(),
        };
        let text = format!("{} {text}\n", timestamp(SystemTime::now()));
        if self.size + text.len() as u64 > self.max {
            let _ = fs::rename(&self.path, older(&self.path));
            if let Ok(file) = OpenOptions::new().create(true).append(true).open(&self.path) {
                self.file = file;
                self.size = 0;
            }
        }
        if self.file.write_all(text.as_bytes()).is_ok() {
            self.size += text.len() as u64;
        }
    }
}

fn older(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// Where the log is.
pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("fokus").join("fokus.log"))
}

/// Start logging, also of crashes.
pub fn init() {
    let home = dirs::home_dir().map(|h| h.display().to_string()).filter(|h| h.len() > 1);
    if let Some(log) = path().and_then(|p| Log::open(p, MAX_BYTES, home)) {
        *LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(log);
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        // The message with its place in the code. Release builds have no
        // symbols, so a backtrace would only be a long list of nothing.
        write(&format!("CRASH in thread {}: {info}", thread.name().unwrap_or("?")));
        default_hook(info);
    }));
}

/// Log to a file of a test's own.
#[cfg(test)]
pub fn log_to(path: PathBuf) {
    *LOG.lock().unwrap_or_else(|e| e.into_inner()) = Log::open(path, MAX_BYTES, None);
}

/// Add a line.
pub fn write(line: &str) {
    // A crash while this very thread holds the lock must not hang: lose the line.
    if let Ok(mut log) = LOG.try_lock() {
        if let Some(log) = log.as_mut() {
            log.write(line);
        }
    }
}

/// The last lines of the log, oldest first.
pub fn tail(lines: usize) -> String {
    path().map(|p| tail_of(&p, lines)).unwrap_or_default()
}

fn tail_of(path: &Path, lines: usize) -> String {
    let read = |p: &Path| fs::read(p).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let text = read(&older(path)) + &read(path);
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// `2026-11-05 09:53:41Z` (UTC, so no time zone crate is needed).
fn timestamp(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

/// Add a line, like `format!`.
#[macro_export]
macro_rules! logln {
    ($($arg:tt)*) => {
        $crate::logfile::write(&format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fokus-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn timestamps() {
        let at = |s: u64| timestamp(UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(at(0), "1970-01-01 00:00:00Z");
        assert_eq!(at(951_782_400), "2000-02-29 00:00:00Z");
        assert_eq!(at(1_793_094_924), "2026-10-27 09:55:24Z");
    }

    #[test]
    fn lines_are_appended_and_survive_a_restart() {
        let d = dir("append");
        let p = d.join("fokus.log");
        Log::open(p.clone(), 1000, None).unwrap().write("one");
        Log::open(p.clone(), 1000, None).unwrap().write("two");
        let text = tail_of(&p, 10);
        assert!(text.contains("one") && text.contains("two"), "{text}");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn large_logs_start_over_and_keep_one_old_file() {
        let d = dir("rotate");
        let p = d.join("fokus.log");
        let mut log = Log::open(p.clone(), 200, None).unwrap();
        for i in 0..40 {
            log.write(&format!("line {i}"));
        }
        assert!(fs::metadata(&p).unwrap().len() <= 200);
        assert!(fs::metadata(older(&p)).unwrap().len() <= 200);
        assert!(!d.join("fokus.log.1.1").exists());
        // The tail runs across both files and ends with the newest line.
        let text = tail_of(&p, 1000);
        let in_new = fs::read_to_string(&p).unwrap().lines().count();
        assert!(text.lines().count() > in_new, "{text}");
        assert!(text.ends_with("line 39"), "{text}");
        assert_eq!(tail_of(&p, 2).lines().count(), 2);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_home_folder_is_not_in_the_log() {
        let d = dir("home");
        let p = d.join("fokus.log");
        Log::open(p.clone(), 1000, Some("/Users/someone".into())).unwrap().write("could not read /Users/someone/Desktop/a.svg");
        let text = tail_of(&p, 1);
        assert!(text.ends_with("could not read ~/Desktop/a.svg"), "{text}");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_missing_log_has_an_empty_tail() {
        assert_eq!(tail_of(&dir("none").join("fokus.log"), 5), "");
    }
}
