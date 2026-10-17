//! Platform-independent core of Mouse Blur: the trail compositor, cursor images, pointer history,
//! settings and the small rasterizer used for icons. The binary (`src/main.rs`) adds the native
//! overlays, tray menu and hotkeys for each operating system.

pub mod art;
pub mod composer;
pub mod config;
pub mod cursor;
pub mod history;
