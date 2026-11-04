//! `general.watch_dir`: watch the queue's directories and report files that appear, change or
//! vanish, so the queue follows the disk live.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::decode;

/// Quiet time before reporting, so a file being written arrives once, complete.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// What changed in the watched directories.
#[derive(Debug, Default)]
pub struct Changes {
    /// Image files that exist now, with their mtime: new ones join the queue, known ones
    /// reload if their mtime moved.
    pub present: Vec<(PathBuf, SystemTime)>,
    /// Paths that no longer exist.
    pub gone: Vec<PathBuf>,
}

enum Msg {
    Fs(notify::Event),
    Dirs(Vec<(PathBuf, bool)>),
}

/// Handle to the watch thread.
pub struct DirWatch {
    tx: Sender<Msg>,
}

impl DirWatch {
    /// Watch these directories instead (after `open`). `true` = recursively.
    pub fn set(&self, dirs: Vec<(PathBuf, bool)>) {
        let _ = self.tx.send(Msg::Dirs(dirs));
    }
}

pub fn spawn(dirs: Vec<(PathBuf, bool)>, deliver: impl Fn(Changes) + Send + 'static) -> DirWatch {
    let (tx, rx) = crossbeam_channel::unbounded();
    let fs_tx = tx.clone();
    let _ = tx.send(Msg::Dirs(dirs));
    std::thread::Builder::new()
        .name("remage-watch".into())
        .spawn(move || {
            let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    let _ = fs_tx.send(Msg::Fs(ev));
                }
            });
            match watcher {
                Ok(w) => run(w, &rx, deliver),
                Err(e) => log::warn!("directory watcher: {e}"),
            }
        })
        .ok();
    DirWatch { tx }
}

fn run(mut w: RecommendedWatcher, rx: &Receiver<Msg>, deliver: impl Fn(Changes)) {
    let mut watched: Vec<(PathBuf, bool)> = Vec::new();
    let mut touched: HashSet<PathBuf> = HashSet::new();
    loop {
        let msg = if touched.is_empty() {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            rx.recv_timeout(DEBOUNCE)
        };
        match msg {
            Ok(Msg::Dirs(dirs)) => {
                for (d, _) in &watched {
                    let _ = w.unwatch(d);
                }
                for (d, rec) in &dirs {
                    let mode = if *rec {
                        RecursiveMode::Recursive
                    } else {
                        RecursiveMode::NonRecursive
                    };
                    if let Err(e) = w.watch(d, mode) {
                        log::warn!("watch {}: {e}", d.display());
                    }
                }
                log::debug!("watching {} directories", dirs.len());
                watched = dirs;
                touched.clear();
            }
            Ok(Msg::Fs(ev)) => {
                if relevant(&ev.kind) {
                    touched.extend(ev.paths.into_iter().filter(|p| visible(p)));
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let changes = classify(touched.drain());
                if !changes.present.is_empty() || !changes.gone.is_empty() {
                    deliver(changes);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn relevant(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    }
}

/// Hidden files stay out, as in the initial scan.
fn visible(p: &Path) -> bool {
    p.file_name()
        .is_some_and(|n| !n.as_encoded_bytes().starts_with(b"."))
}

fn classify(paths: impl Iterator<Item = PathBuf>) -> Changes {
    let mut c = Changes::default();
    for p in paths {
        match std::fs::metadata(&p) {
            Ok(m) if m.is_file() => {
                if decode::is_known_extension(&p) || decode::sniff(&p) {
                    let t = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    c.present.push((p, t));
                }
            }
            Ok(_) => {}
            Err(_) => c.gone.push(p),
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_new_and_removed_files() {
        let dir = std::env::temp_dir().join(format!("remage-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        let _w = spawn(vec![(dir.clone(), false)], move |c| {
            let _ = tx.send(c);
        });
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        std::fs::write(dir.join(".hidden.png"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let c = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let names: Vec<_> = c
            .present
            .iter()
            .map(|(p, _)| p.file_name().unwrap())
            .collect();
        assert_eq!(names, ["a.png"]);
        std::fs::remove_file(dir.join("a.png")).unwrap();
        let c = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(c.gone, [dir.join("a.png")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
