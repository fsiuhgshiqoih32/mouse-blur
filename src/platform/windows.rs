//! Windows: a per-pixel-alpha layered window (UpdateLayeredWindow) owned by the render thread,
//! paced by DwmFlush. The tray, hotkey and controller run on the main thread's message loop.

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmFlush;
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, GetDeviceCaps, GetMonitorInfoW,
    GetObjectW, HBITMAP, HDC, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, ReleaseDC,
    SelectObject, VREFRESH,
};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, CreateWindowExW, DI_NORMAL, DefWindowProcW, DestroyWindow, DispatchMessageW,
    DrawIconEx, GetClassNameW, GetCursorInfo, GetCursorPos, GetForegroundWindow, GetIconInfo, GetMessageW,
    GetSystemMetrics, GetWindowRect, GetWindowThreadProcessId, HCURSOR, HWND_TOPMOST, ICONINFO, MB_ICONERROR, MB_OK,
    MSG, MessageBoxW, PM_REMOVE, PeekMessageW, RegisterClassExW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetTimer, SetWindowPos, ShowWindow, TranslateMessage, ULW_ALPHA,
    UpdateLayeredWindow, WM_DISPLAYCHANGE, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use mouse_blur::composer::Frame;
use mouse_blur::cursor::CursorImage;
use mouse_blur::history::MotionHistory;

use crate::app::{self, Controller, PointerSource, Shared};
use crate::log::error;
use crate::renderer::{self, Foreground, Overlay};

