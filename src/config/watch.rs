//! Live reload: a background thread that reloads the config when a watched file changes, on
//! SIGUSR1, or on request, and hands the result to the event loop.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use super::{LoadError, Loaded, load};

/// Quiet time before reloading, so editors' multi-step saves reload once.
const DEBOUNCE: Duration = Duration::from_millis(100);

enum Msg {
    Fs(notify::Event),
    Reload,
}

/// Handle to the reload thread. Dropping it stops nothing; the thread lives with the process.
#[derive(Clone)]
pub struct ConfigWatch {
    tx: Sender<Msg>,
}

impl ConfigWatch {
    /// Reload now (the `reload-config` action).
    pub fn reload(&self) {
        let _ = self.tx.send(Msg::Reload);
    }
}

/// Start watching `watch` (from the initial load); `deliver` receives every reload result.
pub fn spawn(
    path: Option<PathBuf>,
    watch: Vec<PathBuf>,
    deliver: impl Fn(Result<Loaded, LoadError>) + Send + 'static,
) -> ConfigWatch {
    let (tx, rx) = crossbeam_channel::unbounded();
    let fs_tx = tx.clone();
    let sig_tx = tx.clone();
    std::thread::Builder::new()
        .name("remage-sigusr1".into())
        .spawn(
            move || match signal_hook::iterator::Signals::new([signal_hook::consts::SIGUSR1]) {
                Ok(mut signals) => {
                    for _ in signals.forever() {
                        if sig_tx.send(Msg::Reload).is_err() {
                            break;
                        }
                    }
                }
                Err(e) => log::warn!("SIGUSR1 handler: {e}"),
            },
        )
        .ok();
    std::thread::Builder::new()
        .name("remage-config".into())
        .spawn(move || {
            let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    let _ = fs_tx.send(Msg::Fs(ev));
                }
            });
            let mut watcher = match watcher {
                Ok(w) => Some(w),
                Err(e) => {
                    log::warn!("config watcher: {e}");
                    None
                }
            };
            run(path.as_deref(), watch, &rx, watcher.as_mut(), deliver);
        })
        .ok();
    ConfigWatch { tx }
}

fn run(
    path: Option<&Path>,
    mut files: Vec<PathBuf>,
    rx: &Receiver<Msg>,
    mut watcher: Option<&mut RecommendedWatcher>,
    deliver: impl Fn(Result<Loaded, LoadError>),
) {
    let mut dirs = HashSet::new();
    if let Some(w) = watcher.as_deref_mut() {
        watch_dirs(w, &mut dirs, &files);
    }
    while let Ok(msg) = rx.recv() {
        if let Msg::Fs(ev) = &msg
            && !relevant(ev, &files)
        {
            continue;
        }
        loop {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let result = load(path);
        files = match &result {
            Ok(l) => l.watch.clone(),
            Err(e) => e.watch.clone(),
        };
        if let Some(w) = watcher.as_deref_mut() {
            watch_dirs(w, &mut dirs, &files);
        }
        deliver(result);
    }
}

/// Watch the parent directory of every file: editors replace files by renaming, which a
/// watch on the file itself would lose.
fn watch_dirs(w: &mut RecommendedWatcher, dirs: &mut HashSet<PathBuf>, files: &[PathBuf]) {
    let wanted: HashSet<PathBuf> = files
        .iter()
        .filter_map(|f| f.parent())
        .filter(|d| d.is_dir())
        .map(Path::to_path_buf)
        .collect();
    for d in dirs.difference(&wanted) {
        let _ = w.unwatch(d);
    }
    for d in wanted.difference(dirs) {
        if let Err(e) = w.watch(d, RecursiveMode::NonRecursive) {
            log::warn!("watch {}: {e}", d.display());
        }
    }
    *dirs = wanted;
}

fn relevant(ev: &notify::Event, files: &[PathBuf]) -> bool {
    let changes = match ev.kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    };
    changes && ev.paths.iter().any(|p| files.iter().any(|f| f == p))
}
