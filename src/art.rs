//! A tiny anti-aliased polygon rasterizer, used for the tray icon and the fallback cursor.

/// Classic arrow pointer, in units where it is 11.6 wide and 18.6 tall, tip at the origin.
pub const ARROW: [(f32, f32); 7] =
    [(0.0, 0.0), (0.0, 16.0), (4.0, 12.4), (6.8, 18.6), (9.4, 17.4), (6.7, 11.4), (11.6, 11.4)];

pub fn transform(pts: &[(f32, f32)], dx: f32, dy: f32, scale: f32) -> Vec<(f32, f32)> {
    pts.iter().map(|&(x, y)| (dx + x * scale, dy + y * scale)).collect()
}

/// RGBA canvas with premultiplied floating-point pixels.
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    px: Vec<[f32; 4]>,
}

const SS: usize = 4; // supersamples per axis

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        Canvas { width, height, px: vec![[0.0; 4]; width * height] }
    }

    /// Fills a polygon with `fill` (straight RGBA, 0..=1), optionally with an outline of `stroke`
    /// colour and width drawn just inside the edge, composited over what's already there.
    pub fn fill_polygon(&mut self, pts: &[(f32, f32)], fill: [f32; 4], stroke: Option<([f32; 4], f32)>) {
        if pts.len() < 3 {
            return;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in pts {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let px0 = (x0.floor().max(0.0)) as usize;
        let py0 = (y0.floor().max(0.0)) as usize;
        let px1 = (x1.ceil() as usize + 1).min(self.width);
        let py1 = (y1.ceil() as usize + 1).min(self.height);
        let n = (SS * SS) as f32;
        for py in py0..py1 {
            for px in px0..px1 {
                let mut acc = [0.0f32; 4];
                for sy in 0..SS {
                    for sx in 0..SS {
                        let x = px as f32 + (sx as f32 + 0.5) / SS as f32;
                        let y = py as f32 + (sy as f32 + 0.5) / SS as f32;
                        if !inside(pts, x, y) {
                            continue;
                        }
                        let c = match stroke {
                            Some((sc, w)) if edge_distance(pts, x, y) < w => sc,
                            _ => fill,
                        };
                        acc[0] += c[0] * c[3];
                        acc[1] += c[1] * c[3];
                        acc[2] += c[2] * c[3];
                        acc[3] += c[3];
                    }
                }
                if acc[3] == 0.0 {
                    continue;
                }
                let src = [acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n];
                let dst = &mut self.px[py * self.width + px];
                let inv = 1.0 - src[3];
                for i in 0..4 {
                    dst[i] = src[i] + dst[i] * inv;
                }
            }
        }
    }

    /// Straight-alpha RGBA bytes, as icon APIs expect.
    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 4);
        for p in &self.px {
            let a = p[3].clamp(0.0, 1.0);
            let un = |c: f32| if a > 0.0 { (c / a).clamp(0.0, 1.0) } else { 0.0 };
            out.extend_from_slice(&[
                (un(p[0]) * 255.0 + 0.5) as u8,
                (un(p[1]) * 255.0 + 0.5) as u8,
                (un(p[2]) * 255.0 + 0.5) as u8,
                (a * 255.0 + 0.5) as u8,
            ]);
        }
        out
    }

    /// Premultiplied `0xAARRGGBB`, the compositor's format.
    pub fn to_premultiplied_argb(&self) -> Vec<u32> {
        self.px
            .iter()
            .map(|p| {
                let q = |c: f32| (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
                (q(p[3]) << 24) | (q(p[0]) << 16) | (q(p[1]) << 8) | q(p[2])
            })
            .collect()
    }
}

fn inside(pts: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut c = false;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let (xi, yi) = pts[i];
        let (xj, yj) = pts[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            c = !c;
        }
        j = i;
    }
    c
}

fn edge_distance(pts: &[(f32, f32)], x: f32, y: f32) -> f32 {
    let mut best = f32::MAX;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let (ax, ay) = pts[j];
        let (bx, by) = pts[i];
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 { (((x - ax) * dx + (y - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
        let (cx, cy) = (ax + dx * t - x, ay + dy * t - y);
        best = best.min((cx * cx + cy * cy).sqrt());
        j = i;
    }
    best
}

/// The app icon: an arrow pointer with a motion smear behind it. Straight RGBA bytes.
/// `template` draws a black-only version for the macOS menu bar, which tints it itself.
pub fn app_icon(size: usize, enabled: bool, template: bool) -> Vec<u8> {
    let mut c = Canvas::new(size, size);
    let sz = size as f32;
    let s = sz * 0.92 / 18.6;
    let x = sz - 11.6 * s - sz * 0.06;
    let y = sz * 0.04;
    let accent = if template {
        [0.0, 0.0, 0.0]
    } else if enabled {
        [0.22, 0.71, 1.0]
    } else {
        [0.6, 0.6, 0.6]
    };
    let alphas: [f32; 4] = if template && !enabled { [0.0; 4] } else { [0.75, 0.5, 0.3, 0.15] };
    let step = sz * 0.095;
    for i in (1..=alphas.len()).rev() {
        let a = alphas[i - 1];
        if a > 0.0 {
            let pts = transform(&ARROW, x - step * i as f32, y, s);
            c.fill_polygon(&pts, [accent[0], accent[1], accent[2], a], None);
        }
    }
    let pts = transform(&ARROW, x, y, s);
    if template {
        let a = if enabled { 1.0 } else { 0.55 };
        c.fill_polygon(&pts, [0.0, 0.0, 0.0, a], None);
    } else {
        c.fill_polygon(&pts, [1.0, 1.0, 1.0, 1.0], Some(([0.09, 0.09, 0.09, 1.0], (sz / 20.0).max(1.0))));
    }
    c.to_rgba8()
}
