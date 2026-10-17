// Release builds on Windows are GUI apps (no console window); see platform::windows::init_process.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod cli;
mod hotkey;
mod log;
mod platform;
mod renderer;
mod single_instance;
#[cfg(feature = "tray")]
mod tray;

use std::sync::Arc;

use mouse_blur::config::{self, Settings};
use mouse_blur::history::MotionHistory;

use crate::app::{Controller, Shared};
use crate::log::{error, warning};

fn main() {
    platform::init_process();

    let cli = match cli::parse(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("mouse-blur: {e}\nTry 'mouse-blur --help'.");
            std::process::exit(2);
        }
    };
    if cli.help {
        print!("{}", cli::help());
        return;
    }
    if cli.version {
        println!("mouse-blur {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let config_path = cli.config.clone().unwrap_or_else(config::default_path);
    if cli.print_config_path {
        println!("{}", config_path.display());
        return;
    }
    if cli.self_test {
        // Defaults, no tray, no lock and nothing written, so it can run beside a real instance.
        let mut settings = Settings::default();
        cli.overrides.apply(&mut settings);
        let shared = Arc::new(Shared::new(settings.sanitized(), true));
        let history = Arc::new(MotionHistory::new(4096));
        let (shared2, history2) = (shared.clone(), history.clone());
        let unused_config = std::env::temp_dir().join("mouse-blur-self-test.toml");
        let result = platform::run(shared, history, move |_| Controller::new(shared2, history2, unused_config, false));
        if let Err(e) = result {
            eprintln!("self-test failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    let _lock = match single_instance::acquire() {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => {
            eprintln!("mouse-blur is already running.");
            return;
        }
        Err(e) => {
            warning!("could not check for another running copy: {e}");
            None
        }
    };

    let mut settings = if config_path.exists() {
        config::load(&config_path).unwrap_or_else(|e| {
            warning!("{e}; using defaults");
            Settings::default()
        })
    } else {
        let s = Settings::default();
        if let Err(e) = config::save(&config_path, &s) {
            warning!("could not write {}: {e}", config_path.display());
        }
        s
    };
    cli.overrides.apply(&mut settings);

    let shared = Arc::new(Shared::new(settings.sanitized(), false));
    let history = Arc::new(MotionHistory::new(4096));
    let want_tray = !cli.no_tray;
    let (shared2, history2) = (shared.clone(), history.clone());
    let result = platform::run(shared, history, move |tray_available| {
        Controller::new(shared2, history2, config_path, want_tray && tray_available)
    });
    if let Err(e) = result {
        error!("{e}");
        platform::show_error(&e);
        std::process::exit(1);
    }
}
