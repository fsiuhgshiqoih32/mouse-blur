//! The per-frame loop shared by every platform: read recent pointer motion, compose the trail and
//! hand it to the native overlay.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mouse_blur::composer::{Composer, Frame, TrailParams};
use mouse_blur::config::{Lag, Settings};
use mouse_blur::cursor::CursorImage;
use mouse_blur::history::{MotionHistory, MotionSample, now_ns};

use crate::app::Shared;

/// A native, click-through, always-on-top surface that shows the trail.
pub trait Overlay {
    /// The current cursor image, or `None` while the pointer is hidden.
    fn cursor(&mut self) -> Option<Arc<CursorImage>>;
    /// Shows `frame` at its screen rectangle (in overlay pixels).
    fn present(&mut self, frame: &Frame<'_>);
    /// Hides the trail. Called often, so it must be cheap when already hidden.
    fn hide(&mut self);
    /// Refresh rate of the display, in Hz.
    fn refresh_rate(&mut self) -> f64;
    /// Processes pending window-system events. Threaded overlays call this every frame.
    #[cfg_attr(target_os = "macos", allow(dead_code))] // macOS renders from the main run loop
    fn pump(&mut self) {}
    /// The app in front and whether it covers its whole monitor, for the pause rules.
    fn foreground(&mut self) -> Foreground {
        Foreground::default()
    }
    /// Blocks until the next vertical blank if the platform can; returns false if it can't.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn wait_vsync(&mut self) -> bool {
        false
    }
}

/// What's in front: the focused app's names (program file and display name) and whether its window
/// covers the whole monitor.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Foreground {
    pub names: Vec<String>,
    pub fullscreen: bool,
}

/// True if the pause rules in `s` say the effect should step aside for `fg`.
pub fn should_pause(s: &Settings, fg: &Foreground) -> bool {
    if s.pause_fullscreen && fg.fullscreen {
        return true;
    }
    let norm = |n: &str| {
        let n = n.trim().to_lowercase();
        n.strip_suffix(".exe").or_else(|| n.strip_suffix(".app")).unwrap_or(&n).to_string()
    };
    fg.names.iter().any(|name| s.excluded_apps.iter().any(|ex| norm(ex) == norm(name)))
}

pub enum Tick {
    /// A frame was drawn; pace to the display.
    Drawn,
    /// The pointer moved but nothing needed drawing; poll again next frame.
    Moving,
    /// Nothing is happening; wait for motion.
    Idle,
}

pub struct Renderer {
    composer: Composer,
    samples: Vec<MotionSample>,
    settings: Settings,
    version: u64,
    started: Instant,
    /// Used when the overlay can't read a cursor; only set for `--self-test`, where the real
    /// pointer may well be hidden or idle.
    fallback_cursor: Option<Arc<CursorImage>>,
    last_foreground_check: Option<Instant>,
    paused: bool,
    recent_apps: Vec<String>,
}

/// Apps recently in front, newest first, written for the settings window to offer as exclusions.
pub const RECENT_APPS_FILE: &str = "recent-apps.txt";

impl Renderer {
    pub fn new(shared: &Shared) -> Self {
        Renderer {
            composer: Composer::new(),
            samples: Vec::with_capacity(1024),
            settings: shared.settings(),
            version: shared.version(),
            started: Instant::now(),
            fallback_cursor: shared.self_test.then(|| Arc::new(CursorImage::arrow(1.0))),
            last_foreground_check: None,
            paused: false,
            recent_apps: Vec::new(),
        }
    }

