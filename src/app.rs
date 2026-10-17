//! State shared between threads, the pointer sampler, and the main-thread controller that reacts
//! to the tray menu, the hotkey and edits to the config file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use mouse_blur::config::{self, Settings};
use mouse_blur::history::{MotionHistory, MotionSample, now_ns};

use crate::hotkey::Hotkeys;
use crate::log::{info, warning};

pub const HOMEPAGE: &str = "https://github.com/fsiuhgshiqoih32/mouse-blur";

/// Settings and flags read by the render and sampler threads.
pub struct Shared {
    settings: RwLock<Settings>,
    version: AtomicU64,
    enabled: AtomicBool,
    quit: AtomicBool,
    auto_lag_ms: AtomicU32,
    frames: AtomicU64,
    /// `--self-test`: simulated pointer, fallback cursor, quit after a moment and report.
    pub self_test: bool,
}

impl Shared {
    pub fn new(settings: Settings, self_test: bool) -> Self {
        Shared {
            enabled: AtomicBool::new(settings.enabled),
            settings: RwLock::new(settings),
            version: AtomicU64::new(1),
            quit: AtomicBool::new(false),
            auto_lag_ms: AtomicU32::new(10),
            frames: AtomicU64::new(0),
            self_test,
        }
    }

    pub fn count_frame(&self) {
        self.frames.fetch_add(1, Ordering::Relaxed);
    }

    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn replace(&self, s: Settings) {
        self.enabled.store(s.enabled, Ordering::Relaxed);
        *self.settings.write().unwrap() = s;
        self.version.fetch_add(1, Ordering::Release);
    }

    pub fn quitting(&self) -> bool {
        self.quit.load(Ordering::Relaxed)
    }

    pub fn request_quit(&self) {
        self.quit.store(true, Ordering::Relaxed);
    }

    pub fn auto_lag_ms(&self) -> u32 {
        self.auto_lag_ms.load(Ordering::Relaxed)
    }

    pub fn set_auto_lag_ms(&self, ms: u32) {
        self.auto_lag_ms.store(ms, Ordering::Relaxed);
    }
}

/// Reads the pointer position in overlay pixels, or `None` if it can't right now.
pub type PointerSource = Box<dyn FnMut() -> Option<(f32, f32)> + Send>;

/// For `--self-test`: a pointer circling `center` at 2500 px/s, so the real one is never moved.
pub fn simulated_pointer(center: impl Fn() -> (f32, f32) + Send + 'static) -> PointerSource {
    let start = Instant::now();
    Box::new(move || {
        let (cx, cy) = center();
        let a = start.elapsed().as_secs_f32() * 2500.0 / 150.0;
        Some(((cx + 150.0 * a.cos()).round(), (cy + 150.0 * a.sin()).round()))
    })
}

