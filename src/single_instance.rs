//! Per-user locks that keep a second background instance (two overlays would draw two trails)
//! or a second settings window from starting.

use std::fs::{File, OpenOptions, TryLockError};
use std::time::Duration;

/// Held by the background process that draws the trail.
pub const BACKGROUND: &str = "instance.lock";
/// Held by an open settings window.
pub const SETTINGS: &str = "settings.lock";

/// Held for the life of the process; the OS releases the lock when it exits.
pub struct InstanceLock(#[allow(dead_code)] File);

/// `Ok(None)` means another process already holds the lock.
pub fn acquire(name: &str) -> Result<Option<InstanceLock>, String> {
    let dir = mouse_blur::config::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file =
        OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(name)).map_err(|e| e.to_string())?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceLock(file))),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e.to_string()),
    }
}

/// Like [`acquire`], but keeps trying for a few seconds: the settings window checks whether the
/// background is running by taking its lock for an instant, which could collide with start-up, and
/// after an update the new version starts while the old one is still shutting down.
pub fn acquire_patiently(name: &str) -> Result<Option<InstanceLock>, String> {
    for _ in 0..25 {
        if let Some(lock) = acquire(name)? {
            return Ok(Some(lock));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(None)
}

/// True if another process holds the lock.
pub fn is_held(name: &str) -> bool {
    matches!(acquire(name), Ok(None))
}
