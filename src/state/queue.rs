//! The file list: current index, ordering and navigation.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    clap::ValueEnum,
    serde::Deserialize,
    serde::Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    #[default]
    Natural,
    Name,
    Mtime,
    Size,
    Random,
}

#[derive(Debug, Default)]
pub struct Queue {
    files: Vec<PathBuf>,
    index: usize,
    /// Marked files, by path so reordering keeps them.
    marks: HashSet<PathBuf>,
}

impl Queue {
    /// Build a queue positioned on `start` if present, else on `start_index` (clamped).
    pub fn new(files: Vec<PathBuf>, start: Option<&Path>, start_index: usize) -> Self {
        let index = start
            .and_then(|s| files.iter().position(|f| f == s))
            .unwrap_or(start_index);
        let mut q = Self {
            files,
            index: 0,
            marks: HashSet::new(),
        };
        q.index = q.clamp(index);
        q
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn current(&self) -> Option<&Path> {
        self.files.get(self.index).map(PathBuf::as_path)
    }

    pub fn get(&self, i: usize) -> Option<&Path> {
        self.files.get(i).map(PathBuf::as_path)
    }

    /// Jump to `i` (clamped). Returns whether the current file changed.
    pub fn goto(&mut self, i: usize) -> bool {
        let i = self.clamp(i);
        let changed = i != self.index;
        self.index = i;
        changed
    }

    /// Move by `delta`, wrapping around the ends when `wrap` is set.
    pub fn step(&mut self, delta: isize, wrap: bool) -> bool {
        if self.files.is_empty() {
            return false;
        }
        let len = self.files.len() as isize;
        let target = self.index as isize + delta;
        let i = if wrap {
            target.rem_euclid(len)
        } else {
            target.clamp(0, len - 1)
        };
        self.goto(i as usize)
    }

    /// Remove `path`; the current position moves to the following file.
    pub fn remove(&mut self, path: &Path) -> bool {
        let Some(i) = self.files.iter().position(|f| f == path) else {
            return false;
        };
        let removed = self.files.remove(i);
        self.marks.remove(&removed);
        if i < self.index {
            self.index -= 1;
        }
        self.index = self.clamp(self.index);
        true
    }

    /// Insert paths in natural order, keeping the current file selected.
    pub fn insert_sorted(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        let current = self.current().map(Path::to_path_buf);
        for p in paths {
            if self.files.contains(&p) {
                continue;
            }
            let at = self
                .files
                .partition_point(|f| natural_cmp_path(f, &p) != Ordering::Greater);
            self.files.insert(at, p);
        }
        if let Some(cur) = current
            && let Some(i) = self.files.iter().position(|f| *f == cur)
        {
            self.index = i;
        }
    }

    /// Up to `n` neighbours on each side of the current file, nearest first.
    pub fn neighbours(&self, n: usize, wrap: bool) -> Vec<usize> {
        let len = self.files.len();
        let mut out = Vec::with_capacity(n * 2);
        for k in 1..=n {
            for delta in [k as isize, -(k as isize)] {
                let raw = self.index as isize + delta;
                let i = if wrap {
                    raw.rem_euclid(len as isize) as usize
                } else if (0..len as isize).contains(&raw) {
                    raw as usize
                } else {
                    continue;
                };
                if i != self.index && !out.contains(&i) {
                    out.push(i);
                }
            }
        }
        out
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// Reverse the order, keeping the current file selected.
    pub fn reverse(&mut self) {
        self.files.reverse();
        self.index = self.clamp(self.files.len().saturating_sub(1) - self.index);
    }

    /// Adopt a new order of the same files (a finished re-sort), keeping the current file.
    /// Files removed meanwhile stay removed; files added meanwhile go at the end.
    pub fn reorder(&mut self, order: Vec<PathBuf>) {
        let current = self.current().map(Path::to_path_buf);
        let have: HashSet<&PathBuf> = self.files.iter().collect();
        let mut files: Vec<PathBuf> = order.into_iter().filter(|f| have.contains(f)).collect();
        let placed: HashSet<&PathBuf> = files.iter().collect();
        let missing: Vec<PathBuf> = self
            .files
            .iter()
            .filter(|f| !placed.contains(f))
            .cloned()
            .collect();
        files.extend(missing);
        self.files = files;
        if let Some(cur) = current
            && let Some(i) = self.files.iter().position(|f| *f == cur)
        {
            self.index = i;
        }
    }

    pub fn is_marked(&self, i: usize) -> bool {
        self.files.get(i).is_some_and(|f| self.marks.contains(f))
    }

    pub fn marked_count(&self) -> usize {
        self.marks.len()
    }

    /// Mark or unmark file `i`. Returns whether anything changed.
    pub fn set_mark(&mut self, i: usize, on: bool) -> bool {
        let Some(f) = self.files.get(i) else {
            return false;
        };
        if on {
            self.marks.insert(f.clone())
        } else {
            self.marks.remove(f)
        }
    }

    pub fn mark_all(&mut self) {
        self.marks = self.files.iter().cloned().collect();
    }

    pub fn unmark_all(&mut self) {
        self.marks.clear();
    }

    pub fn invert_marks(&mut self) {
        self.marks = self
            .files
            .iter()
            .filter(|f| !self.marks.contains(*f))
            .cloned()
            .collect();
    }

    /// Marked files in queue order.
    #[allow(dead_code)] // `-o` and command targets arrive in M4
    pub fn marked(&self) -> Vec<&Path> {
        self.files
            .iter()
            .filter(|f| self.marks.contains(*f))
            .map(PathBuf::as_path)
            .collect()
    }

    /// The `n`th marked file after (`forward`) or before the current one.
    pub fn find_mark(&self, forward: bool, n: usize, wrap: bool) -> Option<usize> {
        let len = self.files.len();
        if self.marks.is_empty() || len == 0 {
            return None;
        }
        let mut found = None;
        let mut left = n.max(1);
        for k in 1..len {
            let i = if forward {
                let i = self.index + k;
                if i >= len && !wrap {
                    break;
                }
                i % len
            } else {
                if k > self.index && !wrap {
                    break;
                }
                (self.index + len - k) % len
            };
            if self.is_marked(i) {
                found = Some(i);
                left -= 1;
                if left == 0 {
                    break;
                }
            }
        }
        found
    }

    fn clamp(&self, i: usize) -> usize {
        i.min(self.files.len().saturating_sub(1))
    }
}

/// Sort `files` in place. `meta` supplies (mtime seconds, size) for the stat-based keys.
pub fn sort_files(
    files: &mut [PathBuf],
    key: SortKey,
    reverse: bool,
    meta: impl Fn(&Path) -> (i64, u64),
    seed: u64,
) {
    match key {
        SortKey::Natural => files.sort_by(|a, b| natural_cmp_path(a, b)),
        SortKey::Name => files.sort(),
        SortKey::Mtime => {
            files.sort_by_cached_key(|p| meta(p).0);
        }
        SortKey::Size => {
            files.sort_by_cached_key(|p| meta(p).1);
        }
        SortKey::Random => shuffle(files, seed),
    }
    if reverse {
        files.reverse();
    }
}

fn shuffle<T>(items: &mut [T], seed: u64) {
    // xorshift64*: plenty for shuffling a file list.
    let mut s = seed | 1;
    for i in (1..items.len()).rev() {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        let r = s.wrapping_mul(0x2545_F491_4F6C_DD1D);
        items.swap(i, (r % (i as u64 + 1)) as usize);
    }
}

pub fn natural_cmp_path(a: &Path, b: &Path) -> Ordering {
    natural_cmp(&a.to_string_lossy(), &b.to_string_lossy())
}

/// Compare strings with digit runs ordered numerically and letters case-insensitively,
/// so `img2` sorts before `img10` and `B` sits next to `b`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(&c) = it.peek().filter(|c| c.is_ascii_digit()) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (m, n) = (take(&mut x), take(&mut y));
                let (mt, nt) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                let ord = mt.len().cmp(&nt.len()).then_with(|| mt.cmp(nt));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(c), Some(d)) => {
                let ord = c.to_lowercase().cmp(d.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                x.next();
                y.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn natural_order() {
        let mut v = vec![
            "img10.png",
            "img2.png",
            "Img1.png",
            "img02.png",
            "a.png",
            "img1b.png",
        ];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            v,
            [
                "a.png",
                "Img1.png",
                "img1b.png",
                "img02.png",
                "img2.png",
                "img10.png"
            ]
        );
    }

    #[test]
    fn step_wraps_or_clamps() {
        let mut q = Queue::new(paths(&["a", "b", "c"]), None, 0);
        assert!(q.step(-1, true));
        assert_eq!(q.index(), 2);
        assert!(!q.step(1, false));
        assert!(q.step(-5, false));
        assert_eq!(q.index(), 0);
    }

    #[test]
    fn starts_on_named_file() {
        let q = Queue::new(paths(&["a", "b", "c"]), Some(Path::new("b")), 0);
        assert_eq!(q.current(), Some(Path::new("b")));
        let q = Queue::new(paths(&["a", "b"]), None, 9);
        assert_eq!(q.index(), 1);
    }

    #[test]
    fn remove_keeps_position() {
        let mut q = Queue::new(paths(&["a", "b", "c"]), Some(Path::new("b")), 0);
        q.remove(Path::new("a"));
        assert_eq!(q.current(), Some(Path::new("b")));
        q.remove(Path::new("b"));
        assert_eq!(q.current(), Some(Path::new("c")));
        q.remove(Path::new("c"));
        assert!(q.current().is_none());
    }

    #[test]
    fn insert_sorted_keeps_current() {
        let mut q = Queue::new(paths(&["img1", "img3"]), Some(Path::new("img3")), 0);
        q.insert_sorted(paths(&["img2", "img10", "img1"]));
        assert_eq!(q.len(), 4);
        assert_eq!(q.get(1), Some(Path::new("img2")));
        assert_eq!(q.get(3), Some(Path::new("img10")));
        assert_eq!(q.current(), Some(Path::new("img3")));
    }

    #[test]
    fn neighbours_nearest_first() {
        let q = Queue::new(paths(&["a", "b", "c", "d", "e"]), None, 0);
        assert_eq!(q.neighbours(2, false), vec![1, 2]);
        assert_eq!(q.neighbours(2, true), vec![1, 4, 2, 3]);
        let q = Queue::new(paths(&["a", "b"]), None, 0);
        assert_eq!(q.neighbours(2, true), vec![1]);
    }

    #[test]
    fn reverse_and_reorder_keep_current() {
        let mut q = Queue::new(paths(&["a", "b", "c", "d"]), Some(Path::new("b")), 0);
        q.reverse();
        assert_eq!(q.files(), paths(&["d", "c", "b", "a"]));
        assert_eq!(q.current(), Some(Path::new("b")));
        q.remove(Path::new("d"));
        q.reorder(paths(&["a", "b", "d", "c"]));
        assert_eq!(q.files(), paths(&["a", "b", "c"]));
        assert_eq!(q.current(), Some(Path::new("b")));
        q.reorder(paths(&["c"]));
        assert_eq!(q.files(), paths(&["c", "a", "b"]));
        assert_eq!(q.current(), Some(Path::new("b")));
        let mut empty = Queue::default();
        empty.reverse();
        assert!(empty.is_empty());
    }

    #[test]
    fn marks_follow_files() {
        let mut q = Queue::new(paths(&["a", "b", "c", "d"]), None, 0);
        assert!(q.set_mark(1, true));
        assert!(q.set_mark(3, true));
        assert!(!q.set_mark(3, true));
        assert_eq!(q.marked_count(), 2);
        q.reverse();
        assert!(q.is_marked(0) && q.is_marked(2));
        assert_eq!(q.marked(), [Path::new("d"), Path::new("b")]);
        q.invert_marks();
        assert_eq!(q.marked(), [Path::new("c"), Path::new("a")]);
        q.remove(Path::new("c"));
        assert_eq!(q.marked_count(), 1);
        q.mark_all();
        assert_eq!(q.marked_count(), 3);
        q.unmark_all();
        assert_eq!(q.marked_count(), 0);
    }

    #[test]
    fn finding_marks() {
        let mut q = Queue::new(paths(&["a", "b", "c", "d", "e"]), None, 2);
        assert_eq!(q.find_mark(true, 1, true), None);
        q.set_mark(0, true);
        q.set_mark(4, true);
        assert_eq!(q.find_mark(true, 1, true), Some(4));
        assert_eq!(q.find_mark(true, 2, true), Some(0));
        assert_eq!(q.find_mark(false, 1, true), Some(0));
        assert_eq!(q.find_mark(false, 2, false), Some(0));
        q.goto(4);
        assert_eq!(q.find_mark(true, 1, false), None);
        assert_eq!(q.find_mark(true, 1, true), Some(0));
        // The current file alone is not a next mark.
        q.unmark_all();
        q.set_mark(4, true);
        assert_eq!(q.find_mark(true, 1, true), None);
    }

    #[test]
    fn random_sort_is_a_permutation() {
        let mut v = paths(&["a", "b", "c", "d", "e"]);
        sort_files(&mut v, SortKey::Random, false, |_| (0, 0), 42);
        let mut s = v.clone();
        s.sort();
        assert_eq!(s, paths(&["a", "b", "c", "d", "e"]));
    }
}
