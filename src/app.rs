//! winit `ApplicationHandler`: routes events, owns the state and the renderer.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{ModifiersState, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::action::{Action, Switch, Zoom};
use crate::cache::ImageCache;
use crate::config::watch::ConfigWatch;
use crate::config::{Config, LoadError, Loaded};
use crate::decode::{self, DecodedImage};
use crate::input::keymap::{KeyTables, Keymap};
use crate::input::keys::{Key, Scroll};
use crate::input::{self};
use crate::render::{Renderer, text};
use crate::scan;
use crate::state::view::{Dir, FitMode};
use crate::state::{AppState, Effects, MsgKind, Settings};
use crate::ui;
use crate::worker::{JobKey, Pool, Priority};

/// GPU upload budget per event-loop turn, so preloads never cause a hitch.
const UPLOAD_BUDGET: usize = 32 << 20;
/// Show the window once the first image is ready, or after this long regardless.
const SHOW_TIMEOUT: Duration = Duration::from_millis(300);
/// Wheel pixels per zoom step for touchpads.
const PIXELS_PER_STEP: f32 = 60.0;
/// An ambiguous key sequence (`g` bound, and `gg` too) resolves after this long, like vim's
/// `timeoutlen`.
const SEQUENCE_TIMEOUT: Duration = Duration::from_millis(1000);
/// A press and release closer than this is a click, not a drag.
const CLICK_SLOP: f32 = 4.0;
/// Config errors stay on screen until fixed (or dismissed with `<Esc>`).
const CONFIG_ERROR_TIME: Duration = Duration::from_secs(3600);

pub enum UserEvent {
    Decoded {
        path: PathBuf,
        result: Result<Arc<DecodedImage>, String>,
    },
    /// Unknown-extension files that turned out to be images.
    Found(Vec<PathBuf>),
    /// Embedded plus system fonts, loaded off-thread after the first frame.
    Fonts(glyphon::fontdb::Database),
    /// The config was reloaded (file change, SIGUSR1 or `reload-config`).
    Config(Box<Result<Loaded, LoadError>>),
    /// A re-sort finished: the queue in its new order.
    Sorted(Vec<PathBuf>),
}

/// Command-line settings that beat the config, across reloads too.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub fit: Option<FitMode>,
    pub zoom: Option<f32>,
    pub bar: Option<bool>,
}

impl Overrides {
    pub fn apply(&self, s: &mut Settings) {
        if let Some(fit) = self.fit {
            s.fit = fit;
        }
        if self.zoom.is_some() {
            s.zoom = self.zoom;
        }
    }
}

pub struct Options {
    pub fullscreen: bool,
    pub geometry: Option<(u32, u32)>,
    pub class: String,
    pub overrides: Overrides,
}

struct Gfx {
    renderer: Renderer,
    // Dropped after the renderer, whose surface borrows the window.
    window: Arc<Window>,
}

pub struct App {
    opts: Options,
    state: AppState,
    config: Config,
    keymap: Keymap,
    watch: ConfigWatch,
    /// A config error is on the status line.
    config_error: bool,
    /// `reload-config` ran, so the next reload is confirmed on screen.
    reload_requested: bool,
    /// When the pending key sequence became ambiguous.
    ambiguous_since: Option<Instant>,
    /// Actions a held key repeats.
    repeat: Option<(PhysicalKey, Vec<Action>)>,
    pool: Pool,
    proxy: EventLoopProxy<UserEvent>,
    cache: ImageCache,
    max_dim: Arc<AtomicU32>,
    /// Decoded images whose result event is still in the loop's queue.
    arriving: Arc<Mutex<HashSet<PathBuf>>>,
    gfx: Option<Gfx>,
    uploads: VecDeque<(PathBuf, Arc<DecodedImage>)>,
    modifiers: ModifiersState,
    cursor: [f32; 2],
    dragging: bool,
    /// Where the left button went down, while it is a click candidate.
    press: Option<[f32; 2]>,
    /// Wheel motion not yet turned into whole `<ScrollUp>`/`<ScrollDown>` presses.
    scroll: [f32; 2],
    held: HashMap<PhysicalKey, Dir>,
    started: Instant,
    shown_window: bool,
    pub error: Option<anyhow::Error>,
}

