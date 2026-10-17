//! Keeps a second copy from starting (two overlays would draw two trails).

use std::fs::{File, OpenOptions, TryLockError};

/// Held for the life of the process; the OS releases the lock when it exits.
pub struct InstanceLock(#[allow(dead_code)] File);

/// `Ok(None)` means another instance already holds the lock.
pub fn acquire() -> Result<Option<InstanceLock>, String> {
    let dir = mouse_blur::config::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("instance.lock"))
        .map_err(|e| e.to_string())?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceLock(file))),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e.to_string()),
    }
}
