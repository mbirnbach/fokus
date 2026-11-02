//! Updates: looks for a newer release on GitHub, downloads it in the
//! background, checks that our release builds signed it and puts it in place
//! of the installed app, so the next start is the new version. The running
//! app (and a job on the laser) carries on untouched.
//!
//! How it is installed depends on how Fokus was installed:
//! - macOS: the new Fokus.app from the DMG replaces the app bundle (and must
//!   also carry our Developer ID signature).
//! - Windows, installed: the new installer runs silently when Fokus closes.
//! - Windows, from the zip: the new fokus.exe replaces the running one.
//! - Linux: the new AppImage (or binary from the tar.gz) replaces the file.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

const LATEST: &str = "https://api.github.com/repos/mbirnbach/fokus/releases/latest";
/// The public half of the key the release builds sign the downloads with.
const PUBLIC_KEY: &str = include_str!("../update-key.pub");
/// The Developer ID team that signs the Mac app.
#[cfg(target_os = "macos")]
const TEAM_ID: &str = "J829HHBMPW";
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
/// Not right at start: the window comes first, and the laser may be connecting.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(10);
const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;

/// The download's name after "fokus-<version>-", like scripts/package.sh names it.
const LABEL: Option<&str> = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("mac-apple-silicon")
} else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
    Some("mac-intel")
} else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
    Some("windows")
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("linux-x86_64")
} else {
    None
};

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// This copy can't update itself (a development build, or an unknown install).
    Unavailable,
    Idle,
    Checking,
    UpToDate,
    Downloading { version: String },
    /// Installed; the next start is this version.
    Ready { version: String },
    Failed(String),
}

/// How this copy of Fokus was installed, and where.
#[derive(Debug, Clone, PartialEq)]
enum Install {
    MacApp(PathBuf),
    WindowsSetup,
    WindowsExe(PathBuf),
    AppImage(PathBuf),
    LinuxExe(PathBuf),
}

impl Install {
    fn detect() -> Option<Install> {
        // Development builds never replace themselves (unless asked to, for testing).
        if cfg!(debug_assertions) && std::env::var_os("FOKUS_UPDATE_DEV").is_none() {
            return None;
        }
        LABEL?;
        let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
        if cfg!(target_os = "macos") {
            // .../Fokus.app/Contents/MacOS/fokus
            let bundle = exe.parent()?.parent()?.parent()?;
            return (bundle.extension()? == "app").then(|| Install::MacApp(bundle.to_path_buf()));
        }
        if cfg!(windows) {
            let dir = exe.parent()?;
            return Some(if dir.join("unins000.exe").exists() { Install::WindowsSetup } else { Install::WindowsExe(exe) });
        }
        match std::env::var_os("APPIMAGE") {
            Some(path) => Some(Install::AppImage(PathBuf::from(path))),
            None => Some(Install::LinuxExe(exe)),
        }
    }

    fn suffix(&self) -> &'static str {
        match self {
            Install::MacApp(_) => ".dmg",
            Install::WindowsSetup => "-setup.exe",
            Install::WindowsExe(_) => ".zip",
            Install::AppImage(_) => ".AppImage",
            Install::LinuxExe(_) => ".tar.gz",
        }
    }
}

pub struct Updater {
    install: Option<Install>,
    status: Arc<Mutex<Status>>,
    started: Instant,
    last_check: Option<Instant>,
    /// Windows installs: the downloaded installer, run when Fokus closes.
    installer: Arc<Mutex<Option<PathBuf>>>,
}

impl Updater {
    pub fn new() -> Self {
        let install = Install::detect();
        if let Some(Install::WindowsExe(exe)) = &install {
            // Left over from the last update.
            let _ = std::fs::remove_file(exe.with_extension("exe.old"));
        }
        let status = if install.is_some() { Status::Idle } else { Status::Unavailable };
        Updater { install, status: Arc::new(Mutex::new(status)), started: Instant::now(), last_check: None, installer: Arc::new(Mutex::new(None)) }
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    /// Call every frame: checks shortly after start and then every few hours.
    pub fn tick(&mut self, ctx: &eframe::egui::Context, automatic: bool) {
        if !automatic || self.install.is_none() {
            return;
        }
        let due = match self.last_check {
            None => self.started.elapsed() >= FIRST_CHECK_AFTER,
            Some(t) => t.elapsed() >= CHECK_EVERY,
        };
        if due {
            self.check(ctx);
        }
        ctx.request_repaint_after(FIRST_CHECK_AFTER);
    }

    /// Look for an update now (in the background), unless one is on its way or installed.
    pub fn check(&mut self, ctx: &eframe::egui::Context) {
        let Some(install) = self.install.clone() else { return };
        if matches!(self.status(), Status::Checking | Status::Downloading { .. } | Status::Ready { .. }) {
            return;
        }
        self.last_check = Some(Instant::now());
        *self.status.lock().unwrap() = Status::Checking;
        let (status, installer, ctx) = (self.status.clone(), self.installer.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = run(&install, &status, &installer);
            *status.lock().unwrap() = match result {
                Ok(Some(version)) => Status::Ready { version },
                Ok(None) => Status::UpToDate,
                Err(e) => Status::Failed(e),
            };
            ctx.request_repaint();
        });
    }

