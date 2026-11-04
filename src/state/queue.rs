//! The file list: current index, ordering and navigation.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::fuzzy::Fuzzy;

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
    /// Bumped on every change to the marks, so watchers can spot one.
    marks_version: u64,
    /// `filter <pattern>`: a glob or a plain substring of the file name.
    filter: Option<String>,
    /// Live `/` search: fuzzy over file names.
    search: Option<String>,
    /// Every file, in order, while a filter or search hides some (`files` is then the subset).
    full: Option<Vec<PathBuf>>,
    /// The current file when a filter left nothing shown, to return to afterwards.
    lost: Option<PathBuf>,
}

/// Decides which files a filter and search let through.
struct Matcher {
    /// Lower-cased pattern and whether it is a glob.
    pattern: Option<(String, bool)>,
    fuzzy: Option<Fuzzy>,
}

impl Matcher {
    fn is_match(&mut self, path: &Path) -> bool {
        let name = path
            .file_name()
            .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy());
        if let Some((pat, glob)) = &self.pattern {
            let lower = name.to_lowercase();
            let ok = if *glob {
                glob_match(pat, &lower)
            } else {
                lower.contains(pat.as_str())
            };
            if !ok {
                return false;
            }
        }
        self.fuzzy.as_mut().is_none_or(|f| f.score(&name).is_some())
    }
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
            marks_version: 0,
            filter: None,
            search: None,
            full: None,
            lost: None,
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
        let hidden = self.full.as_mut().is_some_and(|full| {
            full.iter()
                .position(|f| f == path)
                .map(|i| full.remove(i))
                .is_some()
        });
        let Some(i) = self.files.iter().position(|f| f == path) else {
            if hidden && self.marks.remove(path) {
                self.marks_version += 1;
            }
            return hidden;
        };
        let removed = self.files.remove(i);
        if self.marks.remove(&removed) {
            self.marks_version += 1;
        }
        if i < self.index {
            self.index -= 1;
        }
        self.index = self.clamp(self.index);
        true
    }

    /// Insert paths in natural order, keeping the current file selected.
    pub fn insert_sorted(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        let current = self.current().map(Path::to_path_buf);
        let mut m = self.matcher();
        let insert = |list: &mut Vec<PathBuf>, p: &PathBuf| {
            if !list.contains(p) {
                let at = list.partition_point(|f| natural_cmp_path(f, p) != Ordering::Greater);
                list.insert(at, p.clone());
            }
        };
        for p in paths {
            if let Some(full) = &mut self.full {
                insert(full, &p);
            }
            if m.as_mut().is_none_or(|m| m.is_match(&p)) {
                insert(&mut self.files, &p);
            }
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

    /// The files shown: all of them, or those a filter or search lets through.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// Every file, including those a filter hides.
    pub fn all_files(&self) -> &[PathBuf] {
        self.full.as_deref().unwrap_or(&self.files)
    }

    pub fn filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    /// Show only files whose name matches `pattern`: a glob (`*.png`) when it has `*`, `?`
    /// or `[`, otherwise a substring; case-insensitive either way. Returns whether the
    /// current file changed.
    pub fn set_filter(&mut self, pattern: Option<String>) -> bool {
        self.filter = pattern.filter(|p| !p.is_empty());
        self.refilter()
    }

    /// Narrow the files to fuzzy matches of `query` (the live `/` search).
    pub fn set_search(&mut self, query: Option<String>) -> bool {
        self.search = query.filter(|q| !q.is_empty());
        self.refilter()
    }

    fn matcher(&self) -> Option<Matcher> {
        if self.filter.is_none() && self.search.is_none() {
            return None;
        }
        Some(Matcher {
            pattern: self.filter.as_ref().map(|p| {
                let glob = p.contains(['*', '?', '[']);
                (p.to_lowercase(), glob)
            }),
            fuzzy: self.search.as_deref().map(Fuzzy::for_paths),
        })
    }

    /// Recompute the shown files. The current file stays selected if it is still shown,
    /// otherwise the next shown one after it. Returns whether the current file changed.
    fn refilter(&mut self) -> bool {
        let cur = self
            .current()
            .map(Path::to_path_buf)
            .or_else(|| self.lost.take());
        let Some(mut m) = self.matcher() else {
            if let Some(full) = self.full.take() {
                self.files = full;
            }
            return self.reselect(cur.as_deref());
        };
        let full = self.full.get_or_insert_with(|| self.files.clone());
        self.files = full.iter().filter(|p| m.is_match(p)).cloned().collect();
        self.reselect(cur.as_deref())
    }

    /// Select `cur`, or the first shown file after it in the full order.
    fn reselect(&mut self, cur: Option<&Path>) -> bool {
        let Some(cur) = cur else {
            self.index = 0;
            return !self.files.is_empty();
        };
        if self.files.is_empty() {
            self.index = 0;
            self.lost = Some(cur.to_path_buf());
            return true;
        }
        let all = self.all_files();
        let from = all.iter().position(|f| f == cur).unwrap_or(0);
        let shown: HashSet<&PathBuf> = self.files.iter().collect();
        let next = all[from..]
            .iter()
            .chain(all[..from].iter().rev())
            .find(|f| shown.contains(f))
            .cloned();
        let i = next
            .and_then(|n| self.files.iter().position(|f| *f == n))
            .unwrap_or(0);
        self.index = self.clamp(i);
        self.current() != Some(cur)
    }

    /// Reverse the order, keeping the current file selected.
    pub fn reverse(&mut self) {
        if let Some(full) = &mut self.full {
            full.reverse();
        }
        self.files.reverse();
        self.index = self.clamp(self.files.len().saturating_sub(1) - self.index);
    }

    /// Adopt a new order of the same files (a finished re-sort), keeping the current file.
    /// Files removed meanwhile stay removed; files added meanwhile go at the end.
    pub fn reorder(&mut self, order: Vec<PathBuf>) {
        let current = self.current().map(Path::to_path_buf);
        if let Some(full) = &mut self.full {
            *full = reordered(full, &order);
        }
        self.files = reordered(&self.files, &order);
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

    pub fn marks_version(&self) -> u64 {
        self.marks_version
    }

    /// Mark or unmark file `i`. Returns whether anything changed.
    pub fn set_mark(&mut self, i: usize, on: bool) -> bool {
        let Some(f) = self.files.get(i) else {
            return false;
        };
        let changed = if on {
            self.marks.insert(f.clone())
        } else {
            self.marks.remove(f)
        };
        self.marks_version += changed as u64;
        changed
    }

    /// Mark every shown file.
    pub fn mark_all(&mut self) {
        self.marks.extend(self.files.iter().cloned());
        self.marks_version += 1;
    }

    pub fn unmark_all(&mut self) {
        self.marks.clear();
        self.marks_version += 1;
    }

    /// Invert the marks of the shown files.
    pub fn invert_marks(&mut self) {
        self.marks_version += 1;
        for f in &self.files {
            if !self.marks.remove(f) {
                self.marks.insert(f.clone());
            }
        }
    }

    /// Marked files in queue order, including any a filter hides.
    pub fn marked(&self) -> Vec<&Path> {
        self.all_files()
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

/// `list` in the order of `order`; files `order` lacks go at the end, files `list` lacks are
/// dropped.
fn reordered(list: &[PathBuf], order: &[PathBuf]) -> Vec<PathBuf> {
    let have: HashSet<&PathBuf> = list.iter().collect();
    let mut out: Vec<PathBuf> = order.iter().filter(|f| have.contains(f)).cloned().collect();
    let placed: HashSet<&PathBuf> = out.iter().collect();
    let missing: Vec<PathBuf> = list
        .iter()
        .filter(|f| !placed.contains(f))
        .cloned()
        .collect();
    out.extend(missing);
    out
}

/// Shell-style wildcard match: `*`, `?` and `[abc]` / `[a-z]` / `[!x]`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    // Iterative matching with backtracking to the last `*`.
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        let step = match p.get(pi) {
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
                continue;
            }
            Some('?') => Some(1),
            Some('[') => class(&p[pi..], t[ti]),
            Some(c) if *c == t[ti] => Some(1),
            _ => None,
        };
        match step {
            Some(n) if p[pi] == '[' => {
                pi += n;
                ti += 1;
            }
            Some(_) => {
                pi += 1;
                ti += 1;
            }
            None => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Match `c` against the class at the start of `p` (`[...]`). Returns the class's length if
/// it matches. An unclosed `[` matches itself literally.
fn class(p: &[char], c: char) -> Option<usize> {
    let Some(end) = p.iter().skip(2).position(|x| *x == ']').map(|i| i + 2) else {
        return (c == '[').then_some(1);
    };
    let body = &p[1..end];
    let (neg, body) = match body.first() {
        Some('!' | '^') => (true, &body[1..]),
        _ => (false, body),
    };
    let mut hit = false;
    let mut i = 0;
    while i < body.len() {
        if i + 2 < body.len() && body[i + 1] == '-' {
            hit |= (body[i]..=body[i + 2]).contains(&c);
            i += 3;
        } else {
            hit |= body[i] == c;
            i += 1;
        }
    }
    (hit != neg).then_some(end + 1)
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
        let v = q.marks_version();
        assert!(!q.set_mark(3, true));
        assert_eq!(q.marks_version(), v);
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
    fn globs() {
        assert!(glob_match("*.png", "a.png"));
        assert!(!glob_match("*.png", "a.png.jpg"));
        assert!(glob_match("img?.*", "img1.jpg"));
        assert!(glob_match("[a-c]*", "beach.jpg"));
        assert!(!glob_match("[!a-c]*", "beach.jpg"));
        assert!(glob_match("*a*b*", "xxaYYbzz"));
        assert!(glob_match("[", "["));
        assert!(glob_match("", ""));
        assert!(!glob_match("", "a"));
    }

    #[test]
    fn filters_hide_and_restore() {
        let mut q = Queue::new(
            paths(&["a.png", "b.jpg", "c.png", "d.jpg"]),
            Some(Path::new("b.jpg")),
            0,
        );
        q.set_mark(1, true);
        // The current file is hidden: the next shown one after it is selected.
        assert!(q.set_filter(Some("*.PNG".into())));
        assert_eq!(q.files(), paths(&["a.png", "c.png"]));
        assert_eq!(q.current(), Some(Path::new("c.png")));
        assert_eq!(q.all_files().len(), 4);
        // Hidden marks still count and print.
        assert_eq!(q.marked(), [Path::new("b.jpg")]);
        q.insert_sorted(paths(&["e.png", "f.jpg"]));
        assert_eq!(q.files(), paths(&["a.png", "c.png", "e.png"]));
        q.remove(Path::new("f.jpg"));
        q.remove(Path::new("b.jpg"));
        assert_eq!(q.marked_count(), 0);
        // Search narrows within the filter; substring filters are plain text.
        q.set_search(Some("ep".into()));
        assert_eq!(q.files(), paths(&["e.png"]));
        q.set_search(None);
        q.set_filter(Some("c".into()));
        assert_eq!(q.files(), paths(&["c.png"]));
        assert!(!q.set_filter(None));
        assert_eq!(q.files(), paths(&["a.png", "c.png", "d.jpg", "e.png"]));
        assert_eq!(q.current(), Some(Path::new("c.png")));
        // Nothing matches: an empty view, then everything back.
        q.set_filter(Some("zzz".into()));
        assert!(q.is_empty());
        q.set_filter(None);
        assert_eq!(q.current(), Some(Path::new("c.png")));
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
