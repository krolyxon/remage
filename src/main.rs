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
mod render;
mod scan;
mod state;
mod thumbs;
mod ui;
mod worker;

use std::path::Path;
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

    let scan = scan::scan(
        &cli.paths,
        &scan::ScanOptions {
            recursive: cli.recursive || cfg.general.recursive,
            siblings: !cli.no_siblings && cfg.general.open_siblings,
            sort: cli.sort.unwrap_or(cfg.general.sort),
            reverse: cli.reverse || cfg.general.reverse,
        },
    );
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
    };
    let mut app = App::new(opts, state, proxy, loaded, watch);
    if let Some(e) = config_error {
        app.config_failed(&e);
    }
    event_loop.run_app(&mut app)?;
    match app.error.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
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