    /// When Fokus closes: start the new version if `restart`, and on Windows
    /// installs run the downloaded installer (which then starts it).
    pub fn on_exit(&self, restart: bool) {
        if let Some(setup) = self.installer.lock().unwrap().take() {
            let mut cmd = std::process::Command::new(setup);
            cmd.args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]);
            if restart {
                cmd.arg("/RELAUNCH=1");
            }
            let _ = cmd.spawn();
            return;
        }
        if !restart {
            return;
        }
        let _ = match &self.install {
            Some(Install::MacApp(bundle)) => std::process::Command::new("open").arg("-n").arg(bundle).spawn(),
            Some(Install::WindowsExe(exe) | Install::AppImage(exe) | Install::LinuxExe(exe)) => std::process::Command::new(exe).spawn(),
            _ => return,
        };
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Check, download, verify and install. `Ok(None)` when there is nothing newer.
fn run(install: &Install, status: &Mutex<Status>, installer: &Mutex<Option<PathBuf>>) -> Result<Option<String>, String> {
    let latest = std::env::var("FOKUS_UPDATE_URL").unwrap_or_else(|_| LATEST.to_string());
    let release: Release = get(&latest)?.body_mut().read_json().map_err(|e| e.to_string())?;
    let version = release.tag_name.trim_start_matches('v').to_string();
    let current = std::env::var("FOKUS_UPDATE_PRETEND_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    if !newer(&version, &current) {
        return Ok(None);
    }
    let name = format!("fokus-{version}-{}{}", LABEL.unwrap_or_default(), install.suffix());
    let url = |name: &str| release.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone());
    // A release's files appear a while after it is published, while they are built.
    let (Some(file_url), Some(sig_url)) = (url(&name), url(&format!("{name}.sig"))) else { return Ok(None) };

    *status.lock().unwrap() = Status::Downloading { version: version.clone() };
    let data = download(&file_url)?;
    let sig = download(&sig_url)?;
    verify(&data, &sig)?;

    let dir = std::env::temp_dir().join(format!("fokus-update-{version}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    match install {
        Install::MacApp(bundle) => install_mac(bundle, &dir, &data)?,
        Install::WindowsSetup => {
            let setup = dir.join(&name);
            std::fs::write(&setup, &data).map_err(|e| e.to_string())?;
            *installer.lock().unwrap() = Some(setup);
        }
        Install::WindowsExe(exe) => replace_running(exe, &unzip_exe(&data)?)?,
        Install::AppImage(file) => replace_running(file, &data)?,
        Install::LinuxExe(exe) => replace_running(exe, &untar_exe(&data)?)?,
    }
    if !matches!(install, Install::WindowsSetup) {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(Some(version))
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    ureq::get(url)
        .header("User-Agent", concat!("Fokus/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| e.to_string())
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    get(url)?.body_mut().with_config().limit(MAX_DOWNLOAD).read_to_vec().map_err(|e| e.to_string())
}

/// Is `a` a later version than `b`? ("0.1.10" > "0.1.9")
fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> { v.split('.').map(|p| p.parse().ok()).collect() };
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn verify(data: &[u8], sig: &[u8]) -> Result<(), String> {
    let bad = || crate::t!("update-err-signature");
    let key: [u8; 32] = unhex(PUBLIC_KEY.trim()).and_then(|k| k.try_into().ok()).ok_or_else(bad)?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&key).map_err(|_| bad())?;
    let sig = ed25519_dalek::Signature::from_slice(sig).map_err(|_| bad())?;
    key.verify_strict(data, &sig).map_err(|_| bad())
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (s.len() % 2 == 0).then_some(())?;
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// No permission to replace the app, most likely: say where, so it can be updated by hand.
fn write_error(folder: &Path, e: impl std::fmt::Display) -> String {
    format!("{} ({e})", crate::t!("update-err-write", folder = folder.display().to_string()))
}

/// Put `data` in place of `file`, which may be running: write it next to it,
/// then swap. Unix keeps the running copy's file open; Windows lets a running
/// .exe be renamed out of the way.
fn replace_running(file: &Path, data: &[u8]) -> Result<(), String> {
    let new = file.with_extension("new");
    let err = |e: std::io::Error| write_error(file.parent().unwrap_or(file), e);
    std::fs::write(&new, data).map_err(err)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)).map_err(err)?;
    }
    if cfg!(windows) {
        let old = file.with_extension("exe.old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(file, &old).map_err(err)?;
    }
    std::fs::rename(&new, file).map_err(err)
}

#[cfg(windows)]
fn unzip_exe(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| e.to_string())?;
    let name = zip.file_names().find(|n| n.ends_with("/fokus.exe")).map(str::to_string).ok_or("no fokus.exe in the update")?;
    let mut out = Vec::new();
    zip.by_name(&name).map_err(|e| e.to_string())?.read_to_end(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(not(windows))]
fn unzip_exe(_: &[u8]) -> Result<Vec<u8>, String> {
    unreachable!("zip updates are for Windows")
}

#[cfg(target_os = "linux")]
fn untar_exe(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(data));
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?;
        if path.components().count() == 2 && path.file_name().is_some_and(|n| n == "fokus") {
            let mut out = Vec::new();
            entry.read_to_end(&mut out).map_err(|e| e.to_string())?;
            return Ok(out);
        }
    }
    Err("no fokus in the update".into())
}

