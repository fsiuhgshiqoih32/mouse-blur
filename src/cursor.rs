//! Cursor images in the format the compositor consumes.

use crate::art::{self, Canvas};

/// A cursor as premultiplied `0xAARRGGBB` pixels, with per-row bounds of the visible pixels so the
/// compositor can skip transparent areas.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorImage {
    pub width: usize,
    pub height: usize,
    pub hot_x: i32,
    pub hot_y: i32,
    pub pixels: Vec<u32>,
    /// Visible span of each row, `[start, end)`.
    pub row_start: Vec<usize>,
    pub row_end: Vec<usize>,
    /// Bounds of the visible pixels.
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    /// Number of pixels inside the row spans.
    pub opaque_count: usize,
}

impl CursorImage {
    /// Builds a cursor from premultiplied `0xAARRGGBB` pixels. Returns `None` for a fully
    /// transparent image, which is how many apps "hide" the pointer.
    pub fn from_premultiplied(
        width: usize,
        height: usize,
        hot_x: i32,
        hot_y: i32,
        mut pixels: Vec<u32>,
    ) -> Option<Self> {
        if width == 0 || height == 0 || pixels.len() < width * height {
            return None;
        }
        pixels.truncate(width * height);
        let mut img = CursorImage {
            width,
            height,
            hot_x,
            hot_y,
            pixels,
            row_start: vec![0; height],
            row_end: vec![0; height],
            left: width as i32,
            top: height as i32,
            right: 0,
            bottom: 0,
            opaque_count: 0,
        };
        for y in 0..height {
            let row = &mut img.pixels[y * width..(y + 1) * width];
            let mut start = None;
            let mut end = 0;
            for (x, p) in row.iter_mut().enumerate() {
                // Clamp colour to alpha so malformed input can't produce invalid premultiplied pixels.
                let a = *p >> 24;
                if a == 0 {
                    *p = 0;
                    continue;
                }
                let r = ((*p >> 16) & 0xFF).min(a);
                let g = ((*p >> 8) & 0xFF).min(a);
                let b = (*p & 0xFF).min(a);
                *p = (a << 24) | (r << 16) | (g << 8) | b;
                if start.is_none() {
                    start = Some(x);
                }
                end = x + 1;
            }
            if let Some(start) = start {
                img.row_start[y] = start;
                img.row_end[y] = end;
                img.opaque_count += end - start;
                img.left = img.left.min(start as i32);
                img.right = img.right.max(end as i32);
                img.top = img.top.min(y as i32);
                img.bottom = y as i32 + 1;
            }
        }
        (img.opaque_count > 0).then_some(img)
    }

    /// Recovers colour and alpha from two renderings of the same cursor, one over black and one
    /// over white (`0x00RRGGBB`, alpha ignored). This works for every Windows cursor type,
    /// including monochrome ones.
    pub fn from_black_white(
        width: usize,
        height: usize,
        hot_x: i32,
        hot_y: i32,
        on_black: &[u32],
        on_white: &[u32],
    ) -> Option<Self> {
        let n = width * height;
        if on_black.len() < n || on_white.len() < n {
            return None;
        }
        let mut pixels = vec![0u32; n];
        for i in 0..n {
            let k = on_black[i];
            let v = on_white[i];
            let (b, g, r) = (k & 0xFF, (k >> 8) & 0xFF, (k >> 16) & 0xFF);
            // Over black we see colour*alpha; over white, colour*alpha + 255*(1 - alpha).
            let diff = ((v & 0xFF) as i32 - b as i32)
                + (((v >> 8) & 0xFF) as i32 - g as i32)
                + (((v >> 16) & 0xFF) as i32 - r as i32);
            let a = 255 - diff / 3;
            pixels[i] = if a <= 0 {
                0
            } else if a > 255 {
                // Screen-inverting pixel (e.g. the classic I-beam) has no colour of its own; use grey.
                0xFF80_8080
            } else {
                let a = a as u32;
                (a << 24) | (r.min(a) << 16) | (g.min(a) << 8) | b.min(a)
            };
        }
        Self::from_premultiplied(width, height, hot_x, hot_y, pixels)
    }

    /// A classic arrow pointer drawn at the given scale. Used when the system cursor can't be
    /// read, and by tests and the demo renderer.
    pub fn arrow(scale: f32) -> Self {
        let scale = scale.max(0.5);
        let margin = 1.0 * scale;
        let w = (13.0 * scale + 2.0 * margin).ceil() as usize;
        let h = (20.0 * scale + 2.0 * margin).ceil() as usize;
        let mut canvas = Canvas::new(w, h);
        let pts = art::transform(&art::ARROW, margin, margin, scale);
        canvas.fill_polygon(&pts, [1.0, 1.0, 1.0, 1.0], Some(([0.0, 0.0, 0.0, 1.0], scale)));
        let hot = margin.round() as i32;
        Self::from_premultiplied(w, h, hot, hot, canvas.to_premultiplied_argb()).expect("arrow is never empty")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_image_is_none() {
        assert!(CursorImage::from_premultiplied(4, 4, 0, 0, vec![0; 16]).is_none());
    }

    #[test]
    fn spans_and_bounds() {
        let mut px = vec![0u32; 5 * 3];
        px[5 + 1] = 0xFF00_0000;
        px[5 + 3] = 0x8040_4040;
        let img = CursorImage::from_premultiplied(5, 3, 0, 0, px).unwrap();
        assert_eq!((img.row_start[1], img.row_end[1]), (1, 4));
        assert_eq!((img.left, img.top, img.right, img.bottom), (1, 1, 4, 2));
        assert_eq!(img.opaque_count, 3);
    }

    #[test]
    fn black_white_recovers_alpha() {
        // Half-transparent white pixel: over black 128 grey, over white 255.
        let black = [0x0080_8080u32];
        let white = [0x00FF_FFFFu32];
        let img = CursorImage::from_black_white(1, 1, 0, 0, &black, &white).unwrap();
        let p = img.pixels[0];
        assert_eq!(p >> 24, 128);
        assert_eq!(p & 0xFF, 128);
    }

    #[test]
    fn arrow_has_hotspot_at_tip() {
        let img = CursorImage::arrow(1.0);
        let tip = img.pixels[img.hot_y as usize * img.width + img.hot_x as usize];
        assert!(tip >> 24 > 0, "tip pixel should be visible");
    }
}
