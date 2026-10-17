//! Minimal logging to stderr and a small rotating file in the config directory.

use std::fmt::Arguments;
use std::io::Write;
use std::sync::Mutex;

const MAX_LOG_BYTES: u64 = 256 * 1024;

pub fn write(level: &str, args: Arguments<'_>) {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock();
    let line = format!("{level}: {args}");
    eprintln!("mouse-blur: {line}");
    let dir = mouse_blur::config::config_dir();
    let path = dir.join("mouse-blur.log");
    if std::fs::metadata(&path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("mouse-blur.old.log"));
    }
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "[{secs}] {line}");
    }
}

macro_rules! warning {
    ($($arg:tt)*) => { $crate::log::write("warning", format_args!($($arg)*)) };
}

macro_rules! error {
    ($($arg:tt)*) => { $crate::log::write("error", format_args!($($arg)*)) };
}

pub(crate) use {error, warning};
