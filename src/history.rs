//! Recent pointer positions, written by the sampler thread and read by the renderer.

use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// One pointer reading. `t` is nanoseconds on the process clock (see [`now_ns`]); positions are in
/// overlay pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionSample {
    pub t: i64,
    pub x: f32,
    pub y: f32,
}

/// Monotonic nanoseconds since the first call in this process.
pub fn now_ns() -> i64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as i64
}

struct Ring {
    buf: Vec<MotionSample>,
    next: usize,
    len: usize,
    moves: u64,
}

/// Fixed-size ring buffer of samples with a "pointer moved" signal.
pub struct MotionHistory {
    ring: Mutex<Ring>,
    moved: Condvar,
}

impl MotionHistory {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(2);
        MotionHistory {
            ring: Mutex::new(Ring {
                buf: vec![MotionSample { t: 0, x: 0.0, y: 0.0 }; capacity],
                next: 0,
                len: 0,
                moves: 0,
            }),
            moved: Condvar::new(),
        }
    }

    pub fn push(&self, s: MotionSample) {
        let mut r = self.ring.lock().unwrap();
        let moved = if r.len == 0 {
            true
        } else {
            let last = r.buf[(r.next + r.buf.len() - 1) % r.buf.len()];
            last.x != s.x || last.y != s.y
        };
        let next = r.next;
        r.buf[next] = s;
        r.next = (next + 1) % r.buf.len();
        r.len = (r.len + 1).min(r.buf.len());
        if moved {
            r.moves += 1;
            drop(r);
            self.moved.notify_all();
        }
    }

    pub fn clear(&self) {
        self.ring.lock().unwrap().len = 0;
    }

    /// Copies samples newest-first back to `since`, plus one older sample so the start of the
    /// range can be interpolated.
    pub fn copy_since(&self, since: i64, out: &mut Vec<MotionSample>) {
        out.clear();
        let r = self.ring.lock().unwrap();
        let cap = r.buf.len();
        let mut idx = r.next;
        for _ in 0..r.len {
            idx = (idx + cap - 1) % cap;
            let s = r.buf[idx];
            out.push(s);
            if s.t < since {
                break;
            }
        }
    }

    /// Blocks until the pointer moves or the timeout passes.
    pub fn wait_for_motion(&self, timeout: Duration) {
        let r = self.ring.lock().unwrap();
        let seen = r.moves;
        let _unused = self.moved.wait_timeout_while(r, timeout, |r| r.moves == seen).unwrap();
    }

    /// Wakes anything blocked in [`wait_for_motion`](Self::wait_for_motion), e.g. on shutdown.
    pub fn wake(&self) {
        self.ring.lock().unwrap().moves += 1;
        self.moved.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_since_is_newest_first_with_one_older_sample() {
        let h = MotionHistory::new(8);
        for i in 0..20 {
            h.push(MotionSample { t: i * 10, x: i as f32, y: 0.0 });
        }
        let mut out = Vec::new();
        h.copy_since(155, &mut out);
        let ts: Vec<i64> = out.iter().map(|s| s.t).collect();
        assert_eq!(ts, vec![190, 180, 170, 160, 150]);
    }

    #[test]
    fn ring_keeps_only_capacity() {
        let h = MotionHistory::new(4);
        for i in 0..10 {
            h.push(MotionSample { t: i, x: 0.0, y: 0.0 });
        }
        let mut out = Vec::new();
        h.copy_since(i64::MIN, &mut out);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].t, 9);
    }
}
