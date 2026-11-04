//! Turn command-line paths into the initial file list.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::decode;
use crate::state::queue::{SortKey, sort_files};

#[derive(Debug, Default)]
pub struct Scan {
    pub files: Vec<PathBuf>,
    /// The file to open first (a single file argument opened with its siblings).
    pub start: Option<PathBuf>,
    /// Files with unrecognised extensions, to sniff in the background.
    pub unknown: Vec<PathBuf>,
    pub errors: Vec<String>,
}

pub struct ScanOptions {
    pub recursive: bool,
    pub siblings: bool,
    pub sort: SortKey,
    pub reverse: bool,
}

pub fn scan(args: &[PathBuf], opts: &ScanOptions) -> Scan {
    let dot = [PathBuf::from(".")];
    let args = if args.is_empty() { &dot[..] } else { args };
    let mut out = Scan::default();
    for arg in args {
        let path = std::path::absolute(arg).unwrap_or_else(|_| arg.clone());
        let Ok(meta) = std::fs::metadata(&path) else {
            out.errors.push(format!("{}: not found", arg.display()));
            continue;
        };
        if meta.is_dir() {
            let mut files = Vec::new();
            list_dir(&path, opts.recursive, &mut files, &mut out.unknown);
            sort(&mut files, opts);
            out.files.extend(files);
        } else if opts.siblings && args.len() == 1 {
            let parent = path.parent().unwrap_or(Path::new("/"));
            let mut files = Vec::new();
            list_dir(parent, false, &mut files, &mut out.unknown);
            out.unknown.retain(|u| *u != path);
            if !files.contains(&path) {
                files.push(path.clone());
            }
            sort(&mut files, opts);
            out.files.extend(files);
            out.start = Some(path);
        } else {
            // Explicitly named files are always attempted, whatever their extension.
            out.files.push(path);
        }
    }
    out
}

fn sort(files: &mut [PathBuf], opts: &ScanOptions) {
    let meta = |p: &Path| {
        std::fs::metadata(p).map_or((0, 0), |m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            (mtime, m.len())
        })
    };
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64);
    sort_files(files, opts.sort, opts.reverse, meta, seed);
}

/// Collect visible files under `dir`. Symlinked directories are not followed.
fn list_dir(dir: &Path, recursive: bool, known: &mut Vec<PathBuf>, unknown: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        log::warn!("cannot read {}", dir.display());
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().as_encoded_bytes().starts_with(b".") {
            continue;
        }
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if recursive {
                list_dir(&path, true, known, unknown);
            }
            continue;
        }
        if ft.is_symlink() && std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        if decode::is_known_extension(&path) {
            known.push(path);
        } else {
            unknown.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ScanOptions {
        ScanOptions {
            recursive: false,
            siblings: true,
            sort: SortKey::Natural,
            reverse: false,
        }
    }

    #[test]
    fn single_file_opens_with_siblings() {
        let dir = std::env::temp_dir().join(format!("remage-scan-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for f in ["b10.png", "b2.jpg", ".hidden.png", "notes.txt", "sub/c.png"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let s = scan(&[dir.join("b10.png")], &opts());
        let names: Vec<_> = s.files.iter().map(|p| p.file_name().unwrap()).collect();
        assert_eq!(names, ["b2.jpg", "b10.png"]);
        assert_eq!(s.start.as_deref(), Some(dir.join("b10.png").as_path()));
        assert_eq!(s.unknown, [dir.join("notes.txt")]);

        let s = scan(
            std::slice::from_ref(&dir),
            &ScanOptions {
                recursive: true,
                ..opts()
            },
        );
        assert_eq!(s.files.len(), 3);
        assert!(s.start.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
