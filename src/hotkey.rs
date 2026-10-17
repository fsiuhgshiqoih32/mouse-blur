//! The global on/off shortcut.

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::log::warning;

pub struct Hotkeys {
    manager: Option<GlobalHotKeyManager>,
    current: Option<HotKey>,
    label: Option<String>,
}

impl Hotkeys {
    pub fn new() -> Self {
        let manager = GlobalHotKeyManager::new().map_err(|e| warning!("global hotkeys unavailable: {e}")).ok();
        Hotkeys { manager, current: None, label: None }
    }

    /// Registers `spec` (e.g. "Ctrl+Alt+B") in place of the current shortcut. Empty disables it.
    pub fn set(&mut self, spec: &str) {
        let Some(manager) = &self.manager else { return };
        if let Some(old) = self.current.take() {
            let _ = manager.unregister(old);
        }
        self.label = None;
        let spec = spec.trim();
        if spec.is_empty() {
            return;
        }
        match spec.parse::<HotKey>() {
            Ok(hk) => match manager.register(hk) {
                Ok(()) => {
                    self.current = Some(hk);
                    self.label = Some(spec.to_string());
                }
                Err(e) => warning!("could not register hotkey {spec}: {e}"),
            },
            Err(e) => warning!("invalid hotkey {spec:?}: {e}"),
        }
    }

    /// The registered shortcut as the user wrote it, for the tray menu.
    #[cfg_attr(not(feature = "tray"), allow(dead_code))]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// True if the shortcut was pressed since the last call.
    pub fn pressed(&self) -> bool {
        let mut hit = false;
        while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
            if Some(ev.id) == self.current.map(|h| h.id()) && ev.state == HotKeyState::Pressed {
                hit = true;
            }
        }
        hit
    }
}
