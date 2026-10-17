//! Renders simulated pointer motions with each trail style to a PNG, without touching the real
//! pointer. Used for the README preview and for eyeballing changes to the compositor.
//!
//!     cargo run --release --example render_demo -- docs/preview.png

use mouse_blur::composer::{Composer, Tint, TrailParams};
use mouse_blur::cursor::CursorImage;
use mouse_blur::history::MotionSample;

const W: usize = 480;
const H: usize = 300;
const SCALE: f32 = 2.0; // draw at 2x so the preview reads well on high-DPI screens

struct Scene {
    background: u32,
    tint: Tint,
    update_hz: f64,
    glow: f32,
    path: fn(f64) -> (f64, f64),
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "preview.png".into());
    let cursor = CursorImage::arrow(SCALE);
    let scenes = [
        Scene {
            background: 0xFFF3F3F3,
            tint: Tint::Natural,
            update_hz: 0.0,
            glow: 0.0,
            path: |t| (330.0 + 6000.0 * t, 150.0 - 60000.0 * t * t),
        },
        Scene {
            background: 0xFF1E1E1E,
            tint: Tint::Natural,
            update_hz: 125.0,
            glow: 0.0,
            path: |t| {
                let a = 4500.0 / 100.0 * t;
                (240.0 + 100.0 * a.cos(), 150.0 + 100.0 * a.sin())
            },
        },
        Scene {
            background: 0xFF1E1E1E,
            tint: Tint::Gradient([0x38, 0xB6, 0xFF], [0xFF, 0x4F, 0xD8]),
            update_hz: 0.0,
            glow: 12.0,
            path: |t| (330.0 + 5000.0 * t, 190.0 + 2800.0 * t),
        },
        Scene {
            background: 0xFFF3F3F3,
            tint: Tint::Rainbow { phase: 0.0 },
            update_hz: 0.0,
            glow: 4.0,
            path: |t| (330.0 + 6000.0 * t, 150.0 + 70.0 * (t * 40.0).sin()),
        },
    ];
    let mut canvas = vec![0u32; W * 2 * H * 2];
    let mut composer = Composer::new();
    let now: i64 = 1_000_000_000;
    let lead = 0.016;
    for (i, scene) in scenes.iter().enumerate() {
        let (px, py) = ((i % 2) * W, (i / 2) * H);
        for y in 0..H {
            canvas[(py + y) * W * 2 + px..(py + y) * W * 2 + px + W].fill(scene.background);
        }
        let at = |t: f64| {
            let t = if scene.update_hz > 0.0 { (t * scene.update_hz).floor() / scene.update_hz } else { t };
            (scene.path)(t)
        };
        let samples: Vec<MotionSample> = (0..400)
            .map(|k| {
                let (x, y) = at(-(k as f64) / 1000.0);
                MotionSample { t: now - k * 1_000_000, x: x.round() as f32, y: y.round() as f32 }
            })
            .collect();
        let params = TrailParams {
            gain: 3.0,
            window: 0.05,
            lead,
            taper: 0.5,
            min_speed: 150.0,
            tint: scene.tint,
            glow: scene.glow,
        };
        if composer.compose(&samples, now, &cursor, &params).is_some() {
            let frame = composer.frame().unwrap();
            blend(&mut canvas, px, py, frame.rect.x, frame.rect.y, frame.rect.width, frame.rect.height, frame.pixels);
        }
        composer.release();
        // The real pointer, where it is by the time the frame reaches the screen.
        let (x, y) = at(lead as f64);
        blend(
            &mut canvas,
            px,
            py,
            x.round() as i32 - cursor.hot_x,
            y.round() as i32 - cursor.hot_y,
            cursor.width,
            cursor.height,
            &cursor.pixels,
        );
    }

    let mut rgba = Vec::with_capacity(canvas.len() * 4);
    for p in canvas {
        rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]);
    }
    let file = std::fs::File::create(&out).expect("create output");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), (W * 2) as u32, (H * 2) as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&rgba).unwrap();
    println!("wrote {out}");
}

/// Composites premultiplied pixels onto one panel of the canvas, clipped to the panel.
#[allow(clippy::too_many_arguments)]
fn blend(canvas: &mut [u32], px: usize, py: usize, x0: i32, y0: i32, w: usize, h: usize, src: &[u32]) {
    for y in 0..h {
        let ty = y0 + y as i32;
        if ty < 0 || ty >= H as i32 {
            continue;
        }
        for x in 0..w {
            let tx = x0 + x as i32;
            if tx < 0 || tx >= W as i32 {
                continue;
            }
            let s = src[y * w + x];
            let a = s >> 24;
            if a == 0 {
                continue;
            }
            let d = &mut canvas[(py + ty as usize) * W * 2 + px + tx as usize];
            let inv = 255 - a;
            let ch = |sh: u32| (((s >> sh) & 0xFF) + (((*d >> sh) & 0xFF) * inv + 127) / 255).min(255);
            *d = 0xFF00_0000 | (ch(16) << 16) | (ch(8) << 8) | ch(0);
        }
    }
}
