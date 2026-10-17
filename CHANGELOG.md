# Changelog

## 0.3.0

- Presets: Subtle, Natural, Strong, Cinematic, Neon and Rainbow, one click each.
- Gradient style: the trail runs from one colour at the head to another at the tail.
- Glow: a soft halo around the trail, up to 12 px. Works with every style.
- Pauses by itself while a fullscreen app (a game, a video) is in front. On by default.
- Excluded apps: list programs (such as cs2.exe or Photoshop) where the effect pauses while they're
  in front. The settings window offers recently focused apps as one-click additions.
- After an automatic update, the settings window says so and links to what's new.
- The settings window's scrollbar no longer covers its content.

## 0.2.0

- A settings window. Opening the app now shows it (and starts the effect in the background if it
  isn't running); closing it leaves the effect running in the tray. It has a live animated preview
  of the trail and every setting: intensity, blur length, taper, minimum speed, style and colour,
  lag compensation, the toggle shortcut, start at login and updates.
- Automatic updates. The app checks GitHub when it starts and every six hours, downloads the new
  version for your platform, verifies it against the release checksums, swaps it in and restarts.
  It can be turned off in the settings window, which also has a Check for updates button.
- Simpler tray menu: Enabled, Settings, Start at login, Project page, Quit.
- Settings changes reach the running effect within a quarter second.
- Start at login now starts only the background effect, without the window.

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
