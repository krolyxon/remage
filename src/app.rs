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

use crate::action::Action;
use crate::cache::ImageCache;
use crate::config::theme::Theme;
use crate::decode::{self, DecodedImage};
use crate::input::{self, Input};
use crate::render::{Renderer, text};
use crate::state::view::Dir;
use crate::state::{AppState, Effects, MsgKind};
use crate::ui;
use crate::worker::{JobKey, Pool, Priority};

/// GPU upload budget per event-loop turn, so preloads never cause a hitch.
const UPLOAD_BUDGET: usize = 32 << 20;
/// Show the window once the first image is ready, or after this long regardless.
const SHOW_TIMEOUT: Duration = Duration::from_millis(300);
/// Wheel pixels per zoom step for touchpads.
const PIXELS_PER_STEP: f32 = 60.0;

pub enum UserEvent {
    Decoded {
        path: PathBuf,
        result: Result<Arc<DecodedImage>, String>,
    },
    /// Unknown-extension files that turned out to be images.
    Found(Vec<PathBuf>),
    /// Embedded plus system fonts, loaded off-thread after the first frame.
    Fonts(glyphon::fontdb::Database),
}

pub struct Options {
    pub fullscreen: bool,
    pub geometry: Option<(u32, u32)>,
    pub class: String,
    pub cache_mb: usize,
}

struct Gfx {
    renderer: Renderer,
    // Dropped after the renderer, whose surface borrows the window.
    window: Arc<Window>,
}

pub struct App {
    opts: Options,
    state: AppState,
    theme: Theme,
    input: Input,
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
    held: HashMap<PhysicalKey, Dir>,
    started: Instant,
    shown_window: bool,
    pub error: Option<anyhow::Error>,
}

impl App {
    pub fn new(opts: Options, state: AppState, proxy: EventLoopProxy<UserEvent>) -> Self {
        let cache = ImageCache::new(opts.cache_mb << 20);
        let mut app = Self {
            opts,
            state,
            theme: Theme::nothing_dark(),
            input: Input::default(),
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
            self.input.cancel();
            self.state.pending.clear();
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
            let title = match self.state.queue.current() {
                Some(p) => format!("{} — remage", ui::file_name(p)),
                None => "remage".into(),
            };
            g.window.set_title(&title);
        }
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

    fn apply(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        let fx = self.state.apply(action, Instant::now());
        self.effects(fx, event_loop);
    }

    fn effects(&mut self, fx: Effects, event_loop: &ActiveEventLoop) {
        if fx.quit {
            event_loop.exit();
            return;
        }
        if fx.fullscreen
            && let Some(g) = &self.gfx
        {
            let on = g.window.fullscreen().is_some();
            g.window
                .set_fullscreen((!on).then_some(Fullscreen::Borderless(None)));
        }
        if fx.reload {
            self.reload();
        }
        if fx.navigated {
            self.sync(true);
        }
        if fx.redraw || fx.layout {
            self.request_redraw();
        }
    }

    fn on_key(&mut self, event: winit::event::KeyEvent, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if event.state == ElementState::Released {
            if let Some(dir) = self.held.remove(&event.physical_key) {
                self.state.hold_pan(dir, false, now);
                self.request_redraw();
            }
            return;
        }
        let action = self.input.key(&event.logical_key, self.modifiers);
        let pending = self.input.pending();
        if pending != self.state.pending {
            self.state.pending = pending;
            self.request_redraw();
        }
        let Some(action) = action else { return };
        if event.repeat && !input::repeats(&action) {
            return;
        }
        if let Action::Pan(dir) = action {
            if self.state.shown.is_some() {
                self.held.insert(event.physical_key, dir);
                self.state.hold_pan(dir, true, now);
                self.request_redraw();
            }
            return;
        }
        self.apply(action, event_loop);
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
        let scene = ui::scene(&self.state, &self.theme, now, &mut |t| {
            g.renderer.measure(t)
        });
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
            .with_decorations(false)
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
            Ok(r) => r,
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
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let now = Instant::now();
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
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.dragging = state == ElementState::Pressed;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / PIXELS_PER_STEP,
                };
                if steps != 0.0 {
                    self.state.zoom_at(steps, self.cursor, now);
                    self.request_redraw();
                }
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
        let (redraw, mut next) = self.state.tick(now);
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
