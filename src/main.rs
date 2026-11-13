//! remage: a keyboard-centric image viewer.

mod action;
mod anim;
mod app;
mod cache;
mod cli;
mod config;
mod decode;
mod exec;
mod input;
mod ipc;
mod man;
mod render;
mod scan;
mod state;
mod thumbs;
mod ui;
mod watch;
mod worker;

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Parser;
use winit::event_loop::EventLoop;

use app::{App, Options, Overrides, UserEvent};
use state::queue::Queue;
use state::{AppState, MsgKind, Settings};

fn main() -> anyhow::Result<()> {
    let started = Instant::now();
    env_logger::init();
    let cli = cli::Cli::parse();
    match cli.command {
        Some(cli::Sub::Msg {
            pid,
            query,
            subscribe,
            action,
        }) => {
            std::process::exit(ipc::client(ipc::ClientArgs {
                pid,
                query,
                subscribe,
                action,
            }));
        }
        Some(cli::Sub::Completions { shell }) => {
            let mut cmd = <cli::Cli as clap::CommandFactory>::command();
            clap_complete::generate(shell, &mut cmd, "remage", &mut std::io::stdout());
            return Ok(());
        }
        Some(cli::Sub::Man) => {
            man::render(&mut std::io::stdout().lock())?;
            return Ok(());
        }
        None => {}
    }
    if cli.dump_config {
        print!("{}", config::dump());
        return Ok(());
    }
    if cli.check_config {
        check_config(cli.config.as_deref());
    }
    log::info!("remage {}", env!("CARGO_PKG_VERSION"));

    let (loaded, config_error) = match config::load(cli.config.as_deref()) {
        Ok(l) => (l, None),
        Err(e) => {
            log::error!("{e}");
            let mut d = config::defaults();
            d.watch = e.watch;
            (d, Some(e.message))
        }
    };
    for w in &loaded.warnings {
        log::warn!("{w}");
    }
    log::debug!("config loaded after {:.1?}", started.elapsed());
    let cfg = &loaded.config;
    decode::set_decoders(&cfg.decoders);

    let mut paths = cli.paths.clone();
    if cli.stdin {
        paths.extend(read_stdin_paths(cli.null)?);
    }
    let scan = if cli.stdin && paths.is_empty() {
        // An empty list on stdin opens nothing, not the current directory.
        scan::Scan::default()
    } else {
        scan::scan(
            &paths,
            &scan::ScanOptions {
                recursive: cli.recursive || cfg.general.recursive,
                // A list of files is just those files.
                siblings: !cli.stdin && !cli.no_siblings && cfg.general.open_siblings,
                sort: cli.sort.unwrap_or(cfg.general.sort),
                reverse: cli.reverse || cfg.general.reverse,
            },
        )
    };
    log::debug!(
        "scanned {} files ({} to sniff) in {:.1?}",
        scan.files.len(),
        scan.unknown.len(),
        started.elapsed()
    );

    let overrides = Overrides {
        fit: cli.fit,
        zoom: cli.zoom.map(|z| z / 100.0),
        bar: cli.no_bar.then_some(false),
        slideshow_delay: cli
            .slideshow
            .flatten()
            .filter(|s| s.is_finite() && *s > 0.0)
            .map(std::time::Duration::from_secs_f32),
    };
    let mut settings = Settings::from_config(cfg);
    overrides.apply(&mut settings);
    let start_index = cli.start.unwrap_or(1).saturating_sub(1);
    let queue = Queue::new(scan.files, scan.start.as_deref(), start_index);
    let bar = overrides.bar.unwrap_or(cfg.ui.statusbar);
    let mut state = AppState::new(queue, settings, bar, Instant::now());
    if cli.grid || cfg.general.start_mode == config::StartMode::Grid {
        state.set_mode(state::Mode::Grid, Instant::now());
    }
    state.theme = loaded.theme.clone();
    state.nearest = cfg.image.filter.nearest();
    state.alpha_bg = cfg.image.alpha_background;
    if cli.slideshow.is_some() {
        state.apply(
            action::Action::Slideshow(action::Switch::On),
            Instant::now(),
        );
        state.message = None;
    }
    if let Some(err) = scan.errors.first() {
        state.notify(format!("error: {err}"), MsgKind::Error, Instant::now());
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    if !scan.unknown.is_empty() {
        let proxy = proxy.clone();
        let unknown = scan.unknown;
        std::thread::spawn(move || {
            let found: Vec<_> = unknown.into_iter().filter(|p| decode::sniff(p)).collect();
            if !found.is_empty() {
                let _ = proxy.send_event(UserEvent::Found(found));
            }
        });
    }
    let server = {
        let proxy = proxy.clone();
        let proxy = std::sync::Mutex::new(proxy);
        match ipc::serve(move |inc| {
            let proxy = proxy.lock().unwrap_or_else(|e| e.into_inner());
            proxy.send_event(UserEvent::Ipc(inc)).is_ok()
        }) {
            Ok(s) => {
                log::debug!("IPC socket {}", s.path().display());
                Some(s)
            }
            Err(e) => {
                log::warn!("IPC socket: {e}");
                None
            }
        }
    };
    let watch = {
        let proxy = proxy.clone();
        config::watch::spawn(cli.config.clone(), loaded.watch.clone(), move |r| {
            let _ = proxy.send_event(UserEvent::Config(Box::new(r)));
        })
    };

    let opts = Options {
        fullscreen: cli.fullscreen || cfg.window.fullscreen,
        geometry: cli.geometry,
        class: cli.class.unwrap_or_else(|| cfg.window.class.clone()),
        overrides,
        private: cli.private,
        socket: server.as_ref().map(|s| s.path().to_path_buf()),
        dirs: scan.dirs,
    };
    let mut app = App::new(opts, state, proxy, loaded, watch);
    if let Some(e) = config_error {
        app.config_failed(&e);
    }
    event_loop.run_app(&mut app)?;
    // Decode workers are abandoned mid-job from here on.
    thumbs::finish_writes();
    if cli.print_marked
        && let Some(marked) = app.marked_output()
    {
        let sep = if cli.null { b'\0' } else { b'\n' };
        let mut out = std::io::stdout().lock();
        for p in marked {
            out.write_all(p.as_os_str().as_bytes())?;
            out.write_all(&[sep])?;
        }
        out.flush()?;
    }
    // Lets in-flight IPC replies (to a `quit`, say) go out, then removes the socket.
    drop(server);
    match app.error.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// `-i`: paths from stdin, one per line (NUL-separated with `-0`).
fn read_stdin_paths(null: bool) -> anyhow::Result<Vec<PathBuf>> {
    let mut buf = Vec::new();
    std::io::stdin().lock().read_to_end(&mut buf)?;
    let sep = if null { 0 } else { b'\n' };
    Ok(buf
        .split(|b| *b == sep)
        .filter(|l| !l.is_empty())
        .map(|l| PathBuf::from(OsString::from_vec(l.to_vec())))
        .collect())
}

/// `--check-config`: report and exit, non-zero on errors.
fn check_config(path: Option<&Path>) -> ! {
    match config::load(path) {
        Ok(l) => {
            for w in &l.warnings {
                eprintln!("warning: {w}");
            }
            match path.map(Path::to_path_buf).or_else(config::default_path) {
                Some(p) if p.is_file() => println!("{}: ok", p.display()),
                Some(p) => println!("{} not found: using the defaults", p.display()),
                None => println!("no config directory: using the defaults"),
            }
            std::process::exit(0)
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1)
        }
    }
}
