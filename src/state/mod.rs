//! Pure application state: no winit or wgpu types, so everything here is unit-testable.

pub mod fuzzy;
pub mod grid;
pub mod overlay;
pub mod queue;
pub mod view;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::action::{Action, Adjust, ModeArg, Page, Step, Switch, Zoom};
use crate::anim::Tween;
use crate::config::theme::Theme;
use crate::config::{Config, Transition};
use grid::{Grid, Layout};
use overlay::{Edit, Help, Info, Input, InputKind, PaletteItem, WhichRow};
use queue::{Queue, SortKey};
use view::{Affine, Dir, FitMode, View};

/// `[LOADING]` appears only once a load has taken this long, so it never flickers.
pub const LOADING_DELAY: Duration = Duration::from_millis(120);
/// A tap on a pan key moves for at least this long.
const MIN_PAN_HOLD: Duration = Duration::from_millis(120);
/// Held-key pan speed, in viewports per second.
const PAN_SPEED: f32 = 1.25;
/// Logical height of the status bar.
pub const BAR_HEIGHT: f32 = 32.0;
/// Image switch crossfade with `transition = "fade"`.
const CROSSFADE: Duration = Duration::from_millis(100);
/// Theme changes (a new wallpaper palette) crossfade this long.
const THEME_FADE: Duration = Duration::from_millis(250);
/// Thumbnails fade in this long as they arrive.
const THUMB_FADE: Duration = Duration::from_millis(150);
/// Thumbnail names: Space Grotesk at this size.
pub const GRID_NAME_SIZE: f32 = 12.0;
/// Overlay panels: distance from the window edges, inner padding, row height (logical px).
pub const PANEL_MARGIN: f32 = 16.0;
pub const PANEL_PAD: f32 = 16.0;
pub const PANEL_ROW: f32 = 28.0;
/// Help's title line above its rows.
pub const HELP_HEAD: f32 = 36.0;
/// Slideshow delays stay within these bounds.
const SLIDESHOW_MIN: f32 = 0.5;
const SLIDESHOW_MAX: f32 = 3600.0;

/// Which keymap applies and what is on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Image,
    Grid,
    Visual,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Image => "image",
            Mode::Grid => "grid",
            Mode::Visual => "visual",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub fit: FitMode,
    pub zoom: Option<f32>,
    pub zoom_step: f32,
    pub wrap: bool,
    pub preload: usize,
    pub anim: Duration,
    pub crossfade: Duration,
    pub theme_fade: Duration,
    /// UI size multiplier on top of the display scale.
    pub ui_scale: f32,
    pub which_key_delay: Duration,
    pub thumb_fade: Duration,
    /// Logical thumbnail sizes `grid-size` steps through, and the starting one.
    pub grid_sizes: Vec<u32>,
    pub grid_size: u32,
    pub grid_gap: u32,
    pub grid_names: bool,
    pub slideshow_delay: Duration,
    /// Status bar templates.
    pub bar_left: String,
    pub bar_right: String,
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
            crossfade: CROSSFADE,
            theme_fade: THEME_FADE,
            ui_scale: 1.0,
            which_key_delay: Duration::from_millis(300),
            thumb_fade: THUMB_FADE,
            grid_sizes: vec![96, 128, 160, 224, 320],
            grid_size: 160,
            grid_gap: 16,
            grid_names: false,
            slideshow_delay: Duration::from_secs(5),
            bar_left: "{name}".into(),
            bar_right: "{format}  {width}×{height}  {zoom}%  {index}/{count}".into(),
        }
    }
}

