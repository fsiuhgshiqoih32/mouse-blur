//! The tray (menu bar) icon and its menu. Detailed settings live in the settings window.

use tray_icon::menu::{CheckMenuItem, IsMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use mouse_blur::art;
use mouse_blur::config::Settings;

use crate::app::Action;

pub struct Tray {
    icon: TrayIcon,
    enabled: CheckMenuItem,
    settings: MenuItem,
    autostart: CheckMenuItem,
    homepage: MenuItem,
    quit: MenuItem,
    shown_enabled: bool,
}

impl Tray {
    pub fn new(s: &Settings, autostart: Option<bool>, hotkey: Option<&str>) -> Result<Self, String> {
        let enabled = CheckMenuItem::new(enabled_label(hotkey), true, s.enabled, None);
        let settings = MenuItem::new("Settings...", true, None);
        let autostart_item =
            CheckMenuItem::new("Start at login", autostart.is_some(), autostart.unwrap_or(false), None);
        let homepage = MenuItem::new("Project page", true, None);
        let quit = MenuItem::new("Quit Mouse Blur", true, None);

        let menu = Menu::new();
        let items: [&dyn IsMenuItem; 7] = [
            &enabled,
            &settings,
            &PredefinedMenuItem::separator(),
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

        Ok(Tray { icon, enabled, settings, autostart: autostart_item, homepage, quit, shown_enabled: s.enabled })
    }

    pub fn action_for(&self, id: &MenuId) -> Option<Action> {
        if self.enabled.id() == id {
            Some(Action::ToggleEnabled)
        } else if self.settings.id() == id {
            Some(Action::OpenSettings)
        } else if self.autostart.id() == id {
            Some(Action::ToggleAutostart)
        } else if self.homepage.id() == id {
            Some(Action::OpenHomepage)
        } else if self.quit.id() == id {
            Some(Action::Quit)
        } else {
            None
        }
    }

    /// Makes the menu and icon reflect the current settings. Check items toggle themselves when
    /// clicked, so this also corrects them when an action didn't take effect.
    pub fn sync(&mut self, s: &Settings, autostart: Option<bool>, hotkey: Option<&str>) {
        self.enabled.set_checked(s.enabled);
        self.enabled.set_text(enabled_label(hotkey));
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