#[cfg(not(target_os = "linux"))]
fn untar_exe(_: &[u8]) -> Result<Vec<u8>, String> {
    unreachable!("tar.gz updates are for Linux")
}

/// Mount the DMG, check the app in it is ours, copy it next to the installed
/// one and swap the two.
#[cfg(target_os = "macos")]
fn install_mac(bundle: &Path, dir: &Path, dmg: &[u8]) -> Result<(), String> {
    use std::process::Command;
    let run = |cmd: &mut Command| -> Result<(), String> {
        let out = cmd.output().map_err(|e| e.to_string())?;
        if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
    };
    let image = dir.join("update.dmg");
    let mount = dir.join("mount");
    std::fs::write(&image, dmg).map_err(|e| e.to_string())?;
    run(Command::new("hdiutil").args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"]).arg(&mount).arg(&image))?;
    let parent = bundle.parent().ok_or("app has no folder")?;
    let staged = parent.join(".Fokus-update.app");
    let copied = (|| {
        let app = mount.join("Fokus.app");
        let requirement = format!("=anchor apple generic and certificate leaf[subject.OU] = \"{TEAM_ID}\"");
        run(Command::new("codesign").args(["--verify", "--deep", "--strict", "-R", &requirement]).arg(&app))
            .map_err(|e| format!("{} ({e})", crate::t!("update-err-apple")))?;
        let _ = std::fs::remove_dir_all(&staged);
        run(Command::new("ditto").arg(&app).arg(&staged)).map_err(|e| write_error(parent, e))
    })();
    let _ = run(Command::new("hdiutil").args(["detach", "-quiet"]).arg(&mount));
    copied?;
    let old = parent.join(".Fokus-old.app");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(bundle, &old).map_err(|e| write_error(parent, e))?;
    if let Err(e) = std::fs::rename(&staged, bundle) {
        let _ = std::fs::rename(&old, bundle);
        return Err(write_error(parent, e));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn install_mac(_: &Path, _: &Path, _: &[u8]) -> Result<(), String> {
    unreachable!("DMG updates are for macOS")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.1.10", "0.1.9"));
        assert!(newer("0.2.0", "0.1.1"));
        assert!(newer("1.0.0", "0.9.9"));
        assert!(!newer("0.1.1", "0.1.1"));
        assert!(!newer("0.1.0", "0.1.1"));
        assert!(!newer("0.2.0-beta", "0.1.0"));
    }

    #[test]
    fn public_key_is_valid() {
        let key: [u8; 32] = unhex(PUBLIC_KEY.trim()).and_then(|k| k.try_into().ok()).expect("64 hex digits");
        ed25519_dalek::VerifyingKey::from_bytes(&key).expect("an ed25519 public key");
    }

    #[test]
    fn signatures_from_another_key_are_refused() {
        let other = SigningKey::from_bytes(&[7; 32]);
        let data = b"not from the release builds";
        assert!(verify(data, &other.sign(data).to_bytes()).is_err());
        assert!(verify(data, b"short").is_err());
    }

    #[test]
    fn running_file_is_replaced() {
        let dir = std::env::temp_dir().join(format!("fokus-replace-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(if cfg!(windows) { "fokus.exe" } else { "fokus" });
        std::fs::write(&file, b"old").unwrap();
        replace_running(&file, b"new").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
