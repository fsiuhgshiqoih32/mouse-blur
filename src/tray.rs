//! The tray (menu bar) icon and its menu.

use tray_icon::menu::{CheckMenuItem, IsMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use mouse_blur::art;
use mouse_blur::config::{
    BLUR_PRESETS, INTENSITY_PRESETS, LAG_PRESETS, Lag, STYLE_PRESETS, Settings, Style, TAPER_PRESETS,
};

use crate::app::Action;

/// A submenu of mutually exclusive choices.
struct Choice<T> {
    menu: Submenu,
    items: Vec<(T, CheckMenuItem)>,
}

impl<T: Copy + PartialEq> Choice<T> {
    fn new(title: &str, options: impl IntoIterator<Item = (T, String)>) -> Self {
        let menu = Submenu::new(title, true);
        let items: Vec<(T, CheckMenuItem)> =
            options.into_iter().map(|(v, label)| (v, CheckMenuItem::new(label, true, false, None))).collect();
        for (_, item) in &items {
            let _ = menu.append(item);
        }
        Choice { menu, items }
    }

    fn select(&self, value: T) {
        for (v, item) in &self.items {
            item.set_checked(*v == value);
        }
    }

    fn find(&self, id: &MenuId) -> Option<T> {
        self.items.iter().find(|(_, item)| item.id() == id).map(|(v, _)| *v)
    }
}

pub struct Tray {
    icon: TrayIcon,
    enabled: CheckMenuItem,
    intensity: Choice<u32>,
    blur: Choice<u32>,
    taper: Choice<u32>,
    lag: Choice<Lag>,
    style: Choice<Style>,
    open_config: MenuItem,
    autostart: CheckMenuItem,
    homepage: MenuItem,
    quit: MenuItem,
    shown_enabled: bool,
}

impl Tray {
    pub fn new(s: &Settings, autostart: Option<bool>, hotkey: Option<&str>) -> Result<Self, String> {
        let enabled = CheckMenuItem::new(enabled_label(hotkey), true, s.enabled, None);
        let intensity =
            Choice::new("Intensity", INTENSITY_PRESETS.iter().map(|&(v, name)| (v, format!("{name} ({v}%)"))));
        let blur = Choice::new("Blur length", BLUR_PRESETS.iter().map(|&(v, name)| (v, format!("{name} ({v} ms)"))));
        let taper = Choice::new("Taper", TAPER_PRESETS.iter().map(|&(v, name)| (v, format!("{name} ({v}%)"))));
        let lag = Choice::new(
            "Lag compensation",
            LAG_PRESETS.iter().map(|&v| {
                let label = match v {
                    Lag::Auto => "Auto".to_string(),
                    Lag::Ms(ms) => format!("{ms} ms"),
                };
                (v, label)
            }),
        );
        let style = Choice::new("Style", STYLE_PRESETS.iter().map(|&(v, name)| (v, name.to_string())));
        let open_config = MenuItem::new("Edit config file...", true, None);
        let autostart_item =
            CheckMenuItem::new("Start at login", autostart.is_some(), autostart.unwrap_or(false), None);
        let homepage = MenuItem::new("Project page", true, None);
        let quit = MenuItem::new("Quit Mouse Blur", true, None);

        let menu = Menu::new();
        let items: [&dyn IsMenuItem; 13] = [
            &enabled,
            &PredefinedMenuItem::separator(),
            &intensity.menu,
            &blur.menu,
            &taper.menu,
            &style.menu,
            &lag.menu,
            &PredefinedMenuItem::separator(),
            &open_config,
            &autostart_item,
            &homepage,
            &PredefinedMenuItem::separator(),
            &quit,
        ];
        menu.append_items(&items).map_err(|e| e.to_string())?;

        let builder = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("Mouse Blur");
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icon(s.enabled)?);
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(icon(s.enabled)?);
        let icon = builder.build().map_err(|e| e.to_string())?;

        let mut tray = Tray {
            icon,
            enabled,
            intensity,
            blur,
            taper,
            lag,
            style,
            open_config,
            autostart: autostart_item,
            homepage,
            quit,
            shown_enabled: s.enabled,
        };
        tray.sync(s, autostart, hotkey, 0);
        Ok(tray)
    }

    pub fn action_for(&self, id: &MenuId) -> Option<Action> {
        if self.enabled.id() == id {
            return Some(Action::ToggleEnabled);
        }
        if self.open_config.id() == id {
            return Some(Action::OpenConfig);
        }
        if self.autostart.id() == id {
            return Some(Action::ToggleAutostart);
        }
        if self.homepage.id() == id {
            return Some(Action::OpenHomepage);
        }
        if self.quit.id() == id {
            return Some(Action::Quit);
        }
        None.or_else(|| self.intensity.find(id).map(Action::Intensity))
            .or_else(|| self.blur.find(id).map(Action::Blur))
            .or_else(|| self.taper.find(id).map(Action::Taper))
            .or_else(|| self.lag.find(id).map(Action::Lag))
            .or_else(|| self.style.find(id).map(Action::Style))
    }

    /// Makes the menu reflect the current settings. Check items toggle themselves when clicked,
    /// so this also undoes that for radio-style choices.
    pub fn sync(&mut self, s: &Settings, autostart: Option<bool>, hotkey: Option<&str>, auto_lag_ms: u32) {
        self.enabled.set_checked(s.enabled);
        self.enabled.set_text(enabled_label(hotkey));
        self.intensity.select(s.intensity);
        self.blur.select(s.blur_ms);
        self.taper.select(s.taper);
        self.lag.select(s.lag_ms);
        self.style.select(s.style);
        if let Some((_, auto)) = self.lag.items.iter().find(|(v, _)| *v == Lag::Auto) {
            auto.set_text(if auto_lag_ms > 0 { format!("Auto ({auto_lag_ms} ms)") } else { "Auto".to_string() });
        }
        self.autostart.set_checked(autostart.unwrap_or(false));
        if s.enabled != self.shown_enabled {
            self.shown_enabled = s.enabled;
            if let Ok(i) = icon(s.enabled) {
                #[cfg(target_os = "macos")]
                let _ = self.icon.set_icon_templated(Some(i));
                #[cfg(not(target_os = "macos"))]
                let _ = self.icon.set_icon(Some(i));
            }
        }
    }
}

fn enabled_label(hotkey: Option<&str>) -> String {
    match hotkey {
        Some(h) => format!("Enabled ({h})"),
        None => "Enabled".to_string(),
    }
}

/// The macOS menu bar wants a black template image it can tint for light and dark mode.
const TEMPLATE_ICON: bool = cfg!(target_os = "macos");

fn icon(enabled: bool) -> Result<Icon, String> {
    let size = if TEMPLATE_ICON {
        36
    } else if cfg!(target_os = "linux") {
        48
    } else {
        32
    };
    Icon::from_rgba(art::app_icon(size, enabled, TEMPLATE_ICON), size as u32, size as u32).map_err(|e| e.to_string())
}
