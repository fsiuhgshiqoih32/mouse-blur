//! macOS: one transparent, click-through window per screen, each holding a Core Animation layer
//! that shows the trail. AppKit wants the main thread, so the renderer runs there from an NSTimer;
//! the pointer position comes from CGEvent on the sampler thread.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSCursor, NSGraphicsContext, NSScreen,
    NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::{CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextGetData, CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider,
    CGDisplayBounds, CGEvent, CGGetActiveDisplayList, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
    CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID,
    kCGWindowBounds, kCGWindowLayer, kCGWindowOwnerPID,
};
use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer};
use objc2_quartz_core::{CALayer, CATransaction};

use mouse_blur::composer::Frame;
use mouse_blur::cursor::CursorImage;
use mouse_blur::history::MotionHistory;

use crate::app::{self, Action, Controller, PointerSource, Shared};
use crate::renderer::{Foreground, Overlay, Renderer};

/// kCGOverlayWindowLevel: above menus and the Dock, below the screen saver.
const OVERLAY_LEVEL: isize = 102;

pub fn init_process() {}

pub fn show_error(message: &str) {
    eprintln!("mouse-blur: {message}");
}

struct MainState {
    overlay: MacOverlay,
    renderer: Renderer,
    controller: Controller,
    last_poll: Instant,
    was_active: bool,
}

pub fn run(
    shared: Arc<Shared>,
    history: Arc<MotionHistory>,
    make_controller: impl FnOnce(bool) -> Controller,
) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("Mouse Blur has to be started on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    // A menu bar utility: no Dock icon, never steals focus.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let scale = Arc::new(AtomicU32::new(1.0f32.to_bits()));
    let overlay = MacOverlay::new(mtm, scale.clone())?;
    let source = if shared.self_test {
        let (cx, cy) = overlay.main_center_points();
        let scale = scale.clone();
        app::simulated_pointer(move || {
            let s = f32::from_bits(scale.load(Ordering::Relaxed));
            (cx * s, cy * s)
        })
    } else {
        pointer_source(scale)
    };
    let _sampler = app::spawn_sampler(source, shared.clone(), history.clone());

    let controller = make_controller(true);
    let interval = 1.0 / overlay.refresh.clamp(30.0, 240.0);
    let state = RefCell::new(MainState {
        renderer: Renderer::new(&shared),
        overlay,
        controller,
        last_poll: Instant::now(),
        was_active: false,
    });
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        let quit = {
            let Ok(mut st) = state.try_borrow_mut() else { return };
            let st = &mut *st;
            st.overlay.check_screens();
            st.renderer.tick(&mut st.overlay, &shared, &history);
            if st.last_poll.elapsed() >= Duration::from_millis(50) {
                st.last_poll = Instant::now();
                st.controller.poll();
                // Opening the app again while only this background copy runs makes macOS activate
                // us instead of starting a new process; answer that by showing the settings window.
                let active =
                    MainThreadMarker::new().is_some_and(|mtm| NSApplication::sharedApplication(mtm).isActive());
                if active && !st.was_active && !shared.self_test {
                    st.controller.perform(Action::OpenSettings);
                }
                st.was_active = active;
            }
            if shared.quitting() {
                st.overlay.hide();
            }
            shared.quitting()
        };
        if quit && let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).terminate(None);
        }
    });
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(interval, true, &block) };
    // Keep ticking while a menu is open; menus run the loop in event-tracking mode.
    unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    app.run();
    Ok(())
}

/// Pointer position in overlay pixels: CGEvent points (origin top-left of the main display)
/// times the backing scale the overlay renders at.
fn pointer_source(scale: Arc<AtomicU32>) -> PointerSource {
    Box::new(move || {
        let event = CGEvent::new(None)?;
        let p = CGEvent::location(Some(&event));
        let s = f32::from_bits(scale.load(Ordering::Relaxed));
        Some((p.x as f32 * s, p.y as f32 * s))
    })
}

fn bitmap_info() -> u32 {
    CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0
}

struct ScreenWindow {
    window: Retained<NSWindow>,
    layer: Retained<CALayer>,
    frame: CGRect,
}

struct MacOverlay {
    mtm: MainThreadMarker,
    screens: Vec<ScreenWindow>,
    signature: Vec<[f64; 5]>,
    last_screen_check: Instant,
    /// Height of the main screen, for flipping between CoreGraphics (top-left) and Cocoa
    /// (bottom-left) coordinates.
    main_height: f64,
    scale: f64,
    shared_scale: Arc<AtomicU32>,
    refresh: f64,
    visible: bool,
    space: CFRetained<CGColorSpace>,
    cursor_key: usize,
    cursor: Option<Arc<CursorImage>>,
    cursor_checked: Option<Instant>,
}

