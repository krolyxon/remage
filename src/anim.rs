//! Retargetable ease-out tweens; callers redraw only while one is active.

use std::time::{Duration, Instant};

/// Values a [`Tween`] can interpolate.
pub trait Lerp: Copy {
    fn lerp(a: Self, b: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(a: Self, b: Self, t: f32) -> Self {
        a + (b - a) * t
    }
}

impl Lerp for [f32; 2] {
    fn lerp(a: Self, b: Self, t: f32) -> Self {
        [f32::lerp(a[0], b[0], t), f32::lerp(a[1], b[1], t)]
    }
}

/// The Nothing ease-out curve, `cubic-bezier(0.25, 0.1, 0.25, 1)`.
pub fn ease(t: f32) -> f32 {
    const X1: f32 = 0.25;
    const Y1: f32 = 0.1;
    const X2: f32 = 0.25;
    const Y2: f32 = 1.0;
    let t = t.clamp(0.0, 1.0);
    let bez = |a: f32, b: f32, s: f32| {
        let inv = 1.0 - s;
        3.0 * inv * inv * s * a + 3.0 * inv * s * s * b + s * s * s
    };
    // Solve x(s) = t for s with Newton steps, falling back to bisection.
    let mut s = t;
    for _ in 0..8 {
        let x = bez(X1, X2, s) - t;
        if x.abs() < 1e-5 {
            return bez(Y1, Y2, s);
        }
        let inv = 1.0 - s;
        let dx = 3.0 * inv * inv * X1 + 6.0 * inv * s * (X2 - X1) + 3.0 * s * s * (1.0 - X2);
        if dx.abs() < 1e-6 {
            break;
        }
        s = (s - x / dx).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..24 {
        s = (lo + hi) * 0.5;
        if bez(X1, X2, s) < t {
            lo = s;
        } else {
            hi = s;
        }
    }
    bez(Y1, Y2, s)
}

#[derive(Debug, Clone, Copy)]
pub struct Tween<T: Lerp> {
    from: T,
    to: T,
    start: Instant,
    dur: Duration,
}

impl<T: Lerp> Tween<T> {
    pub fn new(value: T, now: Instant) -> Self {
        Self {
            from: value,
            to: value,
            start: now,
            dur: Duration::ZERO,
        }
    }

    pub fn value(&self, now: Instant) -> T {
        if self.dur.is_zero() {
            return self.to;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32() / self.dur.as_secs_f32();
        if t >= 1.0 {
            self.to
        } else {
            T::lerp(self.from, self.to, ease(t))
        }
    }

    /// Where the tween ends up.
    pub fn target(&self) -> T {
        self.to
    }

    pub fn is_active(&self, now: Instant) -> bool {
        !self.dur.is_zero() && now < self.start + self.dur
    }

    /// Animate from the current value to `to`; a zero duration jumps.
    pub fn retarget(&mut self, to: T, now: Instant, dur: Duration) {
        self.from = self.value(now);
        self.to = to;
        self.start = now;
        self.dur = dur;
    }

    /// Animate between explicit endpoints.
    pub fn animate(&mut self, from: T, to: T, now: Instant, dur: Duration) {
        self.from = from;
        self.to = to;
        self.start = now;
        self.dur = dur;
    }

    pub fn set(&mut self, value: T) {
        self.from = value;
        self.to = value;
        self.dur = Duration::ZERO;
    }
}

impl Tween<[f32; 2]> {
    /// Move both endpoints, keeping any animation in flight.
    pub fn shift(&mut self, d: [f32; 2]) {
        self.from = [self.from[0] + d[0], self.from[1] + d[1]];
        self.to = [self.to[0] + d[0], self.to[1] + d[1]];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_endpoints_and_monotonic() {
        assert!(ease(0.0).abs() < 1e-4);
        assert!((ease(1.0) - 1.0).abs() < 1e-4);
        let mut last = 0.0;
        for i in 1..=100 {
            let v = ease(i as f32 / 100.0);
            assert!(v >= last - 1e-5);
            last = v;
        }
        // CSS `ease` is well past linear at the midpoint.
        assert!(ease(0.5) > 0.75);
    }

    #[test]
    fn retarget_starts_from_current_value() {
        let t0 = Instant::now();
        let mut tw = Tween::new(0.0f32, t0);
        tw.retarget(10.0, t0, Duration::from_millis(100));
        let mid = t0 + Duration::from_millis(50);
        let v = tw.value(mid);
        assert!(v > 0.0 && v < 10.0);
        tw.retarget(0.0, mid, Duration::from_millis(100));
        assert!((tw.value(mid) - v).abs() < 1e-5);
        assert_eq!(tw.value(mid + Duration::from_millis(200)), 0.0);
        assert!(!tw.is_active(mid + Duration::from_millis(200)));
    }
}
