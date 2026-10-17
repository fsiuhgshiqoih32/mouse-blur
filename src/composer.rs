//! Turns recent pointer positions into a motion-blurred image of the cursor, the way a camera with
//! an open shutter would record it: the path covered during the shutter window is sampled and
//! copies of the cursor are accumulated, each weighted by how long the pointer spent there.

use crate::cursor::CursorImage;
use crate::history::MotionSample;

/// How the trail is coloured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tint {
    /// The cursor's own colours: a true motion blur.
    Natural,
    /// The cursor's shape filled with one sRGB colour.
    Solid([u8; 3]),
    /// A hue sweep from head to tail; `phase` (0..1) rotates it.
    Rainbow { phase: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrailParams {
    /// 1.0 is a physically averaged exposure; higher makes the smear more visible.
    pub gain: f32,
    /// Shutter length in seconds.
    pub window: f32,
    /// How much later than "now" this frame reaches the screen, in seconds. The head of the trail
    /// is extrapolated by this much so it lines up with the hardware pointer.
    pub lead: f32,
    /// 0 = even smear, 1 = fades to nothing at the tail.
    pub taper: f32,
    /// Average speed (px/s) at which the smear starts fading in. It is fully visible at 4.5x this.
    pub min_speed: f32,
    pub tint: Tint,
}

impl Default for TrailParams {
    fn default() -> Self {
        TrailParams { gain: 3.0, window: 0.05, lead: 0.0, taper: 0.5, min_speed: 150.0, tint: Tint::Natural }
    }
}

/// Screen rectangle covered by a composed trail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: usize,
    pub height: usize,
}

/// A composed trail: premultiplied `0xAARRGGBB` pixels, rows packed without padding.
pub struct Frame<'a> {
    pub rect: Rect,
    pub pixels: &'a [u32],
}

const MAX_STAMPS: usize = 192; // cursor copies per frame (fewer for big cursors)
const PIXEL_BUDGET: usize = 600_000; // cursor pixels blended per frame
const MAX_TRAIL_PX: f32 = 1200.0; // beyond this the smear is too faint to see anyway
const MAX_LEAD_PX: f32 = 160.0;
const MIN_MOTION_PX: f32 = 1.5;
const STOP_GAP_NS: i64 = 25_000_000; // longer than this between updates means the pointer had stopped
const STEP_NS: i64 = 8_000_000; // assumed duration of the move that ends a stop
const FILTER_PAD: usize = 4; // grid margin needed by the smoothing filter
const GRID_MAX: usize = 512;
const NS: f64 = 1e9;

#[derive(Clone, Copy)]
struct Point {
    t: i64,
    x: f32,
    y: f32,
}

#[derive(Clone, Copy)]
struct Stamp {
    x: i32,
    y: i32,
    w: f32,
    age: f32,
}

