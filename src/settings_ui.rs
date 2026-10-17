//! The settings window (egui). It runs as its own process and edits the config file; the
//! background process reloads the file within a quarter second, so every change shows up live.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use global_hotkey::hotkey::HotKey;

use mouse_blur::art;
use mouse_blur::composer::{Composer, TrailParams};
use mouse_blur::config::{self, Lag, Settings, Style, parse_color};
use mouse_blur::cursor::CursorImage;
use mouse_blur::history::MotionSample;

use crate::log::warning;
use crate::{app, autostart, single_instance, updater};

/// Touched by a second launch to bring an open settings window to the front.
const RAISE_FILE: &str = "raise-settings";

pub fn request_raise() {
    let _ = std::fs::write(config::config_dir().join(RAISE_FILE), b"");
}

pub fn run(config_path: PathBuf) -> Result<(), String> {
    let icon = egui::IconData { rgba: art::app_icon(64, true, false), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Mouse Blur")
            .with_app_id("mouse-blur")
            .with_inner_size([460.0, 880.0])
            .with_min_inner_size([400.0, 480.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native("Mouse Blur", options, Box::new(move |cc| Ok(Box::new(SettingsApp::new(cc, config_path)))))
        .map_err(|e| e.to_string())
}

struct SettingsApp {
    path: PathBuf,
    /// What the controls show.
    settings: Settings,
    /// What the config file holds, as far as this window knows.
    written: Settings,
    file_mtime: Option<SystemTime>,
    edited_at: Option<Instant>,
    last_file_check: Instant,
    raise_mtime: Option<SystemTime>,
    running: bool,
    last_running_check: Option<Instant>,
    hotkey_text: String,
    hotkey_valid: bool,
    color: [u8; 3],
    /// Remembered so turning "Automatic" off and on again keeps the manual value.
    manual_lag: u32,
    autostart: Option<auto_launch::AutoLaunch>,
    autostart_on: bool,
    preview: Preview,
    update_check: Option<std::sync::mpsc::Receiver<Result<Option<updater::Update>, String>>>,
    update_status: String,
    update_available: Option<String>,
}

impl SettingsApp {
    fn new(cc: &eframe::CreationContext<'_>, path: PathBuf) -> Self {
        cc.egui_ctx.style_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 7.0);
            style.spacing.slider_width = 210.0;
        });
        let settings = config::load(&path).unwrap_or_default();
        let autostart = autostart::launcher();
        let autostart_on = autostart.as_ref().is_some_and(autostart::is_enabled);
        let mut app = SettingsApp {
            file_mtime: app::mtime(&path),
            raise_mtime: app::mtime(&config::config_dir().join(RAISE_FILE)),
            path,
            settings: Settings::default(),
            written: Settings::default(),
            edited_at: None,
            last_file_check: Instant::now(),
            running: false,
            last_running_check: None,
            hotkey_text: String::new(),
            hotkey_valid: true,
            color: [0x38, 0xB6, 0xFF],
            manual_lag: 10,
            autostart,
            autostart_on,
            preview: Preview::new(),
            update_check: None,
            update_status: String::new(),
            update_available: None,
        };
        app.show_loaded(settings);
        app
    }

    fn show_loaded(&mut self, s: Settings) {
        self.hotkey_text = s.hotkey.clone();
        self.hotkey_valid = true;
        if let Some(c) = parse_color(&s.color) {
            self.color = c;
        }
        if let Lag::Ms(v) = s.lag_ms {
            self.manual_lag = v;
        }
        self.settings = s.clone();
        self.written = s;
    }

    /// Picks up edits made elsewhere (tray, hotkey, a text editor), raise requests from a second
    /// launch, and whether the background effect is running.
    fn poll(&mut self, ctx: &egui::Context) {
        if self.last_file_check.elapsed() >= Duration::from_millis(250) {
            self.last_file_check = Instant::now();
            let m = app::mtime(&self.path);
            if m != self.file_mtime && self.settings == self.written {
                self.file_mtime = m;
                if let Ok(s) = config::load(&self.path) {
                    self.show_loaded(s);
                }
            }
            let r = app::mtime(&config::config_dir().join(RAISE_FILE));
            if r != self.raise_mtime {
                self.raise_mtime = r;
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
        if let Some(rx) = &self.update_check
            && let Ok(result) = rx.try_recv()
        {
            self.update_check = None;
            match result {
                Ok(None) => self.update_status = "Up to date.".into(),
                Ok(Some(update)) => {
                    self.update_status = format!("Version {} is available.", update.version);
                    self.update_available = Some(update.version);
                }
                Err(e) => self.update_status = format!("Couldn't check: {e}"),
            }
        }
        if self.last_running_check.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            self.last_running_check = Some(Instant::now());
            self.running = single_instance::is_held(single_instance::BACKGROUND);
        }
    }

    /// Writes changes to the config file, at most every 120 ms while a slider is dragged.
    fn save_if_edited(&mut self) {
        if self.settings == self.written {
            self.edited_at = None;
            return;
        }
        let since = *self.edited_at.get_or_insert_with(Instant::now);
        if since.elapsed() < Duration::from_millis(120) {
            return;
        }
        let s = self.settings.clone().sanitized();
        if let Err(e) = config::save(&self.path, &s) {
            warning!("could not save {}: {e}", self.path.display());
        }
        self.file_mtime = app::mtime(&self.path);
        self.written = s;
        self.edited_at = None;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Mouse Blur");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.running {
                    ui.label(egui::RichText::new("Running").color(ui.visuals().widgets.active.fg_stroke.color));
                } else {
                    if ui.button("Start").clicked() {
                        app::start_background(&self.path);
                        self.last_running_check = Some(Instant::now());
                    }
                    ui.label(egui::RichText::new("Not running").weak());
                }
            });
        });
        ui.label(egui::RichText::new("Real motion blur for your mouse pointer.").weak());
        ui.add_space(6.0);

        let hint = match self.settings.hotkey.trim() {
            "" => String::new(),
            h => format!("   {h} toggles it anywhere"),
        };
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.settings.enabled, "Enabled");
            ui.label(egui::RichText::new(hint).weak());
        });
        ui.add_space(4.0);

        self.preview.show(ui, &self.settings);
        ui.label(egui::RichText::new("Preview. Move your pointer quickly to see the real thing.").small().weak());

        section(ui, "Trail");
        egui::Grid::new("trail").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
            let s = &mut self.settings;
            ui.label("Intensity").on_hover_text("How visible the smear is. 100% is a physically accurate exposure.");
            ui.add(egui::Slider::new(&mut s.intensity, 50..=600).suffix("%"));
            ui.end_row();
            ui.label("Blur length").on_hover_text("Shutter time: how far back in time the smear reaches.");
            ui.add(egui::Slider::new(&mut s.blur_ms, 10..=200).suffix(" ms"));
            ui.end_row();
            ui.label("Taper").on_hover_text("How much the smear fades towards its tail.");
            ui.add(egui::Slider::new(&mut s.taper, 0..=100).suffix("%"));
            ui.end_row();
            ui.label("Minimum speed").on_hover_text("Slower movements than this stay sharp.");
            ui.add(egui::Slider::new(&mut s.min_speed, 0..=1000).suffix(" px/s"));
            ui.end_row();
        });

        section(ui, "Style");
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.settings.style, Style::Natural, "Natural");
            ui.radio_value(&mut self.settings.style, Style::Tint, "Tinted");
            ui.radio_value(&mut self.settings.style, Style::Rainbow, "Rainbow");
        });
        if self.settings.style == Style::Tint {
            ui.horizontal(|ui| {
                ui.label("Colour");
                if ui.color_edit_button_srgb(&mut self.color).changed() {
                    let [r, g, b] = self.color;
                    self.settings.color = format!("#{r:02X}{g:02X}{b:02X}");
                }
            });
        }

        section(ui, "Timing");
        let mut auto = self.settings.lag_ms == Lag::Auto;
        ui.horizontal(|ui| {
            ui.label("Lag compensation");
            if ui.checkbox(&mut auto, "Automatic").changed() {
                self.settings.lag_ms = if auto { Lag::Auto } else { Lag::Ms(self.manual_lag) };
            }
        });
        ui.add_enabled_ui(!auto, |ui| {
            if ui.add(egui::Slider::new(&mut self.manual_lag, 0..=config::MAX_LAG_MS).suffix(" ms")).changed() {
                self.settings.lag_ms = Lag::Ms(self.manual_lag);
            }
        });
        ui.label(
            egui::RichText::new(
                "The real pointer is drawn a frame or two before any app can draw, so the trail is pushed \
                 ahead to meet it. Automatic uses 1.5 display frames. Raise it if there's a gap; lower it \
                 if the trail overshoots.",
            )
            .small()
            .weak(),
        );

        section(ui, "General");
        ui.horizontal(|ui| {
            ui.label("Toggle shortcut");
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.hotkey_text).desired_width(150.0).hint_text("e.g. Ctrl+Alt+B"),
            );
            if edit.changed() {
                let text = self.hotkey_text.trim();
                self.hotkey_valid = text.is_empty() || text.parse::<HotKey>().is_ok();
                if self.hotkey_valid {
                    self.settings.hotkey = text.to_string();
                }
            }
            if !self.hotkey_valid {
                ui.label(egui::RichText::new("Not a valid shortcut").color(ui.visuals().error_fg_color));
            }
        });
        ui.add_enabled_ui(self.autostart.is_some(), |ui| {
            if ui.checkbox(&mut self.autostart_on, "Start at login").changed()
                && let Some(a) = &self.autostart
            {
                autostart::set_enabled(a, self.autostart_on);
                self.autostart_on = autostart::is_enabled(a);
            }
        });

        section(ui, "Updates");
        ui.checkbox(&mut self.settings.auto_update, "Install updates automatically")
            .on_hover_text("Checks GitHub when the app starts and every six hours.");
        ui.horizontal(|ui| {
            ui.label(format!("Version {}", updater::current_version()));
            let checking = self.update_check.is_some();
            if ui.add_enabled(!checking, egui::Button::new("Check for updates")).clicked() {
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(updater::check());
                });
                self.update_check = Some(rx);
                self.update_status = "Checking...".into();
                self.update_available = None;
            }
            if let Some(version) = self.update_available.clone()
                && ui.button(format!("Update to {version}")).clicked()
            {
                // The background process installs it and restarts itself on the new version.
                let _ = std::fs::write(config::config_dir().join(app::UPDATE_REQUEST_FILE), b"");
                if !self.running {
                    app::start_background(&self.path);
                }
                self.update_available = None;
                self.update_status = "Installing in the background. Mouse Blur restarts by itself; reopen this window afterwards to use the new version.".into();
            }
        });
        if !self.update_status.is_empty() {
            ui.label(egui::RichText::new(&self.update_status).small().weak());
        }

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Reset to defaults").clicked() {
                let keep = (self.settings.enabled, self.settings.hotkey.clone());
                let defaults = Settings { enabled: keep.0, hotkey: keep.1, ..Settings::default() };
                let written = self.written.clone();
                self.show_loaded(defaults);
                self.written = written; // so the reset gets saved
            }
            if ui.button("Open config file").clicked() {
                app::open_file(&self.path);
            }
            if ui.button("Project page").clicked() {
                app::open_url(app::HOMEPAGE);
            }
        });
        ui.add_space(4.0);
        ui.label(egui::RichText::new(self.path.display().to_string()).small().weak());
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(egui::RichText::new(title).strong());
    ui.separator();
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.ui(ui));
        });
        self.save_if_edited();
        // The preview animates; keep drawing while the window is open.
        ctx.request_repaint_after(Duration::from_millis(16));
    }
}

