//! Linux (X11): an override-redirect ARGB window with an empty input shape, moved and resized to
//! the trail every frame. Needs a compositing manager for transparency. The cursor image comes
//! from XFixes. Wayland doesn't let clients read the global pointer position or place overlays,
//! so there the app only works through XWayland, and only over X11 windows.

use std::sync::Arc;
use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xfixes::{ConnectionExt as _, CursorNotifyMask};
use x11rb::protocol::xproto::{
    AtomEnum, ClipOrdering, ColormapAlloc, ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux,
    EventMask, ImageFormat, ImageOrder, PropMode, StackMode, VisualClass, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use mouse_blur::composer::Frame;
use mouse_blur::cursor::CursorImage;
use mouse_blur::history::MotionHistory;

use crate::app::{self, Controller, PointerSource, Shared};
use crate::log::{error, warning};
use crate::renderer::{self, Overlay};

pub fn init_process() {}

pub fn show_error(message: &str) {
    eprintln!("mouse-blur: {message}");
}

pub fn run(
    shared: Arc<Shared>,
    history: Arc<MotionHistory>,
    make_controller: impl FnOnce(bool) -> Controller,
) -> Result<(), String> {
    if std::env::var_os("DISPLAY").is_none() {
        return Err("no X11 display. Mouse Blur needs an X11 session (or XWayland via $DISPLAY); \
                    Wayland doesn't let apps follow the pointer across other windows."
            .into());
    }
    if std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t.eq_ignore_ascii_case("wayland")) {
        warning!(
            "running under Wayland through XWayland: the trail only follows the pointer over X11 \
             windows. Log in to an X11 session for full support."
        );
    }

    let mut overlay = X11Overlay::new()?;
    let source = if shared.self_test {
        let (cx, cy) = (overlay.screen_width as f32 / 2.0, overlay.screen_height as f32 / 2.0);
        app::simulated_pointer(move || (cx, cy))
    } else {
        pointer_source()?
    };
    let sampler = app::spawn_sampler(source, shared.clone(), history.clone());
    let render = {
        let (shared, history) = (shared.clone(), history.clone());
        std::thread::Builder::new()
            .name("render".into())
            .spawn(move || renderer::run_loop(&mut overlay, &shared, &history))
            .map_err(|e| e.to_string())?
    };

    let finish = |shared: &Shared| {
        shared.request_quit();
        history.wake();
    };

    #[cfg(feature = "tray")]
    {
        if !shared.self_test && gtk::init().is_ok() {
            let mut controller = make_controller(true);
            let shared2 = shared.clone();
            gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
                controller.poll();
                if shared2.quitting() {
                    gtk::main_quit();
                    gtk::glib::ControlFlow::Break
                } else {
                    gtk::glib::ControlFlow::Continue
                }
            });
            gtk::main();
            finish(&shared);
            let _ = render.join();
            let _ = sampler.join();
            return Ok(());
        }
    }

    let mut controller = make_controller(false);
    while !shared.quitting() {
        controller.poll();
        std::thread::sleep(Duration::from_millis(50));
    }
    finish(&shared);
    let _ = render.join();
    let _ = sampler.join();
    Ok(())
}

