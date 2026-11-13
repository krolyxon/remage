//! winit `ApplicationHandler`: routes events, owns the state and the renderer.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{ModifiersState, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::action::{Action, CATALOG, ModeArg, Switch, Zoom};
use crate::cache::ImageCache;
use crate::config::watch::ConfigWatch;
use crate::config::{Config, LoadError, Loaded, Output, Targets, suggest};
use crate::decode::{self, DecodedImage};
use crate::exec;
use crate::input::keymap::{KeyTables, Keymap};
use crate::input::keys::{Code, Key, KeySeq, Named, Scroll};
use crate::input::{self};
use crate::ipc::{self, Query, Request, Topic};
use crate::render::{Renderer, text};
use crate::scan;
use crate::state::overlay::{Edit, Help, HelpRow, Info, InputKind, PaletteItem, WhichRow};
use crate::state::queue::Queue;
use crate::state::view::{Dir, FitMode};
use crate::state::{AppState, Effects, Mode, MsgKind, Settings};
use crate::thumbs::{self, CacheOptions, Thumb, ThumbError};
use crate::ui;
use crate::watch::{self, DirWatch};
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
/// In the grid, the selected image decodes once the cursor has rested this long, so `<CR>`
/// opens it at once without decoding everything the cursor passes over.
const GRID_REST: Duration = Duration::from_millis(250);
/// A scalable image is re-rendered for the zoom once the view has rested this long.
const REFINE_DELAY: Duration = Duration::from_millis(150);
/// Two clicks on the same thumbnail this close together open it.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

pub enum UserEvent {
    Decoded {
        path: PathBuf,
        /// The file's mtime when decoding started, for `refresh`.
        mtime: Option<SystemTime>,
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
    /// A thumbnail for cells of `px` pixels.
    Thumb {
        path: PathBuf,
        px: u32,
        mtime: Option<SystemTime>,
        result: Result<Thumb, ThumbError>,
    },
    /// A sharper version of an image (SVG at the current zoom, a developed RAW).
    Refined {
        path: PathBuf,
        result: Result<Arc<DecodedImage>, String>,
    },
    /// Output or the exit of a user command.
    Command { id: u64, event: exec::Event },
    /// `refresh` re-checked these files: their mtime, or `None` if they are gone.
    Refreshed(Vec<(PathBuf, Option<SystemTime>)>),
    /// A request on the IPC socket.
    Ipc(ipc::Incoming),
    /// Metadata for the info panel.
    Info(Info),
    /// The last line `ui.info_command` printed for a file.
    InfoLine { path: PathBuf, line: String },
    /// Files appeared, changed or vanished in a watched directory.
    Dir(watch::Changes),
    /// `open <path>` finished scanning.
    Opened { arg: PathBuf, scan: scan::Scan },
}

/// Command-line settings that beat the config, across reloads too.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub fit: Option<FitMode>,
    pub zoom: Option<f32>,
    pub bar: Option<bool>,
    pub slideshow_delay: Option<Duration>,
}