#[derive(Default)]
pub struct Composer {
    groups: Vec<Point>,
    path: Vec<Point>,
    grid_x: Vec<f32>,
    grid_y: Vec<f32>,
    tmp: Vec<f32>,
    clipped: Vec<(f32, f32, f32)>, // x, y, age
    stamps: Vec<Stamp>,
    accum: Vec<u32>, // 4 channels per pixel, 16.16 fixed point
    row_start: Vec<usize>,
    row_end: Vec<usize>,
    cap_w: usize,
    cap_h: usize,
    out: Vec<u32>,
    rect: Option<Rect>,
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Composes the trail for samples `s` (newest first) at time `now` (ns). Returns the covered
    /// rectangle; read the pixels with [`frame`](Self::frame), then call [`release`](Self::release).
    pub fn compose(&mut self, s: &[MotionSample], now: i64, img: &CursorImage, p: &TrailParams) -> Option<Rect> {
        self.release();
        if s.is_empty() || p.window <= 0.0 || !p.window.is_finite() {
            return None;
        }

        // 1. When the pointer arrived at each distinct position.
        self.groups.clear();
        let mut i = 0;
        while i < s.len() {
            let mut j = i;
            while j + 1 < s.len() && s[j + 1].x == s[i].x && s[j + 1].y == s[i].y {
                j += 1;
            }
            self.groups.push(Point { t: s[j].t, x: s[i].x, y: s[i].y });
            i = j + 1;
        }
        let g = &self.groups;

        // 2. Path between arrivals. Interpolating over the whole gap makes low-rate mice and
        //    touchpads smear smoothly instead of leaving a string of beads; long gaps stay stops.
        self.path.clear();
        for k in 0..g.len() {
            if k > 0 && g[k - 1].t - g[k].t > STOP_GAP_NS {
                self.path.push(Point { t: g[k - 1].t - STEP_NS, x: g[k].x, y: g[k].y });
            }
            self.path.push(g[k]);
        }
        let path = &self.path;

        // 3. Head: our frame reaches the screen a frame or two after the hardware pointer has moved
        //    on, so while the pointer is moving, extrapolate to where it will be by then.
        let lead = (p.lead.max(0.0) as f64 * NS) as i64;
        let head_t = now + lead;
        let (mut hx, mut hy) = (g[0].x, g[0].y);
        let (mut vx, mut vy) = (0.0f32, 0.0f32);
        let elapsed = now - g[0].t;
        let interval = update_interval(g);
        if interval > 0 && elapsed <= 2 * interval + 3_000_000 {
            let span = (16_000_000i64).max(2 * interval);
            let (px, py) = path_at(path, g[0].t - span);
            let per_sec = (NS / span as f64) as f32;
            vx = (g[0].x - px) * per_sec;
            vy = (g[0].y - py) * per_sec;
            let ahead = ((elapsed + lead) as f64 / NS) as f32;
            let (mut dx, mut dy) = (vx * ahead, vy * ahead);
            let d = (dx * dx + dy * dy).sqrt();
            if d > MAX_LEAD_PX {
                dx *= MAX_LEAD_PX / d;
                dy *= MAX_LEAD_PX / d;
            }
            hx += dx;
            hy += dy;
        }

        // 4. Resample at even ~1 ms steps over the shutter window (plus a margin each side) and
        //    low-pass, so whole-pixel positions and uneven update timing don't show up as bands.
        let window = p.window;
        let steps = ((window * 1000.0).ceil() as usize).clamp(4, GRID_MAX - 2 * FILTER_PAD - 1);
        let h = window / steps as f32;
        let count = steps + 1 + 2 * FILTER_PAD;
        self.grid_x.clear();
        self.grid_y.clear();
        let mut idx = 0;
        for j in 0..count {
            let age = (j as f32 - FILTER_PAD as f32) * h; // seconds before the head
            let (x, y) = if age <= 0.0 {
                (hx - vx * age, hy - vy * age)
            } else {
                let t = head_t - (age as f64 * NS) as i64;
                if t >= path[0].t {
                    let f = (t - path[0].t) as f32 / (head_t - path[0].t).max(1) as f32;
                    (path[0].x + (hx - path[0].x) * f, path[0].y + (hy - path[0].y) * f)
                } else {
                    while idx + 1 < path.len() && path[idx + 1].t > t {
                        idx += 1;
                    }
                    if idx + 1 >= path.len() {
                        let last = path[path.len() - 1]; // history shorter than the window: hold
                        (last.x, last.y)
                    } else {
                        let (a, b) = (path[idx + 1], path[idx]);
                        let u = (t - a.t) as f32 / (b.t - a.t).max(1) as f32;
                        (a.x + (b.x - a.x) * u, a.y + (b.y - a.y) * u)
                    }
                }
            };
            self.grid_x.push(x);
            self.grid_y.push(y);
        }
        // Two box passes make a 9 ms triangle filter.
        smooth(&mut self.grid_x, &mut self.tmp);
        smooth(&mut self.grid_x, &mut self.tmp);
        smooth(&mut self.grid_y, &mut self.tmp);
        smooth(&mut self.grid_y, &mut self.tmp);

        self.clipped.clear();
        self.clipped.push((self.grid_x[FILTER_PAD], self.grid_y[FILTER_PAD], 0.0));
        let mut length = 0.0f32;
        for j in FILTER_PAD + 1..=FILTER_PAD + steps {
            let (lx, ly, la) = *self.clipped.last().unwrap();
            let (x, y) = (self.grid_x[j], self.grid_y[j]);
            let (dx, dy) = (x - lx, y - ly);
            let d = (dx * dx + dy * dy).sqrt();
            if length + d > MAX_TRAIL_PX {
                let f = (MAX_TRAIL_PX - length) / d;
                self.clipped.push((lx + dx * f, ly + dy * f, la + h * f));
                length = MAX_TRAIL_PX;
                break;
            }
            self.clipped.push((x, y, (j - FILTER_PAD) as f32 * h));
            length += d;
        }
        if length < MIN_MOTION_PX {
            return None;
        }
        let fade = smoothstep(p.min_speed, p.min_speed.max(1.0) * 4.5, length / window);
        if fade <= 0.01 {
            return None;
        }

        // 5. Stamps along the path, evenly spaced, each weighted by the time it stands for and
        //    split over the four nearest pixels so diagonal smears don't alias.
        let max_stamps = (PIXEL_BUDGET / (3 * img.opaque_count.max(1))).clamp(24, MAX_STAMPS);
        let spacing = (length / max_stamps as f32).max(1.0);
        let taper = p.taper.clamp(0.0, 1.0);
        let scale = p.gain * fade / (window * (1.0 - 0.5 * taper)); // a still pointer's weights sum to gain
        self.stamps.clear();
        for pair in self.clipped.windows(2) {
            let (x0, y0, a0) = pair[0];
            let (x1, y1, a1) = pair[1];
            let dt = a1 - a0;
            if dt <= 0.0 {
                continue;
            }
            let (dx, dy) = (x1 - x0, y1 - y0);
            let k = (((dx * dx + dy * dy).sqrt() / spacing).ceil() as usize).max(1);
            let share = dt / k as f32 * scale;
            for q in 0..k {
                let f = (q as f32 + 0.5) / k as f32;
                let age = a0 + dt * f;
                let w = share * (1.0 - taper * age / window);
                let (x, y) = (x0 + dx * f, y0 + dy * f);
                let (ix, iy) = (x.floor() as i32, y.floor() as i32);
                let (fx, fy) = (x - ix as f32, y - iy as f32);
                add_stamp(&mut self.stamps, ix, iy, w * (1.0 - fx) * (1.0 - fy), age);
                add_stamp(&mut self.stamps, ix + 1, iy, w * fx * (1.0 - fy), age);
                add_stamp(&mut self.stamps, ix, iy + 1, w * (1.0 - fx) * fy, age);
                add_stamp(&mut self.stamps, ix + 1, iy + 1, w * fx * fy, age);
            }
        }
        if self.stamps.is_empty() {
            return None;
        }

        let (mut min_x, mut min_y, mut max_x, mut max_y) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for st in &self.stamps {
            let (ox, oy) = (st.x - img.hot_x, st.y - img.hot_y);
            min_x = min_x.min(ox + img.left);
            min_y = min_y.min(oy + img.top);
            max_x = max_x.max(ox + img.right);
            max_y = max_y.max(oy + img.bottom);
        }
        let rect = Rect { x: min_x, y: min_y, width: (max_x - min_x) as usize, height: (max_y - min_y) as usize };
        self.ensure_capacity(rect.width, rect.height);

        // 6. Accumulate weighted cursor copies.
        let cap_w = self.cap_w;
        for st in &self.stamps {
            let wfix = (st.w * 65536.0 + 0.5) as u32;
            if wfix == 0 {
                continue;
            }
            let color = match p.tint {
                Tint::Natural => None,
                Tint::Solid(c) => Some(c),
                Tint::Rainbow { phase } => Some(hue(phase + 0.8 * st.age / window)),
            };
            // The rectangle is cropped to the cursor's visible pixels, so these offsets can be
            // negative by up to img.left / img.top; adding a visible column or row makes them >= 0.
            let ox = st.x - img.hot_x - rect.x;
            let oy = st.y - img.hot_y - rect.y;
            for r in img.top as usize..img.bottom as usize {
                let (x0, x1) = (img.row_start[r], img.row_end[r]);
                if x0 >= x1 {
                    continue;
                }
                let row = (oy + r as i32) as usize;
                let col = (ox + x0 as i32) as usize;
                let src = &img.pixels[r * img.width + x0..r * img.width + x1];
                let base = (row * cap_w + col) * 4;
                let dst = &mut self.accum[base..base + src.len() * 4];
                match color {
                    None => {
                        for (d, &s) in dst.chunks_exact_mut(4).zip(src) {
                            d[0] += (s & 0xFF) * wfix;
                            d[1] += ((s >> 8) & 0xFF) * wfix;
                            d[2] += ((s >> 16) & 0xFF) * wfix;
                            d[3] += (s >> 24) * wfix;
                        }
                    }
                    Some([cr, cg, cb]) => {
                        let wb = (wfix * (cb as u32 + 1)) >> 8;
                        let wg = (wfix * (cg as u32 + 1)) >> 8;
                        let wr = (wfix * (cr as u32 + 1)) >> 8;
                        for (d, &s) in dst.chunks_exact_mut(4).zip(src) {
                            let a = s >> 24;
                            d[0] += a * wb;
                            d[1] += a * wg;
                            d[2] += a * wr;
                            d[3] += a * wfix;
                        }
                    }
                }
                self.row_start[row] = self.row_start[row].min(col);
                self.row_end[row] = self.row_end[row].max(col + src.len());
            }
        }

        // 7. Resolve into packed premultiplied pixels.
        let w = rect.width;
        for y in 0..rect.height {
            let (x0, x1) = (self.row_start[y], self.row_end[y]);
            for x in x0..x1.max(x0) {
                let a = &mut self.accum[(y * cap_w + x) * 4..(y * cap_w + x) * 4 + 4];
                let al = (a[3] >> 16).min(255);
                let b = (a[0] >> 16).min(al);
                let g = (a[1] >> 16).min(al);
                let r = (a[2] >> 16).min(al);
                self.out[y * w + x] = (al << 24) | (r << 16) | (g << 8) | b;
                a.fill(0);
            }
        }
        self.rect = Some(rect);
        Some(rect)
    }