impl MacOverlay {
    fn new(mtm: MainThreadMarker, shared_scale: Arc<AtomicU32>) -> Result<Self, String> {
        let space = CGColorSpace::new_device_rgb().ok_or("no RGB colour space")?;
        let mut overlay = MacOverlay {
            mtm,
            screens: Vec::new(),
            signature: Vec::new(),
            last_screen_check: Instant::now(),
            main_height: 0.0,
            scale: 1.0,
            shared_scale,
            refresh: 60.0,
            visible: false,
            space,
            cursor_key: 0,
            cursor: None,
            cursor_checked: None,
        };
        overlay.rebuild();
        if overlay.screens.is_empty() {
            return Err("no screens found".into());
        }
        Ok(overlay)
    }

    fn main_center_points(&self) -> (f32, f32) {
        let f = self.screens.first().map(|s| s.frame.size).unwrap_or(CGSize::new(1440.0, 900.0));
        ((f.width / 2.0) as f32, (f.height / 2.0) as f32)
    }

    fn current_signature(&self) -> Vec<[f64; 5]> {
        NSScreen::screens(self.mtm)
            .iter()
            .map(|s| {
                let f = s.frame();
                [f.origin.x, f.origin.y, f.size.width, f.size.height, s.backingScaleFactor()]
            })
            .collect()
    }

    /// Rebuilds the overlay windows when displays are added, removed, moved or rescaled.
    fn check_screens(&mut self) {
        if self.last_screen_check.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_screen_check = Instant::now();
        if self.current_signature() != self.signature {
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        for sw in self.screens.drain(..) {
            sw.window.close();
        }
        self.visible = false;
        self.cursor = None;
        self.signature = self.current_signature();
        let screens = NSScreen::screens(self.mtm);
        self.main_height = screens.firstObject().map(|s| s.frame().size.height).unwrap_or(0.0);
        self.scale = screens.iter().map(|s| s.backingScaleFactor()).fold(1.0, f64::max);
        self.shared_scale.store((self.scale as f32).to_bits(), Ordering::Relaxed);
        self.refresh = screens
            .iter()
            .filter(|s| s.respondsToSelector(sel!(maximumFramesPerSecond)))
            .map(|s| s.maximumFramesPerSecond() as f64)
            .fold(60.0, f64::max);

        for screen in screens.iter() {
            let frame = screen.frame();
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    self.mtm.alloc(),
                    frame,
                    NSWindowStyleMask::Borderless,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            unsafe { window.setReleasedWhenClosed(false) };
            window.setOpaque(false);
            window.setBackgroundColor(Some(&NSColor::clearColor()));
            window.setIgnoresMouseEvents(true);
            window.setHasShadow(false);
            window.setLevel(OVERLAY_LEVEL);
            window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::IgnoresCycle
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
            let Some(view) = window.contentView() else { continue };
            view.setWantsLayer(true);
            let Some(root) = view.layer() else { continue };
            let layer = CALayer::new();
            layer.setHidden(true);
            root.addSublayer(&layer);
            window.orderFrontRegardless();
            self.screens.push(ScreenWindow { window, layer, frame });
        }
    }

    fn rasterize(&self, cursor: &NSCursor) -> Option<CursorImage> {
        let image = cursor.image();
        let size = image.size();
        let hot = cursor.hotSpot();
        let s = self.scale;
        let (pw, ph) = ((size.width * s).ceil() as usize, (size.height * s).ceil() as usize);
        if !(1..=1024).contains(&pw) || !(1..=1024).contains(&ph) {
            return None;
        }
        let ctx = unsafe {
            CGBitmapContextCreate(std::ptr::null_mut(), pw, ph, 8, pw * 4, Some(&self.space), bitmap_info())
        }?;
        let gc = NSGraphicsContext::graphicsContextWithCGContext_flipped(&ctx, false);
        let previous = NSGraphicsContext::currentContext();
        NSGraphicsContext::setCurrentContext(Some(&gc));
        image.drawInRect(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(pw as f64, ph as f64)));
        NSGraphicsContext::setCurrentContext(previous.as_deref());
        let data = CGBitmapContextGetData(Some(&ctx)) as *const u32;
        if data.is_null() {
            return None;
        }
        // Premultiplied, little-endian 32-bit ARGB: exactly the compositor's format.
        let pixels = unsafe { std::slice::from_raw_parts(data, pw * ph) }.to_vec();
        CursorImage::from_premultiplied(pw, ph, (hot.x * s).round() as i32, (hot.y * s).round() as i32, pixels)
    }
}

