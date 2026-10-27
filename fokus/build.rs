fn main() {
    println!("cargo:rerun-if-changed=../assets/brand/icon/fokus.ico");
    // The Windows exe carries the app icon (Explorer, taskbar, Alt+Tab).
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../assets/brand/icon/fokus.ico");
        res.compile().expect("Windows resources");
    }
}