    /// The last composed trail, if [`compose`](Self::compose) succeeded and it hasn't been released.
    pub fn frame(&self) -> Option<Frame<'_>> {
        self.rect.map(|rect| Frame { rect, pixels: &self.out[..rect.width * rect.height] })
    }

    /// Clears the output so the next frame starts from zero. Only touched pixels are cleared.
    pub fn release(&mut self) {
        let Some(rect) = self.rect.take() else { return };
        for y in 0..rect.height {
            let (x0, x1) = (self.row_start[y], self.row_end[y]);
            if x0 < x1 {
                self.out[y * rect.width + x0..y * rect.width + x1].fill(0);
            }
            self.row_start[y] = usize::MAX;
            self.row_end[y] = 0;
        }
    }

    fn ensure_capacity(&mut self, w: usize, h: usize) {
        if w <= self.cap_w && h <= self.cap_h {
            return;
        }
        self.cap_w = self.cap_w.max(w.next_multiple_of(64));
        self.cap_h = self.cap_h.max(h.next_multiple_of(64));
        self.accum = vec![0; self.cap_w * self.cap_h * 4];
        self.out = vec![0; self.cap_w * self.cap_h];
        self.row_start = vec![usize::MAX; self.cap_h];
        self.row_end = vec![0; self.cap_h];
    }
}

