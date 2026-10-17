// Release builds on Windows are GUI apps (no console window); see platform::windows::init_process.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod autostart;
mod cli;
mod hotkey;
mod log;
mod platform;
mod renderer;
mod settings_ui;
mod single_instance;
#[cfg(feature = "tray")]
mod tray;
mod updater;

use std::path::{Path, PathBuf};
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
    if cli.update {
        let result = updater::check().and_then(|u| match u {
            Some(u) => updater::install(&u).map(|()| Some(u.version)),
            None => Ok(None),
        });
        match result {
            Ok(Some(v)) => println!("updated to {v}; restart Mouse Blur to use it"),
            Ok(None) => println!("mouse-blur {} is up to date", updater::current_version()),
            Err(e) => {
                eprintln!("update failed: {e}");
                std::process::exit(1);
            }
        }
    } else if cli.check_update {
        match updater::check() {
            Ok(None) => println!("mouse-blur {} is up to date", updater::current_version()),
            Ok(Some(u)) => println!("mouse-blur {} is available (this is {})", u.version, updater::current_version()),
            Err(e) => {
                eprintln!("update check failed: {e}");
                std::process::exit(1);
            }
        }
    } else if cli.self_test {
        run_self_test(&cli);
    } else if cli.wants_background() && !cli.settings {
        run_background(&cli, config_path);
    } else {
        run_settings(config_path);
    }
}

/// The settings window. Double-clicking the app lands here; it also makes sure the background
/// effect is running, and closing the window leaves the effect running in the tray.
fn run_settings(config_path: PathBuf) {
    let _lock = match single_instance::acquire(single_instance::SETTINGS) {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => {
            // Already open: ask that window to come to the front instead.
            settings_ui::request_raise();
            return;
        }
        Err(e) => {
            warning!("could not check for an open settings window: {e}");
            None
        }
    };
    ensure_config(&config_path);
    if !single_instance::is_held(single_instance::BACKGROUND) {
        app::start_background(&config_path);
    }
    if let Err(e) = settings_ui::run(config_path) {
        error!("settings window: {e}");
        platform::show_error(&format!("Could not open the settings window: {e}"));
        std::process::exit(1);
    }
}

/// The effect itself: overlay, tray icon, hotkey. Started by the settings window, at login, or
/// from the command line.
fn run_background(cli: &cli::Cli, config_path: PathBuf) {
    let _lock = match single_instance::acquire_patiently(single_instance::BACKGROUND) {
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
    let mut settings = ensure_config(&config_path);
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

/// Defaults, no tray, no lock and nothing written, so it can run beside a real instance.
fn run_self_test(cli: &cli::Cli) {
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
}

/// Loads the config, writing the defaults first if there isn't one yet.
fn ensure_config(path: &Path) -> Settings {
    if path.exists() {
        return config::load(path).unwrap_or_else(|e| {
            warning!("{e}; using defaults");
            Settings::default()
        });
    }
    let s = Settings::default();
    if let Err(e) = config::save(path, &s) {
        warning!("could not write {}: {e}", path.display());
    }
    s
}
