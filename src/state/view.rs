//! Zoom/pan/fit/rotate/flip maths and their animation targets.
//!
//! Coordinates: the image is centred on the origin of *display space* (image pixels after
//! rotation and flip). `center` is the display-space point shown at the middle of the viewport.
//! A screen point is `viewport_centre + scale * (q - center)` for display-space point `q`.

use std::f32::consts::FRAC_PI_2;
use std::time::{Duration, Instant};

use crate::anim::Tween;

pub const MIN_SCALE: f32 = 0.01;
pub const MAX_SCALE: f32 = 64.0;
/// Magnification above which `filter = auto` switches to nearest.
pub const NEAREST_ABOVE: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum FitMode {
    /// Fit, but never enlarge.
    Down,
    Fit,
    Fill,
    Width,
    Height,
    Original,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    pub fn vector(self) -> [f32; 2] {
        match self {
            Dir::Left => [-1.0, 0.0],
            Dir::Right => [1.0, 0.0],
            Dir::Up => [0.0, -1.0],
            Dir::Down => [0.0, 1.0],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// Maps the unit quad onto the screen: `screen = m * uv + t`, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub m: [f32; 4],
    pub t: [f32; 2],
}

#[derive(Debug, Clone)]
pub struct View {
    img: [f32; 2],
    viewport: [f32; 2],
    fit: Option<FitMode>,
    // Targets.
    scale: f32,
    center: [f32; 2],
    quarter: i32,
    flip: [f32; 2],
    // Displayed values: scale tweens in log space around a fixed screen anchor `d`
    // (offset from the viewport centre), so `center = anchor - d / scale`.
    log_s: Tween<f32>,
    anchor: Tween<[f32; 2]>,
    d: [f32; 2],
    rot: Tween<f32>,
    flip_t: Tween<[f32; 2]>,
}

fn add(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn mul(a: [f32; 2], s: f32) -> [f32; 2] {
    [a[0] * s, a[1] * s]
}

impl View {
    pub fn new(now: Instant) -> Self {
        Self {
            img: [1.0, 1.0],
            viewport: [1.0, 1.0],
            fit: Some(FitMode::Down),
            scale: 1.0,
            center: [0.0, 0.0],
            quarter: 0,
            flip: [1.0, 1.0],
            log_s: Tween::new(0.0, now),
            anchor: Tween::new([0.0, 0.0], now),
            d: [0.0, 0.0],
            rot: Tween::new(0.0, now),
            flip_t: Tween::new([1.0, 1.0], now),
        }
    }

    /// Show a new image: rotation and flips reset, then `fit` or a fixed `zoom` applies.
    pub fn reset(&mut self, img: [u32; 2], fit: FitMode, zoom: Option<f32>, now: Instant) {
        self.img = [img[0].max(1) as f32, img[1].max(1) as f32];
        self.quarter = 0;
        self.flip = [1.0, 1.0];
        self.rot.set(0.0);
        self.flip_t.set([1.0, 1.0]);
        match zoom {
            Some(z) => {
                self.fit = None;
                self.scale = z.clamp(MIN_SCALE, MAX_SCALE);
            }
            None => {
                self.fit = Some(fit);
                self.scale = self.fit_scale(fit);
            }
        }
        self.center = self.clamp_center([0.0, 0.0], self.scale);
        self.jump();
        let _ = now;
    }

    /// The image area changed; refit if a fit mode is active, without animating.
    pub fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = [w.max(1.0), h.max(1.0)];
        if let Some(fit) = self.fit {
            self.scale = self.fit_scale(fit);
        }
        self.center = self.clamp_center(self.center, self.scale);
        self.jump();
    }

    pub fn viewport(&self) -> [f32; 2] {
        self.viewport
    }

    #[cfg(test)]
    pub fn fit_mode(&self) -> Option<FitMode> {
        self.fit
    }

    /// Target scale (1.0 = one image pixel per physical pixel).
    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn display_scale(&self, now: Instant) -> f32 {
        self.log_s.value(now).exp()
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.log_s.is_active(now)
            || self.anchor.is_active(now)
            || self.rot.is_active(now)
            || self.flip_t.is_active(now)
    }

    pub fn set_fit(&mut self, fit: FitMode, now: Instant, dur: Duration) {
        self.fit = Some(fit);
        let s = self.fit_scale(fit);
        let c = self.clamp_center(self.center, s);
        self.animate_to(s, c, [0.0, 0.0], now, dur);
    }

    /// Zoom by `factor` around `anchor` (viewport pixels), or the centre when `None`.
    pub fn zoom_by(&mut self, factor: f32, anchor: Option<[f32; 2]>, now: Instant, dur: Duration) {
        self.zoom_to(self.scale * factor, anchor, now, dur);
    }

    pub fn zoom_to(&mut self, scale: f32, anchor: Option<[f32; 2]>, now: Instant, dur: Duration) {
        let d = match anchor {
            Some(a) => sub(a, mul(self.viewport, 0.5)),
            None => [0.0, 0.0],
        };
        let s1 = scale.clamp(MIN_SCALE, MAX_SCALE);
        let p = add(self.center, mul(d, 1.0 / self.scale));
        let c1 = self.clamp_center(sub(p, mul(d, 1.0 / s1)), s1);
        self.fit = None;
        self.animate_to(s1, c1, d, now, dur);
    }

    /// Move the image by a screen-space delta immediately (drag, held keys).
    pub fn pan_pixels(&mut self, delta: [f32; 2]) {
        let c1 = self.clamp_center(sub(self.center, mul(delta, 1.0 / self.scale)), self.scale);
        let moved = sub(c1, self.center);
        self.center = c1;
        self.anchor.shift(moved);
    }

    /// Pan by a fraction of the viewport, animated.
    pub fn pan_step(&mut self, dir: Dir, frac: f32, now: Instant, dur: Duration) {
        let v = dir.vector();
        let step = [
            v[0] * self.viewport[0] * frac,
            v[1] * self.viewport[1] * frac,
        ];
        let c1 = self.clamp_center(add(self.center, mul(step, 1.0 / self.scale)), self.scale);
        self.animate_to(self.scale, c1, [0.0, 0.0], now, dur);
    }

    pub fn pan_edge(&mut self, dir: Dir, now: Instant, dur: Duration) {
        let far = mul(dir.vector(), 1e9);
        let c1 = self.clamp_center(far, self.scale);
        let c1 = match dir {
            Dir::Left | Dir::Right => [c1[0], self.center[1]],
            Dir::Up | Dir::Down => [self.center[0], c1[1]],
        };
        self.animate_to(self.scale, c1, [0.0, 0.0], now, dur);
    }

    pub fn recenter(&mut self, now: Instant, dur: Duration) {
        let c1 = self.clamp_center([0.0, 0.0], self.scale);
        self.animate_to(self.scale, c1, [0.0, 0.0], now, dur);
    }

    /// Rotate clockwise on screen by `quarters` quarter turns (negative = counter-clockwise).
    pub fn rotate(&mut self, quarters: i32, now: Instant, dur: Duration) {
        // A single mirror reverses the visual sense of the rotation angle.
        let sign = if self.flip[0] * self.flip[1] < 0.0 {
            -1
        } else {
            1
        };
        self.quarter += sign * quarters;
        let mut c = self.center;
        for _ in 0..quarters.rem_euclid(4) {
            c = [-c[1], c[0]];
        }
        let s = match self.fit {
            Some(fit) => self.fit_scale(fit),
            None => self.scale,
        };
        let c = self.clamp_center(c, s);
        self.rot.retarget(self.quarter as f32 * FRAC_PI_2, now, dur);
        self.animate_to(s, c, [0.0, 0.0], now, dur);
    }

    pub fn flip(&mut self, axis: Axis, now: Instant, dur: Duration) {
        let i = match axis {
            Axis::Horizontal => 0,
            Axis::Vertical => 1,
        };
        self.flip[i] = -self.flip[i];
        let mut c = self.center;
        c[i] = -c[i];
        self.flip_t.retarget(self.flip, now, dur);
        self.animate_to(self.scale, c, [0.0, 0.0], now, dur);
    }

    /// The transform for the frame at `now`.
    pub fn transform(&self, now: Instant) -> Affine {
        let s = self.display_scale(now);
        let c = sub(self.anchor.value(now), mul(self.d, 1.0 / s));
        let (sin, cos) = self.rot.value(now).sin_cos();
        let f = self.flip_t.value(now);
        // M = F · R(θ) · diag(img) · s
        let m = [
            f[0] * cos * self.img[0] * s,
            -f[0] * sin * self.img[1] * s,
            f[1] * sin * self.img[0] * s,
            f[1] * cos * self.img[1] * s,
        ];
        let vc = mul(self.viewport, 0.5);
        let t = [
            vc[0] - s * c[0] - 0.5 * (m[0] + m[1]),
            vc[1] - s * c[1] - 0.5 * (m[2] + m[3]),
        ];
        Affine { m, t }
    }

    fn display_dims(&self) -> [f32; 2] {
        if self.quarter.rem_euclid(2) == 1 {
            [self.img[1], self.img[0]]
        } else {
            self.img
        }
    }

    fn fit_scale(&self, fit: FitMode) -> f32 {
        let [dw, dh] = self.display_dims();
        let [vw, vh] = self.viewport;
        let fit_s = (vw / dw).min(vh / dh);
        let s = match fit {
            FitMode::Down => fit_s.min(1.0),
            FitMode::Fit => fit_s,
            FitMode::Fill => (vw / dw).max(vh / dh),
            FitMode::Width => vw / dw,
            FitMode::Height => vh / dh,
            FitMode::Original => 1.0,
        };
        s.clamp(MIN_SCALE, MAX_SCALE)
    }

    /// Keep the image covering the viewport where it is larger, and centred where smaller.
    fn clamp_center(&self, c: [f32; 2], s: f32) -> [f32; 2] {
        let dims = self.display_dims();
        let mut out = c;
        for i in 0..2 {
            let extent = (dims[i] - self.viewport[i] / s) * 0.5;
            out[i] = if extent <= 0.0 {
                0.0
            } else {
                c[i].clamp(-extent, extent)
            };
        }
        out
    }

    fn animate_to(&mut self, s1: f32, c1: [f32; 2], d: [f32; 2], now: Instant, dur: Duration) {
        let s0 = self.display_scale(now);
        let c0 = sub(self.anchor.value(now), mul(self.d, 1.0 / s0));
        let p0 = add(c0, mul(d, 1.0 / s0));
        let p1 = add(c1, mul(d, 1.0 / s1));
        self.log_s.animate(s0.ln(), s1.ln(), now, dur);
        self.anchor.animate(p0, p1, now, dur);
        self.d = d;
        self.scale = s1;
        self.center = c1;
    }

    fn jump(&mut self) {
        self.log_s.set(self.scale.ln());
        self.anchor.set(self.center);
        self.d = [0.0, 0.0];
        self.rot.set(self.quarter as f32 * FRAC_PI_2);
        self.flip_t.set(self.flip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Duration = Duration::from_millis(100);

    fn view(img: [u32; 2], vp: [f32; 2], fit: FitMode) -> (View, Instant) {
        let now = Instant::now();
        let mut v = View::new(now);
        v.set_viewport(vp[0], vp[1]);
        v.reset(img, fit, None, now);
        (v, now)
    }

    fn apply(a: Affine, u: [f32; 2]) -> [f32; 2] {
        [
            a.m[0] * u[0] + a.m[1] * u[1] + a.t[0],
            a.m[2] * u[0] + a.m[3] * u[1] + a.t[1],
        ]
    }

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01
    }

    #[test]
    fn fit_modes() {
        let vp = [1000.0, 500.0];
        assert_eq!(view([2000, 2000], vp, FitMode::Fit).0.scale(), 0.25);
        assert_eq!(view([2000, 2000], vp, FitMode::Fill).0.scale(), 0.5);
        assert_eq!(view([2000, 2000], vp, FitMode::Width).0.scale(), 0.5);
        assert_eq!(view([2000, 2000], vp, FitMode::Height).0.scale(), 0.25);
        assert_eq!(view([100, 100], vp, FitMode::Down).0.scale(), 1.0);
        assert_eq!(view([100, 100], vp, FitMode::Fit).0.scale(), 5.0);
        assert_eq!(view([100, 100], vp, FitMode::Original).0.scale(), 1.0);
    }

    #[test]
    fn fitted_image_is_centred() {
        let (v, now) = view([200, 100], [400.0, 400.0], FitMode::Fit);
        let a = v.transform(now);
        assert!(close(apply(a, [0.0, 0.0]), [0.0, 100.0]));
        assert!(close(apply(a, [1.0, 1.0]), [400.0, 300.0]));
    }

    #[test]
    fn zoom_keeps_anchor_fixed_throughout() {
        let (mut v, now) = view([4000, 4000], [1000.0, 1000.0], FitMode::Fit);
        let cursor = [700.0, 300.0];
        let a0 = v.transform(now);
        // Image uv under the cursor before zooming.
        let uv = [
            (cursor[0] - a0.t[0]) / a0.m[0],
            (cursor[1] - a0.t[1]) / a0.m[3],
        ];
        v.zoom_by(2.0, Some(cursor), now, D);
        for ms in [0, 20, 50, 80, 100, 150] {
            let a = v.transform(now + Duration::from_millis(ms));
            assert!(close(apply(a, uv), cursor), "drifted at {ms} ms");
        }
        assert!(v.fit_mode().is_none());
    }

    #[test]
    fn rotation_swaps_fit_dimensions() {
        let (mut v, now) = view([200, 100], [400.0, 400.0], FitMode::Fit);
        assert_eq!(v.scale(), 2.0);
        v.rotate(1, now, Duration::ZERO);
        assert_eq!(v.scale(), 2.0);
        let (mut v, now) = view([400, 100], [400.0, 200.0], FitMode::Fit);
        assert_eq!(v.scale(), 1.0);
        v.rotate(1, now, Duration::ZERO);
        assert_eq!(v.scale(), 0.5);
        // A clockwise turn puts the image's top-left corner at the top-right.
        let a = v.transform(now);
        assert!(close(apply(a, [0.0, 0.0]), [225.0, 0.0]));
    }

    #[test]
    fn pan_is_clamped_to_edges() {
        let (mut v, now) = view([1000, 1000], [500.0, 500.0], FitMode::Original);
        v.pan_pixels([10_000.0, 0.0]);
        let a = v.transform(now);
        // Dragging right as far as possible shows the left edge.
        assert!(close(apply(a, [0.0, 0.0]), [0.0, -250.0]));
        v.pan_edge(Dir::Right, now, Duration::ZERO);
        let a = v.transform(now);
        assert!(close(apply(a, [1.0, 1.0]), [500.0, 750.0]));
    }

    #[test]
    fn flip_mirrors_on_screen() {
        let (mut v, now) = view([200, 100], [200.0, 100.0], FitMode::Fit);
        v.flip(Axis::Horizontal, now, Duration::ZERO);
        let a = v.transform(now);
        assert!(close(apply(a, [0.0, 0.0]), [200.0, 0.0]));
    }
}