fn add_stamp(stamps: &mut Vec<Stamp>, x: i32, y: i32, w: f32, age: f32) {
    if w < 1e-5 {
        return;
    }
    let n = stamps.len();
    for st in stamps[n.saturating_sub(8)..].iter_mut().rev() {
        if st.x == x && st.y == y {
            st.w += w;
            return;
        }
    }
    stamps.push(Stamp { x, y, w, age });
}

/// Typical time between pointer updates over the last few moves; 0 if it just started or stopped.
fn update_interval(g: &[Point]) -> i64 {
    let mut sum = 0;
    let mut count = 0;
    for k in 1..g.len().min(4) {
        let gap = g[k - 1].t - g[k].t;
        if gap > STOP_GAP_NS {
            break;
        }
        sum += gap;
        count += 1;
    }
    if count == 0 { 0 } else { (sum / count).clamp(1_000_000, 15_000_000) }
}

/// Position on the (newest-first) path at time `t`.
fn path_at(path: &[Point], t: i64) -> (f32, f32) {
    for i in 0..path.len() {
        if path[i].t > t {
            continue;
        }
        if i == 0 {
            return (path[0].x, path[0].y);
        }
        let (a, b) = (path[i], path[i - 1]);
        let f = (t - a.t) as f32 / (b.t - a.t).max(1) as f32;
        return (a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f);
    }
    let last = path[path.len() - 1];
    (last.x, last.y)
}

/// 5-tap moving average, clamped at the ends.
fn smooth(a: &mut [f32], tmp: &mut Vec<f32>) {
    let n = a.len();
    tmp.clear();
    for i in 0..n {
        let mut sum = 0.0;
        for o in -2i32..=2 {
            sum += a[(i as i32 + o).clamp(0, n as i32 - 1) as usize];
        }
        tmp.push(sum * 0.2);
    }
    a.copy_from_slice(tmp);
}

