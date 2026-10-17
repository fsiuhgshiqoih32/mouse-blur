# Contributing

Bug reports, fixes and new platform backends are welcome. Please open an issue first for anything
large.

## Layout

| Path | What's there |
|---|---|
| `src/composer.rs` | The trail compositor: path reconstruction, smoothing, weighting and accumulation. Platform independent. |
| `src/cursor.rs` | Cursor images (premultiplied ARGB) and the built-in fallback arrow. |
| `src/history.rs` | The pointer sample ring buffer shared between threads. |
| `src/config.rs` | Settings, presets and the TOML config file. |
| `src/art.rs` | A small polygon rasterizer for the tray icon and fallback cursor. |
| `src/renderer.rs` | The per-frame loop and the `Overlay` trait each platform implements. |
| `src/app.rs` | Shared state, the pointer sampler, and the main-thread controller (tray, hotkey, config reload, updates). |
| `src/settings_ui.rs` | The egui settings window. It runs as its own process and edits the config file. |
| `src/updater.rs` | Self-update from GitHub Releases, verified against `SHA256SUMS.txt`. |
| `src/platform/` | Native overlays and main loops: `windows.rs`, `macos.rs`, `linux.rs` (X11). |
| `examples/render_demo.rs` | Renders simulated motions to a PNG without touching the real pointer. |
| `examples/make_icons.rs` | Regenerates `assets/icon.ico` and `assets/icon.png`. |

## Checks

```
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
cargo run --release --example render_demo -- preview.png
cargo run -- --self-test
```

`--self-test` runs the real overlay for two seconds on a simulated circular pointer path (your
actual pointer isn't moved), then prints how many frames were presented. CI runs it on Windows,
macOS and Linux (under Xvfb).

## Releases

Bump `version` in `Cargo.toml`, add a matching section to `CHANGELOG.md`, and push to `main`. The
release workflow sees a version that hasn't been released, runs the tests, builds Windows (x64,
ARM64), macOS (universal app) and Linux packages, writes `SHA256SUMS.txt`, and publishes the
release with its `vX.Y.Z` tag. Installed copies update themselves within six hours, or straight away
with Check for updates. Pushes that don't change the version publish nothing.
