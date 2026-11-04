//! Overlay state: the `:` palette, `/` search, which-key, help and the info panel.

use std::path::PathBuf;

use super::fuzzy;

/// Rows shown at most in the palette and the image-mode search list.
pub const LIST_ROWS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Palette,
    Search,
}

/// One palette row: an action (or a template like `goto N` to complete), what it does and
/// the keys bound to it.
#[derive(Clone, Debug, PartialEq)]
pub struct PaletteItem {
    pub action: String,
    pub desc: String,
    pub keys: String,
    /// `action` has a placeholder argument (`goto N`): `<CR>` completes it instead of running.
    pub template: bool,
}

impl PaletteItem {
    /// What `<Tab>` puts on the line: the action, or a template's name and a space.
    pub fn completion(&self) -> String {
        if self.template {
            let name = self.action.split_whitespace().next().unwrap_or("");
            format!("{name} ")
        } else {
            self.action.clone()
        }
    }
}

/// An edit to the prompt line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Insert(String),
    Backspace,
    DeleteWord,
    Clear,
    /// Towards worse matches (drawn above), or the next file in the grid.
    Up,
    Down,
    Left,
    Right,
    /// Complete the selected palette item into the line.
    Complete,
}

/// The prompt line and what it matches.
#[derive(Debug)]
pub struct Input {
    pub kind: InputKind,
    pub text: String,
    pub items: Vec<PaletteItem>,
    /// Matching palette items, or queue indices in image-mode search; best first.
    pub matches: Vec<usize>,
    pub selected: usize,
    /// The selection was moved by hand, so `<CR>` takes it over the typed text.
    pub moved: bool,
    /// The file and filter before a search, for `<Esc>`.
    pub origin: Option<PathBuf>,
}

impl Input {
    pub fn new(kind: InputKind, items: Vec<PaletteItem>, origin: Option<PathBuf>) -> Self {
        let mut i = Self {
            kind,
            text: String::new(),
            items,
            matches: Vec::new(),
            selected: 0,
            moved: false,
            origin,
        };
        i.rank_items();
        i
    }

    pub fn prefix(&self) -> &'static str {
        match self.kind {
            InputKind::Palette => ":",
            InputKind::Search => "/",
        }
    }

    /// Re-rank palette items against the text.
    pub fn rank_items(&mut self) {
        if self.kind != InputKind::Palette {
            return;
        }
        let hay: Vec<String> = self
            .items
            .iter()
            .map(|i| format!("{} {}", i.action, i.desc))
            .collect();
        self.matches = fuzzy::rank(&self.text, hay.iter().map(String::as_str));
        self.selected = 0;
        self.moved = false;
    }

    /// Apply a text edit. Returns whether the text changed.
    pub fn edit_text(&mut self, e: &Edit) -> bool {
        let before = self.text.len();
        match e {
            Edit::Insert(s) => self.text.extend(s.chars().filter(|c| !c.is_control())),
            Edit::Backspace => {
                self.text.pop();
            }
            Edit::DeleteWord => {
                let t = self.text.trim_end();
                let cut = t.rfind(char::is_whitespace).map_or(0, |i| i + 1);
                self.text.truncate(cut);
            }
            Edit::Clear => self.text.clear(),
            _ => return false,
        }
        self.text.len() != before || matches!(e, Edit::Insert(_))
    }

    /// Move the selection through the match list.
    pub fn select(&mut self, up: bool) {
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        self.selected = if up {
            (self.selected + 1).min(n - 1)
        } else {
            self.selected.saturating_sub(1)
        };
        self.moved = true;
    }

    pub fn selected_item(&self) -> Option<&PaletteItem> {
        self.matches
            .get(self.selected)
            .and_then(|&i| self.items.get(i))
    }

    /// The first row of the visible window of matches, keeping the selection in view.
    pub fn window_start(&self) -> usize {
        self.selected.saturating_sub(LIST_ROWS - 1)
    }
}

/// One which-key row.
#[derive(Clone, Debug, PartialEq)]
pub struct WhichRow {
    pub key: String,
    pub desc: String,
    /// More keys follow this one.
    pub prefix: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HelpRow {
    Header(String),
    Binding { keys: String, desc: String },
}

#[derive(Debug)]
pub struct Help {
    pub rows: Vec<HelpRow>,
    /// First row shown.
    pub scroll: usize,
}

impl Help {
    /// Scroll by `delta` rows, keeping `page` rows on screen.
    pub fn scroll_by(&mut self, delta: isize, page: usize) {
        let max = self.rows.len().saturating_sub(page);
        self.scroll = (self.scroll as isize + delta).clamp(0, max as isize) as usize;
    }
}

/// Metadata rows of a file for the info panel.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub path: PathBuf,
    pub rows: Vec<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<PaletteItem> {
        ["next", "next-mark", "goto N", "help"]
            .iter()
            .map(|a| PaletteItem {
                action: a.to_string(),
                desc: String::new(),
                keys: String::new(),
                template: a.contains('N'),
            })
            .collect()
    }

    #[test]
    fn typing_ranks_and_selects() {
        let mut i = Input::new(InputKind::Palette, items(), None);
        assert_eq!(i.matches.len(), 4);
        assert!(i.edit_text(&Edit::Insert("nxt".into())));
        i.rank_items();
        assert_eq!(i.selected_item().unwrap().action, "next");
        i.select(true);
        assert_eq!(i.selected_item().unwrap().action, "next-mark");
        assert!(i.moved);
        i.select(true);
        assert_eq!(i.selected, 1, "clamped to the matches");
        i.edit_text(&Edit::Insert(" more words".into()));
        i.edit_text(&Edit::DeleteWord);
        assert_eq!(i.text, "nxt more ");
        i.edit_text(&Edit::Clear);
        assert!(!i.edit_text(&Edit::Backspace));
        assert_eq!(i.items[2].completion(), "goto ");
        assert_eq!(i.items[1].completion(), "next-mark");
    }

    #[test]
    fn help_scroll_clamps() {
        let mut h = Help {
            rows: vec![HelpRow::Header("X".into()); 10],
            scroll: 0,
        };
        h.scroll_by(-3, 4);
        assert_eq!(h.scroll, 0);
        h.scroll_by(100, 4);
        assert_eq!(h.scroll, 6);
    }
}