fn smoothstep(lo: f32, hi: f32, v: f32) -> f32 {
    if hi <= lo {
        return if v >= lo { 1.0 } else { 0.0 };
    }
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Saturated colour for a hue in turns (wraps).
fn hue(h: f32) -> [u8; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let (s, v) = (0.75f32, 1.0f32);
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match i as i32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    let q8 = |c: f32| (c * 255.0 + 0.5) as u8;
    [q8(r), q8(g), q8(b)]
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;

    /// 400 ms of samples at 1 kHz from a path function of time in seconds (0 = now).
    fn samples(f: impl Fn(f64) -> (f32, f32)) -> Vec<MotionSample> {
        (0..400)
            .map(|k| {
                let (x, y) = f(-(k as f64) / 1000.0);
                MotionSample { t: 1000 * MS - k * MS, x: x.round(), y: y.round() }
            })
            .collect()
    }

    fn assert_valid(frame: &Frame) {
        for &p in frame.pixels {
            let a = p >> 24;
            assert!((p >> 16) & 0xFF <= a && (p >> 8) & 0xFF <= a && p & 0xFF <= a, "not premultiplied: {p:08x}");
        }
    }

    #[test]
    fn still_pointer_draws_nothing() {
        let mut c = Composer::new();
        let s = samples(|_| (100.0, 100.0));
        assert!(c.compose(&s, 1000 * MS, &CursorImage::arrow(1.0), &TrailParams::default()).is_none());
    }

    #[test]
    fn fast_swipe_covers_the_shutter_distance() {
        let mut c = Composer::new();
        let img = CursorImage::arrow(1.0);
        let s = samples(|t| ((500.0 + 3000.0 * t) as f32, 200.0));
        let rect = c.compose(&s, 1000 * MS, &img, &TrailParams::default()).expect("trail");
        // 50 ms at 3000 px/s = 150 px of travel, plus the cursor's own width.
        assert!((150..190).contains(&rect.width), "width {}", rect.width);
        assert!(rect.x + rect.width as i32 >= 500, "trail should reach the pointer");
        let frame = c.frame().unwrap();
        assert_valid(&frame);
        assert!(frame.pixels.iter().any(|p| p >> 24 > 40), "trail should be visible");
        c.release();
        assert!(c.out.iter().all(|&p| p == 0), "release must clear the output");
    }

    #[test]
    fn slow_drag_stays_crisp() {
        let mut c = Composer::new();
        let s = samples(|t| ((300.0 + 100.0 * t) as f32, 100.0));
        assert!(c.compose(&s, 1000 * MS, &CursorImage::arrow(1.0), &TrailParams::default()).is_none());
    }

    #[test]
    fn lead_moves_the_head_forward() {
        let img = CursorImage::arrow(1.0);
        let s = samples(|t| ((500.0 + 2000.0 * t) as f32, 200.0));
        let mut c = Composer::new();
        let plain = c.compose(&s, 1000 * MS, &img, &TrailParams::default()).unwrap();
        let led = c.compose(&s, 1000 * MS, &img, &TrailParams { lead: 0.02, ..TrailParams::default() }).unwrap();
        let (end_plain, end_led) = (plain.x + plain.width as i32, led.x + led.width as i32);
        assert!((end_led - end_plain - 40).abs() <= 3, "expected ~40 px lead, got {}", end_led - end_plain);
    }

    #[test]
    fn tints_recolour_the_trail() {
        let img = CursorImage::arrow(1.0);
        let s = samples(|t| ((500.0 + 3000.0 * t) as f32, 200.0));
        let mut c = Composer::new();
        let params = TrailParams { tint: Tint::Solid([255, 0, 0]), ..TrailParams::default() };
        c.compose(&s, 1000 * MS, &img, &params).unwrap();
        let frame = c.frame().unwrap();
        assert_valid(&frame);
        let brightest = frame.pixels.iter().copied().max_by_key(|p| p >> 24).unwrap();
        assert!((brightest >> 16) & 0xFF > 0 && brightest & 0xFF == 0, "expected red, got {brightest:08x}");

        let params = TrailParams { tint: Tint::Rainbow { phase: 0.0 }, ..TrailParams::default() };
        c.compose(&s, 1000 * MS, &img, &params).unwrap();
        assert_valid(&c.frame().unwrap());
    }

    #[test]
    fn low_rate_mouse_still_smears() {
        let img = CursorImage::arrow(1.0);
        // 125 Hz updates: positions only change every 8 ms.
        let s = samples(|t| {
            let t = (t * 125.0).floor() / 125.0;
            ((500.0 + 2500.0 * t) as f32, 200.0)
        });
        let mut c = Composer::new();
        let rect = c.compose(&s, 1000 * MS, &img, &TrailParams::default()).expect("trail");
        assert!(rect.width > 100);
    }
}