impl App {
    pub fn new(
        opts: Options,
        state: AppState,
        proxy: EventLoopProxy<UserEvent>,
        loaded: Loaded,
        watch: ConfigWatch,
    ) -> Self {
        let cache = ImageCache::new(loaded.config.image.cache_mb << 20);
        let mut app = Self {
            opts,
            state,
            keymap: keymap(&loaded.keys),
            config: loaded.config,
            watch,
            config_error: false,
            reload_requested: false,
            ambiguous_since: None,
            repeat: None,
            pool: Pool::new(),
            proxy,
            cache,
            // Until the GPU reports its limit; uploads re-check.
            max_dim: Arc::new(AtomicU32::new(16384)),
            arriving: Arc::default(),
            gfx: None,
            uploads: VecDeque::new(),
            modifiers: ModifiersState::empty(),
            cursor: [0.0, 0.0],
            dragging: false,
            press: None,
            scroll: [0.0, 0.0],
            held: HashMap::new(),
            started: Instant::now(),
            shown_window: false,
            error: None,
        };
        // Decode the first image while the window and GPU come up.
        app.sync(true);
        app
    }

    fn request_redraw(&self) {
        if let Some(g) = &self.gfx {
            g.window.request_redraw();
        }
    }

    fn decode(&self, path: &Path, prio: Priority) {
        if lock(&self.arriving).contains(path) {
            return;
        }
        let proxy = self.proxy.clone();
        let max_dim = self.max_dim.clone();
        let arriving = self.arriving.clone();
        let path = path.to_path_buf();
        self.pool
            .submit(JobKey::Decode(path.clone()), prio, move || {
                let t = Instant::now();
                let result = decode::decode(&path, max_dim.load(Ordering::Relaxed))
                    .map(Arc::new)
                    .map_err(|e| e.to_string());
                log::debug!("decoded {} in {:.1?}", path.display(), t.elapsed());
                lock(&arriving).insert(path.clone());
                let _ = proxy.send_event(UserEvent::Decoded { path, result });
            });
    }

    /// The current file and its preload neighbours, current first.
    fn window_paths(&self) -> Vec<PathBuf> {
        let q = &self.state.queue;
        let Some(cur) = q.current() else {
            return Vec::new();
        };
        let mut out = vec![cur.to_path_buf()];
        for i in q.neighbours(self.state.settings.preload, self.state.settings.wrap) {
            if let Some(p) = q.get(i) {
                out.push(p.to_path_buf());
            }
        }
        out
    }

    /// Make the GPU and decode queue match the current position.
    fn sync(&mut self, navigated: bool) {
        let now = Instant::now();
        if navigated {
            self.pool.bump_epoch();
        }
        let wanted = self.window_paths();
        self.update_title();
        let Some(cur) = wanted.first().cloned() else {
            self.state.clear_shown();
            self.uploads.clear();
            if let Some(g) = &mut self.gfx {
                g.renderer.retain(|_| false);
            }
            self.request_redraw();
            return;
        };

        let keep = |p: &Path| {
            wanted.iter().any(|w| w == p)
                || self.state.shown.as_ref().is_some_and(|s| s.path == p)
                || self.state.fade.as_ref().is_some_and(|f| f.path == p)
        };
        if let Some(g) = &mut self.gfx {
            g.renderer.retain(keep);
        }
        self.uploads.retain(|(p, _)| wanted.contains(p));

        let showing_cur = self.state.shown.as_ref().is_some_and(|s| s.path == cur);
        if !showing_cur {
            match self.cache.get(&cur) {
                Some(img) => self.show(&cur, img, now),
                None => {
                    self.state.loading_since.get_or_insert(now);
                    self.decode(&cur, Priority::Current);
                }
            }
        }
        for p in &wanted[1..] {
            match self.cache.get(p) {
                Some(img) => {
                    let ready = self.gfx.as_ref().is_some_and(|g| g.renderer.is_ready(p));
                    if !ready && !self.uploads.iter().any(|(u, _)| u == p) {
                        self.uploads.push_back((p.clone(), img));
                    }
                }
                None => self.decode(p, Priority::Preload),
            }
        }
        self.request_redraw();
    }