    pub fn tick(&mut self, overlay: &mut dyn Overlay, shared: &Shared, history: &MotionHistory) -> Tick {
        if self.version != shared.version() {
            self.version = shared.version();
            self.settings = shared.settings();
            self.last_foreground_check = None;
        }
        if !self.settings.enabled {
            overlay.hide();
            return Tick::Idle;
        }
        if !shared.self_test && self.last_foreground_check.is_none_or(|t| t.elapsed() >= Duration::from_millis(400)) {
            self.last_foreground_check = Some(Instant::now());
            let fg = overlay.foreground();
            self.paused = should_pause(&self.settings, &fg);
            self.remember(&fg);
        }
        if self.paused {
            overlay.hide();
            return Tick::Idle;
        }
        let now = now_ns();
        let window_ns = self.settings.blur_ms as i64 * 1_000_000;
        history.copy_since(now - window_ns - 30_000_000, &mut self.samples);
        let moving = self.samples.len() >= 2
            && self.samples.iter().any(|s| s.x != self.samples[0].x || s.y != self.samples[0].y);
        if !moving {
            overlay.hide();
            return Tick::Idle;
        }
        let Some(cursor) = overlay.cursor().or_else(|| self.fallback_cursor.clone()) else {
            overlay.hide();
            return Tick::Moving;
        };
        let refresh = overlay.refresh_rate().clamp(24.0, 1000.0);
        let auto_lag = 1.5 / refresh;
        shared.set_auto_lag_ms((auto_lag * 1000.0).round() as u32);
        let s = &self.settings;
        let params = TrailParams {
            gain: s.intensity as f32 / 100.0,
            window: s.blur_ms as f32 / 1000.0,
            lead: match s.lag_ms {
                Lag::Auto => auto_lag as f32,
                Lag::Ms(v) => v as f32 / 1000.0,
            },
            taper: s.taper as f32 / 100.0,
            min_speed: s.min_speed as f32,
            tint: s.tint((self.started.elapsed().as_secs_f32() * 0.25).fract()),
            glow: s.glow as f32,
        };
        match self.composer.compose(&self.samples, now, &cursor, &params) {
            Some(_) => {
                if let Some(frame) = self.composer.frame() {
                    overlay.present(&frame);
                    shared.count_frame();
                }
                self.composer.release();
                Tick::Drawn
            }
            None => {
                overlay.hide();
                Tick::Moving
            }
        }
    }
}

impl Renderer {
    fn remember(&mut self, fg: &Foreground) {
        let Some(name) = fg.names.first() else { return };
        let lower = name.to_lowercase();
        if lower.starts_with("mouse-blur") || lower.starts_with("mouse blur") || self.recent_apps.first() == Some(name)
        {
            return;
        }
        self.recent_apps.retain(|n| n != name);
        self.recent_apps.insert(0, name.clone());
        self.recent_apps.truncate(8);
        let path = mouse_blur::config::config_dir().join(RECENT_APPS_FILE);
        let _ = std::fs::write(path, self.recent_apps.join("\n"));
    }
}

/// Runs the renderer on the current thread until `shared` asks to quit. Used by platforms whose
/// overlay can live on a worker thread (Windows, X11).
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn run_loop(overlay: &mut dyn Overlay, shared: &Shared, history: &MotionHistory) {
    let mut renderer = Renderer::new(shared);
    while !shared.quitting() {
        overlay.pump();
        let frame_time = Duration::from_secs_f64(1.0 / overlay.refresh_rate().clamp(24.0, 1000.0));
        match renderer.tick(overlay, shared, history) {
            Tick::Drawn => {
                if !overlay.wait_vsync() {
                    std::thread::sleep(frame_time);
                }
            }
            Tick::Moving => std::thread::sleep(frame_time),
            Tick::Idle => history.wait_for_motion(Duration::from_millis(100)),
        }
    }
    overlay.hide();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_rules() {
        let fg = |names: &[&str], fullscreen| Foreground {
            names: names.iter().map(|n| n.to_string()).collect(),
            fullscreen,
        };
        let s = Settings { excluded_apps: vec!["cs2".into(), "Photoshop.app".into()], ..Settings::default() };
        assert!(should_pause(&s, &fg(&["explorer.exe"], true)), "fullscreen pauses by default");
        assert!(!should_pause(&s, &fg(&["explorer.exe"], false)));
        assert!(should_pause(&s, &fg(&["CS2.exe"], false)), "names match without .exe and case");
        assert!(should_pause(&s, &fg(&["Adobe Photoshop 2026", "Photoshop"], false)));
        let off = Settings { pause_fullscreen: false, ..Settings::default() };
        assert!(!should_pause(&off, &fg(&["game.exe"], true)));
    }
}
