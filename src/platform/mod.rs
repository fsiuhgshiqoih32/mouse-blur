//! Native overlay and main loop for each operating system.
//!
//! Every backend provides:
//! - `init_process()`: process-wide setup, called first thing in `main`.
//! - `run(shared, history, make_controller)`: starts the sampler and renderer, builds the controller
//!   on the main thread (the closure's argument says whether a tray icon can be shown) and runs the
//!   main loop until the user quits.
//! - `show_error(msg)`: reports a fatal error where the user will see it.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use self::linux::*;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
compile_error!("Mouse Blur supports Windows, macOS and Linux (X11).");