impl Overlay for MacOverlay {
    fn cursor(&mut self) -> Option<Arc<CursorImage>> {
        // Cursor shape changes are rare; looking at most every 50 ms keeps this cheap.
        if self.cursor.is_some() && self.cursor_checked.is_some_and(|t| t.elapsed() < Duration::from_millis(50)) {
            return self.cursor.clone();
        }
        self.cursor_checked = Some(Instant::now());
        // Deprecated in favour of ScreenCaptureKit, but still the only public way to read the
        // cursor another app has set; if it ever returns nothing, the arrow is used instead.
        #[allow(deprecated)]
        let cursor = NSCursor::currentSystemCursor().unwrap_or_else(NSCursor::arrowCursor);
        let key = Retained::as_ptr(&cursor.image()) as usize;
        if key != self.cursor_key || self.cursor.is_none() {
            self.cursor_key = key;
            self.cursor = self.rasterize(&cursor).or_else(|| self.rasterize(&NSCursor::arrowCursor())).map(Arc::new);
        }
        self.cursor.clone()
    }

    fn present(&mut self, frame: &Frame<'_>) {
        let (w, h) = (frame.rect.width, frame.rect.height);
        if w == 0 || h == 0 {
            return;
        }
        let bytes = unsafe { std::slice::from_raw_parts(frame.pixels.as_ptr().cast::<u8>(), w * h * 4) };
        let data = CFData::from_bytes(bytes);
        let Some(provider) = CGDataProvider::with_cf_data(Some(&data)) else { return };
        let image = unsafe {
            CGImage::new(
                w,
                h,
                8,
                32,
                w * 4,
                Some(&self.space),
                CGBitmapInfo::from_bits_retain(bitmap_info()),
                Some(&provider),
                std::ptr::null(),
                false,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
        };
        let Some(image) = image else { return };
        let cf: &CFType = &image;
        let contents: &AnyObject = cf.as_ref();

        let s = self.scale;
        let (x, y, pw, ph) = (frame.rect.x as f64 / s, frame.rect.y as f64 / s, w as f64 / s, h as f64 / s);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for sw in &self.screens {
            let f = sw.frame;
            let lx = x - f.origin.x;
            let ly = (self.main_height - y - ph) - f.origin.y;
            if lx < f.size.width && ly < f.size.height && lx + pw > 0.0 && ly + ph > 0.0 {
                sw.layer.setFrame(CGRect::new(CGPoint::new(lx, ly), CGSize::new(pw, ph)));
                sw.layer.setContentsScale(s);
                unsafe { sw.layer.setContents(Some(contents)) };
                sw.layer.setHidden(false);
            } else {
                sw.layer.setHidden(true);
            }
        }
        CATransaction::commit();
        self.visible = true;
    }

    fn hide(&mut self) {
        if !self.visible {
            return;
        }
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for sw in &self.screens {
            sw.layer.setHidden(true);
            unsafe { sw.layer.setContents(None) };
        }
        CATransaction::commit();
        self.visible = false;
    }

    fn refresh_rate(&mut self) -> f64 {
        self.refresh
    }

    fn foreground(&mut self) -> Foreground {
        let Some(app) = NSWorkspace::sharedWorkspace().frontmostApplication() else {
            return Foreground::default();
        };
        let mut names = Vec::new();
        if let Some(name) = app.localizedName() {
            names.push(name.to_string());
        }
        if let Some(name) = app.executableURL().and_then(|url| url.lastPathComponent()) {
            names.push(name.to_string());
        }
        Foreground { names, fullscreen: front_window_fills_a_display(app.processIdentifier()) }
    }
}

/// True if the front-most normal window of process `pid` covers a whole display. Window bounds
/// and owners are readable without Screen Recording permission.
fn front_window_fills_a_display(pid: i32) -> bool {
    let options = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let Some(windows) = CGWindowListCopyWindowInfo(options, kCGNullWindowID) else { return false };
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    let displays: Vec<CGRect> = ids[..count.min(16) as usize].iter().map(|&id| CGDisplayBounds(id)).collect();
    let number = |dict: &CFDictionary, key: &CFString| -> Option<i32> {
        let value = unsafe { dict.value((key as *const CFString).cast()) };
        (!value.is_null()).then(|| unsafe { &*(value as *const CFNumber) }.as_i32()).flatten()
    };
    // The list runs front to back, so the first normal-layer window of the app is its front one.
    for i in 0..windows.count() {
        let dict = unsafe { windows.value_at_index(i) } as *const CFDictionary;
        if dict.is_null() {
            continue;
        }
        let dict = unsafe { &*dict };
        if number(dict, unsafe { kCGWindowOwnerPID }) != Some(pid) || number(dict, unsafe { kCGWindowLayer }) != Some(0)
        {
            continue;
        }
        let bounds = unsafe { dict.value((kCGWindowBounds as *const CFString).cast()) } as *const CFDictionary;
        let mut r = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0));
        if bounds.is_null() || !unsafe { CGRectMakeWithDictionaryRepresentation(Some(&*bounds), &mut r) } {
            return false;
        }
        return displays.iter().any(|d| {
            r.origin.x <= d.origin.x
                && r.origin.y <= d.origin.y
                && r.origin.x + r.size.width >= d.origin.x + d.size.width
                && r.origin.y + r.size.height >= d.origin.y + d.size.height
        });
    }
    false
}
