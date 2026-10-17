# Changelog

## 0.1.0

First release.

- Real motion blur for the mouse pointer: the path covered during a short "shutter" window is
  sampled at about 1000 Hz and weighted copies of the current cursor are accumulated along it.
- Native overlays for Windows (layered window), macOS (Core Animation) and Linux X11 (ARGB
  window with XFixes cursor capture). The real pointer is never hidden, so there is no added lag.
- Works with any cursor shape, high-DPI and multiple monitors.
- Lag compensation, automatic from the display refresh rate or set by hand.
- Three trail styles: natural, tinted and rainbow.
- Tray menu with presets, a global on/off shortcut (Ctrl+Alt+B), start at login, and a config file
  that is reloaded live when edited. Command-line options for everything.
- Low-rate mice and touchpads (125 Hz) smear smoothly instead of leaving a string of copies.