    /// Upload `img` in full and put it on screen.
    fn show(&mut self, path: &Path, img: Arc<DecodedImage>, now: Instant) {
        let Some(g) = &mut self.gfx else {
            // No GPU yet: keep it cached; `resumed` syncs again.
            self.state.loading_since.get_or_insert(now);
            return;
        };
        let img = fit_gpu(img, g.renderer.max_texture_dim());
        g.renderer.upload(path, &img, usize::MAX);
        self.uploads.retain(|(p, _)| p != path);
        self.state.show(path, img.source_size, img.format, now);
        self.request_redraw();
    }

    fn update_title(&self) {
        if let Some(g) = &self.gfx {
            let q = &self.state.queue;
            let title = match q.current() {
                Some(p) => self
                    .config
                    .window
                    .title
                    .replace("{name}", &ui::file_name(p))
                    .replace("{index}", &(q.index() + 1).to_string())
                    .replace("{count}", &q.len().to_string()),
                None => "remage".into(),
            };
            g.window.set_title(&title);
        }
    }

    /// Show a config error until the config is fixed.
    pub fn config_failed(&mut self, message: &str) {
        self.state.notify_for(
            format!("config error: {}", short_error(message)),
            MsgKind::Error,
            Instant::now(),
            CONFIG_ERROR_TIME,
        );
        self.config_error = true;
        self.request_redraw();
    }

    /// Adopt a reloaded config. Runtime toggles (bar, filter, alpha background) only change
    /// when their config value did, so a reload doesn't undo what the user just toggled.
    fn apply_config(&mut self, loaded: Loaded) {
        let now = Instant::now();
        for w in &loaded.warnings {
            log::warn!("{w}");
        }
        let old = std::mem::replace(&mut self.config, loaded.config);
        let new = &self.config;
        let mut settings = Settings::from_config(new);
        self.opts.overrides.apply(&mut settings);
        let preload_changed = settings.preload != self.state.settings.preload;
        self.state.set_settings(settings);
        self.state.set_theme(loaded.theme, now);
        self.keymap = keymap(&loaded.keys);
        self.ambiguous_since = None;
        self.state.pending.clear();
        self.cache.set_budget(new.image.cache_mb << 20);
        if old.image.filter != new.image.filter {
            self.state.nearest = new.image.filter.nearest();
        }
        if old.image.alpha_background != new.image.alpha_background {
            self.state.alpha_bg = new.image.alpha_background;
        }
        if old.ui.statusbar != new.ui.statusbar && self.opts.overrides.bar.is_none() {
            let s = if new.ui.statusbar {
                Switch::On
            } else {
                Switch::Off
            };
            self.state.apply(Action::Bar(s), now);
        }
        if let Some(g) = &mut self.gfx {
            if old.window.decorations != new.window.decorations {
                g.window.set_decorations(new.window.decorations);
            }
            g.renderer.set_font_families(font_families(new));
        }
        self.update_title();
        // Silent on routine changes (a new wallpaper palette); confirm fixes and manual reloads.
        if std::mem::take(&mut self.config_error) || std::mem::take(&mut self.reload_requested) {
            self.state.notify("config reloaded", MsgKind::Info, now);
        }
        if preload_changed {
            self.sync(false);
        }
        self.request_redraw();
    }