pub fn init_process() {
    unsafe {
        // Physical pixels everywhere, so positions, cursor sizes and the overlay agree on every monitor.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // Release builds have no console; reattach to the launching terminal so --help is visible.
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

pub fn show_error(message: &str) {
    let text = wide(message);
    let title = wide("Mouse Blur");
    unsafe { MessageBoxW(null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

pub fn run(
    shared: Arc<Shared>,
    history: Arc<MotionHistory>,
    make_controller: impl FnOnce(bool) -> Controller,
) -> Result<(), String> {
    let source = if shared.self_test {
        app::simulated_pointer(|| unsafe {
            ((GetSystemMetrics(SM_CXSCREEN) / 2) as f32, (GetSystemMetrics(SM_CYSCREEN) / 2) as f32)
        })
    } else {
        pointer_source()
    };
    let sampler = app::spawn_sampler(source, shared.clone(), history.clone());
    let render = {
        let (shared, history) = (shared.clone(), history.clone());
        std::thread::Builder::new()
            .name("render".into())
            .spawn(move || match WinOverlay::new() {
                Ok(mut overlay) => renderer::run_loop(&mut overlay, &shared, &history),
                Err(e) => {
                    error!("could not create the overlay window: {e}");
                    shared.request_quit();
                }
            })
            .map_err(|e| e.to_string())?
    };

    let mut controller = make_controller(true);
    unsafe {
        // A thread timer drives the controller; tray, menu and hotkey windows are dispatched below.
        SetTimer(null_mut(), 0, 50, None);
        let mut msg: MSG = zeroed();
        while !shared.quitting() {
            let r = GetMessageW(&mut msg, null_mut(), 0, 0);
            if r == 0 || r == -1 {
                break;
            }
            if msg.message == WM_TIMER && msg.hwnd.is_null() {
                controller.poll();
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    shared.request_quit();
    history.wake();
    drop(controller);
    let _ = render.join();
    let _ = sampler.join();
    Ok(())
}

fn pointer_source() -> PointerSource {
    Box::new(|| {
        let mut p = POINT { x: 0, y: 0 };
        (unsafe { GetCursorPos(&mut p) } != 0).then_some((p.x as f32, p.y as f32))
    })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Set by the overlay's window procedure when cursor schemes or displays may have changed.
static SYSTEM_CHANGED: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_SETTINGCHANGE || msg == WM_DISPLAYCHANGE {
        SYSTEM_CHANGED.store(true, Ordering::Relaxed);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

struct WinOverlay {
    hwnd: HWND,
    screen_dc: HDC,
    mem_dc: HDC,
    dib: HBITMAP,
    stock: HGDIOBJ,
    bits: *mut u32,
    dib_w: usize,
    dib_h: usize,
    visible: bool,
    frames_shown: u32,
    cursors: HashMap<usize, Option<Arc<CursorImage>>>,
    last_refresh: Instant,
    refresh: f64,
}

impl WinOverlay {
    fn new() -> Result<Self, String> {
        unsafe {
            let instance = GetModuleHandleW(null());
            let class = wide("MouseBlurOverlay");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..zeroed()
            };
            RegisterClassExW(&wc);
            let title = wide("Mouse Blur overlay");
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                instance,
                null(),
            );
            if hwnd.is_null() {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let screen_dc = GetDC(null_mut());
            let mem_dc = CreateCompatibleDC(screen_dc);
            let mut overlay = WinOverlay {
                hwnd,
                screen_dc,
                mem_dc,
                dib: null_mut(),
                stock: null_mut(),
                bits: null_mut(),
                dib_w: 0,
                dib_h: 0,
                visible: false,
                frames_shown: 0,
                cursors: HashMap::new(),
                last_refresh: Instant::now(),
                refresh: 60.0,
            };
            overlay.refresh = overlay.query_refresh();
            Ok(overlay)
        }
    }

    fn query_refresh(&self) -> f64 {
        let hz = unsafe { GetDeviceCaps(self.screen_dc, VREFRESH as _) };
        if hz > 1 { hz as f64 } else { 60.0 } // 0 and 1 mean "hardware default"
    }

    fn ensure_dib(&mut self, w: usize, h: usize) -> bool {
        if w <= self.dib_w && h <= self.dib_h && !self.bits.is_null() {
            return true;
        }
        let (nw, nh) = (w.max(self.dib_w).next_multiple_of(64), h.max(self.dib_h).next_multiple_of(64));
        unsafe {
            if !self.dib.is_null() {
                SelectObject(self.mem_dc, self.stock);
                DeleteObject(self.dib);
                self.dib = null_mut();
                self.bits = null_mut();
            }
            let mut bits: *mut c_void = null_mut();
            let dib = CreateDIBSection(self.mem_dc, &bitmap_info(nw, nh), DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if dib.is_null() || bits.is_null() {
                self.dib_w = 0;
                self.dib_h = 0;
                return false;
            }
            self.stock = SelectObject(self.mem_dc, dib);
            self.dib = dib;
            self.bits = bits.cast();
            self.dib_w = nw;
            self.dib_h = nh;
        }
        true
    }
}

impl Overlay for WinOverlay {
    fn cursor(&mut self) -> Option<Arc<CursorImage>> {
        unsafe {
            let mut ci: CURSORINFO = zeroed();
            ci.cbSize = size_of::<CURSORINFO>() as u32;
            if GetCursorInfo(&mut ci) == 0 || ci.flags & CURSOR_SHOWING == 0 || ci.hCursor.is_null() {
                return None;
            }
            // Cursor schemes and sizes can change behind the same handles; re-read now and then.
            if SYSTEM_CHANGED.swap(false, Ordering::Relaxed) || self.last_refresh.elapsed() > Duration::from_secs(5) {
                self.last_refresh = Instant::now();
                self.cursors.clear();
                self.refresh = self.query_refresh();
            }
            if self.cursors.len() > 64 {
                self.cursors.clear();
            }
            self.cursors.entry(ci.hCursor as usize).or_insert_with(|| capture_cursor(ci.hCursor).map(Arc::new)).clone()
        }
    }

    fn present(&mut self, frame: &Frame<'_>) {
        let (w, h) = (frame.rect.width, frame.rect.height);
        if w == 0 || h == 0 || !self.ensure_dib(w, h) {
            return;
        }
        unsafe {
            for y in 0..h {
                std::ptr::copy_nonoverlapping(frame.pixels[y * w..].as_ptr(), self.bits.add(y * self.dib_w), w);
            }
            let dst = POINT { x: frame.rect.x, y: frame.rect.y };
            let size = SIZE { cx: w as i32, cy: h as i32 };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            UpdateLayeredWindow(self.hwnd, self.screen_dc, &dst, &size, self.mem_dc, &src, 0, &blend, ULW_ALPHA);
            self.frames_shown += 1;
            if !self.visible || self.frames_shown >= 120 {
                // Re-assert topmost now and then: the taskbar and other topmost windows can end up above us.
                SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
                self.visible = true;
                self.frames_shown = 0;
            }
        }
    }

    fn hide(&mut self) {
        if self.visible {
            unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            self.visible = false;
        }
    }

    fn refresh_rate(&mut self) -> f64 {
        self.refresh
    }

    fn pump(&mut self) {
        unsafe {
            let mut msg: MSG = zeroed();
            while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    fn foreground(&mut self) -> Foreground {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return Foreground::default();
            }
            let mut class = [0u16; 64];
            let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
            let class = String::from_utf16_lossy(&class[..len]);
            // The desktop and taskbar cover the screen too, but they aren't a fullscreen app.
            let shell = matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd");
            let mut fullscreen = false;
            let mut r: RECT = zeroed();
            if !shell && GetWindowRect(hwnd, &mut r) != 0 {
                let mut mi: MONITORINFO = zeroed();
                mi.cbSize = size_of::<MONITORINFO>() as u32;
                if GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi) != 0 {
                    let m = mi.rcMonitor;
                    fullscreen = r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom;
                }
            }
            let mut names = Vec::new();
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            let process = if pid != 0 { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) } else { null_mut() };
            if !process.is_null() {
                let mut buf = [0u16; 1024];
                let mut len = buf.len() as u32;
                if QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) != 0 {
                    let path = String::from_utf16_lossy(&buf[..len as usize]);
                    if let Some(name) = std::path::Path::new(&path).file_name() {
                        names.push(name.to_string_lossy().into_owned());
                    }
                }
                CloseHandle(process);
            }
            Foreground { names, fullscreen }
        }
    }

    fn wait_vsync(&mut self) -> bool {
        // DwmFlush returns once the compositor has presented a frame. If it returns at once
        // (composition idle or unavailable), let the caller fall back to sleeping.
        let start = Instant::now();
        let ok = unsafe { DwmFlush() } == 0;
        ok && start.elapsed() >= Duration::from_micros(500)
    }
}

impl Drop for WinOverlay {
    fn drop(&mut self) {
        unsafe {
            if !self.dib.is_null() {
                SelectObject(self.mem_dc, self.stock);
                DeleteObject(self.dib);
            }
            DeleteDC(self.mem_dc);
            ReleaseDC(null_mut(), self.screen_dc);
            DestroyWindow(self.hwnd);
        }
    }
}

fn bitmap_info(w: usize, h: usize) -> BITMAPINFO {
    let mut bi: BITMAPINFO = unsafe { zeroed() };
    bi.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w as i32,
        biHeight: -(h as i32), // top-down
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        ..unsafe { zeroed() }
    };
    bi
}

/// Reads a cursor by drawing it over black and over white, which recovers colour and alpha for
/// every cursor type (alpha, colour and monochrome).
fn capture_cursor(cursor: HCURSOR) -> Option<CursorImage> {
    unsafe {
        let mut ii: ICONINFO = zeroed();
        if GetIconInfo(cursor, &mut ii) == 0 {
            return None;
        }
        let mut bm: BITMAP = zeroed();
        let bm_size = size_of::<BITMAP>() as i32;
        let (w, h) = if !ii.hbmColor.is_null() && GetObjectW(ii.hbmColor, bm_size, (&raw mut bm).cast()) != 0 {
            (bm.bmWidth, bm.bmHeight)
        } else if !ii.hbmMask.is_null() && GetObjectW(ii.hbmMask, bm_size, (&raw mut bm).cast()) != 0 {
            (bm.bmWidth, bm.bmHeight / 2) // monochrome: AND mask stacked on XOR mask
        } else {
            (0, 0)
        };
        if !ii.hbmColor.is_null() {
            DeleteObject(ii.hbmColor);
        }
        if !ii.hbmMask.is_null() {
            DeleteObject(ii.hbmMask);
        }
        if !(1..=1024).contains(&w) || !(1..=1024).contains(&h) {
            return None;
        }
        let (w, h) = (w as usize, h as usize);
        let black = draw_cursor(cursor, w, h, 0xFF00_0000)?;
        let white = draw_cursor(cursor, w, h, 0xFFFF_FFFF)?;
        CursorImage::from_black_white(w, h, ii.xHotspot as i32, ii.yHotspot as i32, &black, &white)
    }
}

fn draw_cursor(cursor: HCURSOR, w: usize, h: usize, background: u32) -> Option<Vec<u32>> {
    unsafe {
        let dc = CreateCompatibleDC(null_mut());
        let mut bits: *mut c_void = null_mut();
        let bmp = CreateDIBSection(dc, &bitmap_info(w, h), DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if bmp.is_null() || bits.is_null() {
            DeleteDC(dc);
            return None;
        }
        let old = SelectObject(dc, bmp);
        let px = std::slice::from_raw_parts_mut(bits.cast::<u32>(), w * h);
        px.fill(background);
        DrawIconEx(dc, 0, 0, cursor, w as i32, h as i32, 0, null_mut(), DI_NORMAL);
        GdiFlush();
        let out = px.to_vec();
        SelectObject(dc, old);
        DeleteObject(bmp);
        DeleteDC(dc);
        Some(out)
    }
}