/// Records the pointer position about 1000 times a second while it moves (125 when still).
pub fn spawn_sampler(mut source: PointerSource, shared: Arc<Shared>, history: Arc<MotionHistory>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("pointer sampler".into())
        .spawn(move || {
            let mut last = (f32::NAN, f32::NAN);
            let mut last_move = Instant::now();
            while !shared.quitting() {
                if !shared.enabled() {
                    history.clear();
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let t = now_ns();
                if let Some((x, y)) = source() {
                    history.push(MotionSample { t, x, y });
                    if (x, y) != last {
                        last = (x, y);
                        last_move = Instant::now();
                    }
                }
                let idle = last_move.elapsed() > Duration::from_millis(250);
                std::thread::sleep(Duration::from_millis(if idle { 8 } else { 1 }));
            }
        })
        .expect("spawn sampler thread")
}

/// Something the user asked for through the tray menu or hotkey.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(feature = "tray"), allow(dead_code))] // only the hotkey's toggle without a tray
pub enum Action {
    ToggleEnabled,
    OpenSettings,
    ToggleAutostart,
    OpenHomepage,
    Quit,
}

/// Main-thread logic of the background process: owns the tray, hotkey and autostart entry, and
/// keeps the config file and the shared settings in sync. The settings window is a separate
/// process that edits the config file; changes are picked up here within a quarter second.
pub struct Controller {
    shared: Arc<Shared>,
    history: Arc<MotionHistory>,
    config_path: PathBuf,
    known_mtime: Option<SystemTime>,
    last_config_check: Instant,
    hotkeys: Hotkeys,
    autostart: Option<auto_launch::AutoLaunch>,
    self_test_until: Option<Instant>,
    updates: Updates,
    #[cfg(feature = "tray")]
    tray: Option<crate::tray::Tray>,
}

/// Touched by the settings window's "Update now" button.
pub const UPDATE_REQUEST_FILE: &str = "update-request";
/// Holds the version an automatic update installed, for the settings window's notice.
pub const UPDATED_FILE: &str = "updated-to";

enum UpdateOutcome {
    UpToDate,
    Installed(String),
    Failed(String),
}

struct Updates {
    next_check: Instant,
    worker: Option<std::sync::mpsc::Receiver<UpdateOutcome>>,
    request_mtime: Option<SystemTime>,
}

impl Controller {
    pub fn new(shared: Arc<Shared>, history: Arc<MotionHistory>, config_path: PathBuf, want_tray: bool) -> Self {
        let settings = shared.settings();
        let mut hotkeys = Hotkeys::new();
        if !shared.self_test {
            hotkeys.set(&settings.hotkey);
        }
        let autostart = if shared.self_test { None } else { crate::autostart::launcher() };
        #[cfg(feature = "tray")]
        let tray = if want_tray {
            let autostart_on = autostart.as_ref().map(crate::autostart::is_enabled);
            let label = hotkeys.label().map(str::to_owned);
            // A missing tray library on Linux can panic deep inside the toolkit; run tray-less then.
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::tray::Tray::new(&settings, autostart_on, label.as_deref())
            })) {
                Ok(Ok(tray)) => Some(tray),
                Ok(Err(e)) => {
                    warning!("no tray icon: {e}");
                    None
                }
                Err(_) => {
                    warning!("no tray icon: the system tray library failed to load");
                    None
                }
            }
        } else {
            None
        };
        #[cfg(not(feature = "tray"))]
        let _ = want_tray;
        let known_mtime = mtime(&config_path);
        let self_test_until = shared.self_test.then(|| Instant::now() + Duration::from_secs(2));
        Controller {
            shared,
            history,
            config_path,
            known_mtime,
            last_config_check: Instant::now(),
            hotkeys,
            autostart,
            self_test_until,
            updates: Updates {
                next_check: Instant::now() + Duration::from_secs(20),
                worker: None,
                request_mtime: mtime(&config::config_dir().join(UPDATE_REQUEST_FILE)),
            },
            #[cfg(feature = "tray")]
            tray,
        }
    }

    /// Handles pending UI events. Call every ~50 ms on the main thread.
    pub fn poll(&mut self) {
        if self.self_test_until.is_some_and(|t| Instant::now() >= t) {
            let frames = self.shared.frames();
            println!("self-test: {frames} frames presented in 2 s (auto lag {} ms)", self.shared.auto_lag_ms());
            // Exit straight away: on macOS the run loop would otherwise outlive main's cleanup.
            std::process::exit(if frames > 0 { 0 } else { 1 });
        }
        #[cfg(feature = "tray")]
        {
            use tray_icon::TrayIconEvent;
            use tray_icon::menu::MenuEvent;
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                if let Some(action) = self.tray.as_ref().and_then(|t| t.action_for(&ev.id)) {
                    self.perform(action);
                }
            }
            while TrayIconEvent::receiver().try_recv().is_ok() {}
        }
        if self.hotkeys.pressed() {
            self.perform(Action::ToggleEnabled);
        }
        if self.last_config_check.elapsed() >= Duration::from_millis(250) {
            self.last_config_check = Instant::now();
            self.reload_if_changed();
            self.poll_updates();
        }
    }

    /// Checks GitHub for a newer release shortly after start and every six hours (release builds
    /// with auto-update on), or right away when the settings window asks. A successful install
    /// restarts the app on the new version.
    fn poll_updates(&mut self) {
        if self.shared.self_test {
            return;
        }
        if let Some(rx) = &self.updates.worker {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.updates.worker = None;
                    match outcome {
                        UpdateOutcome::UpToDate => {}
                        UpdateOutcome::Failed(e) => warning!("update failed: {e}"),
                        UpdateOutcome::Installed(version) => {
                            info!("updated to {version}; restarting");
                            let _ = std::fs::write(config::config_dir().join(UPDATED_FILE), &version);
                            start_background(&self.config_path);
                            self.perform(Action::Quit);
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.updates.worker = None,
            }
        }
        let m = mtime(&config::config_dir().join(UPDATE_REQUEST_FILE));
        let requested = m.is_some() && m != self.updates.request_mtime;
        self.updates.request_mtime = m;
        let due =
            Instant::now() >= self.updates.next_check && !cfg!(debug_assertions) && self.shared.settings().auto_update;
        if !requested && !due {
            return;
        }
        self.updates.next_check = Instant::now() + Duration::from_secs(6 * 3600);
        let (tx, rx) = std::sync::mpsc::channel();
        self.updates.worker = Some(rx);
        std::thread::spawn(move || {
            let outcome = match crate::updater::check() {
                Ok(None) => UpdateOutcome::UpToDate,
                Ok(Some(update)) => match crate::updater::install(&update) {
                    Ok(()) => UpdateOutcome::Installed(update.version),
                    Err(e) => UpdateOutcome::Failed(e),
                },
                Err(e) => UpdateOutcome::Failed(e),
            };
            let _ = tx.send(outcome);
        });
    }

    pub fn perform(&mut self, action: Action) {
        match action {
            Action::ToggleEnabled => self.change(|s| s.enabled = !s.enabled),
            Action::OpenSettings => open_settings(&self.config_path),
            Action::ToggleAutostart => {
                if let Some(a) = &self.autostart {
                    crate::autostart::set_enabled(a, !crate::autostart::is_enabled(a));
                }
                self.sync_ui();
            }
            Action::OpenHomepage => open_url(HOMEPAGE),
            Action::Quit => {
                self.shared.request_quit();
                self.history.wake();
            }
        }
    }

    fn change(&mut self, f: impl FnOnce(&mut Settings)) {
        let mut s = self.shared.settings();
        f(&mut s);
        let s = s.sanitized();
        self.shared.replace(s.clone());
        if let Err(e) = config::save(&self.config_path, &s) {
            warning!("could not save {}: {e}", self.config_path.display());
        }
        self.known_mtime = mtime(&self.config_path);
        self.sync_ui();
    }

    fn reload_if_changed(&mut self) {
        let m = mtime(&self.config_path);
        if m == self.known_mtime || m.is_none() {
            return;
        }
        self.known_mtime = m;
        match config::load(&self.config_path) {
            Ok(new) => {
                let old = self.shared.settings();
                if new != old {
                    if new.hotkey != old.hotkey {
                        self.hotkeys.set(&new.hotkey);
                    }
                    self.shared.replace(new);
                    self.sync_ui();
                }
            }
            Err(e) => warning!("config not reloaded: {e}"),
        }
    }

    fn sync_ui(&mut self) {
        #[cfg(feature = "tray")]
        if let Some(tray) = &mut self.tray {
            let autostart_on = self.autostart.as_ref().map(crate::autostart::is_enabled);
            tray.sync(&self.shared.settings(), autostart_on, self.hotkeys.label());
        }
    }
}