    fn on_decoded(&mut self, path: PathBuf, result: Result<Arc<DecodedImage>, String>) {
        lock(&self.arriving).remove(&path);
        let now = Instant::now();
        let is_current = self.state.queue.current() == Some(path.as_path());
        match result {
            Ok(img) => {
                let wanted = self.window_paths();
                let keep: Vec<&Path> = wanted.iter().map(PathBuf::as_path).collect();
                self.cache.insert(path.clone(), img.clone(), &keep);
                if is_current {
                    // Already on screen means this was a reload: replace the texture.
                    let reload = self.state.shown.as_ref().is_some_and(|s| s.path == path);
                    if reload && let Some(g) = &mut self.gfx {
                        g.renderer.retain(|p| p != path);
                    }
                    self.show(&path, img, now);
                } else if wanted.contains(&path) && !self.uploads.iter().any(|(p, _)| *p == path) {
                    self.uploads.push_back((path, img));
                }
            }
            Err(e) => {
                log::warn!("{}: {e}", path.display());
                self.state.queue.remove(&path);
                self.state.skipped += 1;
                if is_current {
                    self.state.loading_since = None;
                    let msg = format!("error: {}: {e}", ui::file_name(&path));
                    self.state.notify(msg, MsgKind::Error, now);
                    self.sync(true);
                }
                self.request_redraw();
            }
        }
    }

    fn reload(&mut self) {
        if let Some(cur) = self.state.queue.current().map(Path::to_path_buf) {
            self.cache.remove(&cur);
            self.decode(&cur, Priority::Current);
        }
    }

    /// Run actions in order; `anchor` is the cursor for mouse-triggered ones.
    fn run(&mut self, actions: Vec<Action>, anchor: Option<[f32; 2]>, el: &ActiveEventLoop) {
        for action in actions {
            if action == Action::Cancel {
                self.keymap.cancel();
                self.state.pending.clear();
                self.config_error = false;
            }
            let fx = self.state.apply_at(action, anchor, Instant::now());
            self.effects(fx, el);
            if fx.quit {
                return;
            }
        }
    }

    fn effects(&mut self, fx: Effects, event_loop: &ActiveEventLoop) {
        if fx.quit {
            event_loop.exit();
            return;
        }
        if let Some(s) = fx.fullscreen
            && let Some(g) = &self.gfx
        {
            let on = g.window.fullscreen().is_some();
            let want = match s {
                Switch::Toggle => !on,
                Switch::On => true,
                Switch::Off => false,
            };
            if want != on {
                g.window
                    .set_fullscreen(want.then_some(Fullscreen::Borderless(None)));
            }
        }
        if fx.reload {
            self.reload();
        }
        if fx.reload_config {
            self.reload_requested = true;
            self.watch.reload();
        }
        if let Some(key) = fx.sort {
            let mut files = self.state.queue.files().to_vec();
            let proxy = self.proxy.clone();
            std::thread::spawn(move || {
                scan::sort_paths(&mut files, key, false);
                let _ = proxy.send_event(UserEvent::Sorted(files));
            });
        }
        if fx.navigated {
            self.sync(true);
        } else if fx.reordered {
            self.sync(false);
        }
        if fx.redraw || fx.layout {
            self.request_redraw();
        }
    }

    /// Mirror the keymap's pending keys on the status line and arm the ambiguity timeout.
    fn update_pending(&mut self, now: Instant) {
        let pending = self.keymap.pending_display();
        if pending != self.state.pending {
            self.state.pending = pending;
            self.request_redraw();
        }
        self.ambiguous_since = self
            .keymap
            .is_ambiguous(self.state.mode)
            .then(|| self.ambiguous_since.unwrap_or(now));
    }

    fn on_key(&mut self, event: winit::event::KeyEvent, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if event.state == ElementState::Released {
            if let Some(dir) = self.held.remove(&event.physical_key) {
                self.state.hold_pan(dir, false, now);
                self.request_redraw();
            }
            if self
                .repeat
                .as_ref()
                .is_some_and(|(k, _)| *k == event.physical_key)
            {
                self.repeat = None;
            }
            return;
        }
        if event.repeat {
            // Repeats bypass the trie: only a completed, repeatable binding repeats.
            if let Some((k, actions)) = &self.repeat
                && *k == event.physical_key
            {
                let actions = actions.clone();
                self.run(actions, None, event_loop);
            }
            return;
        }
        let Some(key) = input::key(&event.logical_key, self.modifiers) else {
            return;
        };
        let runs = self.keymap.feed(self.state.mode, key);
        self.update_pending(now);
        self.repeat = None;
        for run in runs {
            let actions = run.counted();
            if let [Action::Pan(dir)] = actions[..] {
                if self.state.shown.is_some() {
                    self.held.insert(event.physical_key, dir);
                    self.state.hold_pan(dir, true, now);
                    self.request_redraw();
                }
                continue;
            }
            if !actions.is_empty() && actions.iter().all(Action::repeats) {
                self.repeat = Some((event.physical_key, actions.clone()));
            }
            self.run(actions, None, event_loop);
        }
    }