impl Settings {
    pub fn from_config(c: &Config) -> Self {
        let on = c.animation.enabled;
        let anim = |d: Duration| if on { d } else { Duration::ZERO };
        Self {
            fit: c.image.fit,
            zoom: None,
            zoom_step: c.image.zoom_step,
            wrap: c.general.wrap,
            preload: c.image.preload,
            anim: anim(Duration::from_millis(c.animation.duration_ms)),
            crossfade: match c.animation.transition {
                Transition::Fade => anim(CROSSFADE),
                Transition::None => Duration::ZERO,
            },
            theme_fade: anim(THEME_FADE),
            ui_scale: c.ui.scale,
            which_key_delay: Duration::from_millis(c.ui.which_key_delay_ms),
            thumb_fade: anim(THUMB_FADE),
            grid_sizes: c.grid.sizes.clone(),
            grid_size: c.grid.size,
            grid_gap: c.grid.gap,
            grid_names: c.grid.names,
            slideshow_delay: Duration::from_secs_f32(
                c.image.slideshow_delay.clamp(SLIDESHOW_MIN, SLIDESHOW_MAX),
            ),
            bar_left: c.ui.statusbar_left.clone(),
            bar_right: c.ui.statusbar_right.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AlphaBg {
    #[default]
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

/// Display adjustments, applied in the image shader.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Adjustments {
    pub gamma: f32,
    /// Added to every channel, -1..1.
    pub brightness: f32,
    /// Scales around mid-grey.
    pub contrast: f32,
}

impl Default for Adjustments {
    fn default() -> Self {
        Self {
            gamma: 1.0,
            brightness: 0.0,
            contrast: 1.0,
        }
    }
}

impl Adjustments {
    /// Apply one step; returns the new value for the status line.
    fn step(&mut self, a: Adjust, s: Step) -> f32 {
        let d = Self::default();
        let (v, reset, up, down, lo, hi) = match a {
            Adjust::Gamma => (&mut self.gamma, d.gamma, 1.1, 1.0 / 1.1, 0.1, 10.0),
            Adjust::Brightness => (&mut self.brightness, d.brightness, 0.05, -0.05, -1.0, 1.0),
            Adjust::Contrast => (&mut self.contrast, d.contrast, 0.1, -0.1, 0.0, 4.0),
        };
        let mul = a == Adjust::Gamma;
        *v = match s {
            Step::Reset => reset,
            Step::Up if mul => *v * up,
            Step::Down if mul => *v * down,
            Step::Up => *v + up,
            Step::Down => *v + down,
        };
        // Snap away float drift so stepping back lands exactly on the default.
        *v = ((*v * 1000.0).round() / 1000.0).clamp(lo, hi);
        *v
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

/// Frames of the image on screen: a timed animation, or the pages of a document.
#[derive(Clone, Debug)]
pub struct Playback {
    pub delays: Vec<Duration>,
    pub animated: bool,
    pub index: usize,
    pub playing: bool,
    /// When the next animation frame is due.
    next: Option<Instant>,
}

impl Playback {
    pub fn len(&self) -> usize {
        self.delays.len()
    }

    fn step(&mut self, delta: isize) {
        let n = self.len() as isize;
        self.index = (self.index as isize + delta).rem_euclid(n) as usize;
    }

    fn delay(&self) -> Duration {
        self.delays[self.index].max(Duration::from_millis(10))
    }
}

/// The previous image fading out under the new one.
#[derive(Clone, Debug)]
pub struct Fade {
    pub path: PathBuf,
    pub affine: Affine,
    pub alpha: Tween<f32>,
}

/// A thumbnail in the atlas, as far as drawing it is concerned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThumbState {
    pub size: [u32; 2],
    /// When it arrived, for the fade-in.
    pub at: Instant,
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
    /// The queue order changed around the current file: refresh the preload window.
    pub reordered: bool,
    /// The status bar appeared or vanished, so the image area changed.
    pub layout: bool,
    /// Image ⇄ grid (or visual) switched.
    pub mode: bool,
    pub fullscreen: Option<Switch>,
    pub reload: bool,
    pub reload_config: bool,
    /// Re-check files on disk: drop missing ones, re-decode changed ones.
    pub refresh: bool,
    /// Re-sort the queue (stat-based keys need the filesystem, so the loop does it).
    pub sort: Option<SortKey>,
    /// Open the palette or help: the loop supplies their rows from the live keymap.
    pub palette: bool,
    pub help: bool,
    /// The info panel opened: load the current file's metadata.
    pub info: bool,
    /// `open <path>` ran: [`AppState::open`] holds the path to scan.
    pub open: bool,
    pub quit: bool,
}

#[derive(Debug)]
pub struct AppState {
    pub queue: Queue,
    pub view: View,
    pub settings: Settings,
    pub mode: Mode,
    pub grid: Grid,
    /// Thumbnails in the atlas (or known to fail), by path.
    pub thumbs: HashMap<PathBuf, ThumbState>,
    last_thumb: Option<Instant>,
    pub theme: Theme,
    /// The previous theme fading out after a reload.
    theme_fade: Option<(Theme, Tween<f32>)>,
    pub bar: bool,
    /// `None` = automatic (nearest above 300%), otherwise forced nearest/linear.
    pub nearest: Option<bool>,
    pub alpha_bg: AlphaBg,
    pub shown: Option<Shown>,
    /// Frames of the shown image, when it has more than one.
    pub playback: Option<Playback>,
    pub fade: Option<Fade>,
    pub loading_since: Option<Instant>,
    pub skipped: usize,
    pub message: Option<Message>,
    /// Keys typed so far of an incomplete sequence.
    pub pending: String,
    /// An inline yes/no question (`DELETE 3 FILES?`) waiting for `y`.
    pub prompt: Option<String>,
    /// User commands still running, oldest first: (job id, name).
    pub running: Vec<(u64, String)>,
    /// The `:` palette or `/` search line.
    pub input: Option<Input>,
    /// Keys that continue the pending sequence, once it has waited `which_key_delay`.
    pub which_key: Option<Vec<WhichRow>>,
    pub help: Option<Help>,
    pub info_open: bool,
    /// Metadata of the current file, once loaded.
    pub info: Option<Info>,
    /// Slideshow on: when the next image is due (`None` until the current one shows).
    pub slideshow: Option<Option<Instant>>,
    pub slideshow_delay: Duration,
    pub adjust: Adjustments,
    /// The path of the last `open`, for the loop to scan.
    pub open: Option<String>,
    /// `ui.info_command` output for a file: its last line fills `{info}`.
    pub info_line: Option<(PathBuf, String)>,
    window: [f32; 2],
    scale_factor: f32,
    holds: Vec<Hold>,
    last_tick: Option<Instant>,
}

impl AppState {
    pub fn new(queue: Queue, settings: Settings, bar: bool, now: Instant) -> Self {
        let slideshow_delay = settings.slideshow_delay;
        Self {
            queue,
            view: View::new(now),
            grid: Grid::new(settings.grid_size, now),
            thumbs: HashMap::new(),
            last_thumb: None,
            settings,
            mode: Mode::Image,
            theme: Theme::nothing_dark(),
            theme_fade: None,
            bar,
            nearest: None,
            alpha_bg: AlphaBg::Dots,
            shown: None,
            playback: None,
            fade: None,
            loading_since: None,
            skipped: 0,
            message: None,
            pending: String::new(),
            prompt: None,
            running: Vec::new(),
            input: None,
            which_key: None,
            help: None,
            info_open: false,
            info: None,
            slideshow: None,
            slideshow_delay,
            adjust: Adjustments::default(),
            open: None,
            info_line: None,
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
        if self.in_grid() {
            self.grid_follow(Instant::now(), false);
        }
    }

    pub fn window(&self) -> [f32; 2] {
        self.window
    }

    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    /// Display scale times `ui.scale`: the factor for every UI size.
    pub fn ui_scale(&self) -> f32 {
        self.scale_factor * self.settings.ui_scale
    }

    /// Physical height of the status bar, 0 when hidden.
    pub fn bar_height(&self) -> f32 {
        if self.bar {
            (BAR_HEIGHT * self.ui_scale()).round()
        } else {
            0.0
        }
    }

    /// Help rows that fit on screen at once.
    pub fn help_page(&self) -> usize {
        let sf = self.ui_scale();
        let h = self.window[1] - self.bar_height() - 2.0 * PANEL_MARGIN * sf;
        let rows = (h - (2.0 * PANEL_PAD + HELP_HEAD) * sf) / (PANEL_ROW * sf);
        rows.floor().max(1.0) as usize
    }

    pub fn in_grid(&self) -> bool {
        matches!(self.mode, Mode::Grid | Mode::Visual)
    }

    /// Side of a thumbnail cell in physical pixels.
    pub fn cell_px(&self) -> u32 {
        (self.grid.size as f32 * self.ui_scale()).round().max(16.0) as u32
    }

    /// This frame's grid geometry.
    pub fn grid_layout(&self) -> Layout {
        let sf = self.ui_scale();
        let label = if self.settings.grid_names {
            ((GRID_NAME_SIZE * 1.25).ceil() + 8.0) * sf
        } else {
            0.0
        };
        Layout::new(
            self.queue.len(),
            [
                self.window[0],
                (self.window[1] - self.bar_height()).max(1.0),
            ],
            self.cell_px() as f32,
            (self.settings.grid_gap as f32 * sf).round(),
            label.round(),
        )
    }

    /// Scroll so the cursor's row is on screen.
    fn grid_follow(&mut self, now: Instant, animate: bool) {
        let l = self.grid_layout();
        let to = l.follow(self.queue.index(), self.grid.target(&l));
        if animate {
            self.grid.scroll_to(to, now, self.settings.anim);
        } else {
            self.grid.jump_to(to);
        }
    }

    /// Scroll the grid by `rows` (fractional for touchpads), pulling the cursor along so it
    /// stays on screen. Returns whether the cursor moved.
    pub fn grid_scroll(&mut self, rows: f32, now: Instant) -> bool {
        let l = self.grid_layout();
        let to = l.clamp_scroll(self.grid.target(&l) + rows * l.row_height());
        self.grid.scroll_to(to, now, self.settings.anim);
        self.queue.goto(l.pull_into_view(self.queue.index(), to))
    }

    /// Drag the grid content by `dy` pixels. Returns whether the cursor moved.
    pub fn grid_drag(&mut self, dy: f32, now: Instant) -> bool {
        let l = self.grid_layout();
        let to = l.clamp_scroll(self.grid.scroll(&l, now) - dy);
        self.grid.jump_to(to);
        self.queue.goto(l.pull_into_view(self.queue.index(), to))
    }

    /// The thumbnail under a window point.
    pub fn grid_hit(&self, p: [f32; 2], now: Instant) -> Option<usize> {
        let l = self.grid_layout();
        if p[1] >= l.viewport[1] {
            return None;
        }
        l.hit(p, self.grid.scroll(&l, now))
    }

    /// The selected range in visual mode.
    pub fn visual_range(&self) -> Option<std::ops::Range<usize>> {
        if self.mode != Mode::Visual {
            return None;
        }
        let r = self.grid.range(self.queue.index())?;
        Some(r.start.min(self.queue.len())..r.end.min(self.queue.len()))
    }

    /// Switch modes. Returns whether the mode changed.
    pub fn set_mode(&mut self, mode: Mode, now: Instant) -> bool {
        if mode == self.mode {
            return false;
        }
        let was_grid = self.in_grid();
        self.mode = mode;
        if mode != Mode::Visual {
            self.grid.anchor = None;
        }
        self.holds.clear();
        match mode {
            Mode::Image => {
                if let Some(pb) = self.playback.as_mut().filter(|p| p.playing) {
                    pb.next = Some(now + pb.delay());
                }
                // Don't crossfade from whatever was open before the grid.
                if self.shown.as_ref().map(|s| s.path.as_path()) != self.queue.current() {
                    self.clear_shown();
                }
            }
            Mode::Grid | Mode::Visual if !was_grid => self.grid_follow(now, false),
            _ => {}
        }
        true
    }

    /// A thumbnail landed in the atlas.
    pub fn thumb_ready(&mut self, path: PathBuf, size: [u32; 2], now: Instant) {
        self.thumbs.insert(path, ThumbState { size, at: now });
        self.last_thumb = Some(now);
    }

    /// Opacity of a thumbnail fading in.
    pub fn thumb_alpha(&self, at: Instant, now: Instant) -> f32 {
        let d = self.settings.thumb_fade;
        if d.is_zero() {
            return 1.0;
        }
        let t = now.saturating_duration_since(at).as_secs_f32() / d.as_secs_f32();
        crate::anim::ease(t.min(1.0))
    }

    fn update_viewport(&mut self) {
        let h = (self.window[1] - self.bar_height()).max(1.0);
        self.view.set_viewport(self.window[0], h);
    }

    fn dur(&self) -> Duration {
        self.settings.anim
    }

    /// Apply settings from a (re)loaded config. The view keeps its zoom until the next image.
    pub fn set_settings(&mut self, settings: Settings) {
        if settings.slideshow_delay != self.settings.slideshow_delay {
            self.slideshow_delay = settings.slideshow_delay;
        }
        self.settings = settings;
        self.update_viewport();
    }

    /// Switch to a new theme, crossfading from whatever is on screen now.
    pub fn set_theme(&mut self, theme: Theme, now: Instant) {
        if theme == self.theme {
            return;
        }
        let shown = self.theme_at(now);
        self.theme = theme;
        if self.settings.theme_fade.is_zero() {
            self.theme_fade = None;
            return;
        }
        let mut t = Tween::new(0.0, now);
        t.retarget(1.0, now, self.settings.theme_fade);
        self.theme_fade = Some((shown, t));
    }

    /// The theme for the frame at `now`.
    pub fn theme_at(&self, now: Instant) -> Theme {
        match &self.theme_fade {
            Some((from, t)) if t.is_active(now) => from.mix(&self.theme, t.value(now)),
            _ => self.theme.clone(),
        }
    }

    pub fn apply(&mut self, action: Action, now: Instant) -> Effects {
        self.apply_at(action, None, now)
    }

    /// Apply an action; `anchor` (viewport pixels) is where mouse-driven zoom centres.
    pub fn apply_at(&mut self, action: Action, anchor: Option<[f32; 2]>, now: Instant) -> Effects {
        let mut fx = Effects {
            redraw: true,
            ..Effects::default()
        };
        let dur = self.dur();
        let in_grid = self.in_grid();
        let has_image = self.mode == Mode::Image && self.shown.is_some();
        let wrap = self.settings.wrap;
        let switch = |s: Switch, cur: bool| match s {
            Switch::Toggle => !cur,
            Switch::On => true,
            Switch::Off => false,
        };
        match action {
            Action::Next(n) => fx.navigated = self.queue.step(n as isize, self.settings.wrap),
            Action::Prev(n) => fx.navigated = self.queue.step(-(n as isize), self.settings.wrap),
            Action::First => fx.navigated = self.queue.goto(0),
            Action::Last => fx.navigated = self.queue.goto(usize::MAX),
            Action::Goto(n) => fx.navigated = self.queue.goto(n.saturating_sub(1)),
            Action::Random => {
                let len = self.queue.len();
                if len > 1 {
                    let r = std::hash::BuildHasher::hash_one(
                        &std::collections::hash_map::RandomState::new(),
                        now,
                    ) as usize;
                    // Any image but the current one.
                    let i = (self.queue.index() + 1 + r % (len - 1)) % len;
                    fx.navigated = self.queue.goto(i);
                }
            }
            Action::Zoom(z) if has_image => {
                let step = self.settings.zoom_step;
                match z {
                    Zoom::In(n) => self.view.zoom_by(step.powi(n as i32), anchor, now, dur),
                    Zoom::Out(n) => self.view.zoom_by(step.powi(-(n as i32)), anchor, now, dur),
                    Zoom::Percent(p) => self.view.zoom_to(p / 100.0, anchor, now, dur),
                }
            }
            Action::NextMark | Action::PrevMark => {
                let forward = action == Action::NextMark;
                match self.queue.find_mark(forward, 1, wrap) {
                    Some(i) => fx.navigated = self.queue.goto(i),
                    None => self.notify("no other marks", MsgKind::Info, now),
                }
            }
            Action::Mark(s) => {
                if let Some(range) = self.visual_range() {
                    let on = match s {
                        Switch::Toggle => !range.clone().all(|i| self.queue.is_marked(i)),
                        Switch::On => true,
                        Switch::Off => false,
                    };
                    for i in range {
                        self.queue.set_mark(i, on);
                    }
                    fx.mode = self.set_mode(Mode::Grid, now);
                } else {
                    let i = self.queue.index();
                    let cur = self.queue.is_marked(i);
                    self.queue.set_mark(i, switch(s, cur));
                }
            }
            Action::MarkAll => self.queue.mark_all(),
            Action::UnmarkAll => self.queue.unmark_all(),
            Action::InvertMarks => self.queue.invert_marks(),
            Action::Move(dir, n) if in_grid => {
                let l = self.grid_layout();
                fx.navigated = self.queue.goto(l.step(self.queue.index(), dir, n));
            }
            Action::Page(page) if in_grid => {
                let l = self.grid_layout();
                let full = l.page_rows() as isize;
                let half = ((l.viewport[1] / l.row_height() / 2.0).round() as isize).max(1);
                let rows = match page {
                    Page::Down => full,
                    Page::Up => -full,
                    Page::HalfDown => half,
                    Page::HalfUp => -half,
                };
                let to = l.clamp_scroll(self.grid.target(&l) + rows as f32 * l.row_height());
                self.grid.scroll_to(to, now, dur);
                fx.navigated = self.queue.goto(l.step_rows(self.queue.index(), rows));
            }
            Action::RowStart | Action::RowEnd if in_grid => {
                let l = self.grid_layout();
                let i = self.queue.index();
                let to = if action == Action::RowStart {
                    l.row_start(i)
                } else {
                    l.row_end(i)
                };
                fx.navigated = self.queue.goto(to);
            }
            Action::Scroll(dir, n) if in_grid => match dir {
                Dir::Up => fx.navigated = self.grid_scroll(-(n as f32), now),
                Dir::Down => fx.navigated = self.grid_scroll(n as f32, now),
                Dir::Left | Dir::Right => fx.redraw = false,
            },
            Action::Scroll(dir, n) if has_image => {
                self.view.pan_step(dir, 0.1 * n as f32, now, dur);
            }
            Action::Mode(m) => {
                let target = match m {
                    ModeArg::Image => Mode::Image,
                    ModeArg::Grid => Mode::Grid,
                    ModeArg::Toggle if in_grid => Mode::Image,
                    ModeArg::Toggle => Mode::Grid,
                };
                fx.mode = self.set_mode(target, now);
                fx.redraw = fx.mode;
            }
            Action::Visual => {
                if self.mode == Mode::Visual {
                    fx.mode = self.set_mode(Mode::Grid, now);
                } else {
                    self.set_mode(Mode::Grid, now);
                    self.grid.anchor = Some(self.queue.index());
                    self.mode = Mode::Visual;
                    fx.mode = true;
                }
            }
            Action::GridSize(d) => {
                let size = grid::next_size(&self.settings.grid_sizes, self.grid.size, d);
                if size == self.grid.size {
                    fx.redraw = false;
                } else {
                    self.grid.size = size;
                    if in_grid {
                        self.grid_follow(now, false);
                    }
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
            Action::Play(s) if has_image => match &mut self.playback {
                Some(pb) if pb.animated => {
                    pb.playing = switch(s, pb.playing);
                    pb.next = pb.playing.then(|| now + pb.delay());
                }
                _ => self.notify("not animated", MsgKind::Info, now),
            },
            Action::NextFrame | Action::PrevFrame if has_image => match &mut self.playback {
                Some(pb) => {
                    pb.step(if action == Action::NextFrame { 1 } else { -1 });
                    // Stepping an animation pauses it, so the frame stays put.
                    pb.playing = false;
                    pb.next = None;
                }
                None => self.notify("single frame", MsgKind::Info, now),
            },
            Action::Adjust(a, s) if has_image => {
                let v = self.adjust.step(a, s);
                let name = Action::Adjust(a, s).name();
                self.notify(format!("{name} {v:.2}"), MsgKind::Info, now);
            }
            Action::Slideshow(s) => {
                let on = switch(s, self.slideshow.is_some());
                if on == self.slideshow.is_some() {
                    fx.redraw = false;
                } else if on {
                    // It runs in image mode; the timer starts once the image shows.
                    fx.mode = self.set_mode(Mode::Image, now);
                    self.slideshow = Some(self.shown.is_some().then(|| now + self.slideshow_delay));
                    let secs = self.slideshow_delay.as_secs_f32();
                    self.notify(format!("slideshow {secs}s"), MsgKind::Info, now);
                } else {
                    self.slideshow = None;
                    self.notify("slideshow off", MsgKind::Info, now);
                }
            }
            Action::SlideshowDelay { secs, relative } => {
                let base = if relative {
                    self.slideshow_delay.as_secs_f32()
                } else {
                    0.0
                };
                let d = (base + secs).clamp(SLIDESHOW_MIN, SLIDESHOW_MAX);
                self.slideshow_delay = Duration::from_secs_f32(d);
                if let Some(Some(next)) = &mut self.slideshow {
                    // Restart the count for the image on screen.
                    *next = now + self.slideshow_delay;
                }
                self.notify(
                    format!("slideshow {}s", (d * 10.0).round() / 10.0),
                    MsgKind::Info,
                    now,
                );
            }
            Action::Info(s) => {
                let open = switch(s, self.info_open);
                fx.info = open && !self.info_open;
                fx.redraw = open != self.info_open;
                self.info_open = open;
            }
            Action::Help => {
                if self.help.take().is_none() {
                    fx.help = true;
                }
            }
            Action::Palette => fx.palette = true,
            Action::Search => {
                self.which_key = None;
                self.help = None;
                let origin = self.queue.current().map(Path::to_path_buf);
                self.input = Some(Input::new(InputKind::Search, Vec::new(), origin));
            }
            Action::Filter(p) => {
                fx.navigated = self.queue.set_filter(Some(p));
                fx.reordered = true;
                if self.queue.is_empty() {
                    self.notify("no matches", MsgKind::Info, now);
                }
            }
            Action::FilterClear => {
                if self.queue.filter().is_some() {
                    fx.navigated = self.queue.set_filter(None);
                    fx.reordered = true;
                } else {
                    fx.redraw = false;
                }
            }
            Action::Open(p) => {
                self.open = Some(p);
                fx.open = true;
            }
            Action::Fullscreen(s) => fx.fullscreen = Some(s),
            Action::Bar(s) => {
                let bar = switch(s, self.bar);
                if bar != self.bar {
                    self.bar = bar;
                    self.update_viewport();
                    fx.layout = true;
                }
            }
            Action::Sort(key) => fx.sort = Some(key),
            Action::Reverse => {
                self.queue.reverse();
                fx.reordered = true;
            }
            Action::Reload => fx.reload = true,
            Action::Remove => {
                if let Some(cur) = self.queue.current().map(Path::to_path_buf) {
                    self.queue.remove(&cur);
                    fx.navigated = true;
                }
            }
            Action::ReloadConfig => fx.reload_config = true,
            Action::Refresh => fx.refresh = true,
            // The event loop runs commands: they spawn processes and may wait for a `y`.
            Action::Run(_) => fx.redraw = false,
            Action::Echo(msg) => self.notify(msg, MsgKind::Info, now),
            Action::Cancel => {
                self.message = None;
                self.help = None;
                self.which_key = None;
            }
            Action::Quit | Action::QuitNoOutput => fx.quit = true,
            Action::Zoom(_)
            | Action::Fit(_)
            | Action::Pan(_)
            | Action::PanEdge(_)
            | Action::Center
            | Action::Rotate(_)
            | Action::Flip(_)
            | Action::Move(..)
            | Action::Page(_)
            | Action::RowStart
            | Action::RowEnd
            | Action::Scroll(..)
            | Action::Play(_)
            | Action::NextFrame
            | Action::PrevFrame
            | Action::Adjust(..) => fx.redraw = false,
        }
        if fx.navigated && self.in_grid() {
            self.grid_follow(now, true);
        }
        fx
    }

    /// Open the palette with its rows (built by the loop from the keymap and commands).
    pub fn open_palette(&mut self, items: Vec<PaletteItem>) {
        self.which_key = None;
        self.help = None;
        self.input = Some(Input::new(InputKind::Palette, items, None));
    }

    fn goto_path(&mut self, path: &Path) -> bool {
        match self.queue.files().iter().position(|f| f == path) {
            Some(i) => self.queue.goto(i),
            None => false,
        }
    }

    /// Re-run the search for the current text: the grid filters live, the image view lists
    /// the best matches.
    fn search_update(&mut self, now: Instant) -> Effects {
        let mut fx = Effects {
            redraw: true,
            ..Effects::default()
        };
        let Some(input) = &mut self.input else {
            return fx;
        };
        if self.mode == Mode::Image {
            let names: Vec<String> = self.queue.files().iter().map(|p| file_name(p)).collect();
            input.matches = fuzzy::rank(&input.text, names.iter().map(String::as_str));
            input.selected = 0;
            return fx;
        }
        let text = input.text.clone();
        fx.navigated = self.queue.set_search(Some(text));
        fx.reordered = true;
        self.grid_follow(now, false);
        fx
    }

    /// A key typed at the palette or search line.
    pub fn input_edit(&mut self, e: Edit, now: Instant) -> Effects {
        let mut fx = Effects {
            redraw: true,
            ..Effects::default()
        };
        let in_grid = self.in_grid();
        let Some(input) = &mut self.input else {
            return fx;
        };
        let kind = input.kind;
        if input.edit_text(&e) {
            return match kind {
                InputKind::Palette => {
                    input.rank_items();
                    fx
                }
                InputKind::Search => self.search_update(now),
            };
        }
        match (&e, kind, in_grid) {
            (Edit::Up | Edit::Down, InputKind::Palette, _)
            | (Edit::Up | Edit::Down, InputKind::Search, false) => input.select(e == Edit::Up),
            (Edit::Left | Edit::Right | Edit::Up | Edit::Down, InputKind::Search, true) => {
                let dir = match e {
                    Edit::Left => Dir::Left,
                    Edit::Right => Dir::Right,
                    Edit::Up => Dir::Up,
                    _ => Dir::Down,
                };
                return self.apply(Action::Move(dir, 1), now);
            }
            (Edit::Complete, InputKind::Palette, _) => {
                if let Some(item) = input.selected_item() {
                    input.text = item.completion();
                    input.rank_items();
                }
            }
            _ => fx.redraw = false,
        }
        fx
    }

    /// `<CR>` at the prompt. Returns the action text to run, if any.
    pub fn input_submit(&mut self, now: Instant) -> (Effects, Option<String>) {
        let mut fx = Effects {
            redraw: true,
            ..Effects::default()
        };
        let Some(input) = self.input.take() else {
            return (fx, None);
        };
        match input.kind {
            InputKind::Palette => {
                let typed = input.text.trim();
                let selected = input.selected_item().cloned();
                let parses = typed.parse::<Action>().is_ok();
                let run = match selected {
                    Some(sel) if input.moved || !parses => {
                        if sel.template {
                            // `goto N`: complete it and keep typing.
                            let mut input = input;
                            input.text = sel.completion();
                            input.rank_items();
                            self.input = Some(input);
                            return (fx, None);
                        }
                        Some(sel.action)
                    }
                    _ if typed.is_empty() => None,
                    _ => Some(typed.to_string()),
                };
                (fx, run)
            }
            InputKind::Search if self.mode == Mode::Image => {
                if let Some(&i) = input.matches.get(input.selected) {
                    fx.navigated = self.queue.goto(i);
                }
                (fx, None)
            }
            InputKind::Search => {
                // Keep the file under the cursor, with the whole queue back around it.
                let cur = self.queue.current().map(Path::to_path_buf);
                self.queue.set_search(None);
                fx.reordered = true;
                fx.navigated = cur.as_deref() != input.origin.as_deref();
                if let Some(c) = cur {
                    self.goto_path(&c);
                }
                self.grid_follow(now, false);
                (fx, None)
            }
        }
    }

    /// `<Esc>` at the prompt: a search puts everything back as it was.
    pub fn input_cancel(&mut self, now: Instant) -> Effects {
        let mut fx = Effects {
            redraw: true,
            ..Effects::default()
        };
        let Some(input) = self.input.take() else {
            return fx;
        };
        if input.kind == InputKind::Search && self.in_grid() {
            self.queue.set_search(None);
            fx.reordered = true;
            if let Some(o) = &input.origin {
                self.goto_path(o);
            }
            fx.navigated = self.queue.current() != input.origin.as_deref();
            self.grid_follow(now, false);
        }
        fx
    }

    /// The slideshow's next image is due.
    pub fn slideshow_due(&self, now: Instant) -> bool {
        self.mode == Mode::Image && matches!(self.slideshow, Some(Some(t)) if now >= t)
    }

    /// The slideshow moved on (or couldn't): wait for the next image to show.
    pub fn slideshow_advanced(&mut self, moved: bool) {
        if self.slideshow.is_some() {
            self.slideshow = moved.then_some(None);
        }
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
        self.playback = None;
        if let Some(next) = &mut self.slideshow {
            *next = Some(now + self.slideshow_delay);
        }
    }

    /// The shown image has several frames: play an animation, or page through a document.
    pub fn set_frames(&mut self, delays: Vec<Duration>, animated: bool, now: Instant) {
        if delays.len() < 2 {
            self.playback = None;
            return;
        }
        let mut pb = Playback {
            delays,
            animated,
            index: 0,
            playing: animated,
            next: None,
        };
        pb.next = animated.then(|| now + pb.delay());
        self.playback = Some(pb);
    }

    /// The frame of the shown image to draw.
    pub fn frame(&self) -> usize {
        self.playback.as_ref().map_or(0, |p| p.index)
    }

    /// The current file can't be shown (or the queue emptied).
    pub fn clear_shown(&mut self) {
        self.shown = None;
        self.playback = None;
        self.fade = None;
        self.loading_since = None;
    }

    /// Zoom by `steps` zoom steps (fractional for touchpads) around a viewport point.
    pub fn zoom_at(&mut self, steps: f32, anchor: [f32; 2], now: Instant) {
        if self.shown.is_some() {
            let factor = self.settings.zoom_step.powf(steps);
            self.view
                .zoom_by(factor, Some(anchor), now, self.settings.anim);
        }
    }

    pub fn notify(&mut self, text: impl Into<String>, kind: MsgKind, now: Instant) {
        let secs = if kind == MsgKind::Error { 5 } else { 2 };
        self.notify_for(text, kind, now, Duration::from_secs(secs));
    }

    pub fn notify_for(
        &mut self,
        text: impl Into<String>,
        kind: MsgKind,
        now: Instant,
        d: Duration,
    ) {
        self.message = Some(Message {
            text: text.into(),
            kind,
            until: now + d,
        });
    }

    pub fn effective_nearest(&self, now: Instant) -> bool {
        self.nearest
            .unwrap_or_else(|| self.view.display_scale(now) > view::NEAREST_ABOVE)
    }

    /// The grid never waits on a full decode, so a load left over from image mode stays quiet.
    pub fn loading_visible(&self, now: Instant) -> bool {
        !self.in_grid() && self.loading_since.is_some_and(|t| now >= t + LOADING_DELAY)
    }

    /// Whether frames must keep coming (animations, held keys).
    pub fn is_animating(&self, now: Instant) -> bool {
        let grid = self.in_grid()
            && (self.grid.is_animating(now)
                || self
                    .last_thumb
                    .is_some_and(|t| now < t + self.settings.thumb_fade));
        grid || self.view.is_animating(now)
            || self.fade.as_ref().is_some_and(|f| f.alpha.is_active(now))
            || self
                .theme_fade
                .as_ref()
                .is_some_and(|(_, t)| t.is_active(now))
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
        if let Some(t) = self.loading_since.filter(|_| !self.in_grid()) {
            let at = t + LOADING_DELAY;
            if now < at {
                wake(at);
            }
        }
        if self.mode == Mode::Image
            && let Some(Some(t)) = self.slideshow
        {
            wake(t);
        }
        // Animation frames: only while the image is on screen.
        if self.mode == Mode::Image
            && let Some(pb) = &mut self.playback
            && let Some(mut next) = pb.next
        {
            if now >= next {
                let before = pb.index;
                // Catch up on missed frames, but after a long stall just carry on from now.
                for _ in 0..pb.len() {
                    if now < next {
                        break;
                    }
                    pb.step(1);
                    next += pb.delay();
                }
                if now >= next {
                    next = now + pb.delay();
                }
                pb.next = Some(next);
                redraw |= pb.index != before;
            }
            wake(next);
        }
        if self.fade.as_ref().is_some_and(|f| !f.alpha.is_active(now)) {
            self.fade = None;
            redraw = true;
        }
        if self
            .theme_fade
            .as_ref()
            .is_some_and(|(_, t)| !t.is_active(now))
        {
            self.theme_fade = None;
            redraw = true;
        }
        (redraw, next)
    }
}

pub fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into(),
    )
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
    fn grid_hides_a_pending_load() {
        let (mut s, now) = state(3);
        s.loading_since = Some(now);
        let later = now + LOADING_DELAY;
        assert!(s.loading_visible(later));
        s.set_mode(Mode::Grid, now);
        assert!(!s.loading_visible(later));
    }

    #[test]
    fn bar_shrinks_the_image_area() {
        let (mut s, now) = state(1);
        assert_eq!(s.view.viewport(), [800.0, 600.0]);
        assert!(!s.apply(Action::Bar(Switch::On), now).layout);
        assert!(s.apply(Action::Bar(Switch::Toggle), now).layout);
        assert_eq!(s.view.viewport(), [800.0, 632.0]);
        assert!(!s.apply(Action::Bar(Switch::Off), now).layout);
        s.settings.ui_scale = 2.0;
        s.apply(Action::Bar(Switch::On), now);
        assert_eq!(s.view.viewport(), [800.0, 568.0]);
        s.apply(Action::Bar(Switch::Off), now);
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
    fn theme_changes_crossfade() {
        use crate::config::theme::{Rgba, Theme};
        let (mut s, now) = state(1);
        let light = Theme::derive(
            Rgba::rgb(0xFFFFFF),
            Rgba::rgb(0x000000),
            Rgba::rgb(0xD71921),
        );
        s.set_theme(light.clone(), now);
        assert!(s.is_animating(now));
        let mid = s.theme_at(now + THEME_FADE / 2).background;
        assert!(mid.0[0] > 0 && mid.0[0] < 255, "{mid:?}");
        let later = now + THEME_FADE * 2;
        assert_eq!(s.theme_at(later), light);
        assert!(s.tick(later).0);
        assert!(!s.is_animating(later));
        // With animation off the switch is instant.
        s.settings.theme_fade = Duration::ZERO;
        s.set_theme(Theme::nothing_dark(), later);
        assert_eq!(s.theme_at(later), Theme::nothing_dark());
    }

    #[test]
    fn overlay_actions_ask_the_loop() {
        let (mut s, now) = state(2);
        assert!(s.apply(Action::Palette, now).palette);
        assert!(s.apply(Action::Help, now).help);
        s.help = Some(Help {
            rows: Vec::new(),
            scroll: 0,
        });
        assert!(!s.apply(Action::Help, now).help, "help toggles off");
        assert!(s.help.is_none());
        assert!(s.apply(Action::Info(Switch::Toggle), now).info);
        assert!(!s.apply(Action::Info(Switch::On), now).redraw);
        assert!(s.apply(Action::Open("x.png".into()), now).open);
        assert_eq!(s.open.as_deref(), Some("x.png"));
        assert!(s.apply(Action::Refresh, now).refresh);
        assert!(!s.apply(Action::Run("x".into()), now).redraw);
        s.apply(Action::Echo("hi".into()), now);
        assert_eq!(s.message.as_ref().map(|m| m.text.as_str()), Some("hi"));
        assert!(s.apply(Action::Random, now).navigated);
        assert_eq!(s.queue.index(), 1);
    }

    fn palette_items() -> Vec<PaletteItem> {
        [("next", false), ("goto N", true), ("fit width", false)]
            .iter()
            .map(|(a, t)| PaletteItem {
                action: a.to_string(),
                desc: String::new(),
                keys: String::new(),
                template: *t,
            })
            .collect()
    }

    #[test]
    fn palette_runs_typed_or_selected_actions() {
        let (mut s, now) = state(3);
        let typed = |s: &mut AppState, t: &str| {
            s.open_palette(palette_items());
            s.input_edit(Edit::Insert(t.into()), now);
            s.input_submit(now).1
        };
        assert_eq!(typed(&mut s, "fitw").as_deref(), Some("fit width"));
        // Typed text that parses wins over the best match...
        assert_eq!(typed(&mut s, "goto 3").as_deref(), Some("goto 3"));
        // ...unless the selection was moved by hand.
        s.open_palette(palette_items());
        s.input_edit(Edit::Up, now);
        assert!(s.input_submit(now).1.is_none());
        assert_eq!(
            s.input.as_ref().unwrap().text,
            "goto ",
            "a template completes"
        );
        s.input_edit(Edit::Insert("2".into()), now);
        assert_eq!(s.input_submit(now).1.as_deref(), Some("goto 2"));
        assert!(s.input.is_none());
        // No match: the text runs as typed (and the loop reports it if it doesn't parse).
        assert_eq!(typed(&mut s, "zzzz").as_deref(), Some("zzzz"));
        s.open_palette(palette_items());
        assert!(!s.input_cancel(now).navigated);
        assert!(s.input.is_none());
    }

    #[test]
    fn search_filters_the_grid_live() {
        let now = Instant::now();
        let files = ["cat.png", "dog.png", "cow.png", "bird.png"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let mut s = AppState::new(Queue::new(files, None, 1), Settings::default(), true, now);
        s.set_window(800.0, 632.0, 1.0);
        s.apply(Action::Mode(ModeArg::Grid), now);
        s.apply(Action::Search, now);
        let fx = s.input_edit(Edit::Insert("c".into()), now);
        assert!(fx.reordered);
        assert_eq!(s.queue.len(), 2);
        s.input_edit(Edit::Right, now);
        assert_eq!(s.queue.current(), Some(Path::new("cow.png")));
        let (fx, run) = s.input_submit(now);
        assert!(run.is_none() && fx.navigated);
        assert_eq!(s.queue.len(), 4);
        assert_eq!(s.queue.current(), Some(Path::new("cow.png")));
        // <Esc> puts back the file the search started on.
        s.apply(Action::Search, now);
        s.input_edit(Edit::Insert("bi".into()), now);
        assert_eq!(s.queue.current(), Some(Path::new("bird.png")));
        s.input_cancel(now);
        assert_eq!(s.queue.current(), Some(Path::new("cow.png")));
        assert_eq!(s.queue.len(), 4);
        // In image mode the search lists matches and jumps to the selected one.
        s.apply(Action::Mode(ModeArg::Image), now);
        s.apply(Action::Search, now);
        s.input_edit(Edit::Insert("dg".into()), now);
        assert_eq!(s.queue.len(), 4);
        assert!(s.input_submit(now).0.navigated);
        assert_eq!(s.queue.current(), Some(Path::new("dog.png")));
    }

    #[test]
    fn filter_action_narrows_the_queue() {
        let (mut s, now) = state(12);
        assert!(s.apply(Action::Filter("1*".into()), now).reordered);
        assert_eq!(s.queue.len(), 3, "1, 10, 11");
        s.apply(Action::Filter("zzz".into()), now);
        assert!(s.queue.is_empty());
        assert_eq!(s.message.as_ref().unwrap().text, "no matches");
        assert!(s.apply(Action::FilterClear, now).reordered);
        assert_eq!(s.queue.len(), 12);
        assert!(!s.apply(Action::FilterClear, now).redraw);
    }

    #[test]
    fn slideshow_advances_after_the_delay() {
        let (mut s, now) = state(2);
        s.apply(Action::Mode(ModeArg::Grid), now);
        s.apply(Action::Slideshow(Switch::On), now);
        assert_eq!(s.mode, Mode::Image, "slideshows run in image mode");
        assert!(
            !s.slideshow_due(now + Duration::from_secs(60)),
            "waits for an image"
        );
        s.show(Path::new("0.png"), [10, 10], "PNG", now);
        let d = s.slideshow_delay;
        s.message = None;
        assert_eq!(s.tick(now).1, Some(now + d));
        assert!(!s.slideshow_due(now + d / 2));
        assert!(s.slideshow_due(now + d));
        s.slideshow_advanced(true);
        assert!(
            !s.slideshow_due(now + d * 2),
            "re-armed when the next image shows"
        );
        s.apply(
            Action::SlideshowDelay {
                secs: -100.0,
                relative: true,
            },
            now,
        );
        assert_eq!(s.slideshow_delay, Duration::from_secs_f32(SLIDESHOW_MIN));
        s.apply(Action::Slideshow(Switch::Toggle), now);
        assert!(s.slideshow.is_none());
    }

    #[test]
    fn adjustments_step_and_reset() {
        let (mut s, now) = state(1);
        s.show(Path::new("0.png"), [10, 10], "PNG", now);
        s.apply(Action::Adjust(Adjust::Gamma, Step::Up), now);
        s.apply(Action::Adjust(Adjust::Gamma, Step::Up), now);
        assert_eq!(s.adjust.gamma, 1.21);
        assert_eq!(s.message.as_ref().unwrap().text, "gamma 1.21");
        s.apply(Action::Adjust(Adjust::Gamma, Step::Down), now);
        s.apply(Action::Adjust(Adjust::Gamma, Step::Down), now);
        assert_eq!(s.adjust.gamma, 1.0);
        for _ in 0..30 {
            s.apply(Action::Adjust(Adjust::Brightness, Step::Up), now);
        }
        assert_eq!(s.adjust.brightness, 1.0);
        s.apply(Action::Adjust(Adjust::Brightness, Step::Reset), now);
        assert_eq!(s.adjust, Adjustments::default());
    }

    #[test]
    fn grid_mode_moves_the_queue_cursor() {
        let (mut s, now) = state(40);
        assert!(s.apply(Action::Mode(ModeArg::Toggle), now).mode);
        assert_eq!(s.mode, Mode::Grid);
        // 800 wide, 160 cells, 16 gaps: 4 columns.
        assert_eq!(s.grid_layout().cols, 4);
        assert!(s.apply(Action::Move(Dir::Down, 2), now).navigated);
        assert_eq!(s.queue.index(), 8);
        s.apply(Action::RowEnd, now);
        assert_eq!(s.queue.index(), 11);
        s.apply(Action::Last, now);
        let l = s.grid_layout();
        assert_eq!(s.grid.target(&l), l.max_scroll());
        s.apply(Action::Page(Page::Up), now);
        assert_eq!(s.queue.index(), 39 - 4 * l.page_rows());
        // Image-only actions do nothing in the grid.
        assert!(!s.apply(Action::Zoom(Zoom::In(1)), now).redraw);
        assert!(s.apply(Action::Mode(ModeArg::Toggle), now).mode);
        assert_eq!(s.mode, Mode::Image);
        assert!(!s.apply(Action::Move(Dir::Down, 1), now).redraw);
    }

    #[test]
    fn visual_mode_marks_a_range() {
        let (mut s, now) = state(10);
        s.apply(Action::Goto(2), now);
        s.apply(Action::Visual, now);
        assert_eq!(s.mode, Mode::Visual);
        s.apply(Action::Move(Dir::Right, 3), now);
        assert_eq!(s.visual_range(), Some(1..5));
        s.apply(Action::Mark(Switch::On), now);
        assert_eq!(s.mode, Mode::Grid);
        assert_eq!(s.queue.marked_count(), 4);
        // Toggling a fully marked range unmarks it.
        s.apply(Action::Visual, now);
        s.apply(Action::Move(Dir::Left, 3), now);
        s.apply(Action::Mark(Switch::Toggle), now);
        assert_eq!(s.queue.marked_count(), 0);
    }

    #[test]
    fn marks_and_mark_navigation() {
        let (mut s, now) = state(5);
        s.apply(Action::Mark(Switch::Toggle), now);
        s.apply(Action::Goto(4), now);
        s.apply(Action::Mark(Switch::Toggle), now);
        assert_eq!(s.queue.marked_count(), 2);
        assert!(s.apply(Action::NextMark, now).navigated);
        assert_eq!(s.queue.index(), 0);
        s.apply(Action::InvertMarks, now);
        assert_eq!(s.queue.marked_count(), 3);
        s.apply(Action::UnmarkAll, now);
        assert!(!s.apply(Action::NextMark, now).navigated);
        assert!(s.message.is_some());
        s.apply(Action::MarkAll, now);
        assert_eq!(s.queue.marked_count(), 5);
    }

    #[test]
    fn grid_size_steps_through_sizes() {
        let (mut s, now) = state(3);
        s.apply(Action::GridSize(1), now);
        assert_eq!(s.grid.size, 224);
        s.apply(Action::GridSize(5), now);
        assert_eq!(s.grid.size, 320);
        assert!(!s.apply(Action::GridSize(1), now).redraw);
        assert_eq!(s.cell_px(), 320);
    }

    #[test]
    fn animation_plays_steps_and_pauses() {
        let (mut s, now) = state(1);
        s.show(Path::new("0.png"), [10, 10], "GIF", now);
        let ms = Duration::from_millis;
        s.set_frames(vec![ms(100), ms(50), ms(100)], true, now);
        assert_eq!(s.frame(), 0);
        assert_eq!(s.tick(now + ms(10)).1, Some(now + ms(100)));
        assert!(s.tick(now + ms(100)).0);
        assert_eq!(s.frame(), 1);
        // Late: catch up through frame 1's 50 ms into frame 2.
        s.tick(now + ms(160));
        assert_eq!(s.frame(), 2);
        s.apply(Action::Play(Switch::Toggle), now);
        assert!(!s.tick(now + ms(5000)).0, "paused");
        s.apply(Action::NextFrame, now);
        assert_eq!(s.frame(), 0);
        s.apply(Action::PrevFrame, now);
        assert_eq!(s.frame(), 2);
        // Pages step but never play.
        s.set_frames(vec![Duration::ZERO; 3], false, now);
        assert!(!s.playback.as_ref().unwrap().playing);
        s.apply(Action::Play(Switch::On), now);
        assert_eq!(s.message.as_ref().unwrap().text, "not animated");
        s.apply(Action::NextFrame, now);
        assert_eq!(s.frame(), 1);
        // A new image starts without frames.
        s.show(Path::new("1.png"), [10, 10], "PNG", now);
        assert!(s.playback.is_none());
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