/// An animated preview: the real compositor run on a simulated pointer path, drawn with the
/// current settings.
struct Preview {
    composer: Composer,
    cursor: CursorImage,
    cursor_scale: f32,
    samples: Vec<MotionSample>,
    pixels: Vec<u32>,
    rgba: Vec<u8>,
    texture: Option<egui::TextureHandle>,
    started: Instant,
}

impl Preview {
    fn new() -> Self {
        Preview {
            composer: Composer::new(),
            cursor: CursorImage::arrow(1.0),
            cursor_scale: 1.0,
            samples: Vec::with_capacity(400),
            pixels: Vec::new(),
            rgba: Vec::new(),
            texture: None,
            started: Instant::now(),
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, s: &Settings) {
        let ppp = ui.ctx().pixels_per_point();
        let size = egui::vec2(ui.available_width().max(120.0), 150.0);
        let (w, h) = ((size.x * ppp).round() as usize, (size.y * ppp).round() as usize);
        if (self.cursor_scale - ppp).abs() > 0.01 {
            self.cursor = CursorImage::arrow(ppp);
            self.cursor_scale = ppp;
        }
        let background = if ui.visuals().dark_mode { 0xFF2C_2C2C } else { 0xFFFF_FFFF };
        self.pixels.clear();
        self.pixels.resize(w * h, background);

        // A figure-eight that speeds up through the middle and slows at the ends, so both the
        // crisp and the blurred cases show.
        let (fw, fh) = (w as f64, h as f64);
        let path = |t: f64| {
            let a = t * 3.0;
            let p = a + 0.55 * a.sin();
            (fw * (0.5 + 0.40 * p.sin()), fh * (0.5 + 0.28 * (2.0 * p).sin()))
        };
        let t = self.started.elapsed().as_secs_f64();
        let now = (t * 1e9) as i64;
        self.samples.clear();
        for k in 0..400 {
            let (x, y) = path(t - k as f64 / 1000.0);
            self.samples.push(MotionSample { t: now - k * 1_000_000, x: x.round() as f32, y: y.round() as f32 });
        }
        if s.enabled {
            let params = TrailParams {
                gain: s.intensity as f32 / 100.0,
                window: s.blur_ms as f32 / 1000.0,
                lead: 0.0,
                taper: s.taper as f32 / 100.0,
                min_speed: s.min_speed as f32,
                tint: s.tint((t * 0.25).fract() as f32),
            };
            if self.composer.compose(&self.samples, now, &self.cursor, &params).is_some()
                && let Some(f) = self.composer.frame()
            {
                blend(&mut self.pixels, w, h, (f.rect.x, f.rect.y), (f.rect.width, f.rect.height), f.pixels);
            }
            self.composer.release();
        }
        let (x, y) = path(t);
        let c = &self.cursor;
        blend(
            &mut self.pixels,
            w,
            h,
            (x.round() as i32 - c.hot_x, y.round() as i32 - c.hot_y),
            (c.width, c.height),
            &c.pixels,
        );

        self.rgba.clear();
        for &p in &self.pixels {
            self.rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]);
        }
        let image = egui::ColorImage::from_rgba_premultiplied([w, h], &self.rgba);
        match &mut self.texture {
            Some(tex) => tex.set(image, egui::TextureOptions::NEAREST),
            None => self.texture = Some(ui.ctx().load_texture("preview", image, egui::TextureOptions::NEAREST)),
        }
        if let Some(tex) = &self.texture {
            let rect = ui.add(egui::Image::new((tex.id(), size)).corner_radius(6.0)).rect;
            let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
            ui.painter().rect_stroke(rect, 6.0, stroke, egui::StrokeKind::Inside);
        }
    }
}

/// Composites premultiplied pixels onto an opaque canvas, clipped to it.
fn blend(canvas: &mut [u32], cw: usize, ch: usize, (x0, y0): (i32, i32), (w, h): (usize, usize), src: &[u32]) {
    for y in 0..h {
        let ty = y0 + y as i32;
        if ty < 0 || ty >= ch as i32 {
            continue;
        }
        for x in 0..w {
            let tx = x0 + x as i32;
            if tx < 0 || tx >= cw as i32 {
                continue;
            }
            let s = src[y * w + x];
            let a = s >> 24;
            if a == 0 {
                continue;
            }
            let d = &mut canvas[ty as usize * cw + tx as usize];
            let inv = 255 - a;
            let mix = |sh: u32| (((s >> sh) & 0xFF) + (((*d >> sh) & 0xFF) * inv + 127) / 255).min(255);
            *d = 0xFF00_0000 | (mix(16) << 16) | (mix(8) << 8) | mix(0);
        }
    }
}