    /// A mouse button or wheel notch: bound actions run at the cursor, bypassing sequences.
    fn on_mouse_key(&mut self, key: Key, event_loop: &ActiveEventLoop) {
        self.keymap.cancel();
        self.update_pending(Instant::now());
        if let Some(actions) = self.keymap.lookup(self.state.mode, &[key]) {
            let actions = actions.to_vec();
            self.run(actions, Some(self.cursor), event_loop);
        }
    }

    fn on_wheel(&mut self, delta: [f32; 2], event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let [dx, dy] = delta;
        if dy != 0.0 {
            let dir = if dy > 0.0 { Scroll::Up } else { Scroll::Down };
            let key = input::scroll(dir, self.modifiers);
            // Wheel zoom stays continuous for touchpads rather than stepping per notch.
            match self.keymap.lookup(self.state.mode, &[key]) {
                Some([Action::Zoom(Zoom::In(n))]) => {
                    let steps = dy.abs() * *n as f32;
                    self.state.zoom_at(steps, self.cursor, now);
                    self.request_redraw();
                    return;
                }
                Some([Action::Zoom(Zoom::Out(n))]) => {
                    let steps = -dy.abs() * *n as f32;
                    self.state.zoom_at(steps, self.cursor, now);
                    self.request_redraw();
                    return;
                }
                _ => {}
            }
        }
        for (axis, d, pos, neg) in [
            (0, dx, Scroll::Left, Scroll::Right),
            (1, dy, Scroll::Up, Scroll::Down),
        ] {
            if d == 0.0 {
                continue;
            }
            // Reversing direction drops the leftover fraction.
            if self.scroll[axis] * d < 0.0 {
                self.scroll[axis] = 0.0;
            }
            self.scroll[axis] += d;
            while self.scroll[axis].abs() >= 1.0 {
                let up = self.scroll[axis] > 0.0;
                self.scroll[axis] -= self.scroll[axis].signum();
                let key = input::scroll(if up { pos } else { neg }, self.modifiers);
                self.on_mouse_key(key, event_loop);
            }
        }
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        let ready = self.state.shown.is_some()
            || self.state.queue.is_empty()
            || now >= self.started + SHOW_TIMEOUT;
        if !self.shown_window && !ready {
            return; // about_to_wait wakes us at the timeout
        }
        self.state.advance(now);
        self.state.tick(now);
        let Some(g) = &mut self.gfx else { return };
        if !self.shown_window {
            g.window.set_visible(true);
        }
        let theme = self.state.theme_at(now);
        let scene = ui::scene(&self.state, &theme, now, &mut |t| g.renderer.measure(t));
        let result = g.renderer.render(&scene);
        if let Some(path) = std::env::var_os("REMAGE_SNAPSHOT")
            && !self.state.is_animating(now)
            && self.state.loading_since.is_none()
        {
            match g.renderer.snapshot(&scene, Path::new(&path)) {
                Ok(()) => log::info!("snapshot written to {}", Path::new(&path).display()),
                Err(e) => log::error!("snapshot: {e:#}"),
            }
        }
        drop(scene);
        if let Err(e) = result {
            log::error!("render: {e:#}");
            self.state
                .notify(format!("error: {e}"), MsgKind::Error, now);
        }
        if !self.shown_window {
            self.shown_window = true;
            log::debug!("first frame after {:.1?}", self.started.elapsed());
            let proxy = self.proxy.clone();
            std::thread::spawn(move || {
                let _ = proxy.send_event(UserEvent::Fonts(text::all_fonts()));
            });
        }
        if self.state.is_animating(Instant::now()) {
            g.window.request_redraw();
        }
    }