fn pointer_source() -> Result<PointerSource, String> {
    let (conn, screen) = x11rb::connect(None).map_err(|e| format!("cannot connect to the X server: {e}"))?;
    let root = conn.setup().roots[screen].root;
    Ok(Box::new(move || {
        let reply = conn.query_pointer(root).ok()?.reply().ok()?;
        Some((reply.root_x as f32, reply.root_y as f32))
    }))
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

struct X11Overlay {
    conn: RustConnection,
    window: Window,
    gc: u32,
    screen_width: u16,
    screen_height: u16,
    mapped: bool,
    frames_since_raise: u32,
    lsb_first: bool,
    buf: Vec<u8>,
    cm_atom: u32,
    has_compositor: bool,
    warned_no_compositor: bool,
    last_compositor_check: Option<Instant>,
    cursor: Option<Arc<CursorImage>>,
    cursor_dirty: bool,
    refresh: f64,
}

impl X11Overlay {
    fn new() -> Result<Self, String> {
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| format!("cannot connect to the X server: {e}"))?;
        let screen = conn.setup().roots[screen_num].clone();
        let root = screen.root;
        let visual = screen
            .allowed_depths
            .iter()
            .filter(|d| d.depth == 32)
            .flat_map(|d| d.visuals.iter())
            .find(|v| v.class == VisualClass::TRUE_COLOR)
            .map(|v| v.visual_id)
            .ok_or("the X server has no 32-bit visual, so transparent windows aren't possible")?;

        let colormap = conn.generate_id().map_err(err)?;
        conn.create_colormap(ColormapAlloc::NONE, colormap, root, visual).map_err(err)?;
        let window = conn.generate_id().map_err(err)?;
        conn.create_window(
            32,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &CreateWindowAux::new()
                .background_pixel(0)
                .border_pixel(0)
                .colormap(colormap)
                .override_redirect(1)
                .event_mask(EventMask::NO_EVENT),
        )
        .map_err(err)?;

        // Click-through: an empty input region.
        if conn.extension_information(shape::X11_EXTENSION_NAME).map_err(err)?.is_some() {
            conn.shape_rectangles(shape::SO::SET, shape::SK::INPUT, ClipOrdering::UNSORTED, window, 0, 0, &[])
                .map_err(err)?;
        } else {
            warning!("the X server lacks the SHAPE extension; the trail may block clicks");
        }

        // Hints for compositors: a drag-and-drop style window (no shadows by default in picom),
        // never unredirected.
        let atom = |name: &str| -> Result<u32, String> {
            Ok(conn.intern_atom(false, name.as_bytes()).map_err(err)?.reply().map_err(err)?.atom)
        };
        let wm_type = atom("_NET_WM_WINDOW_TYPE")?;
        let wm_type_dnd = atom("_NET_WM_WINDOW_TYPE_DND")?;
        let bypass = atom("_NET_WM_BYPASS_COMPOSITOR")?;
        let cm_atom = atom(&format!("_NET_WM_CM_S{screen_num}"))?;
        conn.change_property32(PropMode::REPLACE, window, wm_type, AtomEnum::ATOM, &[wm_type_dnd]).map_err(err)?;
        conn.change_property32(PropMode::REPLACE, window, bypass, AtomEnum::CARDINAL, &[2]).map_err(err)?;
        conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME, AtomEnum::STRING, b"mouse-blur overlay")
            .map_err(err)?;
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"mouse-blur\0mouse-blur\0",
        )
        .map_err(err)?;

        let gc = conn.generate_id().map_err(err)?;
        conn.create_gc(gc, window, &CreateGCAux::new()).map_err(err)?;

        // Cursor images and change notifications.
        conn.xfixes_query_version(5, 0)
            .map_err(err)?
            .reply()
            .map_err(|e| format!("the X server lacks XFixes, needed to read the cursor: {e}"))?;
        conn.xfixes_select_cursor_input(root, CursorNotifyMask::DISPLAY_CURSOR).map_err(err)?;

        let refresh = refresh_rate(&conn, root).unwrap_or(60.0);
        let lsb_first = conn.setup().image_byte_order == ImageOrder::LSB_FIRST;
        conn.flush().map_err(err)?;
        Ok(X11Overlay {
            conn,
            window,
            gc,
            screen_width: screen.width_in_pixels,
            screen_height: screen.height_in_pixels,
            mapped: false,
            frames_since_raise: 0,
            lsb_first,
            buf: Vec::new(),
            cm_atom,
            has_compositor: true,
            warned_no_compositor: false,
            last_compositor_check: None,
            cursor: None,
            cursor_dirty: true,
            refresh,
        })
    }

    /// Without a compositing manager an ARGB window shows as a black box, so don't draw then.
    fn check_compositor(&mut self) {
        if self.last_compositor_check.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            return;
        }
        self.last_compositor_check = Some(Instant::now());
        let owner = self.conn.get_selection_owner(self.cm_atom).ok().and_then(|c| c.reply().ok()).map(|r| r.owner);
        self.has_compositor = owner.is_some_and(|o| o != x11rb::NONE);
        if !self.has_compositor && !self.warned_no_compositor {
            self.warned_no_compositor = true;
            error!(
                "no compositing manager is running, so the trail can't be drawn transparently. \
                 Start one (for example picom) or enable compositing in your desktop settings."
            );
        }
        if self.has_compositor {
            self.warned_no_compositor = false;
        }
    }
}