impl Overrides {
    pub fn apply(&self, s: &mut Settings) {
        if let Some(d) = self.slideshow_delay {
            s.slideshow_delay = d;
        }
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
    /// Never write the shared thumbnail cache (`-p`).
    pub private: bool,
    /// The IPC socket, passed to commands as `$REMAGE_SOCKET`.
    pub socket: Option<PathBuf>,
    /// Directories the queue came from, for `watch_dir`.
    pub dirs: Vec<(PathBuf, bool)>,
}

/// Where a chain of actions came from, which decides what some of them act on.
#[derive(Clone, Debug, Default)]
struct Ctx {
    /// The cursor, for mouse-triggered zoom.
    anchor: Option<[f32; 2]>,
    /// A command's targets, which its `refresh` re-checks (otherwise the whole queue).
    targets: Option<Arc<[PathBuf]>>,
}

/// A user command from spawn to exit.
struct Job {
    name: String,
    output: Output,
    targets: Arc<[PathBuf]>,
    /// The command's `after` actions, then whatever followed `run` in its binding: they wait
    /// for a successful exit.
    then: Vec<Action>,
    /// stdout lines run as actions so far.
    lines: usize,
}

/// An IPC connection streaming events.
struct Subscriber {
    topics: Vec<Topic>,
    tx: crossbeam_channel::Sender<String>,
}

/// What subscribers last heard about.
#[derive(Clone, Debug, PartialEq)]
struct Published {
    image: (Option<PathBuf>, usize, usize),
    marks: u64,
    mode: Mode,
}

/// Thumbnail jobs for the current cell size.
#[derive(Default)]
struct ThumbJobs {
    /// Cell size the atlas and requests are for (0 = none yet).
    cell: u32,
    requested: HashMap<PathBuf, Priority>,
    /// Paths near the view; the atlas never evicts these.
    wanted: HashSet<PathBuf>,
    /// Finished thumbnails waiting for the atlas.
    arrivals: VecDeque<(PathBuf, Thumb)>,
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
    /// Decoded images whose result event is still in the loop's queue.
    arriving: Arc<Mutex<HashSet<PathBuf>>>,
    gfx: Option<Gfx>,
    uploads: VecDeque<(PathBuf, Arc<DecodedImage>)>,
    thumbs: ThumbJobs,
    thumb_cache: CacheOptions,
    /// When the grid cursor last moved, until the selected image is requested.
    grid_rest: Option<Instant>,
    /// The last click in the grid, for double-clicks.
    last_click: Option<(Instant, usize)>,
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
    /// File mtimes when they were last decoded or thumbnailed, so `refresh` spots changes.
    stamps: HashMap<PathBuf, SystemTime>,
    jobs: HashMap<u64, Job>,
    next_job: u64,
    /// A command waiting for `y` at its confirm prompt.
    confirm: Option<(Job, exec::Prepared)>,
    subscribers: Vec<Subscriber>,
    published: Option<Published>,
    /// A refinement waiting for the view to rest: (path, target size, since).
    refine_wait: Option<(PathBuf, u32, Instant)>,
    /// The image being refined.
    refining: Option<PathBuf>,
    /// Refinements that failed; not retried.
    refine_failed: HashSet<PathBuf>,
    /// `quit-no-output` ran: `-o` prints nothing.
    no_output: bool,
    /// When the pending key sequence started waiting, for which-key.
    which_key_since: Option<Instant>,
    /// Watches the queue's directories while `watch_dir` is on.
    dir_watch: Option<DirWatch>,
    /// The file whose metadata is being read for the info panel.
    info_loading: Option<PathBuf>,
    /// The file `ui.info_command` last ran for.
    info_line_for: Option<PathBuf>,
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
        let thumb_cache = CacheOptions::new(loaded.config.grid.thumbnail_cache, opts.private);
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
            arriving: Arc::default(),
            gfx: None,
            uploads: VecDeque::new(),
            thumbs: ThumbJobs::default(),
            thumb_cache,
            grid_rest: None,
            last_click: None,
            modifiers: ModifiersState::empty(),
            cursor: [0.0, 0.0],
            dragging: false,
            press: None,
            scroll: [0.0, 0.0],
            held: HashMap::new(),
            started: Instant::now(),
            shown_window: false,
            stamps: HashMap::new(),
            jobs: HashMap::new(),
            next_job: 1,
            confirm: None,
            subscribers: Vec::new(),
            published: None,
            refine_wait: None,
            refining: None,
            refine_failed: HashSet::new(),
            no_output: false,
            which_key_since: None,
            dir_watch: None,
            info_loading: None,
            info_line_for: None,
            error: None,
        };
        app.set_dir_watch();
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
        let opts = self.decode_options();
        let arriving = self.arriving.clone();
        let path = path.to_path_buf();
        self.pool
            .submit(JobKey::Decode(path.clone()), prio, move || {
                let t = Instant::now();
                let mtime = mtime(&path);
                let result = decode::decode(&path, &opts)
                    .map(Arc::new)
                    .map_err(|e| e.to_string());
                log::debug!("decoded {} in {:.1?}", path.display(), t.elapsed());
                lock(&arriving).insert(path.clone());
                let _ = proxy.send_event(UserEvent::Decoded {
                    path,
                    mtime,
                    result,
                });
            });
    }

    fn decode_options(&self) -> decode::Options {
        let [w, h] = self.state.window();
        let long = w.max(h) as u32;
        decode::Options {
            // Before the window exists, a typical screen.
            target: if long > 1 { long } else { 2048 },
            auto_orient: self.config.image.auto_orient,
            color: self.config.image.color_management,
        }
    }

    /// Whether the shown image should be re-rendered sharper for the current zoom. Waits for
    /// the view to rest; returns when to check again.
    fn check_refine(&mut self, now: Instant) -> Option<Instant> {
        if self.state.mode != Mode::Image || self.state.view.is_animating(now) {
            return None;
        }
        let path = self
            .state
            .shown
            .as_ref()
            .filter(|s| Some(s.path.as_path()) == self.state.queue.current())?
            .path
            .clone();
        if self.refining.is_some() || self.refine_failed.contains(&path) {
            return None;
        }
        let img = self.cache.get(&path)?;
        let refine = img.refine.clone()?;
        let source = img.source_size[0].max(img.source_size[1]) as f32;
        let want = (source * self.state.view.scale()).round() as u32;
        let Some(target) = refine.wants(img.size[0].max(img.size[1]), want) else {
            self.refine_wait = None;
            return None;
        };
        match &self.refine_wait {
            Some((p, t, since)) if *p == path && *t == target => {
                let at = *since + REFINE_DELAY;
                if now < at {
                    return Some(at);
                }
            }
            _ => {
                self.refine_wait = Some((path, target, now));
                return Some(now + REFINE_DELAY);
            }
        }
        self.refine_wait = None;
        self.refining = Some(path.clone());
        log::debug!("refining {} to {target} px", path.display());
        let proxy = self.proxy.clone();
        let opts = self.decode_options();
        self.pool
            .submit(JobKey::Refine(path.clone()), Priority::Current, move || {
                let t = Instant::now();
                let result = refine
                    .run(&path, target, &opts)
                    .map(Arc::new)
                    .map_err(|e| format!("{e:#}"));
                log::debug!("refined {} in {:.1?}", path.display(), t.elapsed());
                let _ = proxy.send_event(UserEvent::Refined { path, result });
            });
        None
    }

    fn on_refined(&mut self, path: PathBuf, result: Result<Arc<DecodedImage>, String>) {
        if self.refining.as_ref() == Some(&path) {
            self.refining = None;
        }
        match result {
            Ok(img) => {
                let wanted = self.window_paths();
                let keep: Vec<&Path> = wanted.iter().map(PathBuf::as_path).collect();
                self.cache.insert(path.clone(), img.clone(), &keep);
                let shown = self.state.shown.as_ref().is_some_and(|s| s.path == path);
                if shown && let Some(g) = &mut self.gfx {
                    // Same source size, so the view stays exactly where it is.
                    g.renderer.upload(&path, &img, usize::MAX);
                    g.window.request_redraw();
                }
            }
            Err(e) => {
                log::warn!("{}: {e}", path.display());
                self.refine_failed.insert(path);
            }
        }
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
            // A pending refinement was for the image we left (and its job may be cancelled).
            self.refine_wait = None;
            self.refining = None;
        }
        let wanted = self.window_paths();
        self.update_title();
        self.request_info();
        let Some(cur) = wanted.first().cloned() else {
            self.state.clear_shown();
            self.uploads.clear();
            if let Some(g) = &mut self.gfx {
                g.renderer.retain(|_| false);
            }
            self.request_redraw();
            return;
        };
        if self.state.in_grid() {
            if navigated || !self.cache_has(&cur) {
                self.grid_rest = Some(now);
            }
            self.sync_thumbs();
            self.request_redraw();
            return;
        }

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

    fn cache_has(&mut self, path: &Path) -> bool {
        self.cache.get(path).is_some()
    }

    /// Request thumbnails around the grid view: visible ones first, then a page above and
    /// two below. Requests that scrolled out of range are cancelled.
    fn sync_thumbs(&mut self) {
        if !self.state.in_grid() {
            return;
        }
        let px = self.state.cell_px();
        if px != self.thumbs.cell {
            for p in self.thumbs.requested.drain().map(|(p, _)| p) {
                self.pool.cancel(&JobKey::Thumb(p));
            }
            self.thumbs.arrivals.clear();
            self.state.thumbs.clear();
            // Before the GPU exists, thumbnails wait in `arrivals` for an atlas of this size.
            if let Some(g) = &mut self.gfx {
                g.renderer.reset_thumbs(px);
            }
            self.thumbs.cell = px;
        }
        let l = self.state.grid_layout();
        let visible = l.visible(self.state.grid.target(&l));
        let span = l.page_rows() * l.cols;
        let near = visible.start.saturating_sub(span)..(visible.end + 2 * span).min(l.count);
        let q = &self.state.queue;
        let mut wanted = HashSet::with_capacity(near.len());
        let mut visible_paths = HashSet::with_capacity(visible.len());
        let mut submit = Vec::new();
        for i in near {
            let Some(p) = q.get(i) else { continue };
            wanted.insert(p.to_path_buf());
            let prio = if visible.contains(&i) {
                visible_paths.insert(p.to_path_buf());
                Priority::ThumbVisible
            } else {
                Priority::Thumb
            };
            if !self.state.thumbs.contains_key(p) && self.thumbs.requested.get(p) != Some(&prio) {
                submit.push((p.to_path_buf(), prio));
            }
        }
        // Visible first, in order.
        submit.sort_by_key(|(_, prio)| *prio);
        for (p, prio) in submit {
            self.request_thumb(p.clone(), prio);
            self.thumbs.requested.insert(p, prio);
        }
        let mut demote = Vec::new();
        self.thumbs.requested.retain(|p, prio| {
            if !wanted.contains(p) {
                self.pool.cancel(&JobKey::Thumb(p.clone()));
                return false;
            }
            if *prio == Priority::ThumbVisible && !visible_paths.contains(p) {
                *prio = Priority::Thumb;
                demote.push(p.clone());
            }
            true
        });
        for p in demote {
            self.request_thumb(p, Priority::Thumb);
        }
        self.thumbs.wanted = wanted;
    }

    fn request_thumb(&self, path: PathBuf, prio: Priority) {
        let proxy = self.proxy.clone();
        let cache = self.thumb_cache.clone();
        let px = self.thumbs.cell;
        self.pool
            .submit(JobKey::Thumb(path.clone()), prio, move || {
                let t = Instant::now();
                let mtime = mtime(&path);
                let result = thumbs::load(&path, px, &cache);
                log::trace!("thumbnail {} in {:.1?}", path.display(), t.elapsed());
                let _ = proxy.send_event(UserEvent::Thumb {
                    path,
                    px,
                    mtime,
                    result,
                });
            });
    }

    fn on_thumb(
        &mut self,
        path: PathBuf,
        px: u32,
        mtime: Option<SystemTime>,
        result: Result<Thumb, ThumbError>,
    ) {
        self.thumbs.requested.remove(&path);
        if px != self.thumbs.cell {
            return;
        }
        match result {
            Ok(t) => {
                if let Some(m) = mtime {
                    self.stamps.insert(path.clone(), m);
                }
                self.thumbs.arrivals.push_back((path, t));
            }
            Err(e) => {
                // Undecodable: it leaves the queue, as in image mode.
                log::warn!("{}: {e}", path.display());
                let was_current = self.state.queue.current() == Some(path.as_path());
                if self.state.queue.remove(&path) {
                    self.state.skipped += 1;
                }
                self.sync(was_current);
                self.request_redraw();
            }
        }
    }

    /// Every thumbnail on screen is in the atlas.
    fn grid_settled(&self) -> bool {
        let l = self.state.grid_layout();
        let scroll = self.state.grid.scroll(&l, Instant::now());
        l.visible(scroll).all(|i| {
            self.state
                .queue
                .get(i)
                .is_some_and(|p| self.state.thumbs.contains_key(p))
        })
    }

    /// Upload `img` in full and put it on screen.
    fn show(&mut self, path: &Path, img: Arc<DecodedImage>, now: Instant) {
        let Some(g) = &mut self.gfx else {
            // No GPU yet: keep it cached; `resumed` syncs again.
            self.state.loading_since.get_or_insert(now);
            return;
        };
        g.renderer.upload(path, &img, usize::MAX);
        self.uploads.retain(|(p, _)| p != path);
        self.state.show(path, img.source_size, img.format, now);
        self.state.set_frames(img.delays(), img.animated, now);
        self.request_info_line();
        self.request_redraw();
    }

    fn update_title(&self) {
        if let Some(g) = &self.gfx {
            let title = match self.state.queue.current() {
                Some(_) => {
                    ui::template::expand(&self.config.window.title, &|n| ui::var(&self.state, n))
                }
                None => "remage".into(),
            };
            g.window.set_title(&title);
        }
    }

    /// Start, retarget or stop the directory watch to match `watch_dir` and the queue.
    fn set_dir_watch(&mut self) {
        let dirs = if self.config.general.watch_dir {
            self.opts.dirs.clone()
        } else {
            Vec::new()
        };
        match &self.dir_watch {
            Some(w) => w.set(dirs),
            None if !dirs.is_empty() => {
                let proxy = self.proxy.clone();
                self.dir_watch = Some(watch::spawn(dirs, move |c| {
                    let _ = proxy.send_event(UserEvent::Dir(c));
                }));
            }
            None => {}
        }
    }

    /// Files changed on disk: new images join the queue, changed ones reload, gone ones leave.
    fn on_dir(&mut self, c: watch::Changes) {
        let known: HashSet<&PathBuf> = self.state.queue.all_files().iter().collect();
        let (old, new): (Vec<_>, Vec<_>) =
            c.present.into_iter().partition(|(p, _)| known.contains(p));
        let mut found: Vec<(PathBuf, Option<SystemTime>)> =
            old.into_iter().map(|(p, t)| (p, Some(t))).collect();
        found.extend(c.gone.into_iter().map(|p| (p, None)));
        if !found.is_empty() {
            self.on_refreshed(found);
        }
        if !new.is_empty() {
            log::debug!("{} new files on disk", new.len());
            let was_empty = self.state.queue.is_empty();
            self.state
                .queue
                .insert_sorted(new.into_iter().map(|(p, _)| p));
            self.sync(was_empty);
        }
    }

    /// `open <path>`: a file already queued is just selected; anything else is scanned
    /// (with its siblings) off-thread and replaces the queue.
    fn open(&mut self, arg: &str) {
        let cwd = std::env::current_dir().unwrap_or_default();
        let path = crate::config::expand_path(arg, &cwd);
        if let Some(i) = self.state.queue.files().iter().position(|f| *f == path) {
            let fx = self.state.apply(Action::Goto(i + 1), Instant::now());
            if fx.navigated {
                self.sync(true);
            }
            return;
        }
        let g = &self.config.general;
        let opts = scan::ScanOptions {
            recursive: g.recursive,
            siblings: g.open_siblings,
            sort: g.sort,
            reverse: g.reverse,
        };
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let scan = scan::scan(std::slice::from_ref(&path), &opts);
            let _ = proxy.send_event(UserEvent::Opened { arg: path, scan });
        });
    }

    fn on_opened(&mut self, arg: PathBuf, scan: scan::Scan) {
        let now = Instant::now();
        if scan.files.is_empty() && scan.unknown.is_empty() {
            let why = scan
                .errors
                .first()
                .cloned()
                .unwrap_or_else(|| format!("{}: no images", arg.display()));
            self.state
                .notify(format!("error: {why}"), MsgKind::Error, now);
            self.request_redraw();
            return;
        }
        log::debug!("opened {}: {} files", arg.display(), scan.files.len());
        if !scan.unknown.is_empty() {
            let proxy = self.proxy.clone();
            let unknown = scan.unknown;
            std::thread::spawn(move || {
                let found: Vec<_> = unknown.into_iter().filter(|p| decode::sniff(p)).collect();
                if !found.is_empty() {
                    let _ = proxy.send_event(UserEvent::Found(found));
                }
            });
        }
        self.state.queue = Queue::new(scan.files, scan.start.as_deref(), 0);
        self.state.skipped = 0;
        self.state.input = None;
        self.opts.dirs = scan.dirs;
        self.set_dir_watch();
        if self.state.in_grid() {
            let l = self.state.grid_layout();
            self.state
                .grid
                .jump_to(l.follow(self.state.queue.index(), 0.0));
        }
        self.sync(true);
    }

    /// Read the current file's metadata for the info panel, off-thread.
    fn request_info(&mut self) {
        if !self.state.info_open {
            return;
        }
        let Some(cur) = self.state.queue.current().map(Path::to_path_buf) else {
            return;
        };
        if self.state.info.as_ref().is_some_and(|i| i.path == cur)
            || self.info_loading.as_ref() == Some(&cur)
        {
            return;
        }
        self.info_loading = Some(cur.clone());
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let rows = decode::meta::read(&cur);
            let _ = proxy.send_event(UserEvent::Info(Info { path: cur, rows }));
        });
    }

    /// Run `ui.info_command` for the image on screen (nsxiv's image-info: file, width,
    /// height); its last line fills `{info}`.
    fn request_info_line(&mut self) {
        let cmd = self.config.ui.info_command.trim();
        let Some(shown) = &self.state.shown else {
            return;
        };
        if cmd.is_empty() || self.info_line_for.as_ref() == Some(&shown.path) {
            return;
        }
        let path = shown.path.clone();
        self.info_line_for = Some(path.clone());
        let mut c = std::process::Command::new("sh");
        c.arg("-c")
            .arg(format!("{cmd} \"$@\""))
            .arg("remage")
            .arg(&path)
            .arg(shown.size[0].to_string())
            .arg(shown.size[1].to_string())
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(sock) = &self.opts.socket {
            c.env("REMAGE_SOCKET", sock);
        }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let line = match c.output() {
                Ok(o) => String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                Err(e) => {
                    log::warn!("info_command: {e}");
                    String::new()
                }
            };
            let _ = proxy.send_event(UserEvent::InfoLine { path, line });
        });
    }

    /// What a binding does, in a few words: a command's own `desc` for `run`.
    fn describe(&self, actions: &[Action]) -> String {
        let one = |a: &Action| match a {
            Action::Run(name) => self
                .config
                .commands
                .get(name)
                .map(|c| c.desc.clone())
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| a.description()),
            _ => a.description(),
        };
        actions.iter().map(one).collect::<Vec<_>>().join(" · ")
    }

    /// The keys bound to exactly `action` in the current mode, keyboard first.
    fn keys_for(&self, action: &Action) -> String {
        let mut keys = self.keymap.keys_for(self.state.mode, action);
        keys.sort_by_key(|k| {
            k.0.iter()
                .any(|k| matches!(k.code, Code::Mouse(_) | Code::Scroll(_)))
        });
        keys.iter()
            .take(2)
            .map(|k| ui::pretty_keys(&k.to_string()))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Palette rows: user commands, then every action (each argument choice spelled out,
    /// placeholders like `goto N` left to complete).
    fn palette_items(&self) -> Vec<PaletteItem> {
        let mut seen = HashSet::new();
        let mut items = Vec::new();
        let mut push = |items: &mut Vec<PaletteItem>,
                        action: String,
                        desc: String,
                        keys: String,
                        template: bool| {
            if seen.insert(action.clone()) {
                items.push(PaletteItem {
                    action,
                    desc,
                    keys,
                    template,
                });
            }
        };
        for (name, cmd) in &self.config.commands {
            let a = Action::Run(name.clone());
            let desc = if cmd.desc.is_empty() {
                format!("Run {name}")
            } else {
                cmd.desc.clone()
            };
            push(&mut items, a.to_string(), desc, self.keys_for(&a), false);
        }
        for entry in CATALOG {
            if entry.name == "run" {
                continue;
            }
            let args = entry.args.trim_start_matches('[').trim_end_matches(']');
            // Optional arguments (`[n]`, `[toggle|on|off]`) also offer the bare name.
            let mut options: Vec<&str> = vec![""];
            if !entry.args.is_empty() {
                if !entry.args.starts_with('[') {
                    options.clear();
                }
                options.extend(
                    args.split('|')
                        .map(|o| o.trim_end_matches(" [n]").trim_end_matches(']'))
                        .filter(|o| *o != "n"),
                );
            }
            for opt in options {
                let text = format!("{} {opt}", entry.name);
                let required = !entry.args.is_empty() && !entry.args.starts_with('[');
                let placeholder = opt.contains(['<', 'N']);
                match text.trim().parse::<Action>() {
                    Ok(a) if !placeholder => {
                        let desc = a.description();
                        push(&mut items, a.to_string(), desc, self.keys_for(&a), false);
                    }
                    _ if required || placeholder => {
                        push(
                            &mut items,
                            text.trim().to_string(),
                            entry.desc.to_string(),
                            String::new(),
                            true,
                        );
                    }
                    _ => {}
                }
            }
        }
        items
    }

    /// Help rows: the live keymap of the current mode, grouped, keys for one action merged.
    fn help_rows(&self) -> Vec<HelpRow> {
        const GROUPS: [&str; 8] = [
            "Navigation",
            "Marks",
            "Grid",
            "View",
            "Playback",
            "Interface",
            "Queue",
            "App",
        ];
        let rank = |a: &Action| {
            CATALOG
                .iter()
                .position(|e| e.name == a.name())
                .unwrap_or(usize::MAX)
        };
        // (group, catalog rank, action, keys, description)
        let mut entries: Vec<(usize, usize, String, String, String)> = Vec::new();
        for (keys, actions) in self.keymap.bindings(self.state.mode) {
            let Some(first) = actions.first() else {
                continue;
            };
            let group = GROUPS.iter().position(|g| *g == first.group()).unwrap_or(0);
            let desc = self.describe(&actions);
            let k = ui::pretty_keys(&keys.to_string());
            match entries.iter_mut().find(|e| e.0 == group && e.4 == desc) {
                Some(e) => {
                    e.3.push_str("  ");
                    e.3.push_str(&k);
                }
                None => entries.push((group, rank(first), first.to_string(), k, desc)),
            }
        }
        // Within a group: catalog order, then by action (`next` before `next 10`).
        entries.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
        let mut rows = Vec::new();
        let mut last = None;
        for (group, _, _, keys, desc) in entries {
            if last != Some(group) {
                rows.push(HelpRow::Header(GROUPS[group].to_string()));
                last = Some(group);
            }
            rows.push(HelpRow::Binding { keys, desc });
        }
        rows
    }

    /// Which-key rows for the pending sequence.
    fn which_key_rows(&self) -> Vec<WhichRow> {
        let mode = self.state.mode;
        let pending = self.keymap.pending_keys().to_vec();
        let all = self.keymap.bindings(mode);
        self.keymap
            .candidates(mode)
            .into_iter()
            .map(|c| {
                let desc = match &c.actions {
                    Some(a) => self.describe(a),
                    None => {
                        let mut prefix = pending.clone();
                        prefix.push(c.key);
                        let n = all.iter().filter(|(k, _)| k.0.starts_with(&prefix)).count();
                        format!("+{n} more")
                    }
                };
                WhichRow {
                    key: ui::pretty_keys(&KeySeq(vec![c.key]).to_string()),
                    desc,
                    prefix: c.prefix && c.actions.is_none(),
                }
            })
            .collect()
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
        decode::set_decoders(&new.decoders);
        let mut settings = Settings::from_config(new);
        self.opts.overrides.apply(&mut settings);
        let preload_changed = settings.preload != self.state.settings.preload;
        if old.grid.size != new.grid.size {
            self.state.grid.size = new.grid.size;
        }
        let watch_changed = old.general.watch_dir != new.general.watch_dir;
        if old.grid.thumbnail_cache != new.grid.thumbnail_cache {
            self.thumb_cache = CacheOptions::new(new.grid.thumbnail_cache, self.opts.private);
        }
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
        if watch_changed {
            self.set_dir_watch();
        }
        self.info_line_for = None;
        self.request_info_line();
        self.update_title();
        // Silent on routine changes (a new wallpaper palette); confirm fixes and manual reloads.
        if std::mem::take(&mut self.config_error) || std::mem::take(&mut self.reload_requested) {
            self.state.notify("config reloaded", MsgKind::Info, now);
        }
        if preload_changed {
            self.sync(false);
        }
        self.sync_thumbs();
        self.request_redraw();
    }

    fn on_decoded(
        &mut self,
        path: PathBuf,
        mtime: Option<SystemTime>,
        result: Result<Arc<DecodedImage>, String>,
    ) {
        lock(&self.arriving).remove(&path);
        let now = Instant::now();
        let is_current = self.state.queue.current() == Some(path.as_path());
        let in_grid = self.state.in_grid();
        match result {
            Ok(img) => {
                if let Some(m) = mtime {
                    self.stamps.insert(path.clone(), m);
                }
                let wanted = self.window_paths();
                let keep: Vec<&Path> = wanted.iter().map(PathBuf::as_path).collect();
                self.cache.insert(path.clone(), img.clone(), &keep);
                if in_grid {
                    // Cached for when the grid opens it.
                } else if is_current {
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

    /// Run actions in order. A `run` suspends the rest of the chain until its command exits
    /// successfully. Errors are shown on the status line; the first is also returned for IPC.
    fn run(&mut self, actions: Vec<Action>, ctx: &Ctx, el: &ActiveEventLoop) -> Result<(), String> {
        let mut rest = actions.into_iter();
        while let Some(action) = rest.next() {
            match &action {
                Action::Run(name) => return self.start_command(name, rest.collect()),
                Action::Cancel => {
                    self.keymap.cancel();
                    self.state.pending.clear();
                    self.config_error = false;
                    self.answer(false);
                }
                Action::QuitNoOutput => self.no_output = true,
                _ => {}
            }
            let fx = self.state.apply_at(action, ctx.anchor, Instant::now());
            self.effects(fx, ctx, el);
            if fx.quit {
                break;
            }
        }
        Ok(())
    }

    /// Start `[commands.<name>]`, or ask first when it has a `confirm`.
    fn start_command(&mut self, name: &str, then: Vec<Action>) -> Result<(), String> {
        let now = Instant::now();
        let fail = |state: &mut AppState, msg: String| {
            state.notify(format!("error: {msg}"), MsgKind::Error, now);
            Err(msg)
        };
        let Some(cmd) = self.config.commands.get(name).cloned() else {
            let hint = suggest(name, self.config.commands.keys().map(String::as_str))
                .map(|s| format!(", did you mean `{s}`?"))
                .unwrap_or_default();
            self.request_redraw();
            return fail(&mut self.state, format!("no command named `{name}`{hint}"));
        };
        let q = &self.state.queue;
        let targets: Arc<[PathBuf]> =
            exec::targets(cmd.targets, q.current(), &q.marked(), q.files()).into();
        if targets.is_empty() && exec::needs_files(&cmd) {
            let why = if cmd.targets == Targets::Marked {
                "no marked files"
            } else {
                "no images"
            };
            self.request_redraw();
            return fail(&mut self.state, format!("{name}: {why}"));
        }
        let size = self
            .state
            .shown
            .as_ref()
            .filter(|s| Some(s.path.as_path()) == q.current())
            .map(|s| s.size);
        let ctx = exec::Context {
            current: q.current(),
            index: q.index(),
            targets: &targets,
            size,
            mode: self.state.mode.name(),
            socket: self.opts.socket.as_deref(),
        };
        let prepared = exec::prepare(&cmd, &ctx);
        let question = cmd.confirm.as_deref().map(|c| exec::expand_text(c, &ctx));
        let mut chain = cmd.after.0.unwrap_or_default();
        chain.extend(then);
        let job = Job {
            name: name.to_string(),
            output: cmd.output,
            targets,
            then: chain,
            lines: 0,
        };
        match question {
            Some(q) => {
                self.answer(false);
                self.state.prompt = Some(q);
                self.confirm = Some((job, prepared));
                self.request_redraw();
            }
            None => self.spawn_job(job, prepared),
        }
        Ok(())
    }

    /// Settle a confirm prompt: `y` runs the command, anything else drops it.
    fn answer(&mut self, yes: bool) {
        let Some((job, prepared)) = self.confirm.take() else {
            return;
        };
        self.state.prompt = None;
        if yes {
            self.spawn_job(job, prepared);
        }
        self.request_redraw();
    }

    fn spawn_job(&mut self, job: Job, prepared: exec::Prepared) {
        let id = self.next_job;
        self.next_job += 1;
        log::info!(
            "run {}: {} {:?}",
            job.name,
            prepared.program.to_string_lossy(),
            prepared.args
        );
        let proxy = self.proxy.clone();
        exec::spawn(&job.name, prepared, job.output, move |event| {
            let _ = proxy.send_event(UserEvent::Command { id, event });
        });
        self.state.running.push((id, job.name.clone()));
        self.jobs.insert(id, job);
        self.request_redraw();
    }

    fn on_command(&mut self, id: u64, event: exec::Event, el: &ActiveEventLoop) {
        let now = Instant::now();
        match event {
            exec::Event::Line(line) => {
                let Some(job) = self.jobs.get_mut(&id) else {
                    return;
                };
                job.lines += 1;
                let name = job.name.clone();
                let ctx = Ctx {
                    anchor: None,
                    targets: Some(job.targets.clone()),
                };
                match line.parse::<Action>() {
                    Ok(a) => {
                        let _ = self.run(vec![a], &ctx, el);
                    }
                    Err(e) => {
                        log::warn!("{name}: {line}: {e}");
                        self.state
                            .notify(format!("error: {name}: {e}"), MsgKind::Error, now);
                        self.request_redraw();
                    }
                }
            }
            exec::Event::Done(outcome) => {
                let Some(job) = self.jobs.remove(&id) else {
                    return;
                };
                self.state.running.retain(|(j, _)| *j != id);
                self.request_redraw();
                if !outcome.success() {
                    let msg = format!("error: {}", outcome.error());
                    log::warn!("{}: {msg}", job.name);
                    self.state.notify(msg, MsgKind::Error, now);
                    return;
                }
                log::debug!("{}: done", job.name);
                match (job.output, outcome.last_line) {
                    (Output::Status, Some(line)) => self.state.notify(line, MsgKind::Info, now),
                    // Actions speak for themselves.
                    (Output::Actions, _) if job.lines > 0 => {}
                    _ => self.state.notify("done", MsgKind::Info, now),
                }
                let ctx = Ctx {
                    anchor: None,
                    targets: Some(job.targets),
                };
                let _ = self.run(job.then, &ctx, el);
            }
        }
    }

    /// Re-check `targets` (default: the whole queue, plus the current file) on disk, off-thread.
    fn refresh(&mut self, targets: Option<Arc<[PathBuf]>>) {
        let mut files: Vec<PathBuf> = match targets {
            Some(t) => t.to_vec(),
            None => self.state.queue.all_files().to_vec(),
        };
        if let Some(cur) = self.state.queue.current()
            && !files.iter().any(|f| f == cur)
        {
            files.push(cur.to_path_buf());
        }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let found = files
                .into_iter()
                .map(|p| {
                    let m = std::fs::metadata(&p)
                        .ok()
                        .map(|m| m.modified().unwrap_or(SystemTime::UNIX_EPOCH));
                    (p, m)
                })
                .collect();
            let _ = proxy.send_event(UserEvent::Refreshed(found));
        });
    }

    /// Drop files that vanished and reload ones that changed since they were loaded.
    fn on_refreshed(&mut self, found: Vec<(PathBuf, Option<SystemTime>)>) {
        let cur = self.state.queue.current().map(Path::to_path_buf);
        let showing = |s: &AppState, p: &Path| {
            s.mode == Mode::Image && s.shown.as_ref().is_some_and(|s| s.path == p)
        };
        let mut navigated = false;
        let mut stale = Vec::new();
        let mut gone = 0;
        for (path, mtime) in found {
            match mtime {
                None => {
                    if self.state.queue.remove(&path) {
                        gone += 1;
                        navigated |= cur.as_ref() == Some(&path);
                    }
                    self.cache.remove(&path);
                    self.stamps.remove(&path);
                    self.state.thumbs.remove(&path);
                }
                Some(t) if self.stamps.get(&path).is_some_and(|s| *s != t) => stale.push(path),
                Some(_) => {}
            }
        }
        let mut reload_current = false;
        for path in &stale {
            self.cache.remove(path);
            self.stamps.remove(path);
            self.state.thumbs.remove(path);
            self.uploads.retain(|(p, _)| p != path);
            if cur.as_ref() == Some(path) && showing(&self.state, path) {
                // Keep it on screen until the new version replaces it.
                reload_current = true;
            } else if let Some(g) = &mut self.gfx {
                g.renderer.retain(|p| p != path);
            }
        }
        if gone + stale.len() > 0 {
            log::debug!("refresh: {gone} gone, {} changed", stale.len());
        }
        if reload_current && let Some(cur) = &cur {
            self.decode(cur, Priority::Current);
        }
        if gone + stale.len() > 0 {
            self.sync(navigated);
            self.request_redraw();
        }
    }

    fn on_ipc(&mut self, inc: ipc::Incoming, el: &ActiveEventLoop) {
        let reply = match inc.request {
            Request::Actions(actions) => match self.run(actions, &Ctx::default(), el) {
                Ok(()) => ipc::ok(Value::Null),
                Err(e) => ipc::error(&e),
            },
            Request::Query(q) => ipc::ok(self.query(q)),
            Request::Subscribe(topics) => match inc.events {
                Some(tx) => {
                    if self.published.is_none() {
                        self.published = Some(self.snapshot());
                    }
                    self.subscribers.push(Subscriber { topics, tx });
                    ipc::ok(Value::Null)
                }
                None => ipc::error("no event channel"),
            },
        };
        let _ = inc.reply.send(reply);
    }

    fn query(&self, q: Query) -> Value {
        let s = &self.state;
        let paths = |ps: &mut dyn Iterator<Item = &Path>| -> Value {
            ps.map(ipc::path_json).collect::<Vec<_>>().into()
        };
        match q {
            Query::Current => s.queue.current().map_or(Value::Null, ipc::path_json),
            Query::Marked => paths(&mut s.queue.marked().into_iter()),
            Query::Queue => paths(&mut s.queue.files().iter().map(PathBuf::as_path)),
            Query::State => {
                let shown = s
                    .shown
                    .as_ref()
                    .filter(|sh| Some(sh.path.as_path()) == s.queue.current());
                let fullscreen = self
                    .gfx
                    .as_ref()
                    .is_some_and(|g| g.window.fullscreen().is_some());
                json!({
                    "mode": s.mode.name(),
                    "current": s.queue.current().map(ipc::path_json),
                    "index": (!s.queue.is_empty()).then(|| s.queue.index() + 1),
                    "count": s.queue.len(),
                    "marked": s.queue.marked_count(),
                    "width": shown.map(|sh| sh.size[0]),
                    "height": shown.map(|sh| sh.size[1]),
                    "format": shown.map(|sh| sh.format),
                    "zoom": shown.map(|_| (s.view.scale() * 1000.0).round() / 10.0),
                    "fit": s.settings.fit,
                    "bar": s.bar,
                    "fullscreen": fullscreen,
                    "skipped": s.skipped,
                    "filter": s.queue.filter(),
                    "slideshow": s.slideshow.is_some(),
                    "running": s.running.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>(),
                    "pid": std::process::id(),
                })
            }
        }
    }

    fn snapshot(&self) -> Published {
        let q = &self.state.queue;
        Published {
            image: (q.current().map(Path::to_path_buf), q.index(), q.len()),
            marks: q.marks_version(),
            mode: self.state.mode,
        }
    }

    /// Tell subscribers what changed since they last heard.
    fn publish(&mut self) {
        if self.subscribers.is_empty() {
            self.published = None;
            return;
        }
        let now = self.snapshot();
        let Some(before) = self.published.replace(now.clone()) else {
            return;
        };
        let q = &self.state.queue;
        let mut events = Vec::new();
        if before.image != now.image {
            let data = json!({
                "path": q.current().map(ipc::path_json),
                "index": (!q.is_empty()).then(|| q.index() + 1),
                "count": q.len(),
            });
            events.push((Topic::Image, data));
        }
        if before.marks != now.marks {
            let files: Vec<Value> = q.marked().into_iter().map(ipc::path_json).collect();
            events.push((
                Topic::Marks,
                json!({ "count": files.len(), "files": files }),
            ));
        }
        if before.mode != now.mode {
            events.push((Topic::Mode, json!(now.mode.name())));
        }
        for (topic, data) in events {
            let line = ipc::event(topic, data);
            self.subscribers
                .retain(|s| !s.topics.contains(&topic) || s.tx.send(line.clone()).is_ok());
        }
    }

    /// Marked files for `-o`, unless the user quit with `quit-no-output`.
    pub fn marked_output(&self) -> Option<Vec<PathBuf>> {
        (!self.no_output).then(|| {
            self.state
                .queue
                .marked()
                .into_iter()
                .map(Path::to_path_buf)
                .collect()
        })
    }

    fn effects(&mut self, fx: Effects, ctx: &Ctx, event_loop: &ActiveEventLoop) {
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
        if fx.palette {
            let items = self.palette_items();
            self.keymap.cancel();
            self.state.open_palette(items);
        }
        if fx.help {
            let rows = self.help_rows();
            self.state.help = Some(Help { rows, scroll: 0 });
        }
        if fx.info {
            self.request_info();
        }
        if fx.open
            && let Some(p) = self.state.open.take()
        {
            self.open(&p);
        }
        if fx.reload_config {
            self.reload_requested = true;
            self.watch.reload();
        }
        if fx.refresh {
            self.refresh(ctx.targets.clone());
        }
        if let Some(key) = fx.sort {
            let mut files = self.state.queue.files().to_vec();
            let proxy = self.proxy.clone();
            std::thread::spawn(move || {
                scan::sort_paths(&mut files, key, false);
                let _ = proxy.send_event(UserEvent::Sorted(files));
            });
        }
        if fx.mode {
            // Opening an image from the grid loads it like navigating to it.
            self.sync(self.state.mode == Mode::Image || fx.navigated);
        } else if fx.navigated {
            self.sync(true);
        } else if fx.reordered {
            self.sync(false);
        }
        // Scrolling, a new size or the bar can bring new thumbnails into view.
        self.sync_thumbs();
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
            if !self.keymap.is_pending() {
                self.which_key_since = None;
                self.state.which_key = None;
            } else if self.state.which_key.is_some() {
                // Already showing: follow the sequence at once.
                self.state.which_key = Some(self.which_key_rows());
            } else {
                self.which_key_since = Some(now);
            }
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
        if self.confirm.is_some() {
            // The prompt takes the next key: `y` runs the command, anything else cancels.
            if !event.repeat
                && let Some(key) = input::key(&event.logical_key, self.modifiers)
            {
                self.answer(key == Key::plain('y') || key == Key::plain('Y'));
            }
            return;
        }
        if self.state.input.is_some() {
            self.on_input_key(&event, event_loop);
            return;
        }
        if self.state.help.is_some()
            && !event.repeat
            && let Some(key) = input::key(&event.logical_key, self.modifiers)
            && !self.keymap.is_pending()
            && self.help_key(key)
        {
            return;
        }
        if event.repeat {
            // Repeats bypass the trie: only a completed, repeatable binding repeats.
            if let Some((k, actions)) = &self.repeat
                && *k == event.physical_key
            {
                let actions = actions.clone();
                let _ = self.run(actions, &Ctx::default(), event_loop);
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
            if let [Action::Pan(dir)] = actions[..]
                && self.state.mode == Mode::Image
            {
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
            let _ = self.run(actions, &Ctx::default(), event_loop);
        }
    }

    /// A key while the palette or search line is open: it edits the line.
    fn on_input_key(&mut self, event: &winit::event::KeyEvent, el: &ActiveEventLoop) {
        let now = Instant::now();
        let Some(key) = input::key(&event.logical_key, self.modifiers) else {
            return;
        };
        let ctrl = |c: char| {
            Key::new(
                input::keys::Mods {
                    ctrl: true,
                    ..Default::default()
                },
                Code::Char(c),
            )
        };
        let named = |n: Named| Key::new(Default::default(), Code::Named(n));
        let shift_tab = Key::new(
            input::keys::Mods {
                shift: true,
                ..Default::default()
            },
            Code::Named(Named::Tab),
        );
        let palette = self
            .state
            .input
            .as_ref()
            .is_some_and(|i| i.kind == InputKind::Palette);
        let empty = self.state.input.as_ref().is_some_and(|i| i.text.is_empty());
        let edit = if key == named(Named::Esc) || key == ctrl('c') || key == ctrl('[') {
            let fx = self.state.input_cancel(now);
            self.effects(fx, &Ctx::default(), el);
            return;
        } else if key == named(Named::Enter) {
            if event.repeat {
                return;
            }
            let (fx, run) = self.state.input_submit(now);
            self.effects(fx, &Ctx::default(), el);
            if let Some(text) = run {
                match text.parse::<Action>() {
                    Ok(a) => {
                        let _ = self.run(vec![a], &Ctx::default(), el);
                    }
                    Err(e) => self
                        .state
                        .notify(format!("error: {e}"), MsgKind::Error, now),
                }
            }
            self.request_redraw();
            return;
        } else if key == named(Named::Backspace) && empty {
            let fx = self.state.input_cancel(now);
            self.effects(fx, &Ctx::default(), el);
            return;
        } else if key == named(Named::Backspace) || key == ctrl('h') {
            Edit::Backspace
        } else if key == ctrl('w') {
            Edit::DeleteWord
        } else if key == ctrl('u') {
            Edit::Clear
        } else if key == named(Named::Tab) && palette {
            Edit::Complete
        } else if key == named(Named::Up) || key == ctrl('p') || key == shift_tab {
            Edit::Up
        } else if key == named(Named::Down) || key == ctrl('n') || key == named(Named::Tab) {
            Edit::Down
        } else if key == named(Named::Left) {
            Edit::Left
        } else if key == named(Named::Right) {
            Edit::Right
        } else if key == named(Named::Space) && key.mods.is_empty() {
            Edit::Insert(" ".into())
        } else {
            let m = self.modifiers;
            match &event.text {
                Some(t) if !m.control_key() && !m.alt_key() && !m.super_key() => {
                    Edit::Insert(t.to_string())
                }
                _ => return,
            }
        };
        let fx = self.state.input_edit(edit, now);
        self.effects(fx, &Ctx::default(), el);
    }

    /// Scrolling and closing keys while help is open. Returns whether the key was used.
    fn help_key(&mut self, key: Key) -> bool {
        let page = self.state.help_page();
        let Some(help) = &mut self.state.help else {
            return false;
        };
        let ctrl = |c: char| {
            Key::new(
                input::keys::Mods {
                    ctrl: true,
                    ..Default::default()
                },
                Code::Char(c),
            )
        };
        let named = |n: Named| Key::new(Default::default(), Code::Named(n));
        let half = (page / 2).max(1) as isize;
        let delta = match key {
            k if k == Key::plain('j') || k == named(Named::Down) => 1,
            k if k == Key::plain('k') || k == named(Named::Up) => -1,
            k if k == ctrl('d') || k == ctrl('f') || k == named(Named::PageDown) => half,
            k if k == ctrl('u') || k == ctrl('b') || k == named(Named::PageUp) => -half,
            k if k == Key::plain('g') || k == named(Named::Home) => isize::MIN / 2,
            k if k == Key::plain('G') || k == named(Named::End) => isize::MAX / 2,
            k if k == Key::plain('q') || k == Key::plain('?') || k == named(Named::Esc) => {
                self.state.help = None;
                self.request_redraw();
                return true;
            }
            _ => return false,
        };
        help.scroll_by(delta, page);
        self.request_redraw();
        true
    }

    /// A mouse button or wheel notch: bound actions run at the cursor, bypassing sequences.
    fn on_mouse_key(&mut self, key: Key, event_loop: &ActiveEventLoop) {
        self.keymap.cancel();
        self.update_pending(Instant::now());
        if let Some(actions) = self.keymap.lookup(self.state.mode, &[key]) {
            let actions = actions.to_vec();
            let ctx = Ctx {
                anchor: Some(self.cursor),
                ..Ctx::default()
            };
            let _ = self.run(actions, &ctx, event_loop);
        }
    }

    /// `pixels` is set for touchpads, whose deltas are pixels divided by [`PIXELS_PER_STEP`].
    fn on_wheel(&mut self, delta: [f32; 2], pixels: bool, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let [dx, dy] = delta;
        if self.state.help.is_some() {
            let page = self.state.help_page();
            self.scroll[1] += dy;
            let rows = self.scroll[1].trunc();
            self.scroll[1] -= rows;
            if let Some(h) = &mut self.state.help {
                h.scroll_by(-(rows as isize) * 3, page);
            }
            self.request_redraw();
            return;
        }
        if dy != 0.0 {
            let dir = if dy > 0.0 { Scroll::Up } else { Scroll::Down };
            let key = input::scroll(dir, self.modifiers);
            // Wheel zoom and grid scrolling stay continuous for touchpads rather than stepping
            // per notch.
            let in_grid = self.state.in_grid();
            match self.keymap.lookup(self.state.mode, &[key]) {
                Some([Action::Scroll(d @ (Dir::Up | Dir::Down), n)]) if in_grid => {
                    let sign = if *d == Dir::Up { -1.0 } else { 1.0 };
                    let amount = dy.abs() * *n as f32 * sign;
                    let moved = if pixels {
                        self.state.grid_drag(-amount * PIXELS_PER_STEP, now)
                    } else {
                        self.state.grid_scroll(amount, now)
                    };
                    self.after_grid_scroll(moved);
                    return;
                }
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

    /// The grid scrolled; `moved` says whether it pulled the cursor along.
    fn after_grid_scroll(&mut self, moved: bool) {
        if moved {
            self.sync(true);
        } else {
            self.sync_thumbs();
        }
        self.request_redraw();
    }

    /// A click (press and release in place) in the grid selects; a second one opens.
    fn grid_click(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let Some(i) = self.state.grid_hit(self.cursor, now) else {
            return;
        };
        let double = self
            .last_click
            .is_some_and(|(t, j)| j == i && now < t + DOUBLE_CLICK);
        self.last_click = (!double).then_some((now, i));
        let mut actions = vec![Action::Goto(i + 1)];
        if double {
            actions.push(Action::Mode(ModeArg::Image));
        }
        let _ = self.run(actions, &Ctx::default(), event_loop);
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        let ready = self.state.shown.is_some()
            || self.state.queue.is_empty()
            || (self.state.in_grid() && self.grid_settled())
            || now >= self.started + SHOW_TIMEOUT;
        if !self.shown_window && !ready {
            return; // about_to_wait wakes us at the timeout
        }
        self.state.advance(now);
        self.state.tick(now);
        let settled = if self.state.in_grid() {
            self.thumbs.arrivals.is_empty() && self.grid_settled()
        } else {
            self.state.loading_since.is_none()
        };
        // An animation moved on (or a page turned): put that frame in the texture.
        let frame = self.state.frame();
        if let Some(path) = self
            .state
            .shown
            .as_ref()
            .filter(|_| self.state.mode == Mode::Image)
            .map(|s| s.path.clone())
            && let Some(g) = &self.gfx
            && g.renderer.frame(&path).is_some_and(|f| f != frame)
            && let Some(img) = self.cache.get(&path)
            && let Some(g) = &mut self.gfx
        {
            g.renderer.show_frame(&path, &img, frame);
        }
        let Some(g) = &mut self.gfx else { return };
        if !self.shown_window {
            g.window.set_visible(true);
        }
        let theme = self.state.theme_at(now);
        let scene = ui::scene(&self.state, &theme, now, &mut |t| g.renderer.measure(t));
        let result = g.renderer.render(&scene);
        if let Some(path) = std::env::var_os("REMAGE_SNAPSHOT")
            && !self.state.is_animating(now)
            && settled
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

    /// Upload thumbnails, then preloaded images a slice at a time. Returns whether work remains.
    fn pump_uploads(&mut self) -> bool {
        let Some(g) = &mut self.gfx else { return false };
        let now = Instant::now();
        let mut bytes = 0;
        while bytes < UPLOAD_BUDGET
            && let Some((path, t)) = self.thumbs.arrivals.pop_front()
        {
            bytes += t.pixels.len();
            let wanted = &self.thumbs.wanted;
            for gone in g.renderer.insert_thumb(&path, &t, |p| wanted.contains(p)) {
                self.state.thumbs.remove(&gone);
            }
            if g.renderer.has_thumb(&path) {
                self.state.thumb_ready(path, t.size, now);
            }
            g.window.request_redraw();
        }
        if !self.thumbs.arrivals.is_empty() {
            return true;
        }
        if let Some((path, img)) = self.uploads.front()
            && g.renderer.upload(path, img, UPLOAD_BUDGET)
        {
            self.uploads.pop_front();
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

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
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
        let size = window.inner_size();
        self.state.set_window(
            size.width as f32,
            size.height as f32,
            window.scale_factor() as f32,
        );
        // The grid's size is known now: thumbnails load while the GPU starts up.
        self.sync_thumbs();
        let renderer = match Renderer::new(window.clone(), event_loop.owned_display_handle()) {
            Ok(mut r) => {
                r.set_font_families(font_families(&self.config));
                r.reset_thumbs(self.thumbs.cell);
                r
            }
            Err(e) => {
                self.error = Some(e.context("initialise GPU"));
                event_loop.exit();
                return;
            }
        };
        log::debug!("GPU ready after {:.1?}", self.started.elapsed());
        self.gfx = Some(Gfx { renderer, window });
        self.sync(false);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // The window goes at once; whatever else is still winding down does so unseen.
        self.gfx = None;
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Decoded {
                path,
                mtime,
                result,
            } => self.on_decoded(path, mtime, result),
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
            UserEvent::Thumb {
                path,
                px,
                mtime,
                result,
            } => self.on_thumb(path, px, mtime, result),
            UserEvent::Refined { path, result } => self.on_refined(path, result),
            UserEvent::Command { id, event } => self.on_command(id, event, event_loop),
            UserEvent::Refreshed(found) => self.on_refreshed(found),
            UserEvent::Ipc(inc) => self.on_ipc(inc, event_loop),
            UserEvent::Info(info) => {
                if self.info_loading.as_ref() == Some(&info.path) {
                    self.info_loading = None;
                }
                if self.state.queue.current() == Some(info.path.as_path()) {
                    self.state.info = Some(info);
                    self.request_redraw();
                }
            }
            UserEvent::InfoLine { path, line } => {
                self.state.info_line = Some((path, line));
                self.request_redraw();
            }
            UserEvent::Dir(changes) => self.on_dir(changes),
            UserEvent::Opened { arg, scan } => self.on_opened(arg, scan),
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
                self.sync_thumbs();
                self.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let [w, h] = self.state.window();
                self.state.set_window(w, h, scale_factor as f32);
                self.sync_thumbs();
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
                let delta = [p[0] - self.cursor[0], p[1] - self.cursor[1]];
                self.cursor = p;
                if self.dragging && self.state.in_grid() {
                    let moved = self.state.grid_drag(delta[1], Instant::now());
                    self.after_grid_scroll(moved);
                } else if self.dragging && self.state.shown.is_some() {
                    self.state.view.pan_pixels(delta);
                    self.request_redraw();
                }
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
                    if self.state.in_grid() {
                        self.grid_click(event_loop);
                    }
                } else if !pressed {
                    return;
                }
                if let Some(key) = input::button(button, self.modifiers) {
                    self.on_mouse_key(key, event_loop);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (d, pixels) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => ([x, y], false),
                    MouseScrollDelta::PixelDelta(p) => (
                        [p.x as f32 / PIXELS_PER_STEP, p.y as f32 / PIXELS_PER_STEP],
                        true,
                    ),
                };
                self.on_wheel(d, pixels, event_loop);
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
                let _ = self.run(run.counted(), &Ctx::default(), event_loop);
            }
        }
        if let Some(t) = self.grid_rest
            && now >= t + GRID_REST
        {
            self.grid_rest = None;
            if self.state.in_grid()
                && let Some(cur) = self.state.queue.current().map(Path::to_path_buf)
                && !self.cache_has(&cur)
            {
                self.decode(&cur, Priority::Preload);
            }
        }
        if let Some(t) = self.which_key_since
            && now >= t + self.state.settings.which_key_delay
        {
            self.which_key_since = None;
            if self.keymap.is_pending() && self.state.input.is_none() {
                self.state.which_key = Some(self.which_key_rows());
                self.request_redraw();
            }
        }
        if self.state.slideshow_due(now) {
            let fx = self.state.apply(Action::Next(1), now);
            self.state.slideshow_advanced(fx.navigated);
            if !fx.navigated {
                self.state.notify("slideshow done", MsgKind::Info, now);
            }
            self.effects(fx, &Ctx::default(), event_loop);
        }
        self.publish();
        let (redraw, mut next) = self.state.tick(now);
        if let Some(t) = self.which_key_since {
            let at = t + self.state.settings.which_key_delay;
            next = Some(next.map_or(at, |n| n.min(at)));
        }
        if let Some(t) = self.grid_rest {
            let at = t + GRID_REST;
            next = Some(next.map_or(at, |n| n.min(at)));
        }
        if let Some(t) = self.ambiguous_since {
            let at = t + SEQUENCE_TIMEOUT;
            next = Some(next.map_or(at, |n| n.min(at)));
        }
        if let Some(at) = self.check_refine(now) {
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