    /// Upload preloaded images a slice at a time. Returns whether work remains.
    fn pump_uploads(&mut self) -> bool {
        let Some(g) = &mut self.gfx else { return false };
        if let Some((path, img)) = self.uploads.front() {
            let img = fit_gpu(img.clone(), g.renderer.max_texture_dim());
            if g.renderer.upload(path, &img, UPLOAD_BUDGET) {
                self.uploads.pop_front();
            }
        }
        !self.uploads.is_empty()
    }
}

/// A config error trimmed for the status line: `config.toml:2:1: unknown field `fitt`; did you
/// mean `fit`?` without the directory or the list of every valid field.
fn short_error(message: &str) -> String {
    let mut s = match message.split_once(": ") {
        Some((loc, rest)) if loc.contains('/') => {
            format!("{}: {rest}", loc.rsplit('/').next().unwrap_or(loc))
        }
        _ => message.to_string(),
    };
    if let Some(start) = s.find(", expected one of") {
        let end = s[start..]
            .find("; did you mean")
            .map_or(s.len(), |i| start + i);
        s.replace_range(start..end, "");
    }
    s
}

fn keymap(layers: &[KeyTables]) -> Keymap {
    let refs: Vec<&KeyTables> = layers.iter().collect();
    Keymap::new(&refs)
}