/// Highest refresh rate among active outputs, from RandR.
fn refresh_rate(conn: &RustConnection, root: Window) -> Option<f64> {
    conn.randr_query_version(1, 3).ok()?.reply().ok()?;
    let res = conn.randr_get_screen_resources_current(root).ok()?.reply().ok()?;
    let mut best = 0.0f64;
    for &crtc in &res.crtcs {
        let Some(info) = conn.randr_get_crtc_info(crtc, res.config_timestamp).ok().and_then(|c| c.reply().ok()) else {
            continue;
        };
        if info.mode == 0 {
            continue;
        }
        if let Some(m) = res.modes.iter().find(|m| m.id == info.mode) {
            if m.htotal > 0 && m.vtotal > 0 {
                best = best.max(m.dot_clock as f64 / (m.htotal as f64 * m.vtotal as f64));
            }
        }
    }
    (best > 1.0).then_some(best)
}

impl Overlay for X11Overlay {
    fn cursor(&mut self) -> Option<Arc<CursorImage>> {
        if self.cursor_dirty {
            self.cursor_dirty = false;
            // XFixes cursor pixels are already premultiplied 32-bit ARGB.
            self.cursor = self
                .conn
                .xfixes_get_cursor_image()
                .ok()
                .and_then(|c| c.reply().ok())
                .and_then(|r| {
                    CursorImage::from_premultiplied(
                        r.width as usize,
                        r.height as usize,
                        r.xhot as i32,
                        r.yhot as i32,
                        r.cursor_image,
                    )
                })
                .map(Arc::new);
        }
        self.cursor.clone()
    }

    fn present(&mut self, frame: &Frame<'_>) {
        self.check_compositor();
        if !self.has_compositor {
            self.hide();
            return;
        }
        let (w, h) = (frame.rect.width, frame.rect.height);
        if w == 0 || h == 0 || w > u16::MAX as usize || h > u16::MAX as usize {
            return;
        }
        let mut aux = ConfigureWindowAux::new().x(frame.rect.x).y(frame.rect.y).width(w as u32).height(h as u32);
        self.frames_since_raise += 1;
        if !self.mapped || self.frames_since_raise >= 120 {
            aux = aux.stack_mode(StackMode::ABOVE);
            self.frames_since_raise = 0;
        }
        let _ = self.conn.configure_window(self.window, &aux);
        if !self.mapped {
            let _ = self.conn.map_window(self.window);
            self.mapped = true;
        }

        // Send the pixels in strips that fit the server's maximum request size.
        let row_bytes = w * 4;
        let max_rows = ((self.conn.maximum_request_bytes().saturating_sub(64)) / row_bytes).max(1);
        let mut y0 = 0;
        while y0 < h {
            let rows = max_rows.min(h - y0);
            self.buf.clear();
            for &p in &frame.pixels[y0 * w..(y0 + rows) * w] {
                self.buf.extend_from_slice(&if self.lsb_first { p.to_le_bytes() } else { p.to_be_bytes() });
            }
            let _ = self.conn.put_image(
                ImageFormat::Z_PIXMAP,
                self.window,
                self.gc,
                w as u16,
                rows as u16,
                0,
                y0 as i16,
                0,
                32,
                &self.buf,
            );
            y0 += rows;
        }
        let _ = self.conn.flush();
    }

    fn hide(&mut self) {
        if self.mapped {
            let _ = self.conn.unmap_window(self.window);
            let _ = self.conn.flush();
            self.mapped = false;
        }
    }

    fn refresh_rate(&mut self) -> f64 {
        self.refresh
    }

    fn pump(&mut self) {
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            if let Event::XfixesCursorNotify(_) = event {
                self.cursor_dirty = true;
            }
        }
    }
}

impl Drop for X11Overlay {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}
