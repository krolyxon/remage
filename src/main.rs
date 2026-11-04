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

use std::time::Instant;

use clap::Parser;
use winit::event_loop::EventLoop;

use app::{App, Options, UserEvent};
use state::queue::Queue;
use state::{AppState, MsgKind, Settings};

fn main() -> anyhow::Result<()> {
    let started = Instant::now();
    env_logger::init();
    let cli = cli::Cli::parse();
    log::info!("remage {}", env!("CARGO_PKG_VERSION"));

    let scan = scan::scan(
        &cli.paths,
        &scan::ScanOptions {
            recursive: cli.recursive,
            siblings: !cli.no_siblings,
            sort: cli.sort,
            reverse: cli.reverse,
        },
    );
    log::debug!(
        "scanned {} files ({} to sniff) in {:.1?}",
        scan.files.len(),
        scan.unknown.len(),
        started.elapsed()
    );

    let settings = Settings {
        fit: cli.fit.unwrap_or(Settings::default().fit),
        zoom: cli.zoom.map(|z| z / 100.0),
        ..Settings::default()
    };
    let start_index = cli.start.unwrap_or(1).saturating_sub(1);
    let queue = Queue::new(scan.files, scan.start.as_deref(), start_index);
    let mut state = AppState::new(queue, settings, !cli.no_bar, Instant::now());
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

    let opts = Options {
        fullscreen: cli.fullscreen,
        geometry: cli.geometry,
        class: cli.class,
        cache_mb: 512,
    };
    let mut app = App::new(opts, state, proxy);
    event_loop.run_app(&mut app)?;
    match app.error.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