fn font_families(c: &Config) -> [String; 3] {
    [
        c.ui.font_ui.clone(),
        c.ui.font_mono.clone(),
        c.ui.font_display.clone(),
    ]
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Shrink an image that exceeds the GPU's texture limit (decoded before the limit was known).
fn fit_gpu(img: Arc<DecodedImage>, max_dim: u32) -> Arc<DecodedImage> {
    if img.size[0] <= max_dim && img.size[1] <= max_dim {
        return img;
    }
    let owned = Arc::try_unwrap(img).unwrap_or_else(|a| DecodedImage {
        pixels: a.pixels.clone(),
        ..*a
    });
    match decode::downscale(owned, max_dim) {
        Ok(small) => Arc::new(small),
        Err(e) => {
            log::error!("downscale: {e:#}");
            Arc::new(DecodedImage {
                size: [1, 1],
                pixels: vec![0; 4],
                format: "?",
                source_size: [1, 1],
            })
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let (w, h) = self.opts.geometry.unwrap_or((1280, 800));
        let mut attrs = Window::default_attributes()
            .with_title("remage")
            .with_inner_size(LogicalSize::new(w, h))
            .with_decorations(self.config.window.decorations)
            .with_visible(false);
        if self.opts.fullscreen {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let class = self.opts.class.clone();
        attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(
            attrs,
            class.clone(),
            class.clone(),
        );
        attrs =
            winit::platform::x11::WindowAttributesExtX11::with_name(attrs, class.clone(), class);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.error = Some(anyhow::anyhow!("create window: {e}"));
                event_loop.exit();
                return;
            }
        };
        let renderer = match Renderer::new(window.clone(), event_loop.owned_display_handle()) {
            Ok(mut r) => {
                r.set_font_families(font_families(&self.config));
                r
            }
            Err(e) => {
                self.error = Some(e.context("initialise GPU"));
                event_loop.exit();
                return;
            }
        };
        log::debug!("GPU ready after {:.1?}", self.started.elapsed());
        self.max_dim
            .store(renderer.max_texture_dim(), Ordering::Relaxed);
        let size = window.inner_size();
        self.state.set_window(
            size.width as f32,
            size.height as f32,
            window.scale_factor() as f32,
        );
        self.gfx = Some(Gfx { renderer, window });
        self.sync(false);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Decoded { path, result } => self.on_decoded(path, result),
            UserEvent::Found(paths) => {
                log::debug!("sniffing found {} more images", paths.len());
                let was_empty = self.state.queue.is_empty();
                self.state.queue.insert_sorted(paths);
                self.sync(was_empty);
            }
            UserEvent::Fonts(db) => {
                if let Some(g) = &mut self.gfx {
                    g.renderer.set_fonts(db);
                    g.window.request_redraw();
                }
            }
            UserEvent::Config(result) => match *result {
                Ok(loaded) => {
                    log::info!("config reloaded");
                    self.apply_config(loaded);
                }
                Err(e) => {
                    log::error!("{e}");
                    self.config_failed(&e.message);
                }
            },
            UserEvent::Sorted(files) => {
                self.state.queue.reorder(files);
                self.sync(false);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                let Some(g) = &mut self.gfx else { return };
                g.renderer.resize(size.width, size.height);
                let sf = g.window.scale_factor() as f32;
                self.state
                    .set_window(size.width as f32, size.height as f32, sf);
                self.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let [w, h] = self.state.window();
                self.state.set_window(w, h, scale_factor as f32);
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::Focused(false) => {
                self.held.clear();
                self.state.stop_all_pans();
                self.dragging = false;
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => {
                self.on_key(event, event_loop);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = [position.x as f32, position.y as f32];
                if self.dragging && self.state.shown.is_some() {
                    self.state
                        .view
                        .pan_pixels([p[0] - self.cursor[0], p[1] - self.cursor[1]]);
                    self.request_redraw();
                }
                self.cursor = p;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                if button == MouseButton::Left {
                    // Drag pans; a press and release in place is a `<LeftMouse>` click.
                    self.dragging = pressed;
                    if pressed {
                        self.press = Some(self.cursor);
                        return;
                    }
                    let Some(p) = self.press.take() else { return };
                    let moved = (self.cursor[0] - p[0]).hypot(self.cursor[1] - p[1]);
                    if moved > CLICK_SLOP * self.state.scale_factor() {
                        return;
                    }
                } else if !pressed {
                    return;
                }
                if let Some(key) = input::button(button, self.modifiers) {
                    self.on_mouse_key(key, event_loop);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let d = match delta {
                    MouseScrollDelta::LineDelta(x, y) => [x, y],
                    MouseScrollDelta::PixelDelta(p) => {
                        [p.x as f32 / PIXELS_PER_STEP, p.y as f32 / PIXELS_PER_STEP]
                    }
                };
                self.on_wheel(d, event_loop);
            }
            _ => {}
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            self.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let busy = self.pump_uploads();
        if let Some(t) = self.ambiguous_since
            && now >= t + SEQUENCE_TIMEOUT
        {
            let runs = self.keymap.timeout(self.state.mode);
            self.update_pending(now);
            for run in runs {
                self.run(run.counted(), None, event_loop);
            }
        }
        let (redraw, mut next) = self.state.tick(now);
        if let Some(t) = self.ambiguous_since {
            let at = t + SEQUENCE_TIMEOUT;
            next = Some(next.map_or(at, |n| n.min(at)));
        }
        if redraw {
            self.request_redraw();
        }
        if !self.shown_window {
            let at = self.started + SHOW_TIMEOUT;
            next = Some(next.map_or(at, |n| n.min(at)));
        }
        event_loop.set_control_flow(match (busy, next) {
            (true, _) => ControlFlow::Poll,
            (false, Some(t)) => ControlFlow::WaitUntil(t),
            (false, None) => ControlFlow::Wait,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_errors_are_shortened_for_the_bar() {
        assert_eq!(
            short_error(
                "/home/u/.config/remage/config.toml:2:1: unknown field `fitt`, expected one of \
                 `fit`, `zoom_step`; did you mean `fit`?"
            ),
            "config.toml:2:1: unknown field `fitt`; did you mean `fit`?"
        );
        assert_eq!(
            short_error("a.toml:1:1: unknown field `x`, expected `y`"),
            "a.toml:1:1: unknown field `x`, expected `y`"
        );
    }
}