pub fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Starts another copy of this executable with `args`, detached from this one.
pub fn spawn_self(args: &[&std::ffi::OsStr]) {
    let result = std::env::current_exe().and_then(|exe| {
        let mut cmd = std::process::Command::new(exe);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        {
            // Its own process group, so closing a terminal that launched us doesn't take it down.
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        cmd.spawn()
    });
    match result {
        // Reap it when it exits so no zombie is left behind on Unix.
        Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
        Err(e) => warning!("could not start {args:?}: {e}"),
    }
}

/// Opens the settings window (a separate process). If one is open already, it raises itself.
pub fn open_settings(config_path: &Path) {
    spawn_self(&["--settings".as_ref(), "--config".as_ref(), config_path.as_os_str()]);
}

/// Starts the background effect (tray, hotkey and overlay) for `config_path`.
pub fn start_background(config_path: &Path) {
    spawn_self(&["--background".as_ref(), "--config".as_ref(), config_path.as_os_str()]);
}

pub fn open_file(path: &Path) {
    #[cfg(windows)]
    let result = std::process::Command::new("notepad.exe").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg("-t").arg(path).spawn();
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open").arg(path).spawn();
    if let Err(e) = result {
        warning!("could not open {}: {e}", path.display());
    }
}

pub fn open_url(url: &str) {
    #[cfg(windows)]
    let result = std::process::Command::new("explorer.exe").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = result {
        warning!("could not open {url}: {e}");
    }
}
