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
    /// Blocks until the next vertical blank if the platform can; returns false if it can't.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn wait_vsync(&mut self) -> bool {
        false
    }
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
}

impl Renderer {
    pub fn new(shared: &Shared) -> Self {
        Renderer {
            composer: Composer::new(),
            samples: Vec::with_capacity(1024),
            settings: shared.settings(),
            version: shared.version(),
            started: Instant::now(),
            fallback_cursor: shared.self_test.then(|| Arc::new(CursorImage::arrow(1.0))),
        }
    }

    pub fn tick(&mut self, overlay: &mut dyn Overlay, shared: &Shared, history: &MotionHistory) -> Tick {
        if self.version != shared.version() {
            self.version = shared.version();
            self.settings = shared.settings();
        }
        if !self.settings.enabled {
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
