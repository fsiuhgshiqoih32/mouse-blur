//! State shared between threads, the pointer sampler, and the main-thread controller that reacts
//! to the tray menu, the hotkey and edits to the config file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use mouse_blur::config::{self, Lag, Settings, Style};
use mouse_blur::history::{MotionHistory, MotionSample, now_ns};

use crate::hotkey::Hotkeys;
use crate::log::warning;

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
    Intensity(u32),
    Blur(u32),
    Taper(u32),
    Lag(Lag),
    Style(Style),
    OpenConfig,
    ToggleAutostart,
    OpenHomepage,
    Quit,
}

/// Main-thread logic: owns the tray, hotkey and autostart entry, and keeps the config file and
/// the shared settings in sync.
pub struct Controller {
    shared: Arc<Shared>,
    history: Arc<MotionHistory>,
    config_path: PathBuf,
    known_mtime: Option<SystemTime>,
    last_config_check: Instant,
    hotkeys: Hotkeys,
    autostart: Option<auto_launch::AutoLaunch>,
    self_test_until: Option<Instant>,
    #[cfg(feature = "tray")]
    tray: Option<crate::tray::Tray>,
}

impl Controller {
    pub fn new(shared: Arc<Shared>, history: Arc<MotionHistory>, config_path: PathBuf, want_tray: bool) -> Self {
        let settings = shared.settings();
        let mut hotkeys = Hotkeys::new();
        if !shared.self_test {
            hotkeys.set(&settings.hotkey);
        }
        let autostart = build_autostart();
        #[cfg(feature = "tray")]
        let tray = if want_tray {
            let autostart_on = autostart.as_ref().and_then(|a| a.is_enabled().ok());
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
        if self.last_config_check.elapsed() >= Duration::from_secs(1) {
            self.last_config_check = Instant::now();
            self.reload_if_changed();
        }
    }

    pub fn perform(&mut self, action: Action) {
        match action {
            Action::ToggleEnabled => self.change(|s| s.enabled = !s.enabled),
            Action::Intensity(v) => self.change(|s| s.intensity = v),
            Action::Blur(v) => self.change(|s| s.blur_ms = v),
            Action::Taper(v) => self.change(|s| s.taper = v),
            Action::Lag(v) => self.change(|s| s.lag_ms = v),
            Action::Style(v) => self.change(|s| s.style = v),
            Action::OpenConfig => {
                if !self.config_path.exists() {
                    self.save(&self.shared.settings());
                }
                open_file(&self.config_path);
            }
            Action::ToggleAutostart => {
                if let Some(a) = &self.autostart {
                    let on = a.is_enabled().unwrap_or(false);
                    let result = if on { a.disable() } else { a.enable() };
                    if let Err(e) = result {
                        warning!("could not change start-at-login: {e}");
                    }
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
        self.save(&s);
        self.sync_ui();
    }

    fn save(&mut self, s: &Settings) {
        if let Err(e) = config::save(&self.config_path, s) {
            warning!("could not save {}: {e}", self.config_path.display());
        }
        self.known_mtime = mtime(&self.config_path);
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
            let autostart_on = self.autostart.as_ref().and_then(|a| a.is_enabled().ok());
            tray.sync(&self.shared.settings(), autostart_on, self.hotkeys.label(), self.shared.auto_lag_ms());
        }
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn build_autostart() -> Option<auto_launch::AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.to_str()?;
    auto_launch::AutoLaunchBuilder::new()
        .set_app_name("mouse-blur")
        .set_app_path(path)
        .build()
        .map_err(|e| warning!("start-at-login unavailable: {e}"))
        .ok()
}

fn open_file(path: &Path) {
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

fn open_url(url: &str) {
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
