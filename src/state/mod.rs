//! Pure application state: no winit or wgpu types, so everything here is unit-testable.

pub mod grid;
pub mod queue;
pub mod view;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::action::{Action, Zoom};
use crate::anim::Tween;
use queue::Queue;
use view::{Affine, Dir, FitMode, View};

/// `[LOADING]` appears only once a load has taken this long, so it never flickers.
pub const LOADING_DELAY: Duration = Duration::from_millis(120);
/// A tap on a pan key moves for at least this long.
const MIN_PAN_HOLD: Duration = Duration::from_millis(120);
/// Held-key pan speed, in viewports per second.
const PAN_SPEED: f32 = 1.25;
/// Logical height of the status bar.
pub const BAR_HEIGHT: f32 = 32.0;

#[derive(Clone, Debug)]
pub struct Settings {
    pub fit: FitMode,
    pub zoom: Option<f32>,
    pub zoom_step: f32,
    pub wrap: bool,
    pub preload: usize,
    pub anim: Duration,
    pub crossfade: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fit: FitMode::Down,
            zoom: None,
            zoom_step: 1.25,
            wrap: true,
            preload: 2,
            anim: Duration::from_millis(180),
            crossfade: Duration::from_millis(100),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaBg {
    Dots,
    Checker,
    Solid,
}

impl AlphaBg {
    fn next(self) -> Self {
        match self {
            AlphaBg::Dots => AlphaBg::Checker,
            AlphaBg::Checker => AlphaBg::Solid,
            AlphaBg::Solid => AlphaBg::Dots,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsgKind {
    Info,
    Error,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub text: String,
    pub kind: MsgKind,
    pub until: Instant,
}

/// The image currently on screen (which may lag the queue's current file while it loads).
#[derive(Clone, Debug)]
pub struct Shown {
    pub path: PathBuf,
    /// Pixel size before any downscale for the GPU.
    pub size: [u32; 2],
    pub format: &'static str,
}

/// The previous image fading out under the new one.
#[derive(Clone, Debug)]
pub struct Fade {
    pub path: PathBuf,
    pub affine: Affine,
    pub alpha: Tween<f32>,
}

#[derive(Clone, Copy, Debug)]
struct Hold {
    dir: Dir,
    pressed: Instant,
    released: Option<Instant>,
}

/// What the event loop must do after an action.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Effects {
    pub redraw: bool,
    /// The queue's current file changed: load it and refresh the preload window.
    pub navigated: bool,
    /// The status bar appeared or vanished, so the image area changed.
    pub layout: bool,
    pub fullscreen: bool,
    pub reload: bool,
    pub quit: bool,
}

#[derive(Debug)]
pub struct AppState {
    pub queue: Queue,
    pub view: View,
    pub settings: Settings,
    pub bar: bool,
    /// `None` = automatic (nearest above 300%), otherwise forced nearest/linear.
    pub nearest: Option<bool>,
    pub alpha_bg: AlphaBg,
    pub shown: Option<Shown>,
    pub fade: Option<Fade>,
    pub loading_since: Option<Instant>,
    pub skipped: usize,
    pub message: Option<Message>,
    /// Keys typed so far of an incomplete sequence.
    pub pending: String,
    window: [f32; 2],
    scale_factor: f32,
    holds: Vec<Hold>,
    last_tick: Option<Instant>,
}

impl AppState {
    pub fn new(queue: Queue, settings: Settings, bar: bool, now: Instant) -> Self {
        Self {
            queue,
            view: View::new(now),
            settings,
            bar,
            nearest: None,
            alpha_bg: AlphaBg::Dots,
            shown: None,
            fade: None,
            loading_since: None,
            skipped: 0,
            message: None,
            pending: String::new(),
            window: [1.0, 1.0],
            scale_factor: 1.0,
            holds: Vec::new(),
            last_tick: None,
        }
    }

    /// Window size in physical pixels.
    pub fn set_window(&mut self, w: f32, h: f32, scale_factor: f32) {
        self.window = [w, h];
        self.scale_factor = scale_factor;
        self.update_viewport();
    }

    pub fn window(&self) -> [f32; 2] {
        self.window
    }

    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    /// Physical height of the status bar, 0 when hidden.
    pub fn bar_height(&self) -> f32 {
        if self.bar {
            (BAR_HEIGHT * self.scale_factor).round()
        } else {
            0.0
        }
    }

    fn update_viewport(&mut self) {
        let h = (self.window[1] - self.bar_height()).max(1.0);
        self.view.set_viewport(self.window[0], h);
    }

    fn dur(&self) -> Duration {
        self.settings.anim
    }

    pub fn apply(&mut self, action: Action, now: Instant) -> Effects {
        let mut fx = Effects {
            redraw: true,
            ..Effects::default()
        };
        let dur = self.dur();
        let has_image = self.shown.is_some();
        match action {
            Action::Next(n) => fx.navigated = self.queue.step(n as isize, self.settings.wrap),
            Action::Prev(n) => fx.navigated = self.queue.step(-(n as isize), self.settings.wrap),
            Action::First => fx.navigated = self.queue.goto(0),
            Action::Last => fx.navigated = self.queue.goto(usize::MAX),
            Action::Goto(n) => fx.navigated = self.queue.goto(n.saturating_sub(1)),
            Action::Zoom(z) if has_image => {
                let step = self.settings.zoom_step;
                match z {
                    Zoom::In => self.view.zoom_by(step, None, now, dur),
                    Zoom::Out => self.view.zoom_by(1.0 / step, None, now, dur),
                    Zoom::Percent(p) => self.view.zoom_to(p / 100.0, None, now, dur),
                }
            }
            Action::Fit(fit) if has_image => self.view.set_fit(fit, now, dur),
            Action::Pan(dir) if has_image => self.view.pan_step(dir, 0.2, now, dur),
            Action::PanEdge(dir) if has_image => self.view.pan_edge(dir, now, dur),
            Action::Center if has_image => self.view.recenter(now, dur),
            Action::Rotate(q) if has_image => self.view.rotate(q, now, dur),
            Action::Flip(axis) if has_image => self.view.flip(axis, now, dur),
            Action::FilterToggle => {
                let nearest = !self.effective_nearest(now);
                self.nearest = Some(nearest);
                self.notify(
                    if nearest { "NEAREST" } else { "LINEAR" },
                    MsgKind::Info,
                    now,
                );
            }
            Action::AlphaBgCycle => self.alpha_bg = self.alpha_bg.next(),
            Action::Fullscreen => fx.fullscreen = true,
            Action::BarToggle => {
                self.bar = !self.bar;
                self.update_viewport();
                fx.layout = true;
            }
            Action::Reload => fx.reload = true,
            Action::Remove => {
                if let Some(cur) = self.queue.current().map(Path::to_path_buf) {
                    self.queue.remove(&cur);
                    fx.navigated = true;
                }
            }
            Action::Cancel => self.message = None,
            Action::Quit => fx.quit = true,
            Action::Zoom(_)
            | Action::Fit(_)
            | Action::Pan(_)
            | Action::PanEdge(_)
            | Action::Center
            | Action::Rotate(_)
            | Action::Flip(_) => fx.redraw = false,
        }
        fx
    }

    /// Start or stop a held-key pan.
    pub fn hold_pan(&mut self, dir: Dir, held: bool, now: Instant) {
        if held {
            if !self
                .holds
                .iter()
                .any(|h| h.dir == dir && h.released.is_none())
            {
                self.holds.retain(|h| h.dir != dir);
                self.holds.push(Hold {
                    dir,
                    pressed: now,
                    released: None,
                });
            }
        } else {
            for h in self.holds.iter_mut().filter(|h| h.dir == dir) {
                h.released.get_or_insert(now);
            }
        }
    }

    pub fn stop_all_pans(&mut self) {
        self.holds.clear();
    }

    /// Advance time-driven state (held pans). Call once per frame before drawing.
    pub fn advance(&mut self, now: Instant) {
        let last = self.last_tick.replace(now).unwrap_or(now);
        if self.holds.is_empty() || self.shown.is_none() {
            self.holds.retain(|h| h.released.is_none());
            return;
        }
        let vp = self.view.viewport();
        let mut delta = [0.0f32; 2];
        for h in &self.holds {
            let end = h.released.map(|r| r.max(h.pressed + MIN_PAN_HOLD));
            let from = last.max(h.pressed);
            let to = end.map_or(now, |e| e.min(now));
            let dt = to.saturating_duration_since(from).as_secs_f32();
            let v = h.dir.vector();
            delta[0] -= v[0] * vp[0] * PAN_SPEED * dt;
            delta[1] -= v[1] * vp[1] * PAN_SPEED * dt;
        }
        self.holds.retain(|h| {
            h.released
                .is_none_or(|r| now < r.max(h.pressed + MIN_PAN_HOLD))
        });
        if delta != [0.0, 0.0] {
            self.view.pan_pixels(delta);
        }
    }

    /// The image for the current file is ready to show.
    /// `size` is the original pixel size; the texture may be smaller (huge images), but the
    /// view works in original pixels so zoom percentages stay true.
    pub fn show(&mut self, path: &Path, size: [u32; 2], format: &'static str, now: Instant) {
        if let Some(prev) = self.shown.take()
            && prev.path != path
            && !self.settings.crossfade.is_zero()
        {
            let mut alpha = Tween::new(0.0, now);
            alpha.retarget(1.0, now, self.settings.crossfade);
            let affine = self.view.transform(now);
            self.fade = Some(Fade {
                path: prev.path,
                affine,
                alpha,
            });
        }
        self.shown = Some(Shown {
            path: path.to_path_buf(),
            size,
            format,
        });
        self.view
            .reset(size, self.settings.fit, self.settings.zoom, now);
        self.loading_since = None;
        self.holds.clear();
    }

    /// The current file can't be shown (or the queue emptied).
    pub fn clear_shown(&mut self) {
        self.shown = None;
        self.fade = None;
        self.loading_since = None;
    }

    /// Zoom by `steps` zoom steps around a viewport point (mouse wheel).
    pub fn zoom_at(&mut self, steps: f32, anchor: [f32; 2], now: Instant) {
        if self.shown.is_some() {
            let factor = self.settings.zoom_step.powf(steps);
            self.view
                .zoom_by(factor, Some(anchor), now, self.settings.anim);
        }
    }

    pub fn notify(&mut self, text: impl Into<String>, kind: MsgKind, now: Instant) {
        let secs = if kind == MsgKind::Error { 5 } else { 2 };
        self.message = Some(Message {
            text: text.into(),
            kind,
            until: now + Duration::from_secs(secs),
        });
    }

    pub fn effective_nearest(&self, now: Instant) -> bool {
        self.nearest
            .unwrap_or_else(|| self.view.display_scale(now) > view::NEAREST_ABOVE)
    }

    pub fn loading_visible(&self, now: Instant) -> bool {
        self.loading_since.is_some_and(|t| now >= t + LOADING_DELAY)
    }

    /// Whether frames must keep coming (animations, held keys).
    pub fn is_animating(&self, now: Instant) -> bool {
        self.view.is_animating(now)
            || self.fade.as_ref().is_some_and(|f| f.alpha.is_active(now))
            || !self.holds.is_empty()
    }

    /// Expire timed state. Returns whether a redraw is due and the next wake-up time.
    pub fn tick(&mut self, now: Instant) -> (bool, Option<Instant>) {
        let mut redraw = false;
        let mut next: Option<Instant> = None;
        let mut wake = |t: Instant| next = Some(next.map_or(t, |n| n.min(t)));
        if let Some(m) = &self.message {
            if now >= m.until {
                self.message = None;
                redraw = true;
            } else {
                wake(m.until);
            }
        }
        if let Some(t) = self.loading_since {
            let at = t + LOADING_DELAY;
            if now < at {
                wake(at);
            }
        }
        if self.fade.as_ref().is_some_and(|f| !f.alpha.is_active(now)) {
            self.fade = None;
            redraw = true;
        }
        (redraw, next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(n: usize) -> (AppState, Instant) {
        let now = Instant::now();
        let files = (0..n).map(|i| PathBuf::from(format!("{i}.png"))).collect();
        let mut s = AppState::new(Queue::new(files, None, 0), Settings::default(), true, now);
        s.set_window(800.0, 632.0, 1.0);
        (s, now)
    }

    #[test]
    fn navigation_reports_changes() {
        let (mut s, now) = state(3);
        assert!(s.apply(Action::Next(1), now).navigated);
        assert!(s.apply(Action::Last, now).navigated);
        assert!(!s.apply(Action::Last, now).navigated);
        assert!(s.apply(Action::Goto(1), now).navigated);
        assert_eq!(s.queue.index(), 0);
    }

    #[test]
    fn bar_shrinks_the_image_area() {
        let (mut s, now) = state(1);
        assert_eq!(s.view.viewport(), [800.0, 600.0]);
        assert!(s.apply(Action::BarToggle, now).layout);
        assert_eq!(s.view.viewport(), [800.0, 632.0]);
    }

    #[test]
    fn showing_a_second_image_crossfades() {
        let (mut s, now) = state(2);
        s.show(Path::new("0.png"), [10, 10], "PNG", now);
        assert!(s.fade.is_none());
        s.show(Path::new("1.png"), [10, 10], "PNG", now);
        assert!(s.fade.is_some());
        assert!(s.is_animating(now));
        let later = now + Duration::from_secs(1);
        assert!(s.tick(later).0);
        assert!(s.fade.is_none());
    }

    #[test]
    fn tapped_pan_moves_a_minimum_distance() {
        let (mut s, now) = state(1);
        s.show(Path::new("0.png"), [4000, 4000], "PNG", now);
        s.apply(Action::Fit(FitMode::Original), now);
        s.advance(now);
        let before = s.view.transform(now + Duration::from_secs(1)).t;
        s.hold_pan(Dir::Right, true, now);
        s.hold_pan(Dir::Right, false, now + Duration::from_millis(10));
        s.advance(now + Duration::from_millis(500));
        assert!(!s.is_animating(now + Duration::from_secs(1)));
        let after = s.view.transform(now + Duration::from_secs(1)).t;
        let moved = before[0] - after[0];
        let expected = 800.0 * PAN_SPEED * MIN_PAN_HOLD.as_secs_f32();
        assert!(
            (moved - expected).abs() < 1.0,
            "moved {moved}, expected {expected}"
        );
    }
}
