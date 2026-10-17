//! "Start at login", shared by the tray and the settings window.

use crate::log::warning;

/// The login item starts only the background effect, not the settings window.
pub fn launcher() -> Option<auto_launch::AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.to_str()?;
    auto_launch::AutoLaunchBuilder::new()
        .set_app_name("mouse-blur")
        .set_app_path(path)
        .set_args(&["--background"])
        .build()
        .map_err(|e| warning!("start at login unavailable: {e}"))
        .ok()
}

pub fn is_enabled(launcher: &auto_launch::AutoLaunch) -> bool {
    launcher.is_enabled().unwrap_or(false)
}

pub fn set_enabled(launcher: &auto_launch::AutoLaunch, on: bool) {
    let result = if on { launcher.enable() } else { launcher.disable() };
    if let Err(e) = result {
        warning!("could not change start at login: {e}");
    }
}
